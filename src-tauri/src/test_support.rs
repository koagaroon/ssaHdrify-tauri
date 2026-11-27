//! Shared RAII temporary-directory helper for unit tests across `app_lib`.
//!
//! Ensures every temporary directory created by a unit test is removed on both
//! normal exit and panic unwind, preventing stale `ssahdrify*` directories and
//! SQLite files from accumulating in `%TEMP%`.

use std::fs;
use std::ops::Deref;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

pub(crate) struct TestTempDir(PathBuf);

impl TestTempDir {
    pub(crate) fn new(prefix: &str, label: &str) -> Self {
        static NEXT_ID: AtomicU64 = AtomicU64::new(0);
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let seq = NEXT_ID.fetch_add(1, Ordering::Relaxed);
        let base = std::env::temp_dir();
        fs::create_dir_all(&base).expect("ensure OS temp dir");
        let dir = base.join(format!(
            "{prefix}_{label}_{}_{stamp}_{seq}",
            std::process::id()
        ));
        fs::create_dir(&dir).expect("create unique test temp dir");
        Self(dir)
    }
}

impl Deref for TestTempDir {
    type Target = Path;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl AsRef<Path> for TestTempDir {
    fn as_ref(&self) -> &Path {
        &self.0
    }
}

impl Drop for TestTempDir {
    fn drop(&mut self) {
        for attempt in 0..6 {
            if fs::remove_dir_all(&self.0).is_ok() || !self.0.exists() {
                return;
            }
            if attempt < 5 {
                std::thread::sleep(std::time::Duration::from_millis(15));
            }
        }
    }
}
