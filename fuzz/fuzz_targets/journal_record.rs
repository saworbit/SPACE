//! Journal record headers (`docs/on-disk-format.md` §5), as a crash or
//! tampering can leave them. The checksum is skipped, so every decoder,
//! `BATCH` and its nested records included, sees arbitrary bytes.
//!
//! Two invariants beyond not panicking: a record that decodes encodes to
//! bytes that decode and encode to the same bytes again, and the torn tail
//! walk (`journal_header_prefix`, #137) agrees with the decoder about where
//! a valid header ends, so it never takes a whole record for a torn one.

#![no_main]

use common::format::{journal_header_prefix, Frame, HeaderPrefix, JournalRecord, FRAME_LEN, MAGIC};
use libfuzzer_sys::fuzz_target;

/// A frame for `header`, of `kind` and `version`, with no body. Its
/// checksums are zero: the decoder doesn't read them.
fn frame(kind: u8, version: u8, header: &[u8]) -> Option<Frame> {
    let mut bytes = [0u8; FRAME_LEN];
    bytes[0..4].copy_from_slice(&MAGIC);
    bytes[4] = kind;
    bytes[5] = version;
    bytes[8..12].copy_from_slice(&u32::try_from(header.len()).ok()?.to_le_bytes());
    Frame::parse(&bytes).ok()
}

/// Decode an encoded record, checking its checksum, and encode it again.
fn reencode(record: &[u8]) -> Vec<u8> {
    let frame = Frame::parse(record[..FRAME_LEN].try_into().unwrap()).unwrap();
    let header = &record[FRAME_LEN..];
    frame.verify_head(header).unwrap();
    JournalRecord::decode(&frame, header)
        .unwrap()
        .encode_record()
        .unwrap()
}

fuzz_target!(|input: &[u8]| {
    let [kind, version, header @ ..] = input else {
        return;
    };
    let walked = journal_header_prefix(*kind, *version, header);
    let Some(frame) = frame(*kind, *version, header) else {
        return;
    };
    let Ok(record) = JournalRecord::decode(&frame, header) else {
        return;
    };

    let encoded = record.encode_record().unwrap();
    assert_eq!(reencode(&encoded), encoded);

    if walked.is_some() {
        assert_eq!(walked, Some(HeaderPrefix::Whole(header.len())));
        if let Some(short) = header.len().checked_sub(1) {
            assert_eq!(
                journal_header_prefix(*kind, *version, &header[..short]),
                Some(HeaderPrefix::CutShort)
            );
        }
    }
});
