//! Record codec for the on-disk format (`docs/on-disk-format.md`).
//!
//! Every record in every store file has a fixed 48-byte frame, a header
//! whose layout depends on the record kind, and an optional body:
//!
//! ```text
//! 0   4  magic       "SPCR"
//! 4   1  kind
//! 5   1  version     layout version of this kind
//! 6   2  flags       bit 0 IGNORABLE
//! 8   4  header_len
//! 12  4  body_len
//! 16  16 body_sum    BLAKE3(body)[0..16], zero when there is no body
//! 32  16 head_sum    BLAKE3(frame[0..32] ‖ header)[0..16]
//! 48     header, then body
//! ```
//!
//! This module only encodes and decodes bytes. Reading and writing files is
//! up to the store (`nvram-sim` for the data log). All integers are
//! little-endian, and layouts are defined here field by field rather than by
//! a serde derive, so the format doesn't change when a Rust struct does.

use std::fmt;

use uuid::Uuid;

use crate::{Capsule, CapsuleId, ContentHash, Policy, Segment, SegmentId};

/// First four bytes of every record.
pub const MAGIC: [u8; 4] = *b"SPCR";
/// Size of the fixed frame in front of every record.
pub const FRAME_LEN: usize = 48;
/// A reader rejects a larger header before allocating for it.
pub const MAX_HEADER_LEN: u32 = 64 * 1024 * 1024;
/// A reader rejects a larger body before allocating for it.
pub const MAX_BODY_LEN: u32 = 32 * 1024 * 1024;
/// Store format described by `docs/on-disk-format.md`.
pub const FORMAT_VERSION: u16 = 1;
/// Frame flag: a reader that doesn't know the record's kind may skip it.
pub const FLAG_IGNORABLE: u16 = 1;

/// Record kinds this build understands.
pub mod kind {
    /// First record of every file.
    pub const FILE: u8 = 0x00;
    /// One segment: descriptor in the header, stored bytes in the body.
    pub const SEGMENT: u8 = 0x01;
    /// A capsule's manifest; commits a write.
    pub const CAPSULE: u8 = 0x10;
    /// Commits a delete.
    pub const DROP: u8 = 0x11;
    /// Creates a namespace.
    pub const NAMESPACE: u8 = 0x12;
    /// Points a name at a capsule.
    pub const BIND: u8 = 0x13;
    /// Removes a name.
    pub const UNBIND: u8 = 0x14;
    /// Records the store's encryption key.
    pub const KEY: u8 = 0x15;
    /// Several journal records applied all or nothing.
    pub const BATCH: u8 = 0x1F;
}

/// Layout version of the `FILE` header.
pub const FILE_VERSION: u8 = 1;
/// Layout version of the `SEGMENT` header.
pub const SEGMENT_VERSION: u8 = 1;
/// Layout version of every journal record's header.
pub const JOURNAL_VERSION: u8 = 1;

/// `NAMESPACE` op: create the namespace.
const NAMESPACE_CREATE: u8 = 1;

/// `KEY` cipher: XTS-AES-256 with a BLAKE3 MAC.
const KEY_CIPHER_XTS: u8 = 1;

const SUM_LEN: usize = 16;

/// Why a record or header couldn't be decoded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FormatError {
    /// The frame doesn't start with [`MAGIC`].
    BadMagic,
    /// The frame claims a header or body larger than a reader accepts.
    TooLarge { header_len: u32, body_len: u32 },
    /// `head_sum` doesn't match the frame and header.
    HeadChecksum,
    /// `body_sum` doesn't match the body.
    BodyChecksum,
    /// A header ended before all its fields were read.
    Truncated,
    /// A header had bytes left over after its last field.
    TrailingBytes,
    /// The record kind isn't one this build reads.
    UnknownKind(u8),
    /// The header layout version isn't one this build reads.
    UnsupportedVersion { kind: u8, version: u8 },
    /// A field holds a value the format doesn't allow.
    Invalid(&'static str),
}

impl fmt::Display for FormatError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::BadMagic => write!(f, "record doesn't start with the SPCR magic"),
            Self::TooLarge {
                header_len,
                body_len,
            } => write!(
                f,
                "record claims a {header_len}-byte header and a {body_len}-byte body, \
                 beyond what a reader accepts"
            ),
            Self::HeadChecksum => write!(f, "record frame or header fails its checksum"),
            Self::BodyChecksum => write!(f, "record body fails its checksum"),
            Self::Truncated => write!(f, "record header ends early"),
            Self::TrailingBytes => write!(f, "record header has bytes after its last field"),
            Self::UnknownKind(kind) => write!(f, "unknown record kind {kind:#04x}"),
            Self::UnsupportedVersion { kind, version } => {
                write!(
                    f,
                    "record kind {kind:#04x} has unsupported layout version {version}"
                )
            }
            Self::Invalid(why) => write!(f, "invalid record: {why}"),
        }
    }
}

impl std::error::Error for FormatError {}

fn sum(parts: &[&[u8]]) -> [u8; SUM_LEN] {
    let mut hasher = blake3::Hasher::new();
    for part in parts {
        hasher.update(part);
    }
    let mut out = [0u8; SUM_LEN];
    out.copy_from_slice(&hasher.finalize().as_bytes()[..SUM_LEN]);
    out
}

fn body_sum(body: &[u8]) -> [u8; SUM_LEN] {
    if body.is_empty() {
        [0u8; SUM_LEN]
    } else {
        sum(&[body])
    }
}

/// The fixed 48 bytes in front of every record.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Frame {
    pub kind: u8,
    pub version: u8,
    pub flags: u16,
    pub header_len: u32,
    pub body_len: u32,
    body_sum: [u8; SUM_LEN],
    head_sum: [u8; SUM_LEN],
}

impl Frame {
    /// Parse a frame, checking the magic and the size bounds. The head
    /// checksum also covers the header, so check it with
    /// [`Frame::verify_head`] once the header has been read.
    pub fn parse(bytes: &[u8; FRAME_LEN]) -> Result<Self, FormatError> {
        if bytes[0..4] != MAGIC {
            return Err(FormatError::BadMagic);
        }
        let header_len = u32::from_le_bytes(bytes[8..12].try_into().unwrap());
        let body_len = u32::from_le_bytes(bytes[12..16].try_into().unwrap());
        if header_len > MAX_HEADER_LEN || body_len > MAX_BODY_LEN {
            return Err(FormatError::TooLarge {
                header_len,
                body_len,
            });
        }
        Ok(Self {
            kind: bytes[4],
            version: bytes[5],
            flags: u16::from_le_bytes(bytes[6..8].try_into().unwrap()),
            header_len,
            body_len,
            body_sum: bytes[16..32].try_into().unwrap(),
            head_sum: bytes[32..48].try_into().unwrap(),
        })
    }

    fn summed_prefix(&self) -> [u8; 32] {
        let mut out = [0u8; 32];
        out[0..4].copy_from_slice(&MAGIC);
        out[4] = self.kind;
        out[5] = self.version;
        out[6..8].copy_from_slice(&self.flags.to_le_bytes());
        out[8..12].copy_from_slice(&self.header_len.to_le_bytes());
        out[12..16].copy_from_slice(&self.body_len.to_le_bytes());
        out[16..32].copy_from_slice(&self.body_sum);
        out
    }

    /// Check `head_sum` against the frame and its header.
    pub fn verify_head(&self, header: &[u8]) -> Result<(), FormatError> {
        if header.len() != self.header_len as usize {
            return Err(FormatError::Truncated);
        }
        if sum(&[&self.summed_prefix(), header]) == self.head_sum {
            Ok(())
        } else {
            Err(FormatError::HeadChecksum)
        }
    }

    /// Check `body_sum` against the body.
    pub fn verify_body(&self, body: &[u8]) -> Result<(), FormatError> {
        if body.len() != self.body_len as usize {
            return Err(FormatError::Truncated);
        }
        if body_sum(body) == self.body_sum {
            Ok(())
        } else {
            Err(FormatError::BodyChecksum)
        }
    }

    /// Bytes from the start of the frame to the start of the body.
    pub fn head_len(&self) -> u64 {
        FRAME_LEN as u64 + u64::from(self.header_len)
    }

    /// Bytes of the whole record: frame, header and body.
    pub fn record_len(&self) -> u64 {
        self.head_len() + u64::from(self.body_len)
    }

    pub fn is_ignorable(&self) -> bool {
        self.flags & FLAG_IGNORABLE != 0
    }
}

/// Encode a record's frame and header. The body is written straight after
/// them, unchanged; it's passed here only to be checksummed.
pub fn encode_head(
    kind: u8,
    version: u8,
    flags: u16,
    header: &[u8],
    body: &[u8],
) -> Result<Vec<u8>, FormatError> {
    let header_len = u32::try_from(header.len()).map_err(|_| FormatError::TooLarge {
        header_len: u32::MAX,
        body_len: 0,
    })?;
    let body_len = u32::try_from(body.len()).map_err(|_| FormatError::TooLarge {
        header_len,
        body_len: u32::MAX,
    })?;
    if header_len > MAX_HEADER_LEN || body_len > MAX_BODY_LEN {
        return Err(FormatError::TooLarge {
            header_len,
            body_len,
        });
    }
    let frame = Frame {
        kind,
        version,
        flags,
        header_len,
        body_len,
        body_sum: body_sum(body),
        head_sum: [0u8; SUM_LEN],
    };
    let prefix = frame.summed_prefix();
    let head_sum = sum(&[&prefix, header]);

    let mut out = Vec::with_capacity(FRAME_LEN + header.len());
    out.extend_from_slice(&prefix);
    out.extend_from_slice(&head_sum);
    out.extend_from_slice(header);
    Ok(out)
}

struct Writer(Vec<u8>);

impl Writer {
    fn new() -> Self {
        Self(Vec::new())
    }

    fn u8(&mut self, v: u8) {
        self.0.push(v);
    }

    fn u16(&mut self, v: u16) {
        self.0.extend_from_slice(&v.to_le_bytes());
    }

    fn u32(&mut self, v: u32) {
        self.0.extend_from_slice(&v.to_le_bytes());
    }

    fn u64(&mut self, v: u64) {
        self.0.extend_from_slice(&v.to_le_bytes());
    }

    fn bytes(&mut self, v: &[u8]) {
        self.0.extend_from_slice(v);
    }

    fn str(&mut self, v: &str) -> Result<(), FormatError> {
        let len = u16::try_from(v.len()).map_err(|_| FormatError::Invalid("string too long"))?;
        self.u16(len);
        self.bytes(v.as_bytes());
        Ok(())
    }
}

struct Reader<'a> {
    buf: &'a [u8],
}

impl<'a> Reader<'a> {
    fn new(buf: &'a [u8]) -> Self {
        Self { buf }
    }

    fn take(&mut self, n: usize) -> Result<&'a [u8], FormatError> {
        if self.buf.len() < n {
            return Err(FormatError::Truncated);
        }
        let (head, rest) = self.buf.split_at(n);
        self.buf = rest;
        Ok(head)
    }

    fn array<const N: usize>(&mut self) -> Result<[u8; N], FormatError> {
        Ok(self.take(N)?.try_into().unwrap())
    }

    fn u8(&mut self) -> Result<u8, FormatError> {
        Ok(self.take(1)?[0])
    }

    fn u16(&mut self) -> Result<u16, FormatError> {
        Ok(u16::from_le_bytes(self.array()?))
    }

    fn u32(&mut self) -> Result<u32, FormatError> {
        Ok(u32::from_le_bytes(self.array()?))
    }

    fn u64(&mut self) -> Result<u64, FormatError> {
        Ok(u64::from_le_bytes(self.array()?))
    }

    fn str(&mut self) -> Result<String, FormatError> {
        let len = self.u16()? as usize;
        let bytes = self.take(len)?;
        String::from_utf8(bytes.to_vec()).map_err(|_| FormatError::Invalid("string isn't UTF-8"))
    }

    fn finish(self) -> Result<(), FormatError> {
        if self.buf.is_empty() {
            Ok(())
        } else {
            Err(FormatError::TrailingBytes)
        }
    }
}

fn some_unless_zero<const N: usize>(bytes: [u8; N]) -> Option<[u8; N]> {
    (bytes != [0u8; N]).then_some(bytes)
}

/// Which store file a `FILE` record opens.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileKind {
    DataLog,
    Journal,
    Checkpoint,
    /// `root/store` of a storage root (`docs/on-disk-format.md` §10).
    StorageRoot,
}

impl FileKind {
    fn code(self) -> u8 {
        match self {
            Self::DataLog => 1,
            Self::Journal => 2,
            Self::Checkpoint => 3,
            Self::StorageRoot => 4,
        }
    }

    fn from_code(code: u8) -> Result<Self, FormatError> {
        match code {
            1 => Ok(Self::DataLog),
            2 => Ok(Self::Journal),
            3 => Ok(Self::Checkpoint),
            4 => Ok(Self::StorageRoot),
            _ => Err(FormatError::Invalid("unknown file kind")),
        }
    }
}

/// Header of the `FILE` record at the start of every store file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FileHeader {
    pub file_kind: FileKind,
    /// Store format; [`FORMAT_VERSION`] for files this build writes.
    pub format: u16,
    /// Chosen when the store is created; every file of one store shares it.
    pub store_id: Uuid,
    /// 0 for a new file, +1 each time compaction replaces it.
    pub generation: u64,
    /// Unix seconds.
    pub created_at: u64,
    /// The segment allocator never hands out an id below this.
    pub segment_id_floor: u64,
}

impl FileHeader {
    /// Encoded size of a version 1 header.
    pub const LEN: usize = 44;

    /// A header for a new file of a new store.
    pub fn new(file_kind: FileKind, created_at: u64) -> Self {
        Self {
            file_kind,
            format: FORMAT_VERSION,
            store_id: Uuid::new_v4(),
            generation: 0,
            created_at,
            segment_id_floor: 0,
        }
    }

    pub fn encode(&self) -> Vec<u8> {
        let mut w = Writer::new();
        w.u8(self.file_kind.code());
        w.u8(0);
        w.u16(self.format);
        w.bytes(self.store_id.as_bytes());
        w.u64(self.generation);
        w.u64(self.created_at);
        w.u64(self.segment_id_floor);
        w.0
    }

    pub fn decode(header: &[u8]) -> Result<Self, FormatError> {
        let mut r = Reader::new(header);
        let file_kind = FileKind::from_code(r.u8()?)?;
        if r.u8()? != 0 {
            return Err(FormatError::Invalid("reserved FILE byte isn't zero"));
        }
        let format = r.u16()?;
        let store_id = Uuid::from_bytes(r.array()?);
        let generation = r.u64()?;
        let created_at = r.u64()?;
        let segment_id_floor = r.u64()?;
        r.finish()?;
        Ok(Self {
            file_kind,
            format,
            store_id,
            generation,
            created_at,
            segment_id_floor,
        })
    }

    /// The whole `FILE` record: frame and header, no body.
    pub fn encode_record(&self) -> Vec<u8> {
        encode_head(kind::FILE, FILE_VERSION, 0, &self.encode(), &[])
            .expect("a FILE header is far below the size bounds")
    }
}

const SEGMENT_COMPRESSED: u16 = 1 << 0;
const SEGMENT_ENCRYPTED: u16 = 1 << 1;

/// Header of a `SEGMENT` record: everything about a segment that is fixed
/// when it's written. The stored bytes are the record's body.
///
/// Ref counts aren't here: they change, so they're derived from the capsules
/// that reference the segment instead (`docs/on-disk-format.md` §2).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SegmentDescriptor {
    pub segment_id: SegmentId,
    /// Length after decryption and decompression.
    pub plain_len: u32,
    pub compressed: bool,
    pub encrypted: bool,
    /// The encryption fields are meaningful only when `encrypted` is set.
    pub encryption_version: u16,
    pub key_version: u32,
    /// All zero means none.
    pub tweak: [u8; 16],
    /// All zero means none.
    pub integrity_tag: [u8; 16],
    /// All zero means none: a segment appended without a descriptor.
    pub content_hash: [u8; 32],
    /// Exactly as mixed into the content hash: `identity`, `lz4:1`, ...
    pub compression_algo: String,
}

impl SegmentDescriptor {
    /// Take the write-once fields of `segment`, whose stored bytes are
    /// `stored_len` long. A segment written without a plain length is
    /// stored as is, so its plain length is its stored length.
    pub fn from_segment(segment: &Segment, stored_len: usize) -> Result<Self, FormatError> {
        let plain_len = match segment.plain_len {
            Some(len) => len,
            None if !segment.compressed && !segment.encrypted => u32::try_from(stored_len)
                .map_err(|_| FormatError::Invalid("segment longer than u32"))?,
            None => {
                return Err(FormatError::Invalid(
                    "transformed segment has no plain length",
                ))
            }
        };
        let content_hash = match &segment.content_hash {
            Some(hash) => {
                let bytes = hex::decode(hash.as_str())
                    .map_err(|_| FormatError::Invalid("content hash isn't hex"))?;
                <[u8; 32]>::try_from(bytes.as_slice())
                    .map_err(|_| FormatError::Invalid("content hash isn't 32 bytes"))?
            }
            None => [0u8; 32],
        };
        let encrypted = segment.encrypted;
        Ok(Self {
            segment_id: segment.id,
            plain_len,
            compressed: segment.compressed,
            encrypted,
            encryption_version: if encrypted {
                segment.encryption_version.unwrap_or(0)
            } else {
                0
            },
            key_version: if encrypted {
                segment.key_version.unwrap_or(0)
            } else {
                0
            },
            tweak: if encrypted {
                segment.tweak_nonce.unwrap_or_default()
            } else {
                [0u8; 16]
            },
            integrity_tag: if encrypted {
                segment.integrity_tag.unwrap_or_default()
            } else {
                [0u8; 16]
            },
            content_hash,
            compression_algo: segment.compression_algo.clone(),
        })
    }

    /// The in-memory segment for this descriptor, with its body at `offset`
    /// and `len` bytes long. Its ref count starts at 1 until the store
    /// recounts references from the capsules that hold it.
    pub fn to_segment(&self, offset: u64, len: u32) -> Segment {
        let encrypted = self.encrypted;
        Segment {
            id: self.segment_id,
            offset,
            len,
            plain_len: Some(self.plain_len),
            compressed: self.compressed,
            compression_algo: self.compression_algo.clone(),
            content_hash: some_unless_zero(self.content_hash)
                .map(|bytes| ContentHash::from_bytes(&bytes)),
            ref_count: 1,
            deduplicated: false,
            access_count: 0,
            encryption_version: encrypted.then_some(self.encryption_version),
            key_version: encrypted.then_some(self.key_version),
            tweak_nonce: if encrypted {
                some_unless_zero(self.tweak)
            } else {
                None
            },
            integrity_tag: if encrypted {
                some_unless_zero(self.integrity_tag)
            } else {
                None
            },
            encrypted,
            pq_ciphertext: None,
            pq_nonce: None,
        }
    }

    pub fn encode(&self) -> Result<Vec<u8>, FormatError> {
        let mut flags = 0u16;
        if self.compressed {
            flags |= SEGMENT_COMPRESSED;
        }
        if self.encrypted {
            flags |= SEGMENT_ENCRYPTED;
        }
        let mut w = Writer::new();
        w.u64(self.segment_id.0);
        w.u32(self.plain_len);
        w.u16(flags);
        w.u16(self.encryption_version);
        w.u32(self.key_version);
        w.bytes(&self.tweak);
        w.bytes(&self.integrity_tag);
        w.bytes(&self.content_hash);
        w.str(&self.compression_algo)?;
        Ok(w.0)
    }

    pub fn decode(header: &[u8]) -> Result<Self, FormatError> {
        let mut r = Reader::new(header);
        let segment_id = SegmentId(r.u64()?);
        let plain_len = r.u32()?;
        let flags = r.u16()?;
        if flags & !(SEGMENT_COMPRESSED | SEGMENT_ENCRYPTED) != 0 {
            return Err(FormatError::Invalid("unknown SEGMENT flag"));
        }
        let encryption_version = r.u16()?;
        let key_version = r.u32()?;
        let tweak = r.array()?;
        let integrity_tag = r.array()?;
        let content_hash = r.array()?;
        let compression_algo = r.str()?;
        r.finish()?;
        Ok(Self {
            segment_id,
            plain_len,
            compressed: flags & SEGMENT_COMPRESSED != 0,
            encrypted: flags & SEGMENT_ENCRYPTED != 0,
            encryption_version,
            key_version,
            tweak,
            integrity_tag,
            content_hash,
            compression_algo,
        })
    }

    /// The frame and header of this segment's record. `body` is the stored
    /// bytes, written straight after.
    pub fn encode_head(&self, body: &[u8]) -> Result<Vec<u8>, FormatError> {
        encode_head(kind::SEGMENT, SEGMENT_VERSION, 0, &self.encode()?, body)
    }
}

const CAPSULE_DEDUPE: u32 = 1 << 0;
const CAPSULE_REQUIRES_ENCRYPTION: u32 = 1 << 1;

/// One segment of a capsule, in logical byte order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ManifestEntry {
    pub segment_id: SegmentId,
    /// Logical bytes this segment contributes.
    pub plain_len: u32,
    /// The segment's content hash; all zero means none.
    pub content_hash: [u8; 32],
}

impl ManifestEntry {
    /// Encoded size of one entry.
    pub const LEN: usize = 44;
}

/// A capsule's manifest: the `CAPSULE` journal record that commits a write
/// (`docs/on-disk-format.md` §5.3).
#[derive(Debug, Clone)]
pub struct CapsuleManifest {
    pub capsule_id: CapsuleId,
    /// Unix seconds.
    pub created_at: u64,
    /// Logical bytes; the sum of the entries' `plain_len`.
    pub size: u64,
    /// BLAKE3 of the logical bytes.
    pub digest: [u8; 32],
    /// The capsule's segments go in the content index.
    pub dedupe: bool,
    /// Never serve this capsule from a segment that isn't encrypted.
    pub requires_encryption: bool,
    pub deduped_bytes: u64,
    /// The write-time policy. Informational: nothing here is decided from it.
    pub policy: Policy,
    pub entries: Vec<ManifestEntry>,
}

impl CapsuleManifest {
    /// The registry's record of this capsule.
    pub fn to_capsule(&self) -> Capsule {
        Capsule {
            id: self.capsule_id,
            size: self.size,
            segments: self.entries.iter().map(|e| e.segment_id).collect(),
            created_at: self.created_at,
            policy: self.policy.clone(),
            deduped_bytes: self.deduped_bytes,
            digest: Some(self.digest),
        }
    }

    fn encode(&self) -> Result<Vec<u8>, FormatError> {
        let policy = serde_json::to_vec(&self.policy)
            .map_err(|_| FormatError::Invalid("policy can't be encoded as JSON"))?;
        let policy_len =
            u32::try_from(policy.len()).map_err(|_| FormatError::Invalid("policy too long"))?;
        let count = u32::try_from(self.entries.len())
            .map_err(|_| FormatError::Invalid("too many manifest entries"))?;
        let mut flags = 0u32;
        if self.dedupe {
            flags |= CAPSULE_DEDUPE;
        }
        if self.requires_encryption {
            flags |= CAPSULE_REQUIRES_ENCRYPTION;
        }
        let mut w = Writer::new();
        w.bytes(self.capsule_id.as_uuid().as_bytes());
        w.u64(self.created_at);
        w.u64(self.size);
        w.bytes(&self.digest);
        w.u32(flags);
        w.u64(self.deduped_bytes);
        w.u32(policy_len);
        w.bytes(&policy);
        w.u32(count);
        for entry in &self.entries {
            w.u64(entry.segment_id.0);
            w.u32(entry.plain_len);
            w.bytes(&entry.content_hash);
        }
        Ok(w.0)
    }

    fn decode(header: &[u8]) -> Result<Self, FormatError> {
        let mut r = Reader::new(header);
        let capsule_id = CapsuleId::from_uuid(Uuid::from_bytes(r.array()?));
        let created_at = r.u64()?;
        let size = r.u64()?;
        let digest = r.array()?;
        let flags = r.u32()?;
        if flags & !(CAPSULE_DEDUPE | CAPSULE_REQUIRES_ENCRYPTION) != 0 {
            return Err(FormatError::Invalid("unknown CAPSULE flag"));
        }
        let deduped_bytes = r.u64()?;
        let policy_len = r.u32()? as usize;
        let policy = serde_json::from_slice(r.take(policy_len)?)
            .map_err(|_| FormatError::Invalid("policy isn't valid JSON"))?;
        let count = r.u32()? as usize;
        if r.buf.len() != count.saturating_mul(ManifestEntry::LEN) {
            return Err(FormatError::Invalid(
                "entry count doesn't match the header length",
            ));
        }
        let mut entries = Vec::with_capacity(count);
        for _ in 0..count {
            entries.push(ManifestEntry {
                segment_id: SegmentId(r.u64()?),
                plain_len: r.u32()?,
                content_hash: r.array()?,
            });
        }
        r.finish()?;
        let total: u64 = entries.iter().map(|e| u64::from(e.plain_len)).sum();
        if total != size {
            return Err(FormatError::Invalid(
                "capsule size isn't the sum of its entries",
            ));
        }
        Ok(Self {
            capsule_id,
            created_at,
            size,
            digest,
            dedupe: flags & CAPSULE_DEDUPE != 0,
            requires_encryption: flags & CAPSULE_REQUIRES_ENCRYPTION != 0,
            deduped_bytes,
            policy,
            entries,
        })
    }
}

/// Check that `ns` can name a namespace: it isn't empty and holds no NUL,
/// which the index uses to separate a namespace from the names in it.
pub fn check_namespace(ns: &str) -> Result<(), FormatError> {
    if ns.is_empty() {
        return Err(FormatError::Invalid("namespace is empty"));
    }
    if ns.contains('\0') {
        return Err(FormatError::Invalid("namespace contains NUL"));
    }
    Ok(())
}

/// The key an encrypted store was opened with (#157). The last `KEY`
/// record in the journal is the store's: once there is one, the store opens
/// only with that key.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StoreKey {
    /// The key version new writes use.
    pub key_version: u32,
    /// Names the master key without revealing it.
    pub fingerprint: [u8; 16],
}

impl StoreKey {
    /// Encoded size of a version 1 `KEY` header.
    pub const LEN: usize = 24;

    /// The first 8 bytes of the fingerprint in hex: enough to tell keys
    /// apart in a message.
    pub fn fingerprint_hex(&self) -> String {
        self.fingerprint[..8]
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect()
    }

    fn encode(&self) -> Vec<u8> {
        let mut w = Writer::new();
        w.u8(KEY_CIPHER_XTS);
        w.bytes(&[0; 3]);
        w.u32(self.key_version);
        w.bytes(&self.fingerprint);
        w.0
    }

    fn decode(header: &[u8]) -> Result<Self, FormatError> {
        let mut r = Reader::new(header);
        if r.u8()? != KEY_CIPHER_XTS {
            return Err(FormatError::Invalid("unknown KEY cipher"));
        }
        if r.array::<3>()? != [0; 3] {
            return Err(FormatError::Invalid("reserved KEY bytes aren't zero"));
        }
        let key_version = r.u32()?;
        let fingerprint = r.array()?;
        r.finish()?;
        Ok(Self {
            key_version,
            fingerprint,
        })
    }
}

/// A record in the capsule journal.
#[derive(Debug, Clone)]
pub enum JournalRecord {
    /// Commits a write.
    Capsule(Box<CapsuleManifest>),
    /// Commits a delete.
    Drop(CapsuleId),
    /// Creates a namespace. Creating one that exists changes nothing.
    Namespace {
        ns: String,
        /// Unix seconds.
        created_at: u64,
    },
    /// Points `name` in `ns` at a capsule, replacing any earlier binding.
    Bind {
        ns: String,
        name: String,
        capsule_id: CapsuleId,
    },
    /// Removes `name` from `ns`, if it's there.
    Unbind { ns: String, name: String },
    /// Records the store's encryption key. Never in a batch.
    Key(StoreKey),
    /// Records applied in order, all or nothing. Never nested.
    Batch(Vec<JournalRecord>),
}

impl JournalRecord {
    fn kind(&self) -> u8 {
        match self {
            Self::Capsule(_) => kind::CAPSULE,
            Self::Drop(_) => kind::DROP,
            Self::Namespace { .. } => kind::NAMESPACE,
            Self::Bind { .. } => kind::BIND,
            Self::Unbind { .. } => kind::UNBIND,
            Self::Key(_) => kind::KEY,
            Self::Batch(_) => kind::BATCH,
        }
    }

    fn encode_header(&self) -> Result<Vec<u8>, FormatError> {
        match self {
            Self::Capsule(manifest) => manifest.encode(),
            Self::Drop(id) => Ok(id.as_uuid().as_bytes().to_vec()),
            Self::Namespace { ns, created_at } => {
                check_namespace(ns)?;
                let mut w = Writer::new();
                w.u8(NAMESPACE_CREATE);
                w.u64(*created_at);
                w.str(ns)?;
                Ok(w.0)
            }
            Self::Bind {
                ns,
                name,
                capsule_id,
            } => {
                check_namespace(ns)?;
                let mut w = Writer::new();
                w.bytes(capsule_id.as_uuid().as_bytes());
                w.str(ns)?;
                w.str(name)?;
                Ok(w.0)
            }
            Self::Unbind { ns, name } => {
                check_namespace(ns)?;
                let mut w = Writer::new();
                w.str(ns)?;
                w.str(name)?;
                Ok(w.0)
            }
            Self::Key(key) => Ok(key.encode()),
            Self::Batch(records) => {
                let count = u32::try_from(records.len())
                    .map_err(|_| FormatError::Invalid("batch too large"))?;
                let mut w = Writer::new();
                w.u32(count);
                for record in records {
                    if matches!(record, Self::Batch(_)) {
                        return Err(FormatError::Invalid("batches can't nest"));
                    }
                    if matches!(record, Self::Key(_)) {
                        return Err(FormatError::Invalid("a KEY record can't be in a batch"));
                    }
                    // No magic and no sums: the batch's head_sum covers
                    // these bytes, and without the magic a scan after a
                    // torn batch can't take a sub-record for a record.
                    let header = record.encode_header()?;
                    let header_len = u32::try_from(header.len())
                        .map_err(|_| FormatError::Invalid("batch record too large"))?;
                    w.u8(record.kind());
                    w.u8(JOURNAL_VERSION);
                    w.u16(0);
                    w.u32(header_len);
                    w.bytes(&header);
                }
                Ok(w.0)
            }
        }
    }

    /// The whole record: frame and header. Journal records have no body.
    pub fn encode_record(&self) -> Result<Vec<u8>, FormatError> {
        encode_head(self.kind(), JOURNAL_VERSION, 0, &self.encode_header()?, &[])
    }

    /// The length of [`encode_record`](Self::encode_record)'s output,
    /// without computing the checksums. A record inside a `BATCH` is
    /// measured as a record of its own.
    pub fn record_len(&self) -> Result<u64, FormatError> {
        Ok((FRAME_LEN + self.encode_header()?.len()) as u64)
    }

    /// Decode a journal record from a frame and header whose head checksum
    /// has been verified.
    pub fn decode(frame: &Frame, header: &[u8]) -> Result<Self, FormatError> {
        if frame.body_len != 0 {
            return Err(FormatError::Invalid("journal records have no body"));
        }
        Self::decode_header(frame.kind, frame.version, header)
    }

    fn decode_header(kind: u8, version: u8, header: &[u8]) -> Result<Self, FormatError> {
        if !matches!(
            kind,
            kind::CAPSULE
                | kind::DROP
                | kind::NAMESPACE
                | kind::BIND
                | kind::UNBIND
                | kind::KEY
                | kind::BATCH
        ) {
            return Err(FormatError::UnknownKind(kind));
        }
        if version != JOURNAL_VERSION {
            return Err(FormatError::UnsupportedVersion { kind, version });
        }
        match kind {
            kind::CAPSULE => Ok(Self::Capsule(Box::new(CapsuleManifest::decode(header)?))),
            kind::DROP => {
                let mut r = Reader::new(header);
                let id = CapsuleId::from_uuid(Uuid::from_bytes(r.array()?));
                r.finish()?;
                Ok(Self::Drop(id))
            }
            kind::NAMESPACE => {
                let mut r = Reader::new(header);
                if r.u8()? != NAMESPACE_CREATE {
                    return Err(FormatError::Invalid("unknown NAMESPACE op"));
                }
                let created_at = r.u64()?;
                let ns = r.str()?;
                r.finish()?;
                check_namespace(&ns)?;
                Ok(Self::Namespace { ns, created_at })
            }
            kind::BIND => {
                let mut r = Reader::new(header);
                let capsule_id = CapsuleId::from_uuid(Uuid::from_bytes(r.array()?));
                let ns = r.str()?;
                let name = r.str()?;
                r.finish()?;
                check_namespace(&ns)?;
                Ok(Self::Bind {
                    ns,
                    name,
                    capsule_id,
                })
            }
            kind::UNBIND => {
                let mut r = Reader::new(header);
                let ns = r.str()?;
                let name = r.str()?;
                r.finish()?;
                check_namespace(&ns)?;
                Ok(Self::Unbind { ns, name })
            }
            kind::KEY => Ok(Self::Key(StoreKey::decode(header)?)),
            _ => {
                let mut r = Reader::new(header);
                let count = r.u32()? as usize;
                let mut records = Vec::with_capacity(count.min(1024));
                for _ in 0..count {
                    let sub_kind = r.u8()?;
                    let sub_version = r.u8()?;
                    if r.u16()? != 0 {
                        return Err(FormatError::Invalid("batch record reserved field isn't 0"));
                    }
                    let sub_len = r.u32()? as usize;
                    let sub_header = r.take(sub_len)?;
                    if sub_kind == kind::BATCH {
                        return Err(FormatError::Invalid("batches can't nest"));
                    }
                    if sub_kind == kind::KEY {
                        return Err(FormatError::Invalid("a KEY record can't be in a batch"));
                    }
                    records.push(Self::decode_header(sub_kind, sub_version, sub_header)?);
                }
                r.finish()?;
                Ok(Self::Batch(records))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame_of(record: &[u8]) -> Frame {
        Frame::parse(record[..FRAME_LEN].try_into().unwrap()).unwrap()
    }

    fn encrypted_segment() -> Segment {
        Segment {
            id: SegmentId(42),
            offset: 0,
            len: 64,
            plain_len: Some(100),
            compressed: true,
            compression_algo: "zstd:3".into(),
            content_hash: Some(ContentHash::from_bytes(&[7u8; 32])),
            ref_count: 3,
            deduplicated: true,
            access_count: 9,
            encryption_version: Some(1),
            key_version: Some(2),
            tweak_nonce: Some([5u8; 16]),
            integrity_tag: Some([6u8; 16]),
            encrypted: true,
            pq_ciphertext: None,
            pq_nonce: None,
        }
    }

    #[test]
    fn file_header_round_trips_at_its_documented_size() {
        let header = FileHeader {
            generation: 3,
            segment_id_floor: 99,
            ..FileHeader::new(FileKind::DataLog, 1_700_000_000)
        };
        let bytes = header.encode();
        assert_eq!(bytes.len(), FileHeader::LEN);
        assert_eq!(FileHeader::decode(&bytes).unwrap(), header);

        let record = header.encode_record();
        let frame = frame_of(&record);
        assert_eq!(frame.kind, kind::FILE);
        assert_eq!(frame.record_len(), record.len() as u64);
        frame.verify_head(&record[FRAME_LEN..]).unwrap();
        frame.verify_body(&[]).unwrap();
    }

    #[test]
    fn segment_descriptor_keeps_write_once_fields_and_drops_mutable_ones() {
        let segment = encrypted_segment();
        let desc = SegmentDescriptor::from_segment(&segment, 64).unwrap();
        let decoded = SegmentDescriptor::decode(&desc.encode().unwrap()).unwrap();
        assert_eq!(decoded, desc);

        let back = decoded.to_segment(1234, 64);
        assert_eq!(back.id, segment.id);
        assert_eq!(back.offset, 1234);
        assert_eq!(back.plain_len, segment.plain_len);
        assert_eq!(back.compression_algo, segment.compression_algo);
        assert_eq!(back.content_hash, segment.content_hash);
        assert_eq!(back.encryption_version, segment.encryption_version);
        assert_eq!(back.key_version, segment.key_version);
        assert_eq!(back.tweak_nonce, segment.tweak_nonce);
        assert_eq!(back.integrity_tag, segment.integrity_tag);
        assert!(back.encrypted && back.compressed);
        // Mutable state starts fresh; the store recounts it.
        assert_eq!(back.ref_count, 1);
        assert!(!back.deduplicated);
        assert_eq!(back.access_count, 0);
    }

    #[test]
    fn segment_header_has_documented_layout() {
        let desc = SegmentDescriptor::from_segment(&encrypted_segment(), 64).unwrap();
        let bytes = desc.encode().unwrap();
        assert_eq!(bytes.len(), 84 + 2 + "zstd:3".len());
        assert_eq!(&bytes[0..8], &42u64.to_le_bytes());
        assert_eq!(&bytes[8..12], &100u32.to_le_bytes());
        assert_eq!(&bytes[12..14], &3u16.to_le_bytes());
        assert_eq!(&bytes[52..84], &[7u8; 32]);
        assert_eq!(&bytes[84..86], &6u16.to_le_bytes());
    }

    #[test]
    fn plain_segment_without_descriptor_round_trips() {
        let segment = Segment {
            id: SegmentId(1),
            len: 5,
            compression_algo: "none".into(),
            ref_count: 1,
            ..Segment::default()
        };
        let desc = SegmentDescriptor::from_segment(&segment, 5).unwrap();
        assert_eq!(desc.plain_len, 5);
        let back = SegmentDescriptor::decode(&desc.encode().unwrap())
            .unwrap()
            .to_segment(0, 5);
        assert_eq!(back.content_hash, None);
        assert_eq!(back.plain_len, Some(5));
        assert!(!back.encrypted);
        assert_eq!(back.key_version, None);
        assert_eq!(back.integrity_tag, None);
    }

    #[test]
    fn compressed_segment_needs_a_plain_length() {
        let segment = Segment {
            compressed: true,
            ..Segment::default()
        };
        assert!(SegmentDescriptor::from_segment(&segment, 10).is_err());
    }

    #[test]
    fn checksums_catch_any_flipped_byte() {
        let desc = SegmentDescriptor::from_segment(&encrypted_segment(), 64).unwrap();
        let body = vec![0xAB; 64];
        let head = desc.encode_head(&body).unwrap();
        let mut record = head.clone();
        record.extend_from_slice(&body);

        for i in 0..record.len() {
            let mut damaged = record.clone();
            damaged[i] ^= 0x01;
            let head_part: [u8; FRAME_LEN] = damaged[..FRAME_LEN].try_into().unwrap();
            let ok = match Frame::parse(&head_part) {
                Err(_) => false,
                Ok(frame) => {
                    let header_end = FRAME_LEN + frame.header_len as usize;
                    header_end <= damaged.len()
                        && frame.verify_head(&damaged[FRAME_LEN..header_end]).is_ok()
                        && damaged.len() == frame.record_len() as usize
                        && frame.verify_body(&damaged[header_end..]).is_ok()
                }
            };
            assert!(!ok, "flipping byte {i} went unnoticed");
        }
    }

    #[test]
    fn oversized_frames_are_rejected_before_allocation() {
        let mut frame = [0u8; FRAME_LEN];
        frame[0..4].copy_from_slice(&MAGIC);
        frame[8..12].copy_from_slice(&(MAX_HEADER_LEN + 1).to_le_bytes());
        assert!(matches!(
            Frame::parse(&frame),
            Err(FormatError::TooLarge { .. })
        ));
        frame[0] = b'X';
        assert_eq!(Frame::parse(&frame), Err(FormatError::BadMagic));
    }

    fn sample_manifest() -> CapsuleManifest {
        CapsuleManifest {
            capsule_id: CapsuleId::from_uuid(Uuid::from_u128(7)),
            created_at: 1_700_000_000,
            size: 150,
            digest: [3u8; 32],
            dedupe: true,
            requires_encryption: true,
            deduped_bytes: 50,
            policy: Policy::default(),
            entries: vec![
                ManifestEntry {
                    segment_id: SegmentId(1),
                    plain_len: 100,
                    content_hash: [1u8; 32],
                },
                ManifestEntry {
                    segment_id: SegmentId(1),
                    plain_len: 50,
                    content_hash: [1u8; 32],
                },
            ],
        }
    }

    fn decode_record(bytes: &[u8]) -> Result<JournalRecord, FormatError> {
        let frame = frame_of(bytes);
        assert_eq!(frame.record_len(), bytes.len() as u64);
        frame.verify_head(&bytes[FRAME_LEN..])?;
        JournalRecord::decode(&frame, &bytes[FRAME_LEN..])
    }

    #[test]
    fn capsule_manifest_round_trips_with_documented_layout() {
        let manifest = sample_manifest();
        let bytes = JournalRecord::Capsule(Box::new(manifest.clone()))
            .encode_record()
            .unwrap();
        let header = &bytes[FRAME_LEN..];
        assert_eq!(&header[0..16], manifest.capsule_id.as_uuid().as_bytes());
        assert_eq!(&header[32..64], &[3u8; 32]);
        assert_eq!(&header[64..68], &3u32.to_le_bytes());
        let policy_len = u32::from_le_bytes(header[76..80].try_into().unwrap()) as usize;
        assert_eq!(header.len(), 84 + policy_len + 2 * ManifestEntry::LEN);

        let JournalRecord::Capsule(back) = decode_record(&bytes).unwrap() else {
            panic!("not a CAPSULE record");
        };
        assert_eq!(back.capsule_id, manifest.capsule_id);
        assert_eq!(back.size, 150);
        assert_eq!(back.digest, manifest.digest);
        assert!(back.dedupe && back.requires_encryption);
        assert_eq!(back.deduped_bytes, 50);
        assert_eq!(back.entries, manifest.entries);

        let capsule = back.to_capsule();
        assert_eq!(capsule.segments, vec![SegmentId(1), SegmentId(1)]);
        assert_eq!(capsule.digest, Some([3u8; 32]));
    }

    #[test]
    fn a_manifest_whose_size_disagrees_with_its_entries_is_rejected() {
        let mut manifest = sample_manifest();
        manifest.size = 151;
        let bytes = JournalRecord::Capsule(Box::new(manifest))
            .encode_record()
            .unwrap();
        assert!(matches!(
            decode_record(&bytes),
            Err(FormatError::Invalid(_))
        ));
    }

    #[test]
    fn batches_round_trip_and_never_nest() {
        let id = CapsuleId::from_uuid(Uuid::from_u128(9));
        let batch = JournalRecord::Batch(vec![
            JournalRecord::Capsule(Box::new(sample_manifest())),
            JournalRecord::Bind {
                ns: "s3:b".into(),
                name: "k".into(),
                capsule_id: sample_manifest().capsule_id,
            },
            JournalRecord::Drop(id),
        ]);
        let JournalRecord::Batch(records) = decode_record(&batch.encode_record().unwrap()).unwrap()
        else {
            panic!("not a BATCH record");
        };
        assert!(matches!(records[0], JournalRecord::Capsule(_)));
        assert!(matches!(&records[1], JournalRecord::Bind { name, .. } if name == "k"));
        assert!(matches!(records[2], JournalRecord::Drop(d) if d == id));

        let nested = JournalRecord::Batch(vec![batch.clone()]);
        assert!(nested.encode_record().is_err());

        for record in records.iter().chain([&batch]) {
            assert_eq!(
                record.record_len().unwrap(),
                record.encode_record().unwrap().len() as u64
            );
        }

        // Sub-records carry no magic, so a scan for the next record after a
        // torn batch can't find one inside it (docs/on-disk-format.md §8.1).
        let bytes = batch.encode_record().unwrap();
        assert!(!bytes[FRAME_LEN..].windows(MAGIC.len()).any(|w| w == MAGIC));
        let header = &bytes[FRAME_LEN..];
        assert_eq!(&header[0..4], &3u32.to_le_bytes());
        assert_eq!(header[4], kind::CAPSULE);
        assert_eq!(header[5], JOURNAL_VERSION);
        assert_eq!(&header[6..8], &[0, 0]);
    }

    #[test]
    fn name_records_round_trip_with_documented_layout() {
        let id = CapsuleId::from_uuid(Uuid::from_u128(5));

        let bytes = JournalRecord::Namespace {
            ns: "s3:demo".into(),
            created_at: 1_700_000_000,
        }
        .encode_record()
        .unwrap();
        let header = &bytes[FRAME_LEN..];
        assert_eq!(frame_of(&bytes).kind, kind::NAMESPACE);
        assert_eq!(header[0], 1);
        assert_eq!(&header[1..9], &1_700_000_000u64.to_le_bytes());
        assert_eq!(&header[9..11], &7u16.to_le_bytes());
        assert_eq!(&header[11..], b"s3:demo");
        assert!(matches!(
            decode_record(&bytes).unwrap(),
            JournalRecord::Namespace { ns, created_at: 1_700_000_000 } if ns == "s3:demo"
        ));

        let bytes = JournalRecord::Bind {
            ns: "s3:demo".into(),
            name: "dir/a.txt".into(),
            capsule_id: id,
        }
        .encode_record()
        .unwrap();
        let header = &bytes[FRAME_LEN..];
        assert_eq!(frame_of(&bytes).kind, kind::BIND);
        assert_eq!(&header[0..16], id.as_uuid().as_bytes());
        assert_eq!(&header[16..18], &7u16.to_le_bytes());
        assert_eq!(&header[25..27], &9u16.to_le_bytes());
        assert_eq!(header.len(), 16 + 2 + 7 + 2 + 9);
        assert!(matches!(
            decode_record(&bytes).unwrap(),
            JournalRecord::Bind { ns, name, capsule_id }
                if ns == "s3:demo" && name == "dir/a.txt" && capsule_id == id
        ));

        let bytes = JournalRecord::Unbind {
            ns: "s3:demo".into(),
            name: "dir/a.txt".into(),
        }
        .encode_record()
        .unwrap();
        assert_eq!(frame_of(&bytes).kind, kind::UNBIND);
        assert_eq!(bytes.len() - FRAME_LEN, 2 + 7 + 2 + 9);
        assert!(matches!(
            decode_record(&bytes).unwrap(),
            JournalRecord::Unbind { ns, name } if ns == "s3:demo" && name == "dir/a.txt"
        ));
    }

    #[test]
    fn namespaces_must_be_non_empty_and_free_of_nul() {
        for ns in ["", "s3:a\0b"] {
            let record = JournalRecord::Unbind {
                ns: ns.into(),
                name: "k".into(),
            };
            assert!(record.encode_record().is_err(), "{ns:?} was encoded");

            // A record written by something else is refused on decode too.
            let mut w = Writer::new();
            w.str(ns).unwrap();
            w.str("k").unwrap();
            let bytes = encode_head(kind::UNBIND, JOURNAL_VERSION, 0, &w.0, &[]).unwrap();
            assert!(matches!(
                decode_record(&bytes),
                Err(FormatError::Invalid(_))
            ));
        }
    }

    #[test]
    fn unknown_namespace_ops_are_rejected() {
        let mut header = JournalRecord::Namespace {
            ns: "s3:b".into(),
            created_at: 0,
        }
        .encode_header()
        .unwrap();
        header[0] = 2;
        let bytes = encode_head(kind::NAMESPACE, JOURNAL_VERSION, 0, &header, &[]).unwrap();
        assert!(matches!(
            decode_record(&bytes),
            Err(FormatError::Invalid(_))
        ));
    }

    #[test]
    fn key_round_trips_with_documented_layout() {
        let key = StoreKey {
            key_version: 3,
            fingerprint: [0xab; 16],
        };
        let bytes = JournalRecord::Key(key).encode_record().unwrap();
        let header = &bytes[FRAME_LEN..];
        assert_eq!(header.len(), StoreKey::LEN);
        assert_eq!(header[0], KEY_CIPHER_XTS);
        assert_eq!(&header[1..4], &[0; 3]);
        assert_eq!(&header[4..8], &3u32.to_le_bytes());
        assert_eq!(&header[8..24], &[0xab; 16]);
        assert!(matches!(decode_record(&bytes), Ok(JournalRecord::Key(k)) if k == key));
        assert_eq!(key.fingerprint_hex(), "abababababababab");

        // An unknown cipher, or reserved bytes that aren't zero.
        for (at, value) in [(0, 2u8), (2, 1)] {
            let mut header = header.to_vec();
            header[at] = value;
            let bytes = encode_head(kind::KEY, JOURNAL_VERSION, 0, &header, &[]).unwrap();
            assert!(matches!(
                decode_record(&bytes),
                Err(FormatError::Invalid(_))
            ));
        }

        // Changing the store's key is a commit of its own.
        let batch = JournalRecord::Batch(vec![JournalRecord::Key(key)]);
        assert!(batch.encode_record().is_err());
        let mut header = 1u32.to_le_bytes().to_vec();
        header.extend_from_slice(&[kind::KEY, JOURNAL_VERSION, 0, 0]);
        header.extend_from_slice(&(StoreKey::LEN as u32).to_le_bytes());
        header.extend_from_slice(&key.encode());
        let bytes = encode_head(kind::BATCH, JOURNAL_VERSION, 0, &header, &[]).unwrap();
        assert!(matches!(
            decode_record(&bytes),
            Err(FormatError::Invalid(_))
        ));
    }

    #[test]
    fn decoders_reject_short_and_long_headers() {
        let bytes = FileHeader::new(FileKind::Journal, 0).encode();
        assert_eq!(
            FileHeader::decode(&bytes[..bytes.len() - 1]),
            Err(FormatError::Truncated)
        );
        let mut long = bytes.clone();
        long.push(0);
        assert_eq!(FileHeader::decode(&long), Err(FormatError::TrailingBytes));
    }
}
