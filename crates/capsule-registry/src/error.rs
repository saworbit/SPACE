pub use compression::CompressionError;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compression_error_messages() {
        let entropy = CompressionError::EntropySkip {
            entropy: 7.8,
            size: 4096,
        };
        assert!(entropy
            .to_string()
            .contains("Entropy too high (7.80 bits/byte)"));

        let ineffective = CompressionError::IneffectiveRatio {
            ratio: 1.01,
            size: 8192,
        };
        assert!(ineffective
            .to_string()
            .contains("Compression ineffective (ratio 1.01)"));
    }
}
