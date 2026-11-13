//! Runs the real `spacectl` binary in a scratch directory.

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::time::{Duration, Instant};

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
        .env_remove("SPACE_ALLOW_PLAINTEXT")
        .env_remove("SPACE_STORAGE_ROOT")
        .env_remove("SPACE_STORE_DIR")
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

fn entries(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}

/// spacectl with no SPACE_* variables, its output piped.
fn spawn(dir: &Path, args: &[&str], envs: &[(&str, &Path)]) -> Child {
    let mut command = Command::new(env!("CARGO_BIN_EXE_spacectl"));
    command
        .args(args)
        .current_dir(dir)
        .env_remove("SPACE_MASTER_KEY")
        .env_remove("SPACE_ALLOW_PLAINTEXT")
        .env_remove("SPACE_STORAGE_ROOT")
        .env_remove("SPACE_STORE_DIR")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for (name, value) in envs {
        command.env(name, value);
    }
    command.spawn().unwrap()
}

/// What `child` printed once it exits, which it must do within a minute.
fn exit_of(mut child: Child, what: &str) -> Output {
    let deadline = Instant::now() + Duration::from_secs(60);
    while child.try_wait().unwrap().is_none() {
        if Instant::now() > deadline {
            let _ = child.kill();
            panic!("{what}");
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    child.wait_with_output().unwrap()
}

/// Only `create` and `serve-s3` create a store. Every other command in a
/// directory without one fails and leaves the directory empty, and
/// `--store` or SPACE_STORE_DIR point at a store elsewhere (#166).
#[test]
fn commands_other_than_create_need_an_existing_store() {
    let dir = scratch_dir("no_store");
    let id = "00000000-0000-0000-0000-000000000001";
    for args in [
        &["list"][..],
        &["read", id],
        &["delete", id],
        &["gc"],
        &["compact"],
        &["scrub"],
    ] {
        let out = spacectl(&dir, args);
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(!out.status.success(), "{args:?}");
        assert!(stderr.contains("no SPACE store in ."), "{args:?}: {stderr}");
    }
    assert_eq!(entries(&dir), Vec::<String>::new());

    std::fs::write(dir.join("a.txt"), b"elsewhere").unwrap();
    let store = dir.join("store");
    let store_arg = store.to_str().unwrap();
    let id = created_id(&spacectl(
        &dir,
        &["--store", store_arg, "create", "-f", "a.txt"],
    ));
    assert!(store.join("space.journal").exists());
    let read = spacectl(&dir, &["read", &id, "--store", store_arg]);
    assert_eq!(read.stdout, b"elsewhere");
    let list = spawn(&dir, &["list"], &[("SPACE_STORE_DIR", &store)])
        .wait_with_output()
        .unwrap();
    assert!(String::from_utf8_lossy(&list.stdout).contains(&id));
    assert_eq!(entries(&dir), ["a.txt", "store"]);

    let _ = std::fs::remove_dir_all(&dir);
}

/// A reader that stops early, as `head` does, ends the command quietly with
/// status 0 rather than a panic or an error (#167).
#[test]
fn a_closed_stdout_is_a_clean_exit() {
    let dir = scratch_dir("closed_stdout");
    std::fs::write(dir.join("a.txt"), b"some bytes").unwrap();
    let id = created_id(&spacectl(&dir, &["create", "-f", "a.txt"]));
    for args in [&["list"][..], &["read", &id]] {
        let mut child = spawn(&dir, args, &[]);
        // Closed before the command can have opened the store.
        drop(child.stdout.take());
        let out = child.wait_with_output().unwrap();
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(out.status.success(), "{args:?}: {stderr}");
        assert!(stderr.is_empty(), "{args:?}: {stderr}");
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// SPACE_STORAGE_ROOT set but empty is an error. A store's journal is
/// checked against where its segments are before anything new is created,
/// and the errors say which setting to change (#165).
#[test]
fn a_wrong_storage_root_creates_nothing() {
    let dir = scratch_dir("wrong_root");
    std::fs::write(dir.join("a.txt"), b"in space.nvram").unwrap();
    let id = created_id(&spacectl(&dir, &["create", "-f", "a.txt"]));
    let before = entries(&dir);

    let failure = |dir: &Path, root: Option<&Path>, args: &[&str]| {
        let envs: Vec<(&str, &Path)> = root
            .map(|r| ("SPACE_STORAGE_ROOT", r))
            .into_iter()
            .collect();
        let out = spawn(dir, args, &envs).wait_with_output().unwrap();
        assert!(!out.status.success(), "{args:?} succeeded");
        String::from_utf8_lossy(&out.stderr).into_owned()
    };
    for args in [&["read", &id][..], &["create", "-f", "a.txt"]] {
        let stderr = failure(&dir, Some(Path::new("")), args);
        assert!(
            stderr.contains("SPACE_STORAGE_ROOT is set but empty"),
            "{stderr}"
        );
        let stderr = failure(&dir, Some(&dir.join("sr")), args);
        assert!(stderr.contains("holds no store"), "{stderr}");
        assert!(stderr.contains("unset SPACE_STORAGE_ROOT"), "{stderr}");
    }
    assert_eq!(entries(&dir), before);

    // A store under a root, opened without it.
    let rooted = scratch_dir("rooted");
    let root = rooted.join("root");
    std::fs::write(rooted.join("a.txt"), b"under a root").unwrap();
    let rooted_id = created_id(&spacectl_in_root(
        &rooted,
        &root,
        &["create", "-f", "a.txt"],
    ));
    let stderr = failure(&rooted, None, &["read", &rooted_id]);
    assert!(stderr.contains("Set SPACE_STORAGE_ROOT"), "{stderr}");
    assert!(!rooted.join("space.nvram").exists());

    // Another store's root names the setting.
    let stderr = failure(&dir, Some(&root), &["read", &id]);
    assert!(stderr.contains("SPACE_STORAGE_ROOT"), "{stderr}");
    assert!(stderr.contains("different stores"), "{stderr}");
    assert_eq!(entries(&dir), before);

    let _ = std::fs::remove_dir_all(&dir);
    let _ = std::fs::remove_dir_all(&rooted);
}

/// serve-s3 listens before it opens the store, so a port in use is an
/// error naming the address and leaves no store behind (#166).
#[test]
fn serve_s3_on_a_busy_port_leaves_no_store() {
    let dir = scratch_dir("busy_port");
    let held = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = held.local_addr().unwrap().port().to_string();
    let child = spawn(&dir, &["serve-s3", "--port", &port], &[]);
    let out = exit_of(child, "serve-s3 started on a port that's in use");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success());
    assert!(
        stderr.contains(&format!("couldn't listen on 127.0.0.1:{port}")),
        "{stderr}"
    );
    assert_eq!(entries(&dir), Vec::<String>::new());

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
    std::fs::write(dir.join("a.txt"), b"under a root").unwrap();
    created_id(&spacectl_in_root(
        &dir,
        &dir.join("storage"),
        &["create", "-f", "a.txt"],
    ));
    let out = spacectl_in_root(&dir, &dir.join("storage"), &["compact"]);
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
        .env_remove("SPACE_ALLOW_PLAINTEXT")
        .env_remove("SPACE_STORE_DIR")
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
    spacectl_with(dir, args, &[("SPACE_MASTER_KEY", &"07".repeat(32))])
}

/// spacectl with no SPACE_* variables but `envs`.
fn spacectl_with(dir: &Path, args: &[&str], envs: &[(&str, &str)]) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_spacectl"));
    command
        .args(args)
        .current_dir(dir)
        .env_remove("SPACE_MASTER_KEY")
        .env_remove("SPACE_ALLOW_PLAINTEXT")
        .env_remove("SPACE_STORAGE_ROOT")
        .env_remove("SPACE_STORE_DIR");
    for (name, value) in envs {
        command.env(name, value);
    }
    command.output().unwrap()
}

/// A store opened once with SPACE_MASTER_KEY remembers the key (#157).
/// Another key is refused. Without one, `create` fails before writing
/// anything and `serve-s3` won't start, while commands that write no
/// capsule still work. SPACE_ALLOW_PLAINTEXT=1 lets new objects be stored
/// unencrypted.
#[test]
fn an_encrypted_store_needs_its_key() {
    let dir = scratch_dir("store_key");
    std::fs::write(dir.join("secret.txt"), b"TOP SECRET PAYLOAD").unwrap();
    std::fs::write(dir.join("plain.txt"), b"not secret").unwrap();
    let secret = created_id(&spacectl_with_key(&dir, &["create", "-f", "secret.txt"]));
    let sizes =
        || ["space.journal", "space.nvram"].map(|f| std::fs::metadata(dir.join(f)).unwrap().len());
    let before = sizes();

    let other_key = "08".repeat(32);
    for (args, envs, expected) in [
        (
            &["create", "-f", "plain.txt"][..],
            &[][..],
            "this store is encrypted: set SPACE_MASTER_KEY",
        ),
        (
            &["create", "-f", "plain.txt"],
            &[("SPACE_MASTER_KEY", other_key.as_str())],
            "SPACE_MASTER_KEY isn't this store's key",
        ),
        (
            &["gc"],
            &[("SPACE_MASTER_KEY", other_key.as_str())],
            "SPACE_MASTER_KEY isn't this store's key",
        ),
    ] {
        let out = spacectl_with(&dir, args, envs);
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(!out.status.success(), "{args:?} {envs:?} succeeded");
        assert!(stderr.contains(expected), "{args:?} {envs:?}: {stderr}");
    }
    let out = exit_of(
        spawn(&dir, &["serve-s3", "--port", "0"], &[]),
        "serve-s3 started without the store's key",
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success());
    assert!(stderr.contains("set SPACE_MASTER_KEY"), "{stderr}");
    // Checked before compact, which would reclaim a stray segment.
    assert_eq!(
        sizes(),
        before,
        "something was written without the store's key"
    );
    for args in [&["list"][..], &["gc"], &["compact"], &["scrub"]] {
        let out = spacectl(&dir, args);
        assert!(
            out.status.success(),
            "{args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
    assert_eq!(
        sizes(),
        before,
        "something was written without the store's key"
    );

    // Asked for, new objects are stored unencrypted. The encrypted one
    // still needs the key (#155).
    let plaintext = [("SPACE_ALLOW_PLAINTEXT", "1")];
    let plain = created_id(&spacectl_with(
        &dir,
        &["create", "-f", "plain.txt"],
        &plaintext,
    ));
    let out = spacectl_with(&dir, &["read", &plain], &plaintext);
    assert_eq!(out.stdout, b"not secret");
    let out = spacectl_with(&dir, &["read", &secret], &plaintext);
    assert!(!out.status.success());
    assert!(out.stdout.is_empty());
    assert!(String::from_utf8_lossy(&out.stderr).contains("SPACE_MASTER_KEY"));
    let out = spacectl_with_key(&dir, &["read", &secret]);
    assert_eq!(out.stdout, b"TOP SECRET PAYLOAD");

    // A storage root's store is checked the same way.
    let root = dir.join("root");
    let root_dir = scratch_dir("store_key_root");
    let root_str = root.to_str().unwrap();
    let keyed = [
        ("SPACE_MASTER_KEY", "07".repeat(32)),
        ("SPACE_STORAGE_ROOT", root_str.to_string()),
    ];
    let keyed: Vec<(&str, &str)> = keyed.iter().map(|(k, v)| (*k, v.as_str())).collect();
    std::fs::write(root_dir.join("secret.txt"), b"TOP SECRET PAYLOAD").unwrap();
    created_id(&spacectl_with(
        &root_dir,
        &["create", "-f", "secret.txt"],
        &keyed,
    ));
    let out = spacectl_in_root(&root_dir, &root, &["create", "-f", "secret.txt"]);
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("set SPACE_MASTER_KEY"), "{stderr}");

    let _ = std::fs::remove_dir_all(&root_dir);
    let _ = std::fs::remove_dir_all(&dir);
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

/// serve-s3 logs one line with the addresses it actually listens on, the
/// store and its settings, and serves on every one of them: with
/// `--port 0`, the port the OS picked, on 127.0.0.1 and on ::1 where the
/// host has IPv6 (#158, #170).
#[test]
fn serve_s3_reports_and_serves_every_address_it_listens_on() {
    use std::io::{BufRead, BufReader, Read, Write};
    use std::net::{SocketAddr, TcpStream};

    /// Stops the server however the test ends, so a failed assertion
    /// doesn't leave it running.
    struct Server(Child);
    impl Drop for Server {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }

    let dir = scratch_dir("serve_addrs");
    let mut server = Server(spawn(
        &dir,
        &["serve-s3", "--port", "0"],
        &[("RUST_LOG", Path::new("info"))],
    ));
    let stderr = server.0.stderr.take().unwrap();
    let (lines, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        for line in BufReader::new(stderr).lines().map_while(Result::ok) {
            if lines.send(line).is_err() {
                break;
            }
        }
    });
    let mut before = Vec::new();
    let listening = loop {
        match rx.recv_timeout(Duration::from_secs(60)) {
            Ok(line) if line.contains(" listening ") => break line,
            Ok(line) => before.push(line),
            Err(err) => panic!("no listening line ({err}): {before:?}"),
        }
    };
    let field = |name: &str| {
        listening
            .split_whitespace()
            .find_map(|word| word.strip_prefix(&format!("{name}=")))
            .unwrap_or_else(|| panic!("no {name} in {listening}"))
            .to_string()
    };
    assert_eq!(field("encryption"), "off");
    assert_eq!(field("reclaim"), "300s");
    assert_eq!(field("scrub"), "light:86400s,deep:604800s");
    assert!(field("segments").ends_with("space.nvram"), "{listening}");
    assert!(
        !before.iter().any(|line| line.contains('\u{1F680}')),
        "{before:?}"
    );

    let addrs: Vec<SocketAddr> = field("addr")
        .split(',')
        .map(|a| a.parse().unwrap())
        .collect();
    assert_eq!(addrs[0].ip().to_string(), "127.0.0.1");
    assert_ne!(addrs[0].port(), 0);
    for addr in &addrs {
        assert_eq!(addr.port(), addrs[0].port());
        let mut stream = TcpStream::connect_timeout(addr, Duration::from_secs(5)).unwrap();
        stream
            .write_all(b"GET /-/health HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n")
            .unwrap();
        let mut response = String::new();
        stream.read_to_string(&mut response).unwrap();
        assert!(response.starts_with("HTTP/1.1 200"), "{addr}: {response}");
    }

    drop(server);
    let _ = std::fs::remove_dir_all(&dir);
}
