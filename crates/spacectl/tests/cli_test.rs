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
