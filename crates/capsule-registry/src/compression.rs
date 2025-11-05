use crate::error::CompressionError;
use anyhow::{Context, Result};
use common::CompressionPolicy;
use std::borrow::Cow;
use std::io::Write;
use subtle::ConstantTimeEq;
use tracing::{debug, info, instrument, warn};

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

/// Compression statistics for a segment
#[derive(Debug, Clone)]
pub struct CompressionResult {
    pub original_size: usize,
    pub compressed_size: usize,
    pub compressed: bool,
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

fn verify_integrity(
    policy: &CompressionPolicy,
    compressed: &[u8],
    original: &[u8],
) -> CompressionOpResult<()> {
    let (algorithm, candidate) = match policy {
        CompressionPolicy::LZ4 { .. } => ("lz4", decompress_lz4(compressed)?),
        CompressionPolicy::Zstd { .. } => ("zstd", decompress_zstd(compressed)?),
        CompressionPolicy::None => return Ok(()),
    };

    if !constant_time_equal(candidate.as_slice(), original) {
        return Err(CompressionError::integrity(algorithm));
    }

    Ok(())
}

/// Adaptive compression with policy-driven algorithm selection
#[instrument(skip(data, policy), fields(segment_size = data.len(), policy = ?policy))]
pub fn adaptive_compress(data: &[u8], policy: &CompressionPolicy) -> Result<CompressionResult> {
    let original_size = data.len();

    // Check policy
    match policy {
        CompressionPolicy::None => {
            info!("Compression disabled by policy");
            return Ok(CompressionResult {
                original_size,
                compressed_size: original_size,
                compressed: false,
                algorithm: "none".to_string(),
                reason: None,
            });
        }
        _ => {}
    }

    // Entropy check - skip compression for random data
    if let Some(reason) = entropy_skip_reason(data) {
        if let CompressionSkipReason::Entropy { entropy } = reason {
            let error = CompressionError::EntropySkip {
                entropy,
                size: original_size,
            };
            warn!(
                target = "pipeline::compression",
                entropy, "skipping compression due to high entropy"
            );
            info!(
                target = "telemetry::compression",
                entropy,
                size = original_size,
                outcome = "skip_entropy"
            );
            debug!(%error, "compression skip classified");
        }
        return Ok(CompressionResult {
            original_size,
            compressed_size: original_size,
            compressed: false,
            algorithm: "skipped_entropy".to_string(),
            reason: Some(reason),
        });
    }

    // Compress based on policy
    let (compressed_data, algorithm) = match policy {
        CompressionPolicy::LZ4 { level } => {
            let compressed =
                compress_lz4(data, *level).context("LZ4 compression backend failed")?;
            (compressed, format!("lz4_{}", level))
        }
        CompressionPolicy::Zstd { level } => {
            let compressed =
                compress_zstd(data, *level).context("Zstd compression backend failed")?;
            (compressed, format!("zstd_{}", level))
        }
        CompressionPolicy::None => unreachable!(),
    };

    let compressed_size = compressed_data.len();

    // Only use compression if it actually saves space (+ 5% margin)
    if compressed_size < original_size * 95 / 100 {
        let ratio = if compressed_size == 0 {
            f32::INFINITY
        } else {
            original_size as f32 / compressed_size as f32
        };
        info!(
            target = "pipeline::compression",
            %algorithm,
            compressed_len = compressed_size,
            ratio,
            "compression successful"
        );
        info!(
            target = "telemetry::compression",
            %algorithm,
            ratio,
            original = original_size,
            compressed = compressed_size,
            outcome = "compressed"
        );
        Ok(CompressionResult {
            original_size,
            compressed_size,
            compressed: true,
            algorithm,
            reason: None,
        })
    } else {
        // Compression didn't help, return original
        let ratio = if compressed_size == 0 {
            1.0
        } else {
            original_size as f32 / compressed_size as f32
        };
        let skip = CompressionSkipReason::Ineffective { ratio };
        let error = CompressionError::IneffectiveRatio {
            ratio,
            size: original_size,
        };
        info!(
            target = "pipeline::compression",
            %algorithm,
            ratio,
            "compression ineffective; reverting to original bytes"
        );
        info!(
            target = "telemetry::compression",
            %algorithm,
            ratio,
            size = original_size,
            outcome = "ineffective"
        );
        debug!(%error, "compression skip classified");
        Ok(CompressionResult {
            original_size,
            compressed_size: original_size,
            compressed: false,
            algorithm: "ineffective".to_string(),
            reason: Some(skip),
        })
    }
}

/// Compress and return the actual compressed bytes
#[instrument(skip(data, policy), fields(segment_size = data.len()))]
pub fn compress_segment<'a>(
    data: &'a [u8],
    policy: &CompressionPolicy,
) -> Result<(Cow<'a, [u8]>, CompressionResult)> {
    let result = adaptive_compress(data, policy)?;

    if result.compressed {
        let compressed_data = match policy {
            CompressionPolicy::LZ4 { level } => compress_lz4(data, *level)
                .context("LZ4 compression backend failed while materialising segment")?,
            CompressionPolicy::Zstd { level } => compress_zstd(data, *level)
                .context("Zstd compression backend failed while materialising segment")?,
            CompressionPolicy::None => data.to_vec(),
        };

        verify_integrity(policy, &compressed_data, data)?;
        debug!(
            algorithm = %result.algorithm,
            compressed_len = result.compressed_size,
            "Segment compressed"
        );
        Ok((Cow::Owned(compressed_data), result))
    } else {
        if let Some(reason) = &result.reason {
            debug!(?reason, "Segment left uncompressed");
        }
        Ok((Cow::Borrowed(data), result))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tracing_test::traced_test;

    #[test]
    fn test_entropy_estimation() {
        // All zeros - low entropy
        let zeros = vec![0u8; 1024];
        let entropy = estimate_entropy(&zeros);
        assert!(entropy < 0.1, "Expected low entropy, got {}", entropy);

        // Random data - high entropy
        let random: Vec<u8> = (0..1024).map(|i| (i * 7919) as u8).collect();
        let entropy = estimate_entropy(&random);
        assert!(entropy > 7.0, "Expected high entropy, got {}", entropy);

        // Repeated pattern - medium entropy
        let pattern = b"ABCD".repeat(256);
        let entropy = estimate_entropy(&pattern);
        assert!(
            entropy > 1.5 && entropy < 3.0,
            "Expected medium entropy, got {}",
            entropy
        );
    }

    #[test]
    fn test_entropy_gate() {
        let repetitive = vec![b'A'; 1024];
        assert!(entropy_skip_reason(&repetitive).is_none());

        let random: Vec<u8> = (0..1024).map(|i| ((i * 7919) % 256) as u8).collect();
        assert!(matches!(
            entropy_skip_reason(&random),
            Some(CompressionSkipReason::Entropy { .. })
        ));
    }

    #[test]
    fn test_lz4_compression() {
        let data = b"Hello SPACE! ".repeat(1000);
        let policy = CompressionPolicy::LZ4 { level: 1 };

        let result = adaptive_compress(&data, &policy).unwrap();

        assert!(result.compressed);
        assert!(
            result.ratio() > 3.0,
            "Expected 3x+ compression, got {:.2}x",
            result.ratio()
        );
        assert_eq!(result.algorithm, "lz4_1");

        println!(
            "✅ LZ4: {:.2}x compression ({} -> {} bytes)",
            result.ratio(),
            result.original_size,
            result.compressed_size
        );
    }

    #[test]
    fn test_zstd_compression() {
        let data =
            b"This is SPACE - Storage Platform for Adaptive Computational Ecosystems. ".repeat(500);
        let policy = CompressionPolicy::Zstd { level: 3 };

        let result = adaptive_compress(&data, &policy).unwrap();

        assert!(result.compressed);
        assert!(
            result.ratio() > 4.0,
            "Expected 4x+ compression, got {:.2}x",
            result.ratio()
        );
        assert_eq!(result.algorithm, "zstd_3");

        println!(
            "✅ Zstd: {:.2}x compression ({} -> {} bytes)",
            result.ratio(),
            result.original_size,
            result.compressed_size
        );
    }

    #[test]
    fn test_no_compression_policy() {
        let data = b"Some data".repeat(100);
        let policy = CompressionPolicy::None;

        let result = adaptive_compress(&data, &policy).unwrap();

        assert!(!result.compressed);
        assert_eq!(result.original_size, result.compressed_size);
        assert_eq!(result.algorithm, "none");
    }

    #[test]
    fn test_random_data_skipped() {
        // Generate high-entropy data
        let random: Vec<u8> = (0..4096).map(|i| ((i * 7919) % 256) as u8).collect();
        let policy = CompressionPolicy::LZ4 { level: 1 };

        let result = adaptive_compress(&random, &policy).unwrap();

        // Should skip compression due to high entropy
        assert!(!result.compressed);
        assert!(matches!(
            result.reason,
            Some(CompressionSkipReason::Entropy { .. })
        ));
        println!("✅ Random data handling: {}", result.algorithm);
    }

    #[test]
    fn test_roundtrip_lz4() {
        let original = b"SPACE roundtrip test! ".repeat(500);
        let policy = CompressionPolicy::LZ4 { level: 4 };

        let (compressed, result) = compress_segment(&original, &policy).unwrap();
        assert!(result.compressed);

        let decompressed = decompress_lz4(compressed.as_ref()).unwrap();
        assert_eq!(original.as_slice(), decompressed.as_slice());

        println!(
            "✅ LZ4 roundtrip: {} -> {} -> {} bytes",
            original.len(),
            compressed.len(),
            decompressed.len()
        );
    }

    #[test]
    fn test_roundtrip_zstd() {
        let original = b"SPACE Zstd roundtrip! ".repeat(500);
        let policy = CompressionPolicy::Zstd { level: 6 };

        let (compressed, result) = compress_segment(&original, &policy).unwrap();
        assert!(result.compressed);

        let decompressed = decompress_zstd(compressed.as_ref()).unwrap();
        assert_eq!(original.as_slice(), decompressed.as_slice());

        println!(
            "✅ Zstd roundtrip: {} -> {} -> {} bytes",
            original.len(),
            compressed.len(),
            decompressed.len()
        );
    }

    #[test]
    fn test_ineffective_compression() {
        // Use a pattern that genuinely won't compress well
        // XOR pattern creates high entropy
        let mut pseudo_compressed = Vec::with_capacity(1000);
        for i in 0..1000 {
            pseudo_compressed.push((i ^ (i >> 3) ^ (i >> 5)) as u8);
        }

        let policy = CompressionPolicy::LZ4 { level: 9 };

        let result = adaptive_compress(&pseudo_compressed, &policy).unwrap();

        if result.compressed {
            assert!(
                result.ratio() < 1.5,
                "Expected low compression ratio, got {:.2}x",
                result.ratio()
            );
        } else {
            assert!(
                matches!(
                    result.reason,
                    Some(CompressionSkipReason::Ineffective { .. })
                        | Some(CompressionSkipReason::Entropy { .. })
                ),
                "Expected skip reason for ineffective compression"
            );
        }

        println!(
            "✅ Ineffective compression detected: {} (ratio: {:.2}x)",
            result.algorithm,
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

        assert!(logs_contain("skipping compression due to high entropy"));
        assert!(logs_contain("target=\"telemetry::compression\""));
        assert!(logs_contain("outcome=\"skip_entropy\""));
    }

    #[traced_test]
    #[test]
    fn test_successful_compression_emits_telemetry() {
        let data = b"Space telemetry test ".repeat(1024);
        let policy = CompressionPolicy::LZ4 { level: 1 };

        let result = adaptive_compress(&data, &policy).unwrap();
        assert!(result.compressed);

        assert!(logs_contain("compression successful"));
        assert!(logs_contain("target=\"telemetry::compression\""));
        assert!(logs_contain("outcome=\"compressed\""));
    }
}
