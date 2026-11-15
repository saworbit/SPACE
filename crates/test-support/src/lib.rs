//! Scratch directories for SPACE's tests.
//!
//! [`TempDir`] makes a new directory under `<system temp>/space-tests` and
//! deletes it when it's dropped. It's dropped when a test panics too, so a
//! failed run, or each case proptest tries while shrinking, doesn't leave a
//! directory behind.
//!
//! Create it before anything that opens files inside it. Locals are dropped
//! in reverse order, so the registry, log or view is closed by the time the
//! directory is deleted; Windows won't delete a file that's still open. In a
//! struct, fields are dropped in declaration order, so declare the
//! [`TempDir`] last.

use std::ffi::OsStr;
use std::fmt;
use std::io::ErrorKind;
use std::ops::Deref;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT: AtomicU64 = AtomicU64::new(0);

/// A directory that's deleted, with everything in it, when this is dropped.
pub struct TempDir {
    path: PathBuf,
}

impl TempDir {
    /// Creates `<system temp>/space-tests/<name>-<pid>-<n>`, where `n` counts
    /// up in this process until it names a directory that doesn't exist yet.
    ///
    /// # Panics
    ///
    /// If the directory can't be created.
    pub fn new(name: &str) -> Self {
        let parent = std::env::temp_dir().join("space-tests");
        std::fs::create_dir_all(&parent)
            .unwrap_or_else(|e| panic!("creating {}: {e}", parent.display()));
        loop {
            let n = NEXT.fetch_add(1, Ordering::Relaxed);
            let path = parent.join(format!("{name}-{}-{n}", std::process::id()));
            match std::fs::create_dir(&path) {
                Ok(()) => return Self { path },
                // Left by an earlier run whose process had the same id.
                Err(e) if e.kind() == ErrorKind::AlreadyExists => continue,
                Err(e) => panic!("creating {}: {e}", path.display()),
            }
        }
    }

    /// The directory's path.
    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

impl Deref for TempDir {
    type Target = Path;

    fn deref(&self) -> &Path {
        &self.path
    }
}

impl AsRef<Path> for TempDir {
    fn as_ref(&self) -> &Path {
        &self.path
    }
}

impl AsRef<OsStr> for TempDir {
    fn as_ref(&self) -> &OsStr {
        self.path.as_os_str()
    }
}

impl fmt::Debug for TempDir {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.path.fmt(f)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_dir_is_new_and_removed_on_drop() {
        let a = TempDir::new("support");
        let b = TempDir::new("support");
        assert_ne!(a.path(), b.path());
        assert!(a.is_dir());
        std::fs::create_dir(a.join("sub")).unwrap();
        std::fs::write(a.join("sub").join("file"), b"x").unwrap();
        let path = a.to_path_buf();
        drop(a);
        assert!(!path.exists());
    }

    #[test]
    fn removed_when_the_test_panics() {
        let path = std::sync::Mutex::new(None);
        let result = std::panic::catch_unwind(|| {
            let dir = TempDir::new("support_panic");
            *path.lock().unwrap() = Some(dir.to_path_buf());
            panic!("test failed");
        });
        assert!(result.is_err());
        assert!(!path.into_inner().unwrap().unwrap().exists());
    }
}
