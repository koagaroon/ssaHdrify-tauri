#![allow(dead_code)]

use std::ffi::OsStr;
use std::fs;
use std::ops::Deref;
use std::os::windows::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::OnceLock;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use app_lib::fonts::ALLOWED_FONT_EXTENSIONS;

/// RAII temporary directory for integration tests. Removes the directory tree
/// on both normal return and panic unwind so failed assertions never leak
/// stale `ssahdrify-*` directories into `%TEMP%`.
pub struct TestTempDir(PathBuf);

impl TestTempDir {
    pub fn new(prefix: &str, label: &str) -> Self {
        static NEXT_ID: AtomicU64 = AtomicU64::new(0);
        let pid = std::process::id();
        let nano = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let seq = NEXT_ID.fetch_add(1, Ordering::Relaxed);
        let base = std::env::temp_dir();
        fs::create_dir_all(&base).expect("failed to ensure OS temp dir");
        let dir = base.join(format!("{prefix}-{label}-{pid}-{nano}-{seq}"));
        fs::create_dir(&dir).expect("failed to create unique test temp dir");
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

impl AsRef<OsStr> for TestTempDir {
    fn as_ref(&self) -> &OsStr {
        self.0.as_os_str()
    }
}

impl Drop for TestTempDir {
    fn drop(&mut self) {
        for attempt in 0..6 {
            if fs::remove_dir_all(&self.0).is_ok() || !self.0.exists() {
                return;
            }
            if attempt < 5 {
                std::thread::sleep(Duration::from_millis(15));
            }
        }
    }
}

pub fn cli_path() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_ssahdrify-cli"))
}

pub fn run_cli(args: &[&str]) -> Output {
    Command::new(cli_path())
        .args(args)
        .output()
        .expect("failed to spawn ssahdrify-cli")
}

/// Probe once per test binary whether `dist-engine/engine.js` was bundled into
/// `ssahdrify-cli`, panicking with an actionable message if missing.
pub fn assert_engine_bundle_ready() {
    static MISSING_REASON: OnceLock<Option<String>> = OnceLock::new();
    let reason = MISSING_REASON.get_or_init(|| {
        let output = Command::new(cli_path())
            .args([
                "--lang",
                "en",
                "--no-cache",
                "shift",
                "--offset",
                "+1s",
                "/nonexistent-test-input-do-not-create.srt",
            ])
            .output()
            .ok()?;
        let stderr = String::from_utf8_lossy(&output.stderr);
        if stderr.contains("Run `npm run build:engine`") {
            Some(format!(
                "engine bundle missing — run `npm run build:engine` first ({stderr})"
            ))
        } else {
            None
        }
    });
    if let Some(msg) = reason {
        panic!("{msg}");
    }
}

/// Backdate a file or directory's `modified` timestamp by `seconds` without
/// `thread::sleep`. Uses `FILE_FLAG_BACKUP_SEMANTICS` (`0x0200_0000`) so
/// directory handles can update metadata on Windows.
pub fn backdate_modified_seconds(path: &Path, seconds: u64) {
    const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x0200_0000;
    let current = fs::metadata(path)
        .and_then(|m| m.modified())
        .expect("stat path before backdating mtime");
    let target = current
        .checked_sub(Duration::from_secs(seconds))
        .expect("subtract seconds from mtime");
    let handle = fs::OpenOptions::new()
        .write(true)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS)
        .open(path)
        .expect("open file/dir handle to backdate mtime");
    handle
        .set_modified(target)
        .expect("set_modified on test fixture path");
}

fn os_font_roots() -> Vec<PathBuf> {
    vec![
        PathBuf::from(std::env::var("SYSTEMROOT").unwrap_or_else(|_| "C:\\Windows".to_string()))
            .join("Fonts"),
    ]
}

fn real_os_font() -> &'static PathBuf {
    static FONT: OnceLock<PathBuf> = OnceLock::new();
    FONT.get_or_init(|| {
        let mut pending = os_font_roots();
        let mut visited = 0usize;
        while let Some(directory) = pending.pop() {
            visited += 1;
            assert!(
                visited <= 10_000,
                "operating-system font search exceeded 10,000 directories"
            );
            let Ok(read_dir) = fs::read_dir(&directory) else {
                continue;
            };
            let mut entries: Vec<_> = read_dir.flatten().collect();
            entries.sort_by_key(|entry| entry.path());
            for entry in entries {
                let path = entry.path();
                if app_lib::util::try_is_reparse_point(&path).unwrap_or(true) {
                    continue;
                }
                let Ok(file_type) = entry.file_type() else {
                    continue;
                };
                if file_type.is_dir() {
                    pending.push(path);
                    continue;
                }
                let Some(extension) = path
                    .extension()
                    .and_then(|value| value.to_str())
                    .map(|value| value.to_ascii_lowercase())
                else {
                    continue;
                };
                if file_type.is_file() && ALLOWED_FONT_EXTENSIONS.contains(&extension.as_str()) {
                    return path;
                }
            }
        }
        panic!(
            "no readable operating-system font found under {:?}",
            os_font_roots()
        )
    })
}

/// Copy one installed font into a disposable test source. The source file is
/// never committed or shipped; each integration test removes its temp tree.
pub fn make_real_font_dir(parent: &Path) -> PathBuf {
    let font_dir = parent.join("fonts");
    fs::create_dir_all(&font_dir).expect("failed to create fonts subdir");
    let source = real_os_font();
    let file_name = source
        .file_name()
        .expect("OS font path should have a file name");
    fs::copy(source, font_dir.join(file_name)).expect("failed to copy OS font into test source");
    font_dir
}
