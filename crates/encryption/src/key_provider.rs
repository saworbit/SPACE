use async_trait::async_trait;
use zeroize::Zeroizing;

use crate::error::{EncryptionError, Result};
use crate::keymanager::MASTER_KEY_SIZE;

#[async_trait]
pub trait KeyProvider: Send + Sync {
    /// Fetch the master key material (32 bytes).
    ///
    /// Implementations should avoid leaking the key into logs and should prefer
    /// secure memory handling. The returned buffer is zeroized on drop.
    async fn fetch_master_key(&self) -> Result<Zeroizing<Vec<u8>>>;
}

#[derive(Debug, Clone)]
pub struct EnvKeyProvider {
    var: String,
}

impl EnvKeyProvider {
    pub fn new(var: impl Into<String>) -> Self {
        Self { var: var.into() }
    }
}

impl Default for EnvKeyProvider {
    fn default() -> Self {
        Self::new("SPACE_MASTER_KEY")
    }
}

/// Backwards-compatible name (Phase 6 and earlier).
pub type EnvProvider = EnvKeyProvider;

#[async_trait]
impl KeyProvider for EnvKeyProvider {
    async fn fetch_master_key(&self) -> Result<Zeroizing<Vec<u8>>> {
        Ok(Zeroizing::new(master_key_from_env(&self.var)?.to_vec()))
    }
}

#[derive(Debug, Clone)]
pub struct FileKeyProvider {
    pub path: std::path::PathBuf,
}

impl FileKeyProvider {
    pub fn new(path: impl Into<std::path::PathBuf>) -> Self {
        Self { path: path.into() }
    }
}

/// Backwards-compatible name used in some docs/specs.
pub type FileProvider = FileKeyProvider;

#[async_trait]
impl KeyProvider for FileKeyProvider {
    async fn fetch_master_key(&self) -> Result<Zeroizing<Vec<u8>>> {
        let raw = std::fs::read(&self.path).map_err(|e| {
            EncryptionError::InvalidConfiguration(format!(
                "failed to read master key file {}: {}",
                self.path.display(),
                e
            ))
        })?;

        // A key file holds the key in hex, as the variable does, or as its
        // 32 raw bytes.
        let trimmed = trim_ascii_whitespace(&raw);
        let source = format!("the master key file {}", self.path.display());
        if let Some(key) = std::str::from_utf8(trimmed)
            .ok()
            .and_then(|text| parse_master_key(text, &source).ok())
        {
            return Ok(Zeroizing::new(key.to_vec()));
        }
        if trimmed.len() != MASTER_KEY_SIZE {
            return Err(EncryptionError::InvalidKeyLength {
                expected: MASTER_KEY_SIZE,
                actual: trimmed.len(),
            });
        }
        Ok(Zeroizing::new(trimmed.to_vec()))
    }
}

#[derive(Debug, Clone)]
pub struct AwsKmsProvider {
    pub key_id: String,
}

#[async_trait]
impl KeyProvider for AwsKmsProvider {
    async fn fetch_master_key(&self) -> Result<Zeroizing<Vec<u8>>> {
        Err(EncryptionError::InvalidConfiguration(format!(
            "AwsKmsProvider not implemented (key_id={}); integrate AWS SDK + KMS decrypt",
            self.key_id
        )))
    }
}

#[derive(Debug, Clone)]
pub struct HashiCorpVaultProvider {
    pub address: String,
    pub secret_path: String,
}

#[async_trait]
impl KeyProvider for HashiCorpVaultProvider {
    async fn fetch_master_key(&self) -> Result<Zeroizing<Vec<u8>>> {
        Err(EncryptionError::InvalidConfiguration(format!(
            "HashiCorpVaultProvider not implemented (addr={}, path={}); integrate Vault transit/kv",
            self.address, self.secret_path
        )))
    }
}

/// Parse a master key given as text, as `SPACE_MASTER_KEY` holds it: 64 hex
/// characters, such as `openssl rand -hex 32` prints, optionally after
/// `0x` and with whitespace around them. Anything else is an error, so a
/// key reads the same wherever it's read (#193). `source` names where it
/// came from; the error never holds the key.
pub fn parse_master_key(raw: &str, source: &str) -> Result<Zeroizing<[u8; MASTER_KEY_SIZE]>> {
    let raw = raw.trim();
    let hex = raw
        .strip_prefix("0x")
        .or_else(|| raw.strip_prefix("0X"))
        .unwrap_or(raw);
    let invalid = |why: String| {
        EncryptionError::InvalidConfiguration(format!(
            "{source} must be 64 hex characters, such as `openssl rand -hex 32` prints: {why}"
        ))
    };
    if !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(invalid("it has characters that aren't hex".to_string()));
    }
    if hex.len() != MASTER_KEY_SIZE * 2 {
        return Err(invalid(format!("it has {} of them", hex.len())));
    }
    let mut key = Zeroizing::new([0u8; MASTER_KEY_SIZE]);
    hex::decode_to_slice(hex, key.as_mut_slice()).map_err(|err| invalid(err.to_string()))?;
    Ok(key)
}

/// The master key in environment variable `var` ([`parse_master_key`]). A
/// variable that isn't set is an error, and so is one that's set but
/// invalid: there's no fallback key (#84).
pub fn master_key_from_env(var: &str) -> Result<Zeroizing<[u8; MASTER_KEY_SIZE]>> {
    let raw = std::env::var(var).map_err(|err| {
        EncryptionError::InvalidConfiguration(match err {
            std::env::VarError::NotPresent => format!("{var} environment variable not set"),
            std::env::VarError::NotUnicode(_) => format!("{var} isn't valid text"),
        })
    })?;
    parse_master_key(&raw, var)
}

fn trim_ascii_whitespace(mut bytes: &[u8]) -> &[u8] {
    while let Some((first, rest)) = bytes.split_first() {
        if first.is_ascii_whitespace() {
            bytes = rest;
        } else {
            break;
        }
    }
    while let Some((last, rest)) = bytes.split_last() {
        if last.is_ascii_whitespace() {
            bytes = rest;
        } else {
            break;
        }
    }
    bytes
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::future::Future;
    use std::task::{Context, Poll, Waker};

    fn block_on<F: Future>(fut: F) -> F::Output {
        let mut fut = std::pin::pin!(fut);
        match fut.as_mut().poll(&mut Context::from_waker(Waker::noop())) {
            Poll::Ready(out) => out,
            Poll::Pending => panic!("env provider should not suspend"),
        }
    }

    #[test]
    fn env_provider_errors_when_variable_is_unset() {
        let provider = EnvKeyProvider::new("SPACE_TEST_KEY_PROVIDER_UNSET");
        std::env::remove_var("SPACE_TEST_KEY_PROVIDER_UNSET");
        assert!(block_on(provider.fetch_master_key()).is_err());
    }

    /// Both ways of reading the variable take the same spellings, and refuse
    /// the same ones (#193).
    #[test]
    fn every_reader_takes_the_same_spellings() {
        let var = "SPACE_TEST_KEY_SPELLINGS";
        let hex = "07".repeat(32);
        let accepted = [
            hex.clone(),
            format!("0x{hex}"),
            format!("0X{}", hex.to_uppercase()),
            format!("{hex}\n"),
            format!("  {hex}  "),
        ];
        let refused = [
            ("z".repeat(32), "aren't hex"),
            ("not-valid-hex!".to_string(), "aren't hex"),
            ("07".repeat(16), "it has 32 of them"),
            (format!("{hex}0"), "it has 65 of them"),
            (String::new(), "it has 0 of them"),
        ];
        for value in &accepted {
            std::env::set_var(var, value);
            let provided = block_on(EnvKeyProvider::new(var).fetch_master_key()).unwrap();
            let read = master_key_from_env(var).unwrap();
            assert_eq!(provided.as_slice(), &[7u8; MASTER_KEY_SIZE], "{value:?}");
            assert_eq!(read.as_slice(), provided.as_slice(), "{value:?}");
        }
        for (value, why) in &refused {
            std::env::set_var(var, value);
            let provided = block_on(EnvKeyProvider::new(var).fetch_master_key()).unwrap_err();
            let read = master_key_from_env(var).unwrap_err();
            assert_eq!(provided.to_string(), read.to_string(), "{value:?}");
            assert!(read.to_string().contains(why), "{value:?}: {read}");
            assert!(read.to_string().contains("64 hex characters"), "{read}");
        }
        std::env::remove_var(var);
    }

    /// A value that isn't valid text is named as such, not as unset.
    #[test]
    fn a_key_that_isnt_text_is_not_reported_as_unset() {
        let var = "SPACE_TEST_KEY_NOT_TEXT";
        #[cfg(windows)]
        let value = {
            use std::os::windows::ffi::OsStringExt;
            std::ffi::OsString::from_wide(&[0xD800])
        };
        #[cfg(unix)]
        let value = {
            use std::os::unix::ffi::OsStrExt;
            std::ffi::OsStr::from_bytes(&[0xff]).to_os_string()
        };
        std::env::set_var(var, &value);
        let err = master_key_from_env(var).unwrap_err().to_string();
        std::env::remove_var(var);
        assert!(err.contains("isn't valid text"), "{err}");
    }

    #[test]
    fn a_key_file_holds_hex_or_raw_bytes() {
        let dir = std::env::temp_dir().join(format!("space_key_file_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("key");
        for (contents, key) in [
            (
                format!("{}\n", "07".repeat(32)).into_bytes(),
                [7u8; MASTER_KEY_SIZE],
            ),
            (
                [0xA5u8; MASTER_KEY_SIZE].to_vec(),
                [0xA5u8; MASTER_KEY_SIZE],
            ),
            (
                b"0123456789abcdefghijklmnopqrstuv".to_vec(),
                *b"0123456789abcdefghijklmnopqrstuv",
            ),
        ] {
            std::fs::write(&path, &contents).unwrap();
            let got = block_on(FileKeyProvider::new(&path).fetch_master_key()).unwrap();
            assert_eq!(got.as_slice(), &key);
        }
        std::fs::write(&path, b"short").unwrap();
        assert!(block_on(FileKeyProvider::new(&path).fetch_master_key()).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn env_provider_reads_hex_key() {
        let provider = EnvKeyProvider::new("SPACE_TEST_KEY_PROVIDER_SET");
        std::env::set_var("SPACE_TEST_KEY_PROVIDER_SET", "07".repeat(32));
        let key = block_on(provider.fetch_master_key()).unwrap();
        std::env::remove_var("SPACE_TEST_KEY_PROVIDER_SET");
        assert_eq!(key.as_slice(), &[7u8; MASTER_KEY_SIZE]);
    }
}
