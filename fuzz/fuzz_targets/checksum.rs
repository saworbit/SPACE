//! Digest headers (#196), and the running digests a PUT body is checked
//! with: a body digested in pieces must match the same body digested whole.

#![no_main]

use libfuzzer_sys::fuzz_target;
use protocol_s3::checksum::{digest_of, Algorithm, Expected, Running};

const ALL: [Algorithm; 6] = [
    Algorithm::Md5,
    Algorithm::Crc32,
    Algorithm::Crc32c,
    Algorithm::Crc64Nvme,
    Algorithm::Sha1,
    Algorithm::Sha256,
];

fuzz_target!(|input: &[u8]| {
    let Some((&step, body)) = input.split_first() else {
        return;
    };

    let text = String::from_utf8_lossy(body);
    for algorithm in ALL {
        let _ = Expected::from_base64("x-amz-checksum", algorithm, &text);
        let _ = Expected::from_hex("x-amz-content-sha256", algorithm, &text);
    }

    let mut running = Running::new(ALL);
    for piece in body.chunks(usize::from(step) + 1) {
        running.update(piece);
    }
    let whole: Vec<Expected> = ALL
        .iter()
        .map(|&algorithm| Expected {
            header: "x-amz-checksum".to_string(),
            algorithm,
            digest: digest_of(algorithm, body),
        })
        .collect();
    assert_eq!(running.check(&whole), None);
});
