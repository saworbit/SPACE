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

use crate::{ContentHash, Segment, SegmentId};

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
}

/// Layout version of the `FILE` header.
pub const FILE_VERSION: u8 = 1;
/// Layout version of the `SEGMENT` header.
pub const SEGMENT_VERSION: u8 = 1;

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
}

impl FileKind {
    fn code(self) -> u8 {
        match self {
            Self::DataLog => 1,
            Self::Journal => 2,
            Self::Checkpoint => 3,
        }
    }

    fn from_code(code: u8) -> Result<Self, FormatError> {
        match code {
            1 => Ok(Self::DataLog),
            2 => Ok(Self::Journal),
            3 => Ok(Self::Checkpoint),
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
