mod error;

use std::borrow::Cow;
use std::io::Write;

use anyhow::{Context, Result};
use common::{
    traits::{CompressionSummary, Compressor},
    CompressionPolicy,
};
use subtle::ConstantTimeEq;
use tracing::{debug, info, instrument, warn};

pub use error::CompressionError;

type CompressionOpResult<T> = std::result::Result<T, CompressionError>;

fn constant_time_equal(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    bool::from(a.ct_eq(b))
}

/// Lightweight context for why compression was skipped or reverted.
#[derive(Debug, Clone)]
pub enum CompressionSkipReason {
    Entropy { entropy: f32 },
    Ineffective { ratio: f32 },
}

impl std::fmt::Display for CompressionSkipReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CompressionSkipReason::Entropy { entropy } => {
                write!(f, "entropy {:.2} bits/byte", entropy)
            }
            CompressionSkipReason::Ineffective { ratio } => {
                write!(f, "ineffective ratio {:.2}", ratio)
            }
        }
    }
}

/// Compression statistics for a segment
#[derive(Debug, Clone)]
pub struct CompressionResult {
    pub original_size: usize,
    pub compressed_size: usize,
    pub compressed: bool,
    /// Indicates whether we reused the original slice without allocating.
    pub reused_original: bool,
    pub algorithm: String,
    pub reason: Option<CompressionSkipReason>,
}

impl CompressionResult {
    pub fn ratio(&self) -> f32 {
        if self.compressed_size == 0 {
            return 1.0;
        }
        self.original_size as f32 / self.compressed_size as f32
    }
}

/// Estimate Shannon entropy of data sample
/// Returns bits per byte (0.0 = constant, 8.0 = random)
fn estimate_entropy(data: &[u8]) -> f32 {
    if data.is_empty() {
        return 0.0;
    }

    let mut freq = [0u32; 256];
    for &byte in data {
        freq[byte as usize] += 1;
    }

    let total = data.len() as f32;
    let mut entropy = 0.0;

    for &count in freq.iter() {
        if count > 0 {
            let p = count as f32 / total;
            entropy -= p * p.log2();
        }
    }

    entropy
}

/// Determine whether data should be compressed based on entropy analysis.
/// Returns a skip reason if compression would be wasteful.
fn entropy_skip_reason(data: &[u8]) -> Option<CompressionSkipReason> {
    if data.len() < 1024 {
        return None;
    }

    let sample_size = data.len().min(1024);
    let entropy = estimate_entropy(&data[..sample_size]);

    if entropy >= 7.5 {
        Some(CompressionSkipReason::Entropy { entropy })
    } else {
        None
    }
}

/// Compress data using LZ4
#[instrument(skip(data), fields(algorithm = "lz4", level, input_len = data.len()))]
fn compress_lz4(data: &[u8], level: i32) -> CompressionOpResult<Vec<u8>> {
    let mut encoder = lz4::EncoderBuilder::new()
        .level(level as u32)
        .build(Vec::new())
        .map_err(|err| CompressionError::codec("lz4", err.to_string()))?;

    encoder
        .write_all(data)
        .map_err(|err| CompressionError::io("lz4", err))?;
    let (compressed, result) = encoder.finish();
    result.map_err(|err| CompressionError::codec("lz4", err.to_string()))?;

    debug!(
        compressed_len = compressed.len(),
        "lz4 compression complete"
    );
    Ok(compressed)
}

/// Decompress LZ4 data
#[instrument(skip(data), fields(algorithm = "lz4", input_len = data.len()))]
pub fn decompress_lz4(data: &[u8]) -> CompressionOpResult<Vec<u8>> {
    let mut decoder =
        lz4::Decoder::new(data).map_err(|err| CompressionError::codec("lz4", err.to_string()))?;
    let mut decompressed = Vec::new();
    std::io::copy(&mut decoder, &mut decompressed)
        .map_err(|err| CompressionError::io("lz4", err))?;
    Ok(decompressed)
}

/// Compress data using Zstd
#[instrument(skip(data), fields(algorithm = "zstd", level, input_len = data.len()))]
fn compress_zstd(data: &[u8], level: i32) -> CompressionOpResult<Vec<u8>> {
    let compressed = zstd::encode_all(data, level)
        .map_err(|err| CompressionError::codec("zstd", err.to_string()))?;
    Ok(compressed)
}

/// Decompress Zstd data
#[instrument(skip(data), fields(algorithm = "zstd", input_len = data.len()))]
pub fn decompress_zstd(data: &[u8]) -> CompressionOpResult<Vec<u8>> {
    let decompressed =
        zstd::decode_all(data).map_err(|err| CompressionError::codec("zstd", err.to_string()))?;
    Ok(decompressed)
}

/// Adjust compression level based on policy
fn adjusted_level(level: i32, algorithm: &'static str) -> CompressionOpResult<i32> {
    let clamped = match algorithm {
        "lz4" => level.clamp(1, 16),
        "zstd" => level.clamp(-5, 22),
        _ => level,
    };

    if clamped != level {
        warn!(
            original_level = level,
            clamped_level = clamped,
            algorithm,
            "Compression level clamped to supported range"
        );
    }

    Ok(clamped)
}

/// Attempt compression and return compressed data with metadata.
fn attempt_compress(
    data: &[u8],
    policy: &CompressionPolicy,
) -> CompressionOpResult<(Vec<u8>, CompressionResult)> {
    match policy {
        CompressionPolicy::None => Ok((
            data.to_vec(),
            CompressionResult {
                original_size: data.len(),
                compressed_size: data.len(),
                compressed: false,
                reused_original: true,
                algorithm: "identity".into(),
                reason: None,
            },
        )),
        CompressionPolicy::LZ4 { level } => {
            let level = adjusted_level(*level, "lz4")?;
            let compressed = compress_lz4(data, level)?;
            Ok((
                compressed,
                CompressionResult {
                    original_size: data.len(),
                    compressed_size: data.len(),
                    compressed: true,
                    reused_original: false,
                    algorithm: format!("lz4:{level}"),
                    reason: None,
                },
            ))
        }
        CompressionPolicy::Zstd { level } => {
            let level = adjusted_level(*level, "zstd")?;
            let compressed = compress_zstd(data, level)?;
            Ok((
                compressed,
                CompressionResult {
                    original_size: data.len(),
                    compressed_size: data.len(),
                    compressed: true,
                    reused_original: false,
                    algorithm: format!("zstd:{level}"),
                    reason: None,
                },
            ))
        }
    }
}

/// Verify integrity by comparing recompressed output with original.
fn verify_integrity(
    policy: &CompressionPolicy,
    compressed: &[u8],
    original: &[u8],
) -> CompressionOpResult<()> {
    match policy {
        CompressionPolicy::LZ4 { .. } => {
            let decompressed = decompress_lz4(compressed)?;
            if !constant_time_equal(&decompressed, original) {
                return Err(CompressionError::integrity("lz4"));
            }
        }
        CompressionPolicy::Zstd { .. } => {
            let decompressed = decompress_zstd(compressed)?;
            if !constant_time_equal(&decompressed, original) {
                return Err(CompressionError::integrity("zstd"));
            }
        }
        CompressionPolicy::None => {}
    }
    Ok(())
}

/// Adaptive compression that skips high-entropy or ineffective compressions.
#[instrument(skip(data, policy), fields(input_len = data.len()))]
pub fn adaptive_compress<'a>(
    data: &'a [u8],
    policy: &CompressionPolicy,
) -> Result<(Cow<'a, [u8]>, CompressionResult)> {
    if matches!(policy, CompressionPolicy::None) {
        return Ok((
            Cow::Borrowed(data),
            CompressionResult {
                original_size: data.len(),
                compressed_size: data.len(),
                compressed: false,
                reused_original: true,
                algorithm: "identity".into(),
                reason: Some(CompressionSkipReason::Ineffective { ratio: 1.0 }),
            },
        ));
    }

    if let Some(reason) = entropy_skip_reason(data) {
        info!(
            entropy = ?reason,
            "Skipping compression due to high entropy"
        );
        return Ok((
            Cow::Borrowed(data),
            CompressionResult {
                original_size: data.len(),
                compressed_size: data.len(),
                compressed: false,
                reused_original: true,
                algorithm: "identity".into(),
                reason: Some(reason),
            },
        ));
    }

    let (compressed, mut result) =
        attempt_compress(data, policy).context("compression backend failure")?;

    if compressed.len() >= data.len() {
        let ratio = compressed.len() as f32 / data.len() as f32;
        info!(
            original_len = data.len(),
            compressed_len = compressed.len(),
            ratio,
            "Compression ineffective, using original data"
        );
        result.compressed = false;
        result.compressed_size = data.len();
        result.reused_original = true;
        result.algorithm = "identity".into();
        result.reason = Some(CompressionSkipReason::Ineffective { ratio });
        return Ok((Cow::Borrowed(data), result));
    }

    verify_integrity(policy, &compressed, data).context("integrity verification failed")?;

    Ok((Cow::Owned(compressed), result))
}

/// Primary entry point used by the existing pipeline.
pub fn compress_segment<'a>(
    data: &'a [u8],
    policy: &CompressionPolicy,
) -> Result<(Cow<'a, [u8]>, CompressionResult)> {
    adaptive_compress(data, policy)
}

pub struct Lz4ZstdCompressor;

impl Lz4ZstdCompressor {
    pub fn new() -> Self {
        Self
    }
}

impl Default for Lz4ZstdCompressor {
    fn default() -> Self {
        Self::new()
    }
}

impl Compressor for Lz4ZstdCompressor {
    fn compress<'a>(
        &'a self,
        data: &'a [u8],
        policy: &CompressionPolicy,
    ) -> Result<(Cow<'a, [u8]>, CompressionSummary)> {
        let (view, result) = compress_segment(data, policy)?;
        let mut summary = CompressionSummary::new(
            result.original_size,
            result.compressed_size,
            result.algorithm,
        );
        summary.compressed = result.compressed;
        summary.reused_input = result.reused_original;
        summary.reason = result.reason.as_ref().map(|r| r.to_string());
        Ok((view, summary))
    }

    fn decompress(&self, data: &[u8], algorithm: &str) -> Result<Vec<u8>> {
        match algorithm {
            "identity" => Ok(data.to_vec()),
            algo if algo.starts_with("lz4") => decompress_lz4(data).map_err(Into::into),
            algo if algo.starts_with("zstd") => decompress_zstd(data).map_err(Into::into),
            other => Err(CompressionError::invalid_policy(format!(
                "unsupported algorithm {other}"
            ))
            .into()),
        }
    }

    fn supports_algorithm(&self, algorithm: &str) -> bool {
        algorithm == "identity" || algorithm.starts_with("lz4") || algorithm.starts_with("zstd")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use common::CompressionPolicy;
    use tracing_test::traced_test;

    #[test]
    fn test_roundtrip_lz4() {
        let original = b"SPACE roundtrip test! ".repeat(500);
        let policy = CompressionPolicy::LZ4 { level: 4 };

        let (compressed, result) = compress_segment(&original, &policy).unwrap();
        assert!(result.compressed);

        let decompressed = decompress_lz4(compressed.as_ref()).unwrap();
        assert_eq!(original.as_slice(), decompressed.as_slice());
    }

    #[test]
    fn test_roundtrip_zstd() {
        let original = b"SPACE Zstd roundtrip! ".repeat(500);
        let policy = CompressionPolicy::Zstd { level: 6 };

        let (compressed, result) = compress_segment(&original, &policy).unwrap();
        assert!(result.compressed);

        let decompressed = decompress_zstd(compressed.as_ref()).unwrap();
        assert_eq!(original.as_slice(), decompressed.as_slice());
    }

    #[test]
    fn test_ineffective_compression() {
        let mut pseudo_compressed = Vec::with_capacity(1000);
        for i in 0..1000 {
            pseudo_compressed.push((i ^ (i >> 3) ^ (i >> 5)) as u8);
        }

        let policy = CompressionPolicy::LZ4 { level: 9 };

        let (_view, result) = adaptive_compress(&pseudo_compressed, &policy).unwrap();

        assert!(
            !result.compressed || result.ratio() < 1.5,
            "Expected low compression ratio, got {:.2}x",
            result.ratio()
        );
    }

    #[test]
    fn test_verify_integrity_detects_tampering() {
        let payload = b"Tamper detection payload".repeat(256);
        let policy = CompressionPolicy::LZ4 { level: 4 };
        let compressed = compress_lz4(payload.as_slice(), 4).unwrap();
        let mut altered = payload.clone();
        altered[0] ^= 0xAA;

        let error = verify_integrity(&policy, &compressed, &altered).unwrap_err();
        assert!(matches!(
            error,
            CompressionError::IntegrityFailure { algorithm: "lz4" }
        ));
    }

    #[test]
    fn test_verify_integrity_accepts_valid_payload() {
        let payload = b"Integrity ok payload".repeat(256);
        let policy = CompressionPolicy::Zstd { level: 3 };
        let compressed = compress_zstd(payload.as_slice(), 3).unwrap();

        let result = verify_integrity(&policy, &compressed, payload.as_slice());
        assert!(result.is_ok());
    }

    #[traced_test]
    #[test]
    fn test_entropy_skip_emits_tracing() {
        let random: Vec<u8> = (0..4096).map(|i| ((i * 7919) % 256) as u8).collect();
        let policy = CompressionPolicy::Zstd { level: 3 };

        let _ = adaptive_compress(&random, &policy).unwrap();

        assert!(logs_contain("Skipping compression due to high entropy"));
    }

    #[traced_test]
    #[test]
    fn test_successful_compression_emits_telemetry() {
        let data = b"Space telemetry test ".repeat(1024);
        let policy = CompressionPolicy::LZ4 { level: 1 };

        let (_view, result) = adaptive_compress(&data, &policy).unwrap();
        assert!(result.compressed);
        assert!(!result.reused_original);
    }
}
