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

use app_lib::fonts::{cancel_font_scan, clear_font_sources, scan_font_directory, ScanProgress};
use tauri::ipc::{Channel, InvokeResponseBody};

static SCAN_TEST_LOCK: Mutex<()> = Mutex::new(());
static NEXT_TEST_SCAN_ID: AtomicU64 = AtomicU64::new(1_000);

fn next_test_scan_id() -> u64 {
    NEXT_TEST_SCAN_ID.fetch_add(1, Ordering::Relaxed)
}

/// A `Channel<ScanProgress>` that drops every event. Used for path-validation
/// tests where the scan errors out before any batch is emitted.
fn discard_channel() -> Channel<ScanProgress> {
    Channel::new(|_: InvokeResponseBody| Ok(()))
}

#[derive(Debug, Default)]
struct DoneStats {
    cancelled: bool,
    added: usize,
    duplicated: usize,
}

/// A `Channel<ScanProgress>` that records cumulative batch totals and the
/// final Done payload. This pins the low-memory contract: scan progress
/// streams counts to the frontend while the heavy source index stays in Rust.
///
/// The public scan command guards against overlapping scans, so integration
/// tests that call it directly take SCAN_TEST_LOCK before invoking it.
fn collecting_channel() -> (
    Channel<ScanProgress>,
    Arc<Mutex<Vec<usize>>>,
    Arc<Mutex<Option<DoneStats>>>,
) {
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
                    cancelled: event
                        .get("cancelled")
                        .and_then(|v| v.as_bool())
                        .unwrap_or(false),
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

fn cancelling_channel(
    scan_id: u64,
) -> (
    Channel<ScanProgress>,
    Arc<Mutex<Vec<usize>>>,
    Arc<Mutex<Option<DoneStats>>>,
    Arc<AtomicUsize>,
) {
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
                    cancelled: event
                        .get("cancelled")
                        .and_then(|v| v.as_bool())
                        .unwrap_or(false),
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

#[test]
fn scans_os_font_directory_streams_progress_and_registers_source() {
    let _guard = SCAN_TEST_LOCK.lock().unwrap();
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
    assert!(!done.cancelled);
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
    assert!(!done.cancelled);
}

#[test]
fn mid_scan_cancel_keeps_partial_results_when_directory_is_large_enough() {
    let _guard = SCAN_TEST_LOCK.lock().unwrap();
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
    assert!(cancel_done.cancelled);
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
