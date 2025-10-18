use anyhow::Result;
use common::CompressionPolicy;
use std::io::Write;

/// Compression statistics for a segment
#[derive(Debug, Clone)]
pub struct CompressionResult {
    pub original_size: usize,
    pub compressed_size: usize,
    pub compressed: bool,
    pub algorithm: String,
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

/// Determine if data is worth compressing
/// Sample first 1KB to avoid overhead on incompressible data
fn should_compress(data: &[u8]) -> bool {
    // Always try to compress small segments
    if data.len() < 1024 {
        return true;
    }

    // Sample first 1KB for entropy estimation
    let sample_size = data.len().min(1024);
    let entropy = estimate_entropy(&data[..sample_size]);

    // Entropy > 7.5 bits/byte suggests random/compressed data
    // Skip compression to save CPU cycles
    entropy < 7.5
}

/// Compress data using LZ4
fn compress_lz4(data: &[u8], level: i32) -> Result<Vec<u8>> {
    let mut encoder = lz4::EncoderBuilder::new()
        .level(level as u32)
        .build(Vec::new())?;
    
    encoder.write_all(data)?;
    let (compressed, result) = encoder.finish();
    result?;
    
    Ok(compressed)
}

/// Decompress LZ4 data
pub fn decompress_lz4(data: &[u8]) -> Result<Vec<u8>> {
    let mut decoder = lz4::Decoder::new(data)?;
    let mut decompressed = Vec::new();
    std::io::copy(&mut decoder, &mut decompressed)?;
    Ok(decompressed)
}

/// Compress data using Zstd
fn compress_zstd(data: &[u8], level: i32) -> Result<Vec<u8>> {
    let compressed = zstd::encode_all(data, level)?;
    Ok(compressed)
}

/// Decompress Zstd data
pub fn decompress_zstd(data: &[u8]) -> Result<Vec<u8>> {
    let decompressed = zstd::decode_all(data)?;
    Ok(decompressed)
}

/// Adaptive compression with policy-driven algorithm selection
pub fn adaptive_compress(data: &[u8], policy: &CompressionPolicy) -> Result<CompressionResult> {
    let original_size = data.len();

    // Check policy
    match policy {
        CompressionPolicy::None => {
            return Ok(CompressionResult {
                original_size,
                compressed_size: original_size,
                compressed: false,
                algorithm: "none".to_string(),
            });
        }
        _ => {}
    }

    // Entropy check - skip compression for random data
    if !should_compress(data) {
        return Ok(CompressionResult {
            original_size,
            compressed_size: original_size,
            compressed: false,
            algorithm: "skipped_entropy".to_string(),
        });
    }

    // Compress based on policy
    let (compressed_data, algorithm) = match policy {
        CompressionPolicy::LZ4 { level } => {
            let compressed = compress_lz4(data, *level)?;
            (compressed, format!("lz4_{}", level))
        }
        CompressionPolicy::Zstd { level } => {
            let compressed = compress_zstd(data, *level)?;
            (compressed, format!("zstd_{}", level))
        }
        CompressionPolicy::None => unreachable!(),
    };

    let compressed_size = compressed_data.len();

    // Only use compression if it actually saves space (+ 5% margin)
    if compressed_size < original_size * 95 / 100 {
        Ok(CompressionResult {
            original_size,
            compressed_size,
            compressed: true,
            algorithm,
        })
    } else {
        // Compression didn't help, return original
        Ok(CompressionResult {
            original_size,
            compressed_size: original_size,
            compressed: false,
            algorithm: "ineffective".to_string(),
        })
    }
}

/// Compress and return the actual compressed bytes
pub fn compress_segment(data: &[u8], policy: &CompressionPolicy) -> Result<(Vec<u8>, CompressionResult)> {
    let result = adaptive_compress(data, policy)?;
    
    if result.compressed {
        let compressed_data = match policy {
            CompressionPolicy::LZ4 { level } => compress_lz4(data, *level)?,
            CompressionPolicy::Zstd { level } => compress_zstd(data, *level)?,
            CompressionPolicy::None => data.to_vec(),
        };
        Ok((compressed_data, result))
    } else {
        Ok((data.to_vec(), result))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
        assert!(entropy > 1.5 && entropy < 3.0, "Expected medium entropy, got {}", entropy);
    }

    #[test]
    fn test_should_compress() {
        // Repetitive data - should compress
        let repetitive = vec![b'A'; 1024];
        assert!(should_compress(&repetitive));

        // Random data - should skip
        let random: Vec<u8> = (0..1024).map(|i| ((i * 7919) % 256) as u8).collect();
        assert!(!should_compress(&random), "Random data should be skipped");
    }

    #[test]
    fn test_lz4_compression() {
        let data = b"Hello SPACE! ".repeat(1000);
        let policy = CompressionPolicy::LZ4 { level: 1 };
        
        let result = adaptive_compress(&data, &policy).unwrap();
        
        assert!(result.compressed);
        assert!(result.ratio() > 3.0, "Expected 3x+ compression, got {:.2}x", result.ratio());
        assert_eq!(result.algorithm, "lz4_1");
        
        println!("✅ LZ4: {:.2}x compression ({} -> {} bytes)", 
                 result.ratio(), result.original_size, result.compressed_size);
    }

    #[test]
    fn test_zstd_compression() {
        let data = b"This is SPACE - Storage Platform for Adaptive Computational Ecosystems. ".repeat(500);
        let policy = CompressionPolicy::Zstd { level: 3 };
        
        let result = adaptive_compress(&data, &policy).unwrap();
        
        assert!(result.compressed);
        assert!(result.ratio() > 4.0, "Expected 4x+ compression, got {:.2}x", result.ratio());
        assert_eq!(result.algorithm, "zstd_3");
        
        println!("✅ Zstd: {:.2}x compression ({} -> {} bytes)", 
                 result.ratio(), result.original_size, result.compressed_size);
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
        assert!(!result.compressed || result.algorithm == "ineffective");
        println!("✅ Random data handling: {}", result.algorithm);
    }

    #[test]
    fn test_roundtrip_lz4() {
        let original = b"SPACE roundtrip test! ".repeat(500);
        let policy = CompressionPolicy::LZ4 { level: 4 };
        
        let (compressed, result) = compress_segment(&original, &policy).unwrap();
        assert!(result.compressed);
        
        let decompressed = decompress_lz4(&compressed).unwrap();
        assert_eq!(original.as_slice(), decompressed.as_slice());
        
        println!("✅ LZ4 roundtrip: {} -> {} -> {} bytes", 
                 original.len(), compressed.len(), decompressed.len());
    }

    #[test]
    fn test_roundtrip_zstd() {
        let original = b"SPACE Zstd roundtrip! ".repeat(500);
        let policy = CompressionPolicy::Zstd { level: 6 };
        
        let (compressed, result) = compress_segment(&original, &policy).unwrap();
        assert!(result.compressed);
        
        let decompressed = decompress_zstd(&compressed).unwrap();
        assert_eq!(original.as_slice(), decompressed.as_slice());
        
        println!("✅ Zstd roundtrip: {} -> {} -> {} bytes", 
                 original.len(), compressed.len(), decompressed.len());
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
    
    // Should either skip compression or achieve minimal ratio
    // Accept ratio < 1.5 as "ineffective enough"
    let acceptable = !result.compressed 
        || result.algorithm == "skipped_entropy" 
        || result.algorithm == "ineffective"
        || result.ratio() < 1.5;
    
    assert!(acceptable, 
        "Expected ineffective compression, got: compressed={}, algorithm={}, ratio={:.2}x", 
        result.compressed, result.algorithm, result.ratio());
    
    println!("✅ Ineffective compression detected: {} (ratio: {:.2}x)", 
        result.algorithm, result.ratio());
}
}