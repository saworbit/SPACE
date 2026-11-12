//! Runs the real `spacectl` binary in a scratch directory.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn scratch_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("spacectl_{name}_{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn spacectl(dir: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_spacectl"))
        .args(args)
        .current_dir(dir)
        .env_remove("SPACE_MASTER_KEY")
        .env_remove("SPACE_STORAGE_ROOT")
        .env("RUST_LOG", "debug")
        .output()
        .unwrap()
}

#[test]
fn stdout_carries_only_command_output() {
    let dir = scratch_dir("stdout");
    std::fs::write(dir.join("hello.txt"), b"Hello SPACE!\n").unwrap();

    let out = spacectl(&dir, &["create", "-f", "hello.txt"]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8(out.stdout).unwrap();
    let lines: Vec<&str> = stdout.lines().collect();
    assert_eq!(lines.len(), 2, "unexpected stdout: {stdout:?}");
    assert!(lines[0].starts_with("Capsule created: "));
    assert!(!stdout.contains('\x1b'), "ANSI codes on stdout");
    // Debug logs were requested, so they must have gone to stderr, uncoloured.
    assert!(!out.stderr.is_empty());
    assert!(
        !out.stderr.contains(&0x1b),
        "ANSI codes on a non-terminal stderr"
    );

    let id = lines[0].trim_start_matches("Capsule created: ");
    let read = spacectl(&dir, &["read", id]);
    assert!(read.status.success());
    assert_eq!(read.stdout, b"Hello SPACE!\n");

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn locked_store_is_an_error_not_a_panic() {
    let dir = scratch_dir("locked");
    let held = capsule_registry::CapsuleRegistry::open(dir.join("space.db")).unwrap();

    let out = spacectl(&dir, &["list"]);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success());
    assert!(!stderr.contains("panicked"), "{stderr}");
    assert!(stderr.contains("locked by another process"), "{stderr}");

    drop(held);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn delete_removes_the_capsule() {
    let dir = scratch_dir("delete");
    std::fs::write(dir.join("a.txt"), b"delete me").unwrap();
    let out = spacectl(&dir, &["create", "-f", "a.txt"]);
    assert!(out.status.success());
    let stdout = String::from_utf8(out.stdout).unwrap();
    let id = stdout
        .lines()
        .next()
        .unwrap()
        .trim_start_matches("Capsule created: ");

    let out = spacectl(&dir, &["delete", id]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(!spacectl(&dir, &["read", id]).status.success());
    let list = spacectl(&dir, &["list"]);
    assert_eq!(String::from_utf8(list.stdout).unwrap(), "(no capsules)\n");
    assert!(!spacectl(&dir, &["delete", id]).status.success());

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn removed_flags_and_commands_are_rejected() {
    let dir = scratch_dir("removed");
    for args in [&["put", "file.txt"][..], &["--token", "secret", "list"][..]] {
        let out = spacectl(&dir, args);
        assert_eq!(
            out.status.code(),
            Some(2),
            "{args:?} should be a usage error"
        );
    }
    let _ = std::fs::remove_dir_all(&dir);
}

fn created_id(out: &Output) -> String {
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .next()
        .unwrap()
        .trim_start_matches("Capsule created: ")
        .to_string()
}

/// Bytes that don't compress, so the log grows by about their size.
fn noise(len: usize, seed: u64) -> Vec<u8> {
    let mut x = (seed << 1) | 1;
    (0..len)
        .map(|_| {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            x as u8
        })
        .collect()
}

#[test]
fn gc_and_compact_reclaim_what_no_capsule_references() {
    let dir = scratch_dir("reclaim");
    let kept = noise(300_000, 1);
    std::fs::write(dir.join("kept.bin"), &kept).unwrap();
    std::fs::write(dir.join("gone.bin"), noise(200_000, 2)).unwrap();
    let kept_id = created_id(&spacectl(&dir, &["create", "-f", "kept.bin"]));
    let gone_id = created_id(&spacectl(&dir, &["create", "-f", "gone.bin"]));
    assert!(spacectl(&dir, &["delete", &gone_id]).status.success());

    // A segment a crashed write left behind.
    let log = nvram_sim::NvramLog::open(dir.join("space.nvram")).unwrap();
    log.append(common::SegmentId(1_000_000), b"orphan").unwrap();
    drop(log);

    let out = spacectl(&dir, &["gc"]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    // The orphan, and the deleted capsule's segment: its record maps again,
    // with no references, each time the log opens until compaction.
    let stdout = String::from_utf8(out.stdout).unwrap();
    assert!(
        stdout.starts_with("Removed 2 unreferenced segments\n"),
        "{stdout}"
    );
    assert!(stdout.contains("run `spacectl compact`"), "{stdout}");

    let before = std::fs::metadata(dir.join("space.nvram")).unwrap().len();
    let out = spacectl(&dir, &["compact"]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8(out.stdout).unwrap();
    let reclaimed: u64 = stdout
        .strip_prefix("Reclaimed ")
        .and_then(|rest| rest.split(' ').next())
        .and_then(|n| n.parse().ok())
        .unwrap_or_else(|| panic!("unexpected output: {stdout}"));
    assert!(reclaimed > 200_000, "{stdout}");
    assert!(stdout.contains("generation 1"), "{stdout}");
    assert_eq!(
        std::fs::metadata(dir.join("space.nvram")).unwrap().len(),
        before - reclaimed
    );

    let read = spacectl(&dir, &["read", &kept_id]);
    assert!(read.status.success());
    assert!(read.stdout == kept, "kept capsule changed");
    let again = spacectl(&dir, &["compact"]);
    assert_eq!(
        String::from_utf8(again.stdout).unwrap(),
        "Nothing to reclaim\n"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn compact_has_nothing_to_do_under_a_storage_root() {
    let dir = scratch_dir("storage_root");
    let out = Command::new(env!("CARGO_BIN_EXE_spacectl"))
        .arg("compact")
        .current_dir(&dir)
        .env_remove("SPACE_MASTER_KEY")
        .env("SPACE_STORAGE_ROOT", dir.join("storage"))
        .output()
        .unwrap();
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("nothing to compact"), "{stderr}");
    assert!(!dir.join("space.nvram").exists());

    let _ = std::fs::remove_dir_all(&dir);
}
