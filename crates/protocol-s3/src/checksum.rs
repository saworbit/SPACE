//! The digests an S3 PUT can say its body has (#196): `Content-MD5`, the
//! `x-amz-checksum-*` headers, and a hex `x-amz-content-sha256`. They're
//! computed as the body streams in, and checked before the object is
//! stored.

use base64::Engine as _;
use sha2::Digest as _;

/// A digest algorithm a PUT header can name.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Algorithm {
    Md5,
    Crc32,
    Crc32c,
    Crc64Nvme,
    Sha1,
    Sha256,
}

impl Algorithm {
    /// The algorithm an `x-amz-checksum-*` header or
    /// `x-amz-sdk-checksum-algorithm` names, ignoring case.
    pub fn from_checksum_name(name: &str) -> Option<Self> {
        [
            ("crc32", Self::Crc32),
            ("crc32c", Self::Crc32c),
            ("crc64nvme", Self::Crc64Nvme),
            ("sha1", Self::Sha1),
            ("sha256", Self::Sha256),
        ]
        .into_iter()
        .find(|(n, _)| n.eq_ignore_ascii_case(name))
        .map(|(_, algorithm)| algorithm)
    }

    /// The name S3 gives it in errors.
    pub fn name(self) -> &'static str {
        match self {
            Self::Md5 => "Content-MD5",
            Self::Crc32 => "CRC32",
            Self::Crc32c => "CRC32C",
            Self::Crc64Nvme => "CRC64NVME",
            Self::Sha1 => "SHA1",
            Self::Sha256 => "SHA256",
        }
    }

    fn len(self) -> usize {
        match self {
            Self::Md5 => 16,
            Self::Crc32 | Self::Crc32c => 4,
            Self::Crc64Nvme => 8,
            Self::Sha1 => 20,
            Self::Sha256 => 32,
        }
    }
}

/// A digest a request header says the body has.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Expected {
    /// The header it came from, lowercase.
    pub header: String,
    pub algorithm: Algorithm,
    pub digest: Vec<u8>,
}

impl Expected {
    /// From a header that holds the digest in base64, as `Content-MD5` and
    /// `x-amz-checksum-*` do. `None` unless it's a digest of the right
    /// length.
    pub fn from_base64(header: &str, algorithm: Algorithm, value: &str) -> Option<Self> {
        let digest = base64::engine::general_purpose::STANDARD
            .decode(value.trim())
            .ok()?;
        Self::new(header, algorithm, digest)
    }

    /// From a header that holds the digest in hex, as
    /// `x-amz-content-sha256` does.
    pub fn from_hex(header: &str, algorithm: Algorithm, value: &str) -> Option<Self> {
        Self::new(header, algorithm, hex::decode(value.trim()).ok()?)
    }

    fn new(header: &str, algorithm: Algorithm, digest: Vec<u8>) -> Option<Self> {
        (digest.len() == algorithm.len()).then(|| Self {
            header: header.to_ascii_lowercase(),
            algorithm,
            digest,
        })
    }
}

#[derive(Clone)]
enum Hasher {
    Md5(md5::Md5),
    Sha1(sha1::Sha1),
    Sha256(sha2::Sha256),
    Crc32(u32, &'static [u32; 256]),
    Crc64(u64),
}

impl Hasher {
    fn new(algorithm: Algorithm) -> Self {
        match algorithm {
            Algorithm::Md5 => Self::Md5(md5::Md5::new()),
            Algorithm::Sha1 => Self::Sha1(sha1::Sha1::new()),
            Algorithm::Sha256 => Self::Sha256(sha2::Sha256::new()),
            Algorithm::Crc32 => Self::Crc32(!0, &CRC32),
            Algorithm::Crc32c => Self::Crc32(!0, &CRC32C),
            Algorithm::Crc64Nvme => Self::Crc64(!0),
        }
    }

    fn update(&mut self, bytes: &[u8]) {
        match self {
            Self::Md5(h) => h.update(bytes),
            Self::Sha1(h) => h.update(bytes),
            Self::Sha256(h) => h.update(bytes),
            Self::Crc32(crc, table) => {
                for b in bytes {
                    *crc = table[((*crc ^ u32::from(*b)) & 0xFF) as usize] ^ (*crc >> 8);
                }
            }
            Self::Crc64(crc) => {
                for b in bytes {
                    *crc = CRC64NVME[((*crc ^ u64::from(*b)) & 0xFF) as usize] ^ (*crc >> 8);
                }
            }
        }
    }

    /// The digest as S3 writes it: CRCs big-endian.
    fn finish(self) -> Vec<u8> {
        match self {
            Self::Md5(h) => h.finalize().to_vec(),
            Self::Sha1(h) => h.finalize().to_vec(),
            Self::Sha256(h) => h.finalize().to_vec(),
            Self::Crc32(crc, _) => (!crc).to_be_bytes().to_vec(),
            Self::Crc64(crc) => (!crc).to_be_bytes().to_vec(),
        }
    }
}

/// The digest of `bytes` with `algorithm`, as S3 writes it: CRCs
/// big-endian.
pub fn digest_of(algorithm: Algorithm, bytes: &[u8]) -> Vec<u8> {
    let mut hasher = Hasher::new(algorithm);
    hasher.update(bytes);
    hasher.finish()
}

/// The digests of a body as it streams in, with each of a set of
/// algorithms. A trailer's digest arrives after the body, so what to check
/// against is given at the end.
pub struct Running {
    hashers: Vec<(Algorithm, Hasher)>,
}

impl Running {
    pub fn new(algorithms: impl IntoIterator<Item = Algorithm>) -> Self {
        let mut hashers: Vec<(Algorithm, Hasher)> = Vec::new();
        for algorithm in algorithms {
            if !hashers.iter().any(|(a, _)| *a == algorithm) {
                hashers.push((algorithm, Hasher::new(algorithm)));
            }
        }
        Self { hashers }
    }

    pub fn update(&mut self, bytes: &[u8]) {
        for (_, hasher) in &mut self.hashers {
            hasher.update(bytes);
        }
    }

    /// The first of `expected` the body read so far doesn't have. One with
    /// an algorithm this doesn't compute doesn't match.
    pub fn check(&self, expected: &[Expected]) -> Option<Expected> {
        expected
            .iter()
            .find(|e| {
                self.hashers
                    .iter()
                    .find(|(a, _)| *a == e.algorithm)
                    .is_none_or(|(_, hasher)| hasher.clone().finish() != e.digest)
            })
            .cloned()
    }
}

/// The table of a reflected CRC with polynomial `poly`, initial value and
/// final XOR all ones, as CRC-32, CRC-32C and CRC-64/NVME are.
const fn crc32_table(poly: u32) -> [u32; 256] {
    let mut table = [0u32; 256];
    let mut i = 0;
    while i < 256 {
        let mut crc = i as u32;
        let mut bit = 0;
        while bit < 8 {
            crc = if crc & 1 != 0 {
                (crc >> 1) ^ poly
            } else {
                crc >> 1
            };
            bit += 1;
        }
        table[i] = crc;
        i += 1;
    }
    table
}

const fn crc64_table(poly: u64) -> [u64; 256] {
    let mut table = [0u64; 256];
    let mut i = 0;
    while i < 256 {
        let mut crc = i as u64;
        let mut bit = 0;
        while bit < 8 {
            crc = if crc & 1 != 0 {
                (crc >> 1) ^ poly
            } else {
                crc >> 1
            };
            bit += 1;
        }
        table[i] = crc;
        i += 1;
    }
    table
}

static CRC32: [u32; 256] = crc32_table(0xEDB8_8320);
static CRC32C: [u32; 256] = crc32_table(0x82F6_3B78);
static CRC64NVME: [u64; 256] = crc64_table(0x9A6C_9329_AC4B_C9B5);

#[cfg(test)]
mod tests {
    use super::*;

    fn digest(algorithm: Algorithm, data: &[u8]) -> Vec<u8> {
        let mut hasher = Hasher::new(algorithm);
        hasher.update(data);
        hasher.finish()
    }

    #[test]
    fn crcs_match_their_check_values() {
        // The CRC catalogue's check value: the CRC of "123456789".
        let check = b"123456789";
        assert_eq!(
            digest(Algorithm::Crc32, check),
            0xCBF4_3926u32.to_be_bytes()
        );
        assert_eq!(
            digest(Algorithm::Crc32c, check),
            0xE306_9283u32.to_be_bytes()
        );
        assert_eq!(
            digest(Algorithm::Crc64Nvme, check),
            0xAE8B_1486_0A79_9888u64.to_be_bytes()
        );
    }

    #[test]
    fn digests_match_what_clients_send() {
        // From #196: what S3 and boto3 give for "hello".
        for (header, algorithm, value) in [
            ("Content-MD5", Algorithm::Md5, "XUFAKrxLKna5cZ2REBfFkg=="),
            ("x-amz-checksum-crc32", Algorithm::Crc32, "NhCmhg=="),
            (
                "x-amz-checksum-sha1",
                Algorithm::Sha1,
                "qvTGHdzF6KLavt4PO0gs2a6pQ00=",
            ),
            (
                "x-amz-checksum-sha256",
                Algorithm::Sha256,
                "LPJNul+wow4m6DsqxbninhsWHlwfp0JecwQzYpOLmCQ=",
            ),
        ] {
            let expected = Expected::from_base64(header, algorithm, value).unwrap();
            assert_eq!(expected.header, header.to_ascii_lowercase());
            let mut running = Running::new([algorithm, algorithm]);
            // In pieces, as a body streams in.
            running.update(b"he");
            running.update(b"llo");
            let expected = std::slice::from_ref(&expected);
            assert_eq!(running.check(expected), None, "{header}");
            running.update(b"!");
            assert_eq!(
                running.check(expected),
                Some(expected[0].clone()),
                "{header}"
            );
        }
        let sha = "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824";
        let expected = Expected::from_hex("x-amz-content-sha256", Algorithm::Sha256, sha).unwrap();
        let mut running = Running::new([Algorithm::Sha256]);
        running.update(b"hello");
        assert_eq!(running.check(std::slice::from_ref(&expected)), None);
        // A digest with an algorithm it doesn't compute doesn't match.
        let crc =
            Expected::from_base64("x-amz-checksum-crc32", Algorithm::Crc32, "NhCmhg==").unwrap();
        assert_eq!(running.check(&[expected, crc.clone()]), Some(crc));
    }

    #[test]
    fn digests_of_the_wrong_length_or_encoding_are_refused() {
        assert!(Expected::from_base64("content-md5", Algorithm::Md5, "NhCmhg==").is_none());
        assert!(Expected::from_base64("content-md5", Algorithm::Md5, "not base64!").is_none());
        assert!(Expected::from_hex("x-amz-content-sha256", Algorithm::Sha256, "abc").is_none());
        assert_eq!(
            Algorithm::from_checksum_name("CRC64NVME"),
            Some(Algorithm::Crc64Nvme)
        );
        assert_eq!(Algorithm::from_checksum_name("xxhash64"), None);
    }
}
