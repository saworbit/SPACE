#![cfg(all(target_os = "linux", feature = "rdma"))]

use anyhow::{anyhow, Result};
use rdma_sys::{
    ibv_ah_attr, ibv_gid, ibv_modify_qp, ibv_qp, ibv_qp_attr, ibv_qp_state, IBV_ACCESS_LOCAL_WRITE,
    IBV_ACCESS_REMOTE_READ, IBV_ACCESS_REMOTE_WRITE, IBV_MTU_1024, IBV_QP_AV, IBV_QP_DEST_QPN,
    IBV_QP_PATH_MTU, IBV_QP_PKEY_INDEX, IBV_QP_PORT, IBV_QP_QP_STATE, IBV_QP_RQ_PSN, IBV_QP_SQ_PSN,
    IBV_QP_TIMEOUT,
};
use serde::{Deserialize, Serialize};

use super::RdmaTransport;

/// Handshake payload exchanged over the OOB (Phase B) control channel prior to RDMA use.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RdmaHandshake {
    pub qpn: u32,
    pub psn: u32,
    pub lid: u16,
    pub gid: [u8; 16],
    pub rkey: u32,
    pub vaddr: u64,
}

impl RdmaTransport {
    /// Create an address handle for the remote peer based on LID/GID discovery.
    fn create_ah(&self, lid: u16, gid: [u8; 16]) -> Result<ibv_ah_attr> {
        let mut attr: ibv_ah_attr = unsafe { std::mem::zeroed() };
        attr.is_global = 1;
        attr.dlid = lid;
        attr.sl = 0;
        attr.src_path_bits = 0;
        attr.port_num = 1;
        attr.grh.dgid = ibv_gid { raw: gid };
        attr.grh.hop_limit = 1;
        attr.grh.sgid_index = 0;
        attr.grh.traffic_class = 0;
        attr.grh.flow_label = 0;
        Ok(attr)
    }

    /// Transition a QP from RESET -> INIT -> RTR -> RTS using remote handshake data.
    ///
    /// # Safety
    ///
    /// Caller must ensure that `qp` is not accessed concurrently by other threads
    /// during this state transition. The underlying `ibv_modify_qp` C-function is
    /// not thread-safe for the same Queue Pair, and concurrent state transitions
    /// (RESET -> INIT -> RTR -> RTS) will cause undefined behavior.
    #[allow(dead_code)]
    pub(crate) unsafe fn connect_qp(
        &self,
        qp: *mut ibv_qp,
        remote: RdmaHandshake,
        local_psn: u32,
    ) -> Result<()> {
        // RESET -> INIT
        let mut init_attr: ibv_qp_attr = std::mem::zeroed();
        init_attr.qp_state = ibv_qp_state::IBV_QPS_INIT;
        init_attr.pkey_index = 0;
        init_attr.port_num = 1;
        init_attr.qp_access_flags =
            (IBV_ACCESS_REMOTE_WRITE | IBV_ACCESS_LOCAL_WRITE | IBV_ACCESS_REMOTE_READ) as i32;

        ibv_modify_qp(
            qp,
            &mut init_attr,
            (IBV_QP_QP_STATE | IBV_QP_PKEY_INDEX | IBV_QP_PORT | IBV_QP_ACCESS_FLAGS()) as i32,
        )
        .to_result()?;

        // INIT -> RTR
        let ah_attr = self.create_ah(remote.lid, remote.gid)?;
        let mut rtr_attr: ibv_qp_attr = std::mem::zeroed();
        rtr_attr.qp_state = ibv_qp_state::IBV_QPS_RTR;
        rtr_attr.path_mtu = IBV_MTU_1024; // conservative default
        rtr_attr.dest_qp_num = remote.qpn;
        rtr_attr.rq_psn = remote.psn;
        rtr_attr.ah_attr = ah_attr;
        rtr_attr.max_dest_rd_atomic = 1;
        rtr_attr.min_rnr_timer = 12;

        ibv_modify_qp(
            qp,
            &mut rtr_attr,
            (IBV_QP_QP_STATE
                | IBV_QP_AV
                | IBV_QP_PATH_MTU
                | IBV_QP_DEST_QPN
                | IBV_QP_RQ_PSN
                | IBV_QP_MAX_DEST_RD_ATOMIC()
                | IBV_QP_MIN_RNR_TIMER()) as i32,
        )
        .to_result()?;

        // RTR -> RTS
        let mut rts_attr: ibv_qp_attr = std::mem::zeroed();
        rts_attr.qp_state = ibv_qp_state::IBV_QPS_RTS;
        rts_attr.sq_psn = local_psn;
        rts_attr.timeout = 14; // ~1.024us * 2^timeout
        rts_attr.retry_cnt = 7;
        rts_attr.rnr_retry = 7;
        rts_attr.max_rd_atomic = 1;

        ibv_modify_qp(
            qp,
            &mut rts_attr,
            (IBV_QP_QP_STATE
                | IBV_QP_SQ_PSN
                | IBV_QP_TIMEOUT
                | IBV_QP_RETRY_CNT()
                | IBV_QP_RNR_RETRY()
                | IBV_QP_MAX_RD_ATOMIC()) as i32,
        )
        .to_result()?;

        Ok(())
    }
}

/// Helper trait to map ibv_modify_qp return codes into Results.
trait IbvExt {
    fn to_result(self) -> Result<()>;
}

impl IbvExt for i32 {
    fn to_result(self) -> Result<()> {
        if self == 0 {
            Ok(())
        } else {
            Err(anyhow!("ibv_modify_qp returned {}", self))
        }
    }
}

// Flags not exposed as constants in rdma-sys (keep localized here)
const fn IBV_QP_ACCESS_FLAGS() -> u32 {
    rdma_sys::ibv_qp_attr_mask_IBV_QP_ACCESS_FLAGS
}

const fn IBV_QP_MAX_DEST_RD_ATOMIC() -> u32 {
    rdma_sys::ibv_qp_attr_mask_IBV_QP_MAX_DEST_RD_ATOMIC
}

const fn IBV_QP_MIN_RNR_TIMER() -> u32 {
    rdma_sys::ibv_qp_attr_mask_IBV_QP_MIN_RNR_TIMER
}

const fn IBV_QP_RETRY_CNT() -> u32 {
    rdma_sys::ibv_qp_attr_mask_IBV_QP_RETRY_CNT
}

const fn IBV_QP_RNR_RETRY() -> u32 {
    rdma_sys::ibv_qp_attr_mask_IBV_QP_RNR_RETRY
}

const fn IBV_QP_MAX_RD_ATOMIC() -> u32 {
    rdma_sys::ibv_qp_attr_mask_IBV_QP_MAX_RD_ATOMIC
}
