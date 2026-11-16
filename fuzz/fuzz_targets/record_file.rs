//! Opening a journal or data log whose bytes after the `FILE` record are
//! arbitrary, as a crash or tampering can leave them (#137, #192): the
//! frame checks, the torn tail and damage searches, and the decoders the
//! journal and the data log run on every record they find.

#![no_main]

use std::path::PathBuf;
use std::sync::OnceLock;

use common::format::{FileHeader, FileKind, JournalRecord};
use libfuzzer_sys::fuzz_target;
use nvram_sim::{record_file, NvramLog};

/// A directory of this process's own, emptied before each input.
fn scratch() -> &'static PathBuf {
    static DIR: OnceLock<PathBuf> = OnceLock::new();
    DIR.get_or_init(|| {
        std::env::temp_dir().join(format!("space-fuzz-record-file-{}", std::process::id()))
    })
}

fuzz_target!(|input: &[u8]| {
    let Some((&which, rest)) = input.split_first() else {
        return;
    };
    let dir = scratch();
    let _ = std::fs::remove_dir_all(dir);
    std::fs::create_dir_all(dir).unwrap();

    // A valid FILE record first, so the input is the records after it, as
    // a store's own file would hold them. The low bit picks the kind.
    let journal = which & 1 == 0;
    let kind = if journal {
        FileKind::Journal
    } else {
        FileKind::DataLog
    };
    let mut bytes = FileHeader::new(kind, 1).encode_record();
    bytes.extend_from_slice(rest);

    if journal {
        let path = dir.join("space.journal");
        std::fs::write(&path, &bytes).unwrap();
        // What Journal::open does with each record it finds.
        let _ = record_file::open(&path, FileKind::Journal, |_, frame, header| {
            JournalRecord::decode(frame, header)?;
            Ok(())
        });
    } else {
        let path = dir.join("space.nvram");
        std::fs::write(&path, &bytes).unwrap();
        let _ = NvramLog::open(&path);
    }
});
