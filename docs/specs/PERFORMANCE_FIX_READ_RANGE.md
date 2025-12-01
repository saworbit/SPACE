# Performance Specification: Native Range Reads

Severity: Medium  
Target Component: crates/capsule-registry  
Status: PROPOSED

## 1. Problem Analysis
`WritePipeline::read_range` currently performs a full capsule read and then slices the returned `Vec<u8>`. A 4 KiB range read against a 1 GiB capsule therefore loads the entire capsule into RAM, amplifying both I/O and memory usage.

```rust
// CURRENT IMPLEMENTATION
pub fn read_range(&self, id: CapsuleId, offset: u64, len: usize) -> Result<Vec<u8>> {
    let full = self.read_capsule(id)?; // <--- READS 100% OF DATA
    // ... discard 99.9% of data ...
    Ok(full[offset..].to_vec())
}
```

Impacts:
- Video streaming: seeking mid-file pulls the whole asset.
- Database WAL/log tails: fetching recent bytes requires parsing history.
- Memory pressure: tiny reads can allocate gigabytes.

## 2. Proposed Solution: Trait Evolution
Promote `read_range` to a first-class method on `PipelineStrategy` so strategies can implement efficient range-aware backends (e.g., `pread`, HTTP range headers, segment lookups).

```rust
pub trait PipelineStrategy: Send + Sync + 'static {
    // ... existing methods ...

    /// Read a specific byte range.
    /// Defaults to a full read + slice for backward compatibility.
    async fn read_range(&self, id: CapsuleId, offset: u64, len: usize) -> Result<Vec<u8>> {
        let full = self.read_capsule(id).await?;
        if offset >= full.len() as u64 {
            return Ok(Vec::new());
        }
        let end = std::cmp::min(offset + len as u64, full.len() as u64);
        Ok(full[offset as usize..end as usize].to_vec())
    }
}
```

`LegacyPipeline` overrides this to avoid cloning the full capsule, fetching only the relevant segment data and copying just the requested bytes. Other strategies inherit the safe fallback until they provide optimized implementations.

## 3. Verification
Add an integration test (`tests/range_read_test.rs`) that writes a known pattern and validates:
- Start range: offset 0, len 10 → bytes 0..9
- Middle range: offset 100, len 5 → bytes 100..104
- End range (saturating): offset 250, len 10 → bytes 250..255
- Out-of-bounds start: offset past end → empty vector
