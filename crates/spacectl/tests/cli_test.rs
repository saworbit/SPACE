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

    let size = |name: &str| std::fs::metadata(dir.join(name)).unwrap().len();
    let (log_before, journal_before) = (size("space.nvram"), size("space.journal"));
    let out = spacectl(&dir, &["compact"]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8(out.stdout).unwrap();
    let reclaimed = |file: &str| -> u64 {
        let line = stdout
            .lines()
            .find_map(|line| line.strip_prefix(&format!("{file}: reclaimed ")))
            .unwrap_or_else(|| panic!("no {file} line: {stdout}"));
        assert!(line.ends_with(", now generation 1"), "{stdout}");
        line.split(' ').next().unwrap().parse().unwrap()
    };
    // The deleted capsule's segment, and its CAPSULE and DROP records.
    let (log_reclaimed, journal_reclaimed) = (reclaimed("space.nvram"), reclaimed("space.journal"));
    assert!(log_reclaimed > 200_000, "{stdout}");
    assert!(journal_reclaimed > 0, "{stdout}");
    assert_eq!(size("space.nvram"), log_before - log_reclaimed);
    assert_eq!(size("space.journal"), journal_before - journal_reclaimed);

    let read = spacectl(&dir, &["read", &kept_id]);
    assert!(read.status.success());
    assert!(read.stdout == kept, "kept capsule changed");
    let list = spacectl(&dir, &["list"]);
    let listed = String::from_utf8(list.stdout).unwrap();
    assert!(
        listed.contains(&kept_id) && !listed.contains(&gone_id),
        "{listed}"
    );
    let again = spacectl(&dir, &["compact"]);
    assert_eq!(
        String::from_utf8(again.stdout).unwrap(),
        "space.nvram: nothing to reclaim\nspace.journal: nothing to reclaim\n"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn compact_under_a_storage_root_compacts_only_the_journal() {
    let dir = scratch_dir("storage_root");
    let out = Command::new(env!("CARGO_BIN_EXE_spacectl"))
        .arg("compact")
        .current_dir(&dir)
        .env_remove("SPACE_MASTER_KEY")
        .env("SPACE_STORAGE_ROOT", dir.join("storage"))
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8(out.stdout).unwrap();
    assert!(stdout.contains("need no compaction"), "{stdout}");
    assert!(
        stdout.ends_with("space.journal: nothing to reclaim\n"),
        "{stdout}"
    );
    assert!(!dir.join("space.nvram").exists());

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn scrub_names_the_damaged_segment_and_its_capsule() {
    let dir = scratch_dir("scrub");
    std::fs::write(dir.join("a.bin"), noise(100_000, 3)).unwrap();
    std::fs::write(dir.join("b.bin"), noise(100_000, 4)).unwrap();
    let damaged = created_id(&spacectl(&dir, &["create", "-f", "a.bin"]));
    created_id(&spacectl(&dir, &["create", "-f", "b.bin"]));

    let out = spacectl(&dir, &["scrub"]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8(out.stdout).unwrap();
    assert!(
        stdout.starts_with("Light scrub: checked 2 segments"),
        "{stdout}"
    );
    assert!(stdout.ends_with("All healthy\n"), "{stdout}");

    // Rot a byte of the first capsule's segment.
    let registry = capsule_registry::CapsuleRegistry::open(dir.join("space.db")).unwrap();
    let segment = registry
        .lookup(common::CapsuleId::from_uuid(damaged.parse().unwrap()))
        .unwrap()
        .segments[0];
    drop(registry);
    let log = nvram_sim::NvramLog::open(dir.join("space.nvram")).unwrap();
    let offset = log.get_segment_metadata(segment).unwrap().offset as usize;
    drop(log);
    let mut bytes = std::fs::read(dir.join("space.nvram")).unwrap();
    bytes[offset + 10] ^= 0x01;
    std::fs::write(dir.join("space.nvram"), &bytes).unwrap();

    for args in [&["scrub"][..], &["scrub", "--deep"][..]] {
        let out = spacectl(&dir, args);
        assert!(!out.status.success(), "{args:?} passed a damaged store");
        let stdout = String::from_utf8(out.stdout).unwrap();
        let line = format!("segment {}: stored bytes don't match", segment.0);
        assert!(stdout.contains(&line), "{args:?}: {stdout}");
        assert!(stdout.contains(&damaged), "{args:?}: {stdout}");
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(
            stderr.contains("1 segment failed integrity checks"),
            "{args:?}: {stderr}"
        );
    }

    let _ = std::fs::remove_dir_all(&dir);
}

fn spacectl_in_root(dir: &Path, root: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_spacectl"))
        .args(args)
        .current_dir(dir)
        .env_remove("SPACE_MASTER_KEY")
        .env("SPACE_STORAGE_ROOT", root)
        .output()
        .unwrap()
}

fn segment_files(root: &Path) -> Vec<String> {
    std::fs::read_dir(root.join("segments"))
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|name| name.ends_with(".seg"))
        .collect()
}

#[test]
fn create_read_and_delete_use_the_storage_root() {
    let dir = scratch_dir("root_commands");
    let root = dir.join("storage");
    let data = noise(100_000, 9);
    std::fs::write(dir.join("a.bin"), &data).unwrap();

    let id = created_id(&spacectl_in_root(&dir, &root, &["create", "-f", "a.bin"]));
    assert_eq!(segment_files(&root).len(), 1);

    let read = spacectl_in_root(&dir, &root, &["read", &id]);
    assert!(
        read.status.success(),
        "{}",
        String::from_utf8_lossy(&read.stderr)
    );
    assert!(read.stdout == data, "read returned the wrong bytes");

    let scrub = spacectl_in_root(&dir, &root, &["scrub"]);
    assert!(
        scrub.status.success(),
        "{}",
        String::from_utf8_lossy(&scrub.stderr)
    );

    let delete = spacectl_in_root(&dir, &root, &["delete", &id]);
    assert!(
        delete.status.success(),
        "{}",
        String::from_utf8_lossy(&delete.stderr)
    );
    assert!(
        segment_files(&root).is_empty(),
        "delete left the segment file"
    );
    assert!(!spacectl_in_root(&dir, &root, &["read", &id])
        .status
        .success());
    assert!(
        !dir.join("space.nvram").exists(),
        "something used space.nvram"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

fn spacectl_with_key(dir: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_spacectl"))
        .args(args)
        .current_dir(dir)
        .env("SPACE_MASTER_KEY", "07".repeat(32))
        .env_remove("SPACE_STORAGE_ROOT")
        .output()
        .unwrap()
}

#[test]
fn reading_an_encrypted_capsule_needs_its_key() {
    let dir = scratch_dir("keyless_read");
    let secret = b"TOP SECRET PAYLOAD";
    std::fs::write(dir.join("secret.txt"), secret).unwrap();
    let id = created_id(&spacectl_with_key(&dir, &["create", "-f", "secret.txt"]));

    // Without the key, read used to print the ciphertext and exit 0 (#155).
    let out = spacectl(&dir, &["read", &id]);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success(), "read succeeded without the key");
    assert!(
        out.stdout.is_empty(),
        "read wrote {} bytes without the key",
        out.stdout.len()
    );
    assert!(stderr.contains("SPACE_MASTER_KEY"), "{stderr}");

    let out = spacectl_with_key(&dir, &["read", &id]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(out.stdout, secret);

    let _ = std::fs::remove_dir_all(&dir);
}

/// A misspelled key in a policy file is an error, not a setting silently
/// left at its default (#161), and so is a setting nothing implements
/// (#162). Settings the file leaves out keep the default.
#[test]
fn policy_files_refuse_unknown_and_unsupported_settings() {
    let dir = scratch_dir("policy_file");
    std::fs::write(dir.join("a.txt"), b"policy file test").unwrap();
    for (name, yaml, expected) in [
        (
            "typo.yaml",
            "dedup: false\ncompresion: None\n",
            "unknown field `dedup`",
        ),
        (
            "kyber.yaml",
            "crypto_profile: HybridKyber\n",
            "unknown field `crypto_profile`",
        ),
        (
            "layout.yaml",
            "layout:\n  strategy: AdaptiveEntropy\n",
            "a layout strategy other than Fixed is not supported",
        ),
    ] {
        std::fs::write(dir.join(name), yaml).unwrap();
        let out = spacectl(&dir, &["create", "-f", "a.txt", "--policy-file", name]);
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(!out.status.success(), "{name} was accepted");
        assert!(stderr.contains(expected), "{name}: {stderr}");
        assert!(
            stderr.contains(&format!("invalid policy file {name}")),
            "{stderr}"
        );
    }
    let list = spacectl(&dir, &["list"]);
    assert!(String::from_utf8_lossy(&list.stdout).contains("(no capsules)"));

    std::fs::write(
        dir.join("ok.yaml"),
        "compression: !Zstd { level: 3 }\ndedupe: false\nlayout:\n  strategy: !Fixed { segment_size: 4 }\n",
    )
    .unwrap();
    let out = spacectl(&dir, &["create", "-f", "a.txt", "--policy-file", "ok.yaml"]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let list = String::from_utf8_lossy(&spacectl(&dir, &["list"]).stdout).into_owned();
    // 16 bytes in 4-byte segments.
    assert!(
        list.lines().any(|line| line.trim_end().ends_with(" 4")),
        "{list}"
    );

    let _ = std::fs::remove_dir_all(&dir);
}
