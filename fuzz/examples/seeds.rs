//! Writes the seed corpora in `fuzz/seeds/`, one directory per target, from
//! the encoders and the test vectors, so the fuzzer starts from inputs the
//! parsers accept. Run it again when the on-disk format changes:
//!
//! ```text
//! cargo run --manifest-path fuzz/Cargo.toml --example seeds
//! ```

use std::fs;
use std::path::Path;

use common::format::{
    CapsuleManifest, FileHeader, FileKind, JournalRecord, ManifestEntry, StoreKey, FRAME_LEN,
};
use common::{CapsuleId, Policy, SegmentId};
use nvram_sim::NvramLog;

fn write(target: &str, name: &str, bytes: &[u8]) {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("seeds")
        .join(target);
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join(name), bytes).unwrap();
}

fn records() -> Vec<(&'static str, JournalRecord)> {
    let id = CapsuleId::from_uuid(uuid::Uuid::from_u128(
        0x0123_4567_89ab_cdef_0123_4567_89ab_cdef,
    ));
    let manifest = CapsuleManifest {
        capsule_id: id,
        created_at: 1_700_000_000,
        size: 150,
        digest: [3; 32],
        dedupe: true,
        requires_encryption: false,
        deduped_bytes: 50,
        policy: Policy::default(),
        entries: vec![
            ManifestEntry {
                segment_id: SegmentId(1),
                plain_len: 100,
                content_hash: [1; 32],
            },
            ManifestEntry {
                segment_id: SegmentId(2),
                plain_len: 50,
                content_hash: [0; 32],
            },
        ],
    };
    let bind = JournalRecord::Bind {
        ns: "s3:bkt".to_string(),
        name: "dir/key.txt".to_string(),
        capsule_id: id,
        metadata: vec![
            ("content-type".to_string(), "text/plain".to_string()),
            ("x-amz-meta-a".to_string(), "b".to_string()),
        ],
    };
    vec![
        ("capsule", JournalRecord::Capsule(Box::new(manifest))),
        ("drop", JournalRecord::Drop(id)),
        (
            "namespace",
            JournalRecord::Namespace {
                ns: "s3:bkt".to_string(),
                created_at: 1_700_000_000,
            },
        ),
        (
            "drop_namespace",
            JournalRecord::DropNamespace {
                ns: "mpu:0123".to_string(),
                dropped_at: 1_700_000_001,
            },
        ),
        ("bind", bind.clone()),
        (
            "unbind",
            JournalRecord::Unbind {
                ns: "s3:bkt".to_string(),
                name: "dir/key.txt".to_string(),
            },
        ),
        (
            "key",
            JournalRecord::Key(StoreKey {
                key_version: 1,
                fingerprint: [7; 16],
            }),
        ),
        (
            "batch",
            JournalRecord::Batch(vec![
                bind,
                JournalRecord::Drop(id),
                JournalRecord::Unbind {
                    ns: "mpu:0123".to_string(),
                    name: "part:00001".to_string(),
                },
            ]),
        ),
    ]
}

fn main() {
    // journal_record: kind, layout version, then the header.
    let mut journal = vec![0u8];
    for (name, record) in records() {
        let bytes = record.encode_record().unwrap();
        let mut seed = vec![bytes[4], bytes[5]];
        seed.extend_from_slice(&bytes[FRAME_LEN..]);
        write("journal_record", name, &seed);
        journal.extend_from_slice(&bytes);
    }

    // record_file: 0 and a journal's records, or 1 and a data log's.
    write("record_file", "journal", &journal);
    write("record_file", "journal_torn", &journal[..journal.len() - 7]);
    let dir = std::env::temp_dir().join(format!("space-fuzz-seeds-{}", std::process::id()));
    fs::create_dir_all(&dir).unwrap();
    let path = dir.join("space.nvram");
    {
        let log = NvramLog::open(&path).unwrap();
        log.append(SegmentId(1), b"the first segment").unwrap();
        log.append(SegmentId(2), &[0xa5; 300]).unwrap();
    }
    let file = fs::read(&path).unwrap();
    let skip = FileHeader::new(FileKind::DataLog, 1).encode_record().len();
    let mut data_log = vec![1u8];
    data_log.extend_from_slice(&file[skip..]);
    write("record_file", "data_log", &data_log);
    fs::remove_dir_all(&dir).unwrap();

    // aws_chunked: how the body arrives, the decoded length it claims, then
    // the body (crates/protocol-s3/src/aws_chunked.rs tests).
    let signed: &[u8] = b"5;chunk-signature=abc\r\nhello\r\n6;chunk-signature=def\r\n world\r\n0;chunk-signature=0\r\n\r\n";
    let trailer: &[u8] = b"5;chunk-signature=a\r\nhello\r\n0;chunk-signature=b\r\nX-Amz-Checksum-Crc32: NhCmhg==\r\nx-amz-trailer-signature:c\r\n\r\n";
    let unsigned: &[u8] =
        b"b\r\nhello world\r\n0\r\nx-amz-checksum-crc64nvme: AAAAAAAAAAA=\r\n\r\n";
    write(
        "aws_chunked",
        "signed",
        &[&[0x83, 0, 11][..], signed].concat(),
    );
    write(
        "aws_chunked",
        "trailer",
        &[&[0x01, 0, 0][..], trailer].concat(),
    );
    write(
        "aws_chunked",
        "unsigned",
        &[&[0xbf, 0, 11][..], unsigned].concat(),
    );

    // delete_xml (crates/protocol-s3/tests/http_test.rs).
    write(
        "delete_xml",
        "delete",
        concat!(
            r#"<?xml version="1.0" encoding="UTF-8"?>"#,
            r#"<Delete xmlns="http://s3.amazonaws.com/doc/2006-03-01/">"#,
            "<Object><Key>a</Key></Object>",
            "<Object><Key>b&amp;c</Key></Object>",
            "<Object><Key>keep</Key><VersionId>v1</VersionId></Object>",
            "</Delete>",
        )
        .as_bytes(),
    );
    write(
        "delete_xml",
        "quiet",
        b"<Delete><Quiet>true</Quiet><Object><Key>q</Key><ETag>\"e\"</ETag></Object></Delete>",
    );

    // complete_xml (crates/protocol-s3/tests/multipart_test.rs).
    write(
        "complete_xml",
        "complete",
        concat!(
            r#"<CompleteMultipartUpload xmlns="http://s3.amazonaws.com/doc/2006-03-01/">"#,
            "<Part><PartNumber>1</PartNumber><ETag>\"0123\"</ETag>",
            "<ChecksumCRC32>AAAAAA==</ChecksumCRC32></Part>",
            "<Part><PartNumber>2</PartNumber><ETag>&quot;4567&quot;</ETag></Part>",
            "</CompleteMultipartUpload>",
        )
        .as_bytes(),
    );

    // checksum: the piece size, then a header value.
    write("checksum", "base64", b"\x03XUFAKrxLKna5cZ2REBfFkg==");
    write(
        "checksum",
        "hex",
        b"\x00e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855",
    );

    // policy_yaml (the PolicyFile doc comment).
    write(
        "policy_yaml",
        "policy",
        b"compression: !Zstd { level: 3 }\ndedupe: false\nencryption: !XtsAes256 { key_version: null }\nlayout:\n  strategy: !Fixed { segment_size: 1048576 }\n",
    );
    write("policy_yaml", "lz4", b"compression: !LZ4 { level: 1 }\n");
}
