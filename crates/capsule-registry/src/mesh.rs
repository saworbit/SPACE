use crate::consensus::MetadataStateMachine;
use crate::metadata_ops::{MetadataOp, OpResult};
use crate::raft_rpc;
use crate::store::MetadataStore;
use crate::store::SledStore;
use anyhow::{anyhow, Context, Result};
use openraft::entry::RaftPayload;
use openraft::error::{ClientWriteError, NetworkError, RPCError, RaftError, RemoteError};
use openraft::network::{RPCOption, RaftNetwork, RaftNetworkFactory};
use openraft::raft::{
    AppendEntriesRequest, AppendEntriesResponse, InstallSnapshotRequest, InstallSnapshotResponse,
    VoteRequest, VoteResponse,
};
use openraft::storage::LogFlushed;
use openraft::storage::{LogState, RaftLogStorage, RaftStateMachine};
use openraft::{
    BasicNode, Entry, EntryPayload, LogId, Raft, Snapshot, SnapshotMeta, StorageError,
    StorageIOError, StoredMembership, Vote,
};
use std::collections::BTreeSet;
use std::io::Cursor;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::sync::{Mutex, RwLock};
use tokio::time::{sleep, Duration};
use tonic::{Request, Response, Status};
use tracing::info;

openraft::declare_raft_types!(
    pub RegistryRaftConfig:
        D = MetadataOp,
        R = OpResult,
);

pub type RegistryRaft = Raft<RegistryRaftConfig>;
pub type RegistryNodeId = <RegistryRaftConfig as openraft::RaftTypeConfig>::NodeId;

type SnapshotCache = Option<(SnapshotMeta<RegistryNodeId, BasicNode>, Vec<u8>)>;

const META_VOTE_KEY: &[u8] = b"raft_vote";
const META_LAST_PURGED_KEY: &[u8] = b"raft_last_purged";

const SM_LAST_APPLIED_KEY: &[u8] = b"sm_last_applied";
const SM_LAST_MEMBERSHIP_KEY: &[u8] = b"sm_last_membership";
const SM_SNAPSHOT_META_KEY: &[u8] = b"sm_snapshot_meta";
const SM_SNAPSHOT_DATA_KEY: &[u8] = b"sm_snapshot_data";

fn now_ts() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn sto_err_write_logs<NID: openraft::NodeId>(e: impl std::fmt::Display) -> StorageError<NID> {
    let io_err = std::io::Error::other(e.to_string());
    StorageError::IO {
        source: StorageIOError::write_logs(&io_err),
    }
}

fn sto_err_read_logs<NID: openraft::NodeId>(e: impl std::fmt::Display) -> StorageError<NID> {
    let io_err = std::io::Error::other(e.to_string());
    StorageError::IO {
        source: StorageIOError::read_logs(&io_err),
    }
}

fn sto_err_read_sm<NID: openraft::NodeId>(e: impl std::fmt::Display) -> StorageError<NID> {
    let io_err = std::io::Error::other(e.to_string());
    StorageError::IO {
        source: StorageIOError::read_state_machine(&io_err),
    }
}

fn sto_err_write_sm<NID: openraft::NodeId>(e: impl std::fmt::Display) -> StorageError<NID> {
    let io_err = std::io::Error::other(e.to_string());
    StorageError::IO {
        source: StorageIOError::write_state_machine(&io_err),
    }
}

fn index_key(idx: u64) -> [u8; 8] {
    idx.to_be_bytes()
}

#[derive(Clone)]
pub struct RegistryLogReader {
    logs: sled::Tree,
}

impl openraft::RaftLogReader<RegistryRaftConfig> for RegistryLogReader {
    async fn try_get_log_entries<
        RB: std::ops::RangeBounds<u64> + Clone + std::fmt::Debug + openraft::OptionalSend,
    >(
        &mut self,
        range: RB,
    ) -> Result<Vec<Entry<RegistryRaftConfig>>, StorageError<RegistryNodeId>> {
        let start = match range.start_bound() {
            std::ops::Bound::Included(v) => *v,
            std::ops::Bound::Excluded(v) => v.saturating_add(1),
            std::ops::Bound::Unbounded => 0,
        };

        let end = match range.end_bound() {
            std::ops::Bound::Included(v) => v.saturating_add(1),
            std::ops::Bound::Excluded(v) => *v,
            std::ops::Bound::Unbounded => {
                let last = self
                    .logs
                    .last()
                    .map_err(sto_err_read_logs::<RegistryNodeId>)?
                    .and_then(|(k, _)| {
                        let mut arr = [0u8; 8];
                        if k.len() == 8 {
                            arr.copy_from_slice(&k);
                            Some(u64::from_be_bytes(arr))
                        } else {
                            None
                        }
                    })
                    .unwrap_or(0);
                last.saturating_add(1)
            }
        };

        let mut out = Vec::new();
        for idx in start..end {
            if let Some(bytes) = self
                .logs
                .get(index_key(idx))
                .map_err(sto_err_read_logs::<RegistryNodeId>)?
            {
                let entry: Entry<RegistryRaftConfig> =
                    bincode::deserialize(&bytes).map_err(sto_err_read_logs::<RegistryNodeId>)?;
                out.push(entry);
            }
        }
        Ok(out)
    }
}

impl openraft::RaftLogReader<RegistryRaftConfig> for RegistryLogStore {
    async fn try_get_log_entries<
        RB: std::ops::RangeBounds<u64> + Clone + std::fmt::Debug + openraft::OptionalSend,
    >(
        &mut self,
        range: RB,
    ) -> Result<Vec<Entry<RegistryRaftConfig>>, StorageError<RegistryNodeId>> {
        let mut reader = RegistryLogReader {
            logs: self.logs.clone(),
        };
        reader.try_get_log_entries(range).await
    }
}

pub struct RegistryLogStore {
    logs: sled::Tree,
    meta: sled::Tree,
    write_lock: Arc<Mutex<()>>,
}

impl RegistryLogStore {
    pub fn open(path: &str) -> Result<Self> {
        let db = sled::open(path)?;
        let logs = db.open_tree("raft_logs")?;
        let meta = db.open_tree("raft_meta")?;

        Ok(Self {
            logs,
            meta,
            write_lock: Arc::new(Mutex::new(())),
        })
    }

    fn read_meta<T: serde::de::DeserializeOwned>(&self, key: &[u8]) -> Result<Option<T>> {
        match self.meta.get(key)? {
            Some(v) => Ok(Some(bincode::deserialize(&v)?)),
            None => Ok(None),
        }
    }

    async fn write_meta<T: serde::Serialize>(&self, key: &[u8], value: &T) -> Result<()> {
        let bytes = bincode::serialize(value)?;
        self.meta.insert(key, bytes)?;
        self.meta.flush_async().await?;
        Ok(())
    }
}

impl RaftLogStorage<RegistryRaftConfig> for RegistryLogStore {
    type LogReader = RegistryLogReader;

    async fn get_log_state(
        &mut self,
    ) -> Result<LogState<RegistryRaftConfig>, StorageError<RegistryNodeId>> {
        let last_purged_log_id: Option<LogId<RegistryNodeId>> = self
            .read_meta(META_LAST_PURGED_KEY)
            .map_err(sto_err_read_logs::<RegistryNodeId>)?;

        let last_log_id = match self
            .logs
            .last()
            .map_err(sto_err_read_logs::<RegistryNodeId>)?
        {
            Some((_k, v)) => {
                let entry: Entry<RegistryRaftConfig> =
                    bincode::deserialize(&v).map_err(sto_err_read_logs::<RegistryNodeId>)?;
                Some(entry.log_id)
            }
            None => last_purged_log_id,
        };

        Ok(LogState {
            last_purged_log_id,
            last_log_id,
        })
    }

    async fn get_log_reader(&mut self) -> Self::LogReader {
        RegistryLogReader {
            logs: self.logs.clone(),
        }
    }

    async fn save_vote(
        &mut self,
        vote: &Vote<RegistryNodeId>,
    ) -> Result<(), StorageError<RegistryNodeId>> {
        let _guard = self.write_lock.lock().await;
        self.write_meta(META_VOTE_KEY, vote)
            .await
            .map_err(sto_err_write_logs::<RegistryNodeId>)?;
        Ok(())
    }

    async fn read_vote(
        &mut self,
    ) -> Result<Option<Vote<RegistryNodeId>>, StorageError<RegistryNodeId>> {
        self.read_meta(META_VOTE_KEY)
            .map_err(sto_err_read_logs::<RegistryNodeId>)
    }

    async fn append<I>(
        &mut self,
        entries: I,
        callback: LogFlushed<RegistryRaftConfig>,
    ) -> Result<(), StorageError<RegistryNodeId>>
    where
        I: IntoIterator<Item = Entry<RegistryRaftConfig>> + openraft::OptionalSend,
        I::IntoIter: openraft::OptionalSend,
    {
        let _guard = self.write_lock.lock().await;
        for entry in entries {
            let idx = entry.log_id.index;
            let bytes = bincode::serialize(&entry).map_err(sto_err_write_logs::<RegistryNodeId>)?;
            self.logs
                .insert(index_key(idx), bytes)
                .map_err(sto_err_write_logs::<RegistryNodeId>)?;
        }

        self.logs
            .flush_async()
            .await
            .map_err(sto_err_write_logs::<RegistryNodeId>)?;

        callback.log_io_completed(Ok(()));
        Ok(())
    }

    async fn truncate(
        &mut self,
        log_id: LogId<RegistryNodeId>,
    ) -> Result<(), StorageError<RegistryNodeId>> {
        let _guard = self.write_lock.lock().await;

        let start = log_id.index;
        let mut keys: Vec<u64> = Vec::new();
        for item in self.logs.range(index_key(start)..).keys() {
            let k = item.map_err(sto_err_write_logs::<RegistryNodeId>)?;
            if k.len() == 8 {
                let mut arr = [0u8; 8];
                arr.copy_from_slice(&k);
                keys.push(u64::from_be_bytes(arr));
            }
        }
        keys.sort_unstable_by(|a, b| b.cmp(a));
        for idx in keys {
            self.logs
                .remove(index_key(idx))
                .map_err(sto_err_write_logs::<RegistryNodeId>)?;
        }

        self.logs
            .flush_async()
            .await
            .map_err(sto_err_write_logs::<RegistryNodeId>)?;
        Ok(())
    }

    async fn purge(
        &mut self,
        log_id: LogId<RegistryNodeId>,
    ) -> Result<(), StorageError<RegistryNodeId>> {
        let _guard = self.write_lock.lock().await;
        let end = log_id.index;

        let mut keys: Vec<u64> = Vec::new();
        for item in self.logs.range(..=index_key(end)).keys() {
            let k = item.map_err(sto_err_write_logs::<RegistryNodeId>)?;
            if k.len() == 8 {
                let mut arr = [0u8; 8];
                arr.copy_from_slice(&k);
                keys.push(u64::from_be_bytes(arr));
            }
        }
        keys.sort_unstable();
        for idx in keys {
            self.logs
                .remove(index_key(idx))
                .map_err(sto_err_write_logs::<RegistryNodeId>)?;
        }

        self.write_meta(META_LAST_PURGED_KEY, &Some(log_id))
            .await
            .map_err(sto_err_write_logs::<RegistryNodeId>)?;

        self.logs
            .flush_async()
            .await
            .map_err(sto_err_write_logs::<RegistryNodeId>)?;
        Ok(())
    }
}

#[derive(Clone)]
pub struct RegistryStateMachine {
    fsm: MetadataStateMachine,
    meta: sled::Tree,
    snapshot_cache: Arc<RwLock<SnapshotCache>>,
}

impl RegistryStateMachine {
    pub fn open(store: Arc<dyn MetadataStore>, raft_path: &str) -> Result<Self> {
        let fsm = MetadataStateMachine::new(store);

        // NOTE: On Windows (and on some filesystems), opening the same sled DB path more than once
        // in a single process fails due to file locking. The Raft log store and state machine are
        // independent, so keep their on-disk metadata in separate sled directories.
        let raft_sm_path = std::path::PathBuf::from(raft_path).join("sm");
        let raft_db = sled::open(raft_sm_path)?;
        let meta = raft_db.open_tree("raft_sm")?;
        Ok(Self {
            fsm,
            meta,
            snapshot_cache: Arc::new(RwLock::new(None)),
        })
    }

    fn read_meta<T: serde::de::DeserializeOwned>(&self, key: &[u8]) -> Result<Option<T>> {
        match self.meta.get(key)? {
            Some(v) => Ok(Some(bincode::deserialize(&v)?)),
            None => Ok(None),
        }
    }

    async fn write_meta<T: serde::Serialize>(&self, key: &[u8], value: &T) -> Result<()> {
        let bytes = bincode::serialize(value)?;
        self.meta.insert(key, bytes)?;
        self.meta.flush_async().await?;
        Ok(())
    }
}

#[derive(Clone)]
pub struct RegistrySnapshotBuilder {
    sm: RegistryStateMachine,
}

impl openraft::RaftSnapshotBuilder<RegistryRaftConfig> for RegistrySnapshotBuilder {
    async fn build_snapshot(
        &mut self,
    ) -> Result<Snapshot<RegistryRaftConfig>, StorageError<RegistryNodeId>> {
        let (last_applied, last_membership) = self.sm.applied_state().await?;

        let bytes = tokio::task::spawn_blocking({
            let sm = self.sm.clone();
            move || sm.fsm.snapshot()
        })
        .await
        .map_err(sto_err_read_sm::<RegistryNodeId>)?
        .map_err(sto_err_read_sm::<RegistryNodeId>)?;

        let snapshot_id = format!("snap-{}", now_ts());
        let meta = SnapshotMeta {
            last_log_id: last_applied,
            last_membership: last_membership.clone(),
            snapshot_id,
        };

        self.sm
            .write_meta(SM_SNAPSHOT_META_KEY, &meta)
            .await
            .map_err(sto_err_write_sm::<RegistryNodeId>)?;
        self.sm
            .write_meta(SM_SNAPSHOT_DATA_KEY, &bytes)
            .await
            .map_err(sto_err_write_sm::<RegistryNodeId>)?;

        *self.sm.snapshot_cache.write().await = Some((meta.clone(), bytes.clone()));

        Ok(Snapshot {
            meta,
            snapshot: Box::new(Cursor::new(bytes)),
        })
    }
}

impl RaftStateMachine<RegistryRaftConfig> for RegistryStateMachine {
    type SnapshotBuilder = RegistrySnapshotBuilder;

    async fn applied_state(
        &mut self,
    ) -> Result<
        (
            Option<LogId<RegistryNodeId>>,
            StoredMembership<RegistryNodeId, BasicNode>,
        ),
        StorageError<RegistryNodeId>,
    > {
        let last_applied: Option<LogId<RegistryNodeId>> = self
            .read_meta(SM_LAST_APPLIED_KEY)
            .map_err(sto_err_read_sm::<RegistryNodeId>)?;
        let last_membership: StoredMembership<RegistryNodeId, BasicNode> = self
            .read_meta(SM_LAST_MEMBERSHIP_KEY)
            .map_err(sto_err_read_sm::<RegistryNodeId>)?
            .unwrap_or_default();
        Ok((last_applied, last_membership))
    }

    async fn apply<I>(&mut self, entries: I) -> Result<Vec<OpResult>, StorageError<RegistryNodeId>>
    where
        I: IntoIterator<Item = Entry<RegistryRaftConfig>> + openraft::OptionalSend,
        I::IntoIter: openraft::OptionalSend,
    {
        let mut results = Vec::new();
        let mut last_applied: Option<LogId<RegistryNodeId>> = None;
        let mut last_membership: Option<StoredMembership<RegistryNodeId, BasicNode>> = None;

        for entry in entries {
            last_applied = Some(entry.log_id);

            if let Some(mem) = entry.payload.get_membership() {
                last_membership = Some(StoredMembership::new(Some(entry.log_id), mem.clone()));
                results.push(OpResult::Ok);
                continue;
            }

            match entry.payload {
                EntryPayload::Blank => results.push(OpResult::Ok),
                EntryPayload::Normal(op) => {
                    let fsm = self.fsm.clone();
                    let applied = tokio::task::spawn_blocking(move || fsm.apply(op))
                        .await
                        .map_err(sto_err_write_sm::<RegistryNodeId>)?
                        .map_err(sto_err_write_sm::<RegistryNodeId>)?;
                    results.push(applied);
                }
                EntryPayload::Membership(_) => {
                    results.push(OpResult::Ok);
                }
            }
        }

        if let Some(applied) = &last_applied {
            self.write_meta(SM_LAST_APPLIED_KEY, applied)
                .await
                .map_err(sto_err_write_sm::<RegistryNodeId>)?;
        }
        if let Some(mem) = last_membership {
            self.write_meta(SM_LAST_MEMBERSHIP_KEY, &mem)
                .await
                .map_err(sto_err_write_sm::<RegistryNodeId>)?;
        }

        Ok(results)
    }

    async fn get_snapshot_builder(&mut self) -> Self::SnapshotBuilder {
        RegistrySnapshotBuilder { sm: self.clone() }
    }

    async fn begin_receiving_snapshot(
        &mut self,
    ) -> Result<Box<Cursor<Vec<u8>>>, StorageError<RegistryNodeId>> {
        Ok(Box::new(Cursor::new(Vec::new())))
    }

    async fn install_snapshot(
        &mut self,
        meta: &SnapshotMeta<RegistryNodeId, BasicNode>,
        mut snapshot: Box<Cursor<Vec<u8>>>,
    ) -> Result<(), StorageError<RegistryNodeId>> {
        use tokio::io::{AsyncReadExt, AsyncSeekExt};

        snapshot
            .seek(std::io::SeekFrom::Start(0))
            .await
            .map_err(sto_err_read_sm::<RegistryNodeId>)?;

        let mut bytes = Vec::new();
        snapshot
            .read_to_end(&mut bytes)
            .await
            .map_err(sto_err_read_sm::<RegistryNodeId>)?;

        let fsm = self.fsm.clone();
        let bytes_for_restore = bytes.clone();
        tokio::task::spawn_blocking(move || fsm.restore_snapshot(&bytes_for_restore))
            .await
            .map_err(sto_err_write_sm::<RegistryNodeId>)?
            .map_err(sto_err_write_sm::<RegistryNodeId>)?;

        self.write_meta(SM_LAST_APPLIED_KEY, &meta.last_log_id)
            .await
            .map_err(sto_err_write_sm::<RegistryNodeId>)?;
        self.write_meta(SM_LAST_MEMBERSHIP_KEY, &meta.last_membership)
            .await
            .map_err(sto_err_write_sm::<RegistryNodeId>)?;

        self.write_meta(SM_SNAPSHOT_META_KEY, meta)
            .await
            .map_err(sto_err_write_sm::<RegistryNodeId>)?;
        self.write_meta(SM_SNAPSHOT_DATA_KEY, &bytes)
            .await
            .map_err(sto_err_write_sm::<RegistryNodeId>)?;

        *self.snapshot_cache.write().await = Some((meta.clone(), bytes));

        Ok(())
    }

    async fn get_current_snapshot(
        &mut self,
    ) -> Result<Option<Snapshot<RegistryRaftConfig>>, StorageError<RegistryNodeId>> {
        if let Some((meta, bytes)) = self.snapshot_cache.read().await.clone() {
            return Ok(Some(Snapshot {
                meta,
                snapshot: Box::new(Cursor::new(bytes)),
            }));
        }

        let meta: Option<SnapshotMeta<RegistryNodeId, BasicNode>> = self
            .read_meta(SM_SNAPSHOT_META_KEY)
            .map_err(sto_err_read_sm::<RegistryNodeId>)?;
        let data: Option<Vec<u8>> = self
            .read_meta(SM_SNAPSHOT_DATA_KEY)
            .map_err(sto_err_read_sm::<RegistryNodeId>)?;

        match (meta, data) {
            (Some(meta), Some(bytes)) => {
                *self.snapshot_cache.write().await = Some((meta.clone(), bytes.clone()));
                Ok(Some(Snapshot {
                    meta,
                    snapshot: Box::new(Cursor::new(bytes)),
                }))
            }
            _ => Ok(None),
        }
    }
}

#[derive(Clone, Default)]
pub struct RegistryNetworkFactory;

pub struct RegistryNetworkClient {
    target: RegistryNodeId,
    target_node: BasicNode,
    client: Option<raft_rpc::raft_rpc_client::RaftRpcClient<tonic::transport::Channel>>,
}

impl RegistryNetworkClient {
    async fn connect(
        &mut self,
    ) -> Result<
        &mut raft_rpc::raft_rpc_client::RaftRpcClient<tonic::transport::Channel>,
        tonic::transport::Error,
    > {
        if self.client.is_none() {
            let endpoint = format!("http://{}", self.target_node.addr);
            let client = raft_rpc::raft_rpc_client::RaftRpcClient::connect(endpoint).await?;
            self.client = Some(client);
        }
        Ok(self.client.as_mut().expect("client set"))
    }
}

fn rpc_net_err<NID, N, E>(e: impl std::fmt::Display) -> RPCError<NID, N, E>
where
    NID: openraft::NodeId,
    N: openraft::Node,
    E: std::error::Error,
{
    let io_err = std::io::Error::other(e.to_string());
    RPCError::Network(NetworkError::new(&io_err))
}

impl RaftNetworkFactory<RegistryRaftConfig> for RegistryNetworkFactory {
    type Network = RegistryNetworkClient;

    async fn new_client(&mut self, target: RegistryNodeId, node: &BasicNode) -> Self::Network {
        RegistryNetworkClient {
            target,
            target_node: node.clone(),
            client: None,
        }
    }
}

impl RaftNetwork<RegistryRaftConfig> for RegistryNetworkClient {
    async fn append_entries(
        &mut self,
        rpc: AppendEntriesRequest<RegistryRaftConfig>,
        _option: RPCOption,
    ) -> Result<
        AppendEntriesResponse<RegistryNodeId>,
        RPCError<RegistryNodeId, BasicNode, RaftError<RegistryNodeId>>,
    > {
        let target = self.target;
        let target_node = self.target_node.clone();
        let payload = bincode::serialize(&rpc)
            .map_err(rpc_net_err::<RegistryNodeId, BasicNode, RaftError<RegistryNodeId>>)?;

        let client = self
            .connect()
            .await
            .map_err(rpc_net_err::<RegistryNodeId, BasicNode, RaftError<RegistryNodeId>>)?;
        let res = client
            .append_entries(Request::new(raft_rpc::Bytes { data: payload }))
            .await
            .map_err(rpc_net_err::<RegistryNodeId, BasicNode, RaftError<RegistryNodeId>>)?
            .into_inner();

        let decoded: std::result::Result<
            AppendEntriesResponse<RegistryNodeId>,
            RaftError<RegistryNodeId>,
        > = bincode::deserialize(&res.data)
            .map_err(rpc_net_err::<RegistryNodeId, BasicNode, RaftError<RegistryNodeId>>)?;

        decoded
            .map_err(|e| RPCError::RemoteError(RemoteError::new_with_node(target, target_node, e)))
    }

    async fn install_snapshot(
        &mut self,
        rpc: InstallSnapshotRequest<RegistryRaftConfig>,
        _option: RPCOption,
    ) -> Result<
        InstallSnapshotResponse<RegistryNodeId>,
        RPCError<
            RegistryNodeId,
            BasicNode,
            RaftError<RegistryNodeId, openraft::error::InstallSnapshotError>,
        >,
    > {
        let target = self.target;
        let target_node = self.target_node.clone();
        let payload = bincode::serialize(&rpc).map_err(
            rpc_net_err::<
                RegistryNodeId,
                BasicNode,
                RaftError<RegistryNodeId, openraft::error::InstallSnapshotError>,
            >,
        )?;

        let client = self.connect().await.map_err(
            rpc_net_err::<
                RegistryNodeId,
                BasicNode,
                RaftError<RegistryNodeId, openraft::error::InstallSnapshotError>,
            >,
        )?;
        let res = client
            .install_snapshot(Request::new(raft_rpc::Bytes { data: payload }))
            .await
            .map_err(
                rpc_net_err::<
                    RegistryNodeId,
                    BasicNode,
                    RaftError<RegistryNodeId, openraft::error::InstallSnapshotError>,
                >,
            )?
            .into_inner();

        let decoded: std::result::Result<
            InstallSnapshotResponse<RegistryNodeId>,
            RaftError<RegistryNodeId, openraft::error::InstallSnapshotError>,
        > = bincode::deserialize(&res.data).map_err(
            rpc_net_err::<
                RegistryNodeId,
                BasicNode,
                RaftError<RegistryNodeId, openraft::error::InstallSnapshotError>,
            >,
        )?;

        decoded
            .map_err(|e| RPCError::RemoteError(RemoteError::new_with_node(target, target_node, e)))
    }

    async fn vote(
        &mut self,
        rpc: VoteRequest<RegistryNodeId>,
        _option: RPCOption,
    ) -> Result<
        VoteResponse<RegistryNodeId>,
        RPCError<RegistryNodeId, BasicNode, RaftError<RegistryNodeId>>,
    > {
        let target = self.target;
        let target_node = self.target_node.clone();
        let payload = bincode::serialize(&rpc)
            .map_err(rpc_net_err::<RegistryNodeId, BasicNode, RaftError<RegistryNodeId>>)?;

        let client = self
            .connect()
            .await
            .map_err(rpc_net_err::<RegistryNodeId, BasicNode, RaftError<RegistryNodeId>>)?;
        let res = client
            .vote(Request::new(raft_rpc::Bytes { data: payload }))
            .await
            .map_err(rpc_net_err::<RegistryNodeId, BasicNode, RaftError<RegistryNodeId>>)?
            .into_inner();

        let decoded: std::result::Result<VoteResponse<RegistryNodeId>, RaftError<RegistryNodeId>> =
            bincode::deserialize(&res.data)
                .map_err(rpc_net_err::<RegistryNodeId, BasicNode, RaftError<RegistryNodeId>>)?;

        decoded
            .map_err(|e| RPCError::RemoteError(RemoteError::new_with_node(target, target_node, e)))
    }
}

#[derive(Clone)]
pub struct RaftRpcService {
    raft: RegistryRaft,
}

#[tonic::async_trait]
impl raft_rpc::raft_rpc_server::RaftRpc for RaftRpcService {
    async fn append_entries(
        &self,
        request: Request<raft_rpc::Bytes>,
    ) -> Result<Response<raft_rpc::Bytes>, Status> {
        let rpc: AppendEntriesRequest<RegistryRaftConfig> =
            bincode::deserialize(&request.into_inner().data)
                .map_err(|e| Status::invalid_argument(e.to_string()))?;

        let result = self.raft.append_entries(rpc).await;
        let bytes = bincode::serialize(&result).map_err(|e| Status::internal(e.to_string()))?;
        Ok(Response::new(raft_rpc::Bytes { data: bytes }))
    }

    async fn vote(
        &self,
        request: Request<raft_rpc::Bytes>,
    ) -> Result<Response<raft_rpc::Bytes>, Status> {
        let rpc: VoteRequest<RegistryNodeId> = bincode::deserialize(&request.into_inner().data)
            .map_err(|e| Status::invalid_argument(e.to_string()))?;

        let result = self.raft.vote(rpc).await;
        let bytes = bincode::serialize(&result).map_err(|e| Status::internal(e.to_string()))?;
        Ok(Response::new(raft_rpc::Bytes { data: bytes }))
    }

    async fn install_snapshot(
        &self,
        request: Request<raft_rpc::Bytes>,
    ) -> Result<Response<raft_rpc::Bytes>, Status> {
        let rpc: InstallSnapshotRequest<RegistryRaftConfig> =
            bincode::deserialize(&request.into_inner().data)
                .map_err(|e| Status::invalid_argument(e.to_string()))?;

        let result = self.raft.install_snapshot(rpc).await;
        let bytes = bincode::serialize(&result).map_err(|e| Status::internal(e.to_string()))?;
        Ok(Response::new(raft_rpc::Bytes { data: bytes }))
    }
}

#[derive(Clone)]
pub struct ClusterAdminService {
    raft: RegistryRaft,
    store: Arc<dyn MetadataStore>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[allow(clippy::large_enum_variant)]
enum ClientWriteReply {
    Applied(OpResult),
    Forward {
        leader_id: u64,
        leader_addr: Option<String>,
    },
    Error(String),
}

#[tonic::async_trait]
impl raft_rpc::cluster_admin_server::ClusterAdmin for ClusterAdminService {
    async fn join(
        &self,
        request: Request<raft_rpc::JoinRequest>,
    ) -> Result<Response<raft_rpc::JoinResponse>, Status> {
        let req = request.into_inner();
        let node_id = req.node_id;
        let raft_addr = req.raft_addr;

        let node = BasicNode {
            addr: raft_addr.clone(),
        };

        info!(node_id = node_id, addr = %raft_addr, "cluster join requested");

        if let Err(err) = self.raft.add_learner(node_id, node, true).await {
            if let RaftError::APIError(ClientWriteError::ForwardToLeader(fwd)) = err {
                return Ok(Response::new(raft_rpc::JoinResponse {
                    leader_id: fwd.leader_id.unwrap_or(0),
                    leader_addr: fwd.leader_node.map(|n| n.addr).unwrap_or_default(),
                    accepted: false,
                }));
            }
            return Err(Status::failed_precondition(err.to_string()));
        }

        let metrics = self.raft.metrics();
        let current = metrics.borrow().clone();
        let mut voters: BTreeSet<RegistryNodeId> = current.membership_config.voter_ids().collect();
        voters.insert(node_id);

        if let Err(err) = self.raft.change_membership(voters, true).await {
            if let RaftError::APIError(ClientWriteError::ForwardToLeader(fwd)) = err {
                return Ok(Response::new(raft_rpc::JoinResponse {
                    leader_id: fwd.leader_id.unwrap_or(0),
                    leader_addr: fwd.leader_node.map(|n| n.addr).unwrap_or_default(),
                    accepted: false,
                }));
            }
            return Err(Status::failed_precondition(err.to_string()));
        }

        let leader_id = metrics.borrow().current_leader.unwrap_or(0);
        Ok(Response::new(raft_rpc::JoinResponse {
            leader_id,
            leader_addr: String::new(),
            accepted: true,
        }))
    }

    async fn status(
        &self,
        _request: Request<raft_rpc::StatusRequest>,
    ) -> Result<Response<raft_rpc::StatusResponse>, Status> {
        let metrics = self.raft.metrics();
        let current = metrics.borrow().clone();

        let leader_id = current.current_leader.unwrap_or(0);
        let voters: Vec<RegistryNodeId> = current.membership_config.voter_ids().collect();

        // openraft does not guarantee a stable "learner_ids" iterator across versions;
        // derive learners by inspecting the node set minus voters.
        let voters_set: std::collections::BTreeSet<RegistryNodeId> =
            voters.iter().copied().collect();
        let learners: Vec<RegistryNodeId> = current
            .membership_config
            .nodes()
            .map(|(id, _)| *id)
            .filter(|id| !voters_set.contains(id))
            .collect();

        Ok(Response::new(raft_rpc::StatusResponse {
            leader_id,
            voters,
            learners,
        }))
    }

    async fn propose(
        &self,
        request: Request<raft_rpc::Bytes>,
    ) -> Result<Response<raft_rpc::Bytes>, Status> {
        let data = request.into_inner().data;
        let op: MetadataOp = match serde_json::from_slice(&data) {
            Ok(op) => op,
            Err(err) => {
                let head_len = data.len().min(16);
                tracing::error!(
                    len = data.len(),
                    head = ?&data[..head_len],
                    error = %err,
                    "cluster admin propose payload decode failed"
                );
                return Err(Status::invalid_argument(err.to_string()));
            }
        };

        let reply = match self.raft.client_write(op).await {
            Ok(resp) => ClientWriteReply::Applied(resp.data),
            Err(RaftError::APIError(ClientWriteError::ForwardToLeader(fwd))) => {
                ClientWriteReply::Forward {
                    leader_id: fwd.leader_id.unwrap_or(0),
                    leader_addr: fwd.leader_node.map(|n| n.addr),
                }
            }
            Err(err) => ClientWriteReply::Error(err.to_string()),
        };

        let bytes = serde_json::to_vec(&reply).map_err(|e| Status::internal(e.to_string()))?;
        Ok(Response::new(raft_rpc::Bytes { data: bytes }))
    }

    async fn get_capsule(
        &self,
        request: Request<raft_rpc::Bytes>,
    ) -> Result<Response<raft_rpc::Bytes>, Status> {
        let id: common::CapsuleId = serde_json::from_slice(&request.into_inner().data)
            .map_err(|e| Status::invalid_argument(e.to_string()))?;

        let result = self
            .store
            .get_capsule(&id)
            .map_err(|e| Status::internal(e.to_string()))?;

        let bytes = serde_json::to_vec(&result).map_err(|e| Status::internal(e.to_string()))?;
        Ok(Response::new(raft_rpc::Bytes { data: bytes }))
    }
}

#[derive(Clone)]
pub struct MeshRegistryRaft {
    pub raft: RegistryRaft,
    pub raft_addr: std::net::SocketAddr,
}

impl MeshRegistryRaft {
    pub async fn start(
        node_id: RegistryNodeId,
        raft_addr: std::net::SocketAddr,
        metadata_path: &str,
        raft_store_path: &str,
        bootstrap: bool,
    ) -> Result<Self> {
        let config = openraft::Config::default()
            .validate()
            .map_err(|e| anyhow!(e))?;
        let config = Arc::new(config);

        let store: Arc<dyn MetadataStore> = Arc::new(SledStore::open(metadata_path)?);
        let log_store = RegistryLogStore::open(raft_store_path).context("open raft log store")?;
        let state_machine = RegistryStateMachine::open(Arc::clone(&store), raft_store_path)
            .context("open raft state machine")?;
        let network = RegistryNetworkFactory;

        let raft = Raft::new(node_id, config, network, log_store, state_machine)
            .await
            .map_err(|e| anyhow!(e))?;

        let raft_clone = raft.clone();
        let store_clone = store.clone();
        tokio::spawn(async move {
            let addr = raft_addr;
            let svc = RaftRpcService {
                raft: raft_clone.clone(),
            };
            let admin = ClusterAdminService {
                raft: raft_clone,
                store: store_clone,
            };
            info!(addr = %addr, "raft gRPC server starting");
            tonic::transport::Server::builder()
                .add_service(raft_rpc::raft_rpc_server::RaftRpcServer::new(svc))
                .add_service(raft_rpc::cluster_admin_server::ClusterAdminServer::new(
                    admin,
                ))
                .serve(addr)
                .await
                .unwrap();
        });

        if bootstrap {
            let mut members = std::collections::BTreeMap::new();
            members.insert(
                node_id,
                BasicNode {
                    addr: raft_addr.to_string(),
                },
            );
            raft.initialize(members).await.map_err(|e| anyhow!(e))?;
        }

        Ok(Self { raft, raft_addr })
    }
}

pub async fn join_cluster(
    known_addr: std::net::SocketAddr,
    node_id: u64,
    raft_addr: std::net::SocketAddr,
) -> Result<()> {
    let mut target = known_addr;
    let mut last_err: Option<anyhow::Error> = None;

    for _ in 0..50 {
        let endpoint = format!("http://{}", target);
        let connect = raft_rpc::cluster_admin_client::ClusterAdminClient::connect(endpoint).await;
        let mut client = match connect {
            Ok(c) => c,
            Err(e) => {
                last_err = Some(anyhow::anyhow!(e).context("connect join endpoint"));
                sleep(Duration::from_millis(200)).await;
                continue;
            }
        };

        let call = client
            .join(raft_rpc::JoinRequest {
                node_id,
                raft_addr: raft_addr.to_string(),
            })
            .await;

        let resp = match call {
            Ok(r) => r.into_inner(),
            Err(e) => {
                last_err = Some(anyhow::anyhow!(e).context("join RPC"));
                sleep(Duration::from_millis(200)).await;
                continue;
            }
        };

        if resp.accepted {
            return Ok(());
        }

        if !resp.leader_addr.is_empty() {
            target = resp
                .leader_addr
                .parse()
                .context("parse leader_addr from join response")?;
            continue;
        }

        last_err = Some(anyhow::anyhow!(
            "join rejected; leader unknown (node may not be initialized yet)"
        ));
        sleep(Duration::from_millis(200)).await;
    }

    Err(last_err.unwrap_or_else(|| anyhow::anyhow!("join failed")))
}

pub async fn cluster_status(addr: std::net::SocketAddr) -> Result<raft_rpc::StatusResponse> {
    let endpoint = format!("http://{}", addr);
    let mut client = raft_rpc::cluster_admin_client::ClusterAdminClient::connect(endpoint)
        .await
        .context("connect status endpoint")?;
    let resp = client
        .status(raft_rpc::StatusRequest {})
        .await
        .context("status RPC")?
        .into_inner();
    Ok(resp)
}

async fn propose_via(addr: std::net::SocketAddr, op: MetadataOp) -> Result<OpResult> {
    let mut target = addr;
    for _ in 0..5 {
        let endpoint = format!("http://{}", target);
        let mut client = raft_rpc::cluster_admin_client::ClusterAdminClient::connect(endpoint)
            .await
            .context("connect propose endpoint")?;

        let req = raft_rpc::Bytes {
            data: serde_json::to_vec(&op)?,
        };

        let bytes = client
            .propose(req)
            .await
            .context("propose RPC")?
            .into_inner()
            .data;

        let reply: ClientWriteReply = serde_json::from_slice(&bytes)?;
        match reply {
            ClientWriteReply::Applied(res) => return Ok(res),
            ClientWriteReply::Forward { leader_addr, .. } => {
                let Some(addr) = leader_addr else {
                    anyhow::bail!("write forwarded but leader address unknown");
                };
                target = addr
                    .parse()
                    .context("parse leader_addr from propose response")?;
            }
            ClientWriteReply::Error(e) => anyhow::bail!(e),
        }
    }
    anyhow::bail!("propose failed after redirects")
}

/// Create/update capsule metadata via the cluster leader.
pub async fn put_capsule(addr: std::net::SocketAddr, capsule: common::Capsule) -> Result<()> {
    match propose_via(addr, MetadataOp::PutCapsule(capsule)).await? {
        OpResult::Ok => Ok(()),
        other => anyhow::bail!("unexpected raft response: {:?}", other),
    }
}

/// Delete capsule metadata via the cluster leader.
pub async fn delete_capsule(
    addr: std::net::SocketAddr,
    id: common::CapsuleId,
) -> Result<Option<common::Capsule>> {
    match propose_via(addr, MetadataOp::DeleteCapsule(id)).await? {
        OpResult::CapsuleFound(c) => Ok(Some(c)),
        OpResult::NotFound => Ok(None),
        other => anyhow::bail!("unexpected raft response: {:?}", other),
    }
}

/// Best-effort read of capsule metadata from the contacted node.
pub async fn get_capsule(
    addr: std::net::SocketAddr,
    id: common::CapsuleId,
) -> Result<Option<common::Capsule>> {
    let endpoint = format!("http://{}", addr);
    let mut client = raft_rpc::cluster_admin_client::ClusterAdminClient::connect(endpoint)
        .await
        .context("connect get_capsule endpoint")?;

    let req = raft_rpc::Bytes {
        data: serde_json::to_vec(&id)?,
    };

    let bytes = client
        .get_capsule(req)
        .await
        .context("get_capsule RPC")?
        .into_inner()
        .data;

    let result: Option<common::Capsule> = serde_json::from_slice(&bytes)?;
    Ok(result)
}

/// Subscribe to gossip events and attempt to expand the Raft membership.
///
/// This is intentionally best-effort: only the current leader can apply membership changes.
pub async fn monitor_peers(
    raft: RegistryRaft,
    mut gossip_events: tokio::sync::mpsc::Receiver<mesh_core::GossipEvent>,
) {
    while let Some(event) = gossip_events.recv().await {
        match event {
            mesh_core::GossipEvent::NodeDiscovered(peer) => {
                tracing::debug!(peer_id = %peer.id, "node discovered via gossip");
            }
            mesh_core::GossipEvent::NodeLost(peer_id) => {
                tracing::warn!(peer_id = %peer_id, "node lost from gossip (membership change not automatic)");
            }
            mesh_core::GossipEvent::Heartbeat {
                peer_id,
                raft_port,
                gossip_addr,
                ..
            } => {
                let Ok(node_id) = peer_id.parse::<u64>() else {
                    continue;
                };
                let Some(gossip_addr) = gossip_addr else {
                    continue;
                };

                let raft_addr = std::net::SocketAddr::new(gossip_addr.ip(), raft_port);
                let node = BasicNode {
                    addr: raft_addr.to_string(),
                };
                let _ = raft.add_learner(node_id, node, false).await;
            }
        }
    }
}
