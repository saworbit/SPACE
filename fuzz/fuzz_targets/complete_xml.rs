//! CompleteMultipartUpload bodies (#201), parsed by the hand-written XML
//! reader in `crates/protocol-s3/src/handlers.rs`.

#![no_main]

use libfuzzer_sys::fuzz_target;
use protocol_s3::handlers::fuzzing::parse_complete;

fuzz_target!(|body: &[u8]| {
    let _ = parse_complete(body);
});
