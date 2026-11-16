//! Policy files, as `spacectl create --policy-file` reads them (#161).

#![no_main]

use common::{Policy, PolicyFile};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|input: &[u8]| {
    let Ok(text) = std::str::from_utf8(input) else {
        return;
    };
    if let Ok(file) = serde_norway::from_str::<PolicyFile>(text) {
        let _ = file.apply(Policy::default());
    }
});
