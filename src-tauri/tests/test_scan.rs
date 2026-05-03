//! Smoke test for the directory-scan path.
//!
//! Scans the OS font directory (a guaranteed-present folder with real
//! fonts) and asserts that `scan_font_directory` streams progress, registers
//! the Rust-owned source index, and honors targeted cancellation.
//!
//! Run with:
//!     cargo test --manifest-path src-tauri/Cargo.toml --test test_scan

use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use app_lib::fonts::{
    cancel_font_scan, clear_font_sources, init_user_font_db, scan_font_directory, scan_font_files,
    ScanProgress,
};
use tauri::ipc::{Channel, InvokeResponseBody};

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

/// A `Channel<ScanProgress>` that drops every event. Tests using this
/// MUST early-error before any progress event fires (i.e., the scan
/// command rejects the request before spawning the worker). For
/// anything else, use `collecting_channel` so batch + done events are
/// observable.
fn discard_channel() -> Channel<ScanProgress> {
    Channel::new(|_: InvokeResponseBody| Ok(()))
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
        let event: serde_json::Value = serde_json::from_str(&json).unwrap();
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
        let event: serde_json::Value = serde_json::from_str(&json).unwrap();
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

#[cfg(windows)]
fn os_font_dir() -> String {
    let sys_root = std::env::var("SYSTEMROOT").unwrap_or_else(|_| "C:\\Windows".to_string());
    format!("{sys_root}\\Fonts")
}

#[cfg(target_os = "macos")]
fn os_font_dir() -> String {
    "/Library/Fonts".to_string()
}

#[cfg(all(unix, not(target_os = "macos")))]
fn os_font_dir() -> String {
    "/usr/share/fonts".to_string()
}

fn take_done(done: &Arc<Mutex<Option<DoneStats>>>) -> DoneStats {
    done.lock()
        .unwrap()
        .take()
        .expect("scan should emit Done on the Ok path")
}

fn init_scan_test_db(name: &str) {
    let dir = std::env::temp_dir().join(format!(
        "ssahdrify-scan-test-db-{}-{name}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    init_user_font_db(&dir).expect("scan test DB should initialize");
}

#[test]
fn scans_os_font_directory_streams_progress_and_registers_source() {
    let _guard = SCAN_TEST_LOCK.lock().unwrap();
    init_scan_test_db("stream");
    clear_font_sources().unwrap();
    let dir = os_font_dir();
    let (channel, totals, done) = collecting_channel();
    if let Err(e) = tauri::async_runtime::block_on(scan_font_directory(
        dir.clone(),
        channel,
        next_test_scan_id(),
        "scan-test-os-fonts".to_string(),
    )) {
        panic!("scan_font_directory failed for '{dir}': {e}");
    }
    let totals = std::mem::take(&mut *totals.lock().unwrap());
    let done = take_done(&done);

    assert!(
        done.added > 0,
        "expected at least one font in {dir}, got zero"
    );
    assert_eq!(done.reason, REASON_NATURAL);
    assert_eq!(done.duplicated, 0);
    assert_eq!(totals.last().copied(), Some(done.added));

    // Streaming-actually-streamed assertion. SCAN_BATCH_SIZE = 40 (see
    // fonts.rs), so any directory with > 40 faces should produce ≥ 2
    // batches. A regression to a single end-of-scan emit would silently
    // pass without this check.
    if done.added > 40 {
        assert!(
            totals.len() >= 2,
            "expected ≥ 2 batches for {} entries, got {}",
            done.added,
            totals.len()
        );
    }

    println!(
        "Scanned '{}' → {} added faces in {} progress batches",
        dir,
        done.added,
        totals.len(),
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
        if matches!(ext.as_str(), "ttf" | "otf" | "ttc" | "otc") && p.is_file() {
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
    let _guard = SCAN_TEST_LOCK.lock().unwrap();
    init_scan_test_db("files");
    clear_font_sources().unwrap();

    let paths = os_font_file_sample(20);
    if paths.len() < 2 {
        // Portability tradeoff: stripped-down CI containers may not have
        // a populated OS font directory. We accept the silent skip
        // rather than a hard fail because a real dev workstation always
        // has fonts; CI without them is intentional minimalism, not a
        // bug. If this ever needs to gate releases, swap to assert!.
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
    // Final batch total must equal added when the scan reports no
    // duplicates — both numbers come from the same source-of-truth
    // (the SQLite import outcome).
    if done.duplicated == 0 {
        assert_eq!(totals.last().copied(), Some(done.added));
    }
}

#[test]
fn scan_font_files_rejects_oversize_path_list() {
    let _guard = SCAN_TEST_LOCK.lock().unwrap();
    init_scan_test_db("files-oversize");
    clear_font_sources().unwrap();

    // MAX_INPUT_PATHS is 1000 in fonts.rs — supply 1001 paths so the
    // command rejects the call before any worker thread spins up.
    // Absolute paths anchored at the OS temp dir — relative `dummy-N.ttf`
    // would resolve against cargo's working directory, which is
    // non-hermetic across `cargo test` invocation modes.
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
        discard_channel(),
        next_test_scan_id(),
        "scan-test-files-oversize".to_string(),
    ));
    let err =
        result.expect_err("scan_font_files should reject path lists exceeding MAX_INPUT_PATHS");
    // Anchor on the contract-bearing prefix so unrelated future
    // failures (e.g., DB open) don't masquerade as the cap firing.
    assert!(
        err.starts_with("Too many file paths"),
        "expected MAX_INPUT_PATHS error, got: {err}"
    );
}

#[test]
fn scan_font_files_accepts_max_input_paths_boundary() {
    let _guard = SCAN_TEST_LOCK.lock().unwrap();
    init_scan_test_db("files-boundary");
    clear_font_sources().unwrap();

    // Boundary partner of `scan_font_files_rejects_oversize_path_list`.
    // Exactly MAX_INPUT_PATHS (1000) paths must pass length validation
    // and reach the worker — even though every path is a non-existent
    // dummy that fails canonicalize and contributes zero faces, the
    // command must NOT reject before spawning the worker. Catches a
    // future `> ↔ >=` flip in the validation.
    // Absolute paths anchored at the OS temp dir; see oversize test.
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
    assert_eq!(done.reason, REASON_NATURAL);
}

#[test]
fn rejects_invalid_directory_inputs() {
    let _guard = SCAN_TEST_LOCK.lock().unwrap();
    assert!(tauri::async_runtime::block_on(scan_font_directory(
        String::new(),
        discard_channel(),
        next_test_scan_id(),
        "invalid-empty".to_string()
    ))
    .is_err());
    assert!(tauri::async_runtime::block_on(scan_font_directory(
        "\0path\0with\0nulls".to_string(),
        discard_channel(),
        next_test_scan_id(),
        "invalid-control".to_string()
    ))
    .is_err());
    assert!(tauri::async_runtime::block_on(scan_font_directory(
        "Z:\\definitely\\does\\not\\exist".to_string(),
        discard_channel(),
        next_test_scan_id(),
        "invalid-missing".to_string()
    ))
    .is_err());
}

/// Public-API stale cancel exercise: a cancel for an old scan id must not
/// abort a fresh scan using a different id.
#[test]
fn stale_cancel_id_does_not_abort_fresh_scan() {
    let _guard = SCAN_TEST_LOCK.lock().unwrap();
    init_scan_test_db("stale");
    clear_font_sources().unwrap();
    let old_scan_id = next_test_scan_id();
    let fresh_scan_id = next_test_scan_id();
    cancel_font_scan(old_scan_id);
    let dir = os_font_dir();
    let (channel, _totals, done) = collecting_channel();
    if let Err(e) = tauri::async_runtime::block_on(scan_font_directory(
        dir.clone(),
        channel,
        fresh_scan_id,
        "scan-test-stale".to_string(),
    )) {
        panic!("scan_font_directory failed for '{dir}': {e}");
    }
    let done = take_done(&done);
    assert!(
        done.added > 0,
        "stale cancel id should not affect a fresh scan"
    );
    assert_eq!(done.reason, REASON_NATURAL);
}

#[test]
fn mid_scan_cancel_keeps_partial_results_when_directory_is_large_enough() {
    let _guard = SCAN_TEST_LOCK.lock().unwrap();
    init_scan_test_db("cancel");
    clear_font_sources().unwrap();
    let dir = os_font_dir();

    let (full_channel, _full_totals, full_done) = collecting_channel();
    if let Err(e) = tauri::async_runtime::block_on(scan_font_directory(
        dir.clone(),
        full_channel,
        next_test_scan_id(),
        "scan-test-full".to_string(),
    )) {
        panic!("baseline scan_font_directory failed for '{dir}': {e}");
    }
    let full_done = take_done(&full_done);

    if full_done.added <= 40 {
        println!(
            "Skipping mid-scan cancel strength assertion: only {} faces in '{}'",
            full_done.added, dir
        );
        return;
    }

    clear_font_sources().unwrap();
    let scan_id = next_test_scan_id();
    let (cancel_channel, _cancel_totals, cancel_done, cancel_batches) = cancelling_channel(scan_id);
    if let Err(e) = tauri::async_runtime::block_on(scan_font_directory(
        dir.clone(),
        cancel_channel,
        scan_id,
        "scan-test-cancel".to_string(),
    )) {
        panic!("cancel scan_font_directory failed for '{dir}': {e}");
    }
    let cancel_done = take_done(&cancel_done);

    assert!(
        cancel_done.added > 0,
        "mid-scan cancel should preserve the first emitted batch"
    );
    // User cancel — single-variant assertion replaces the previous
    // (cancelled, ceiling_hit) flag pair.
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
