//! Smoke test for the directory-scan path.
//!
//! Scans the OS font directory (a guaranteed-present folder with real
//! fonts) and asserts that `scan_font_directory` streams progress, registers
//! the Rust-owned source index, and honors targeted cancellation.
//!
//! Run with:
//!     cargo test --manifest-path src-tauri/Cargo.toml --test test_scan

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use app_lib::font_cache::FontDirectoryScope;
use app_lib::fonts::{
    cancel_font_scan, clear_font_sources, clear_user_font_db_path, init_user_font_db,
    preflight_font_directory_with_policy, preflight_font_files_with_policy, scan_font_directory,
    scan_font_directory_with_policy, scan_font_files, scan_font_files_with_policy, ScanProgress,
};
use tauri::ipc::{Channel, InvokeResponseBody};

mod common;

// Type aliases for the test channel return shapes — keep clippy happy and
// give the test bodies a more meaningful name than the raw tuple.
type CollectingChannel = (
    Channel<ScanProgress>,
    Arc<Mutex<Vec<usize>>>,
    Arc<Mutex<Option<DoneStats>>>,
);
type CancellingChannel = (
    Channel<ScanProgress>,
    Arc<Mutex<Vec<usize>>>,
    Arc<Mutex<Option<DoneStats>>>,
    Arc<AtomicUsize>,
);

static SCAN_TEST_LOCK: Mutex<()> = Mutex::new(());
static NEXT_TEST_SCAN_ID: AtomicU64 = AtomicU64::new(1_000);

fn next_test_scan_id() -> u64 {
    NEXT_TEST_SCAN_ID.fetch_add(1, Ordering::Relaxed)
}

/// A `Channel<ScanProgress>` that panics if any event is delivered.
/// Tests using this MUST early-error before the scan worker emits any
/// progress event (i.e., the scan command rejects the request before
/// spawning the worker). The panic enforces that contract — a future
/// code path that starts emitting on a previously-rejected branch will
/// surface here instead of silently passing because dropped events
/// produce no assertion failures. For any test that expects events,
/// use `collecting_channel`.
fn early_error_channel() -> Channel<ScanProgress> {
    Channel::new(|_: InvokeResponseBody| {
        panic!("early_error_channel received an event — caller must reject before emitting")
    })
}

/// Wire-format reason values mirroring `fonts::ScanStopReason`. Tests
/// could pull the enum directly via `app_lib::fonts::ScanStopReason`,
/// but parsing string-from-JSON keeps the test surface decoupled from
/// internal type changes — only the wire format matters. `REASON_CEILING_HIT`
/// has no integration test today (would require a 100k-face fixture);
/// `#[allow(dead_code)]` keeps the constant alongside its siblings as
/// documentation of the wire contract.
const REASON_NATURAL: &str = "natural";
const REASON_USER_CANCEL: &str = "userCancel";
#[allow(dead_code)]
const REASON_CEILING_HIT: &str = "ceilingHit";

#[derive(Debug, Default)]
struct DoneStats {
    /// Wire-format string from `ScanProgress::Done.reason`. Compare
    /// against the `REASON_*` constants above.
    reason: String,
    added: usize,
    duplicated: usize,
}

/// A `Channel<ScanProgress>` that records cumulative batch totals and the
/// final Done payload. This pins the low-memory contract: scan progress
/// streams counts to the frontend while the heavy source index stays in Rust.
///
/// The public scan command guards against overlapping scans, so integration
/// tests that call it directly take SCAN_TEST_LOCK before invoking it.
fn collecting_channel() -> CollectingChannel {
    let totals: Arc<Mutex<Vec<usize>>> = Arc::new(Mutex::new(Vec::new()));
    let done: Arc<Mutex<Option<DoneStats>>> = Arc::new(Mutex::new(None));
    let total_sink = totals.clone();
    let done_sink = done.clone();
    let channel = Channel::new(move |body: InvokeResponseBody| {
        let json = match body {
            InvokeResponseBody::Json(s) => s,
            // The Rust side only emits JSON for ScanProgress; raw bodies
            // would indicate an unrelated message and can be ignored.
            InvokeResponseBody::Raw(_) => return Ok(()),
        };
        let event: serde_json::Value =
            serde_json::from_str(&json).expect("ScanProgress channel event must be valid JSON");
        match event.get("kind").and_then(|v| v.as_str()) {
            Some("batch") => {
                let total = event.get("total").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
                total_sink.lock().unwrap().push(total);
            }
            Some("done") => {
                *done_sink.lock().unwrap() = Some(DoneStats {
                    reason: event
                        .get("reason")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string(),
                    added: event.get("added").and_then(|v| v.as_u64()).unwrap_or(0) as usize,
                    duplicated: event
                        .get("duplicated")
                        .and_then(|v| v.as_u64())
                        .unwrap_or(0) as usize,
                });
            }
            _ => {}
        }
        Ok(())
    });
    (channel, totals, done)
}

fn cancelling_channel(scan_id: u64) -> CancellingChannel {
    let totals: Arc<Mutex<Vec<usize>>> = Arc::new(Mutex::new(Vec::new()));
    let done: Arc<Mutex<Option<DoneStats>>> = Arc::new(Mutex::new(None));
    let batches = Arc::new(AtomicUsize::new(0));
    let total_sink = totals.clone();
    let done_sink = done.clone();
    let counter = batches.clone();
    let channel = Channel::new(move |body: InvokeResponseBody| {
        let json = match body {
            InvokeResponseBody::Json(s) => s,
            InvokeResponseBody::Raw(_) => return Ok(()),
        };
        let event: serde_json::Value =
            serde_json::from_str(&json).expect("ScanProgress channel event must be valid JSON");
        match event.get("kind").and_then(|v| v.as_str()) {
            Some("batch") => {
                let total = event.get("total").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
                total_sink.lock().unwrap().push(total);
                let previous_batches = counter.fetch_add(1, Ordering::Relaxed);
                if previous_batches == 0 {
                    cancel_font_scan(scan_id);
                }
            }
            Some("done") => {
                *done_sink.lock().unwrap() = Some(DoneStats {
                    reason: event
                        .get("reason")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string(),
                    added: event.get("added").and_then(|v| v.as_u64()).unwrap_or(0) as usize,
                    duplicated: event
                        .get("duplicated")
                        .and_then(|v| v.as_u64())
                        .unwrap_or(0) as usize,
                });
            }
            _ => {}
        }
        Ok(())
    });
    (channel, totals, done, batches)
}

fn os_font_dir() -> String {
    let sys_root = std::env::var("SYSTEMROOT").unwrap_or_else(|_| "C:\\Windows".to_string());
    format!("{sys_root}\\Fonts")
}

fn take_done(done: &Arc<Mutex<Option<DoneStats>>>) -> DoneStats {
    done.lock()
        .unwrap()
        .take()
        .expect("scan should emit Done on the Ok path")
}

struct ScanTempDir(PathBuf);

impl ScanTempDir {
    fn new(label: &str) -> Self {
        static NEXT_ID: AtomicU64 = AtomicU64::new(0);
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let seq = NEXT_ID.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "ssahdrify-scan-test-{}-{stamp}-{seq}-{label}",
            std::process::id()
        ));
        fs::create_dir_all(&dir).expect("create scan temp dir");
        Self(dir)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for ScanTempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

struct ScanTestDbGuard {
    _dir: ScanTempDir,
}

impl Drop for ScanTestDbGuard {
    fn drop(&mut self) {
        clear_user_font_db_path();
    }
}

fn init_scan_test_db(name: &str) -> ScanTestDbGuard {
    let dir = ScanTempDir::new(&format!("db-{name}"));
    init_user_font_db(dir.path()).expect("scan test DB should initialize");
    ScanTestDbGuard { _dir: dir }
}

/// Build a fast, deterministic multi-batch directory containing `count` copies
/// of a real OS font (`count > 40` triggers multi-batch streaming without
/// reading gigabytes of CJK collections from `C:\Windows\Fonts` four times).
fn make_multi_batch_font_dir(label: &str, count: usize) -> ScanTempDir {
    let holder = ScanTempDir::new(label);
    let seed_dir = common::make_real_font_dir(holder.path());
    let seed_file = fs::read_dir(&seed_dir)
        .expect("read seed font dir")
        .filter_map(Result::ok)
        .map(|e| e.path())
        .find(|p| p.is_file())
        .expect("seed font file must exist");
    let ext = seed_file
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("ttf");
    let font_bytes = fs::read(&seed_file).expect("read seed font bytes");
    let batch_dir = holder.path().join("batch_fonts");
    fs::create_dir_all(&batch_dir).expect("create batch_fonts dir");
    for idx in 0..count {
        fs::write(batch_dir.join(format!("font_{idx:03}.{ext}")), &font_bytes)
            .expect("write copied OS font");
    }
    let _ = fs::remove_dir_all(&seed_dir);
    holder
}

#[test]
fn scans_os_font_directory_streams_progress_and_registers_source() {
    let _guard = SCAN_TEST_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let _db = init_scan_test_db("stream");
    clear_font_sources().unwrap();
    let fixture = make_multi_batch_font_dir("stream-fixture", 60);
    let dir = fixture
        .path()
        .join("batch_fonts")
        .to_string_lossy()
        .into_owned();
    let (channel, totals, done) = collecting_channel();
    if let Err(e) = tauri::async_runtime::block_on(scan_font_directory(
        dir.clone(),
        channel,
        next_test_scan_id(),
        "scan-test-os-fonts".to_string(),
        None,
    )) {
        panic!("scan_font_directory failed for '{dir}': {e}");
    }
    let totals = std::mem::take(&mut *totals.lock().unwrap());
    let done = take_done(&done);

    assert!(
        done.added >= 60,
        "expected at least 60 faces in {dir}, got {}",
        done.added
    );
    assert_eq!(done.reason, REASON_NATURAL);
    assert_eq!(done.duplicated, 0);
    assert_eq!(totals.last().copied(), Some(done.added));
    assert!(
        totals.len() >= 2,
        "expected >= 2 batches for {} entries, got {}",
        done.added,
        totals.len()
    );
}

/// Pick a small set of real font files from the OS font directory so the
/// `scan_font_files` integration test exercises the file-list path with
/// actual parseable fonts. Returns up to `max` entries; skips the test
/// when fewer than 2 are available.
fn os_font_file_sample(max: usize) -> Vec<String> {
    let dir = os_font_dir();
    let mut paths: Vec<String> = Vec::new();
    let Ok(read) = std::fs::read_dir(&dir) else {
        return paths;
    };
    for entry in read.flatten() {
        let p = entry.path();
        let Some(ext) = p
            .extension()
            .and_then(|e| e.to_str())
            .map(|e| e.to_ascii_lowercase())
        else {
            continue;
        };
        // Pattern over enumeration : reuse the
        // canonical allowlist from app_lib::fonts so a future
        // extension addition in fonts.rs flows here automatically.
        if app_lib::fonts::ALLOWED_FONT_EXTENSIONS.contains(&ext.as_str()) && p.is_file() {
            paths.push(p.to_string_lossy().into_owned());
            if paths.len() >= max {
                break;
            }
        }
    }
    paths
}

#[test]
fn scans_user_picked_file_list_streams_progress_and_registers_source() {
    let _guard = SCAN_TEST_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let _db = init_scan_test_db("files");
    clear_font_sources().unwrap();

    let paths = os_font_file_sample(8);
    if paths.len() < 2 {
        println!(
            "Skipping scan_font_files test: only {} fonts available in OS dir",
            paths.len()
        );
        return;
    }

    let (channel, totals, done) = collecting_channel();
    if let Err(e) = tauri::async_runtime::block_on(scan_font_files(
        paths.clone(),
        channel,
        next_test_scan_id(),
        "scan-test-files".to_string(),
    )) {
        panic!("scan_font_files failed: {e}");
    }
    let totals = std::mem::take(&mut *totals.lock().unwrap());
    let done = take_done(&done);

    assert!(done.added > 0, "expected at least one face from {paths:?}");
    assert_eq!(done.reason, REASON_NATURAL);
    if done.duplicated == 0 {
        assert_eq!(totals.last().copied(), Some(done.added));
    }
}

#[test]
fn scan_font_files_rejects_oversize_path_list() {
    let _guard = SCAN_TEST_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let _db = init_scan_test_db("files-oversize");
    clear_font_sources().unwrap();

    let temp_root = std::env::temp_dir();
    let oversize: Vec<String> = (0..1001)
        .map(|i| {
            temp_root
                .join(format!("ssahdrify-nonexistent-{i}.ttf"))
                .to_string_lossy()
                .into_owned()
        })
        .collect();
    let result = tauri::async_runtime::block_on(scan_font_files(
        oversize,
        early_error_channel(),
        next_test_scan_id(),
        "scan-test-files-oversize".to_string(),
    ));
    let err =
        result.expect_err("scan_font_files should reject path lists exceeding MAX_INPUT_PATHS");
    assert!(
        err.starts_with("Too many file paths"),
        "expected MAX_INPUT_PATHS error, got: {err}"
    );
}

#[test]
fn scan_font_files_accepts_max_input_paths_boundary() {
    let _guard = SCAN_TEST_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let _db = init_scan_test_db("files-boundary");
    clear_font_sources().unwrap();

    let temp_root = std::env::temp_dir();
    let boundary: Vec<String> = (0..1000)
        .map(|i| {
            temp_root
                .join(format!("ssahdrify-boundary-{i}.ttf"))
                .to_string_lossy()
                .into_owned()
        })
        .collect();
    let (channel, _totals, done) = collecting_channel();
    let result = tauri::async_runtime::block_on(scan_font_files(
        boundary,
        channel,
        next_test_scan_id(),
        "scan-test-files-boundary".to_string(),
    ));
    assert!(
        result.is_ok(),
        "scan_font_files should accept exactly MAX_INPUT_PATHS paths"
    );
    let done = take_done(&done);
    assert_eq!(done.added, 0);
    assert_eq!(done.reason, "incompleteIo");
}

#[test]
fn rejects_empty_directory_path() {
    let _guard = SCAN_TEST_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let _db = init_scan_test_db("invalid-empty");
    let err = tauri::async_runtime::block_on(scan_font_directory(
        String::new(),
        early_error_channel(),
        next_test_scan_id(),
        "invalid-empty".to_string(),
        None,
    ))
    .unwrap_err();
    assert!(
        err.contains("must be 1-"),
        "empty path should hit the length-floor branch of validate_ipc_path; got: {err}"
    );
}

#[test]
fn rejects_directory_path_with_control_characters() {
    let _guard = SCAN_TEST_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let _db = init_scan_test_db("invalid-control");
    let err = tauri::async_runtime::block_on(scan_font_directory(
        "\0path\0with\0nulls".to_string(),
        early_error_channel(),
        next_test_scan_id(),
        "invalid-control".to_string(),
        None,
    ))
    .unwrap_err();
    assert!(
        err.contains("invalid characters"),
        "control-char path should hit the character-allowlist branch of validate_ipc_path; got: {err}"
    );
}

#[test]
fn rejects_nonexistent_directory_path() {
    let _guard = SCAN_TEST_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let _db = init_scan_test_db("invalid-missing");
    let err = tauri::async_runtime::block_on(scan_font_directory(
        "Z:\\definitely\\does\\not\\exist".to_string(),
        early_error_channel(),
        next_test_scan_id(),
        "invalid-missing".to_string(),
        None,
    ))
    .unwrap_err();
    assert!(
        err.contains("Cannot inspect font library root"),
        "nonexistent path should hit the fail-closed root metadata probe in scan_font_directory; got: {err}"
    );
}

#[test]
fn prerun_cancel_for_old_id_does_not_affect_subsequent_scan() {
    let _guard = SCAN_TEST_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let _db = init_scan_test_db("stale");
    clear_font_sources().unwrap();
    let old_scan_id = next_test_scan_id();
    let fresh_scan_id = next_test_scan_id();
    cancel_font_scan(old_scan_id);
    let fixture = make_multi_batch_font_dir("stale-fixture", 8);
    let dir = fixture
        .path()
        .join("batch_fonts")
        .to_string_lossy()
        .into_owned();
    let (channel, _totals, done) = collecting_channel();
    if let Err(e) = tauri::async_runtime::block_on(scan_font_directory(
        dir.clone(),
        channel,
        fresh_scan_id,
        "scan-test-stale".to_string(),
        None,
    )) {
        panic!("scan_font_directory failed for '{dir}': {e}");
    }
    let done = take_done(&done);
    assert!(
        done.added >= 8,
        "stale cancel id should not affect a fresh scan"
    );
    assert_eq!(done.reason, REASON_NATURAL);
}

#[test]
fn mid_scan_cancel_keeps_partial_results_when_directory_is_large_enough() {
    let _guard = SCAN_TEST_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let _db = init_scan_test_db("cancel");
    clear_font_sources().unwrap();
    let fixture = make_multi_batch_font_dir("cancel-fixture", 80);
    let dir = fixture
        .path()
        .join("batch_fonts")
        .to_string_lossy()
        .into_owned();

    let (full_channel, _full_totals, full_done) = collecting_channel();
    if let Err(e) = tauri::async_runtime::block_on(scan_font_directory(
        dir.clone(),
        full_channel,
        next_test_scan_id(),
        "scan-test-full".to_string(),
        None,
    )) {
        panic!("baseline scan_font_directory failed for '{dir}': {e}");
    }
    let full_done = take_done(&full_done);
    assert!(full_done.added >= 80);

    clear_font_sources().unwrap();
    let scan_id = next_test_scan_id();
    let (cancel_channel, _cancel_totals, cancel_done, cancel_batches) = cancelling_channel(scan_id);
    if let Err(e) = tauri::async_runtime::block_on(scan_font_directory(
        dir.clone(),
        cancel_channel,
        scan_id,
        "scan-test-cancel".to_string(),
        None,
    )) {
        panic!("cancel scan_font_directory failed for '{dir}': {e}");
    }
    let cancel_done = take_done(&cancel_done);

    assert!(
        cancel_done.added > 0,
        "mid-scan cancel should preserve the first emitted batch"
    );
    assert_eq!(cancel_done.reason, REASON_USER_CANCEL);
    assert!(
        cancel_done.added < full_done.added,
        "cancel should stop before the full directory is loaded: partial={}, full={}",
        cancel_done.added,
        full_done.added
    );
    assert!(
        cancel_batches.load(Ordering::Relaxed) >= 1,
        "expected at least one streamed batch before cancellation"
    );
}

#[test]
fn font_preflight_and_scan_commands_enforce_app_filesystem_scope_policy() {
    let _guard = SCAN_TEST_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let _db = init_scan_test_db("fs-scope");
    clear_font_sources().unwrap();

    let holder = ScanTempDir::new("scope-fixture");
    let allowed_dir = common::make_real_font_dir(&holder.path().join("allowed"));
    let denied_dir = common::make_real_font_dir(&holder.path().join("denied"));
    let denied_dir_canonical = denied_dir.canonicalize().unwrap();
    let denied_file = fs::read_dir(&denied_dir)
        .unwrap()
        .filter_map(Result::ok)
        .map(|e| e.path())
        .find(|p| p.is_file())
        .unwrap();
    let allowed_file = fs::read_dir(&allowed_dir)
        .unwrap()
        .filter_map(Result::ok)
        .map(|e| e.path())
        .find(|p| p.is_file())
        .unwrap();

    // 1. Denied root directory is rejected by both preflight and scan.
    let preflight_err = match tauri::async_runtime::block_on(preflight_font_directory_with_policy(
        denied_dir.to_string_lossy().into_owned(),
        Some(FontDirectoryScope::Shallow),
        next_test_scan_id(),
        |_| false,
    )) {
        Ok(_) => panic!("denied preflight directory must error"),
        Err(err) => err,
    };
    assert!(
        preflight_err.contains("filesystem scope policy"),
        "got: {preflight_err}"
    );

    let scan_err = tauri::async_runtime::block_on(scan_font_directory_with_policy(
        denied_dir.to_string_lossy().into_owned(),
        early_error_channel(),
        next_test_scan_id(),
        "scope-denied-root".to_string(),
        Some(FontDirectoryScope::Shallow),
        |_| false,
    ))
    .unwrap_err();
    assert!(
        scan_err.contains("filesystem scope policy"),
        "got: {scan_err}"
    );

    // 2. Recursive scan of parent skips a scope-denied child subtree while importing allowed siblings.
    let deny_subtree = denied_dir.clone();
    let deny_subtree_canon = denied_dir_canonical.clone();
    let (channel, _totals, done) = collecting_channel();
    tauri::async_runtime::block_on(scan_font_directory_with_policy(
        holder.path().to_string_lossy().into_owned(),
        channel,
        next_test_scan_id(),
        "scope-partial-recursive".to_string(),
        Some(FontDirectoryScope::Recursive),
        move |p| !p.starts_with(&deny_subtree) && !p.starts_with(&deny_subtree_canon),
    ))
    .expect("allowed parent with denied child subtree should scan allowed subtree");
    let done = take_done(&done);
    assert!(done.added >= 1);

    // 3. Explicit file preflight and scan skip scope-denied files while keeping allowed files.
    let deny_file_raw = denied_file.clone();
    let deny_file_canon = denied_file.canonicalize().unwrap();
    let preflight = tauri::async_runtime::block_on(preflight_font_files_with_policy(
        vec![
            denied_file.to_string_lossy().into_owned(),
            allowed_file.to_string_lossy().into_owned(),
        ],
        move |p| p != deny_file_raw && p != deny_file_canon,
    ))
    .unwrap();
    let preflight_json = serde_json::to_value(&preflight).unwrap();
    assert_eq!(preflight_json["fontFiles"], 1);

    clear_font_sources().unwrap();
    let deny_file_raw = denied_file.clone();
    let deny_file_canon = denied_file.canonicalize().unwrap();
    let (file_channel, _file_totals, file_done) = collecting_channel();
    tauri::async_runtime::block_on(scan_font_files_with_policy(
        vec![denied_file.to_string_lossy().into_owned()],
        file_channel,
        next_test_scan_id(),
        "scope-denied-files".to_string(),
        move |p| p != deny_file_raw && p != deny_file_canon,
    ))
    .unwrap();
    let file_done = take_done(&file_done);
    assert_eq!(file_done.added, 0);
}
