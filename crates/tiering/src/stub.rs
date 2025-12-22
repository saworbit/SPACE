use anyhow::{anyhow, Context, Result};
use common::StorageStub;
use object_store::path::Path as ObjPath;

pub const STUB_MAGIC: &str = "SPACE_STUB_V1";

pub fn is_stub_bytes(bytes: &[u8]) -> bool {
    bytes.starts_with(br#"{"magic":"SPACE_STUB_V1""#)
}

pub fn parse_stub(bytes: &[u8]) -> Result<StorageStub> {
    let stub: StorageStub = serde_json::from_slice(bytes).context("deserialize storage stub")?;
    if stub.magic != STUB_MAGIC {
        return Err(anyhow!("unexpected stub magic: {}", stub.magic));
    }
    Ok(stub)
}

pub fn make_stub(remote_url: String, original_size: u64, checksum: String) -> StorageStub {
    StorageStub {
        magic: STUB_MAGIC.to_string(),
        original_size,
        remote_url,
        checksum,
    }
}

/// Extract the object-store path from a remote URL.
///
/// Supported:
/// - `s3://bucket/<key>`
/// - `local://<key>`
/// - `<key>` (fallback)
pub fn object_path_from_remote_url(remote_url: &str) -> Result<ObjPath> {
    if let Some(rest) = remote_url.strip_prefix("s3://") {
        // s3://bucket/key...
        let rest = rest.trim_start_matches('/');
        let (_bucket, key) = rest.split_once('/').ok_or_else(|| {
            anyhow!("invalid s3 remote url (expected s3://bucket/key): {remote_url}")
        })?;
        return Ok(ObjPath::from(key));
    }

    if let Some(rest) = remote_url.strip_prefix("local://") {
        return Ok(ObjPath::from(rest.trim_start_matches('/')));
    }

    Ok(ObjPath::from(remote_url.trim_start_matches('/')))
}
