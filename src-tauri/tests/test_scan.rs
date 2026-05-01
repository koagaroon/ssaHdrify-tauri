//! Smoke test for the directory-scan path.
//!
//! Scans the OS font directory (a guaranteed-present folder with real
//! fonts) and asserts that `scan_font_directory` streams `Batch` events
//! containing canonicalized entries. Path-validation tests use a no-op
//! channel since the rejection happens before any batch could be emitted.
//!
//! Run with:
//!     cargo test --manifest-path src-tauri/Cargo.toml --test test_scan

use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use app_lib::fonts::{cancel_font_scan, scan_font_directory, LocalFontEntry, ScanProgress};
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

/// A `Channel<ScanProgress>` that decodes each batch back into entries and
/// pushes them onto a shared `Vec`. Also exposes a batch counter so tests
/// can assert that streaming actually emitted multiple chunks (a regression
/// to a single end-of-scan emit would otherwise pass).
///
/// The public scan command guards against overlapping scans, so integration
/// tests that call it directly take SCAN_TEST_LOCK before invoking it.
fn collecting_channel() -> (
    Channel<ScanProgress>,
    Arc<Mutex<Vec<LocalFontEntry>>>,
    Arc<AtomicUsize>,
) {
    let collected: Arc<Mutex<Vec<LocalFontEntry>>> = Arc::new(Mutex::new(Vec::new()));
    let batches = Arc::new(AtomicUsize::new(0));
    let sink = collected.clone();
    let counter = batches.clone();
    let channel = Channel::new(move |body: InvokeResponseBody| {
        let json = match body {
            InvokeResponseBody::Json(s) => s,
            // The Rust side only emits JSON for ScanProgress; raw bodies
            // would indicate an unrelated message and can be ignored.
            InvokeResponseBody::Raw(_) => return Ok(()),
        };
        let event: serde_json::Value = serde_json::from_str(&json).unwrap();
        // `Done` sentinel is not a batch — ignore here; the test
        // doesn't gate on it the way the production frontend does, since
        // the integration channel is synchronous and the command's Ok
        // return already guarantees the buffer is fully populated.
        if event.get("kind").and_then(|v| v.as_str()) != Some("batch") {
            return Ok(());
        }
        let entries = event
            .get("entries")
            .cloned()
            .unwrap_or(serde_json::Value::Null);
        let parsed: Vec<LocalFontEntry> = serde_json::from_value(entries).unwrap_or_default();
        counter.fetch_add(1, Ordering::Relaxed);
        sink.lock().unwrap().extend(parsed);
        Ok(())
    });
    (channel, collected, batches)
}

fn cancelling_channel(
    scan_id: u64,
) -> (
    Channel<ScanProgress>,
    Arc<Mutex<Vec<LocalFontEntry>>>,
    Arc<AtomicUsize>,
) {
    let collected: Arc<Mutex<Vec<LocalFontEntry>>> = Arc::new(Mutex::new(Vec::new()));
    let batches = Arc::new(AtomicUsize::new(0));
    let sink = collected.clone();
    let counter = batches.clone();
    let channel = Channel::new(move |body: InvokeResponseBody| {
        let json = match body {
            InvokeResponseBody::Json(s) => s,
            InvokeResponseBody::Raw(_) => return Ok(()),
        };
        let event: serde_json::Value = serde_json::from_str(&json).unwrap();
        if event.get("kind").and_then(|v| v.as_str()) != Some("batch") {
            return Ok(());
        }
        let entries = event
            .get("entries")
            .cloned()
            .unwrap_or(serde_json::Value::Null);
        let parsed: Vec<LocalFontEntry> = serde_json::from_value(entries).unwrap_or_default();
        let previous_batches = counter.fetch_add(1, Ordering::Relaxed);
        sink.lock().unwrap().extend(parsed);
        if previous_batches == 0 {
            cancel_font_scan(scan_id);
        }
        Ok(())
    });
    (channel, collected, batches)
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

#[test]
fn scans_os_font_directory_and_populates_metadata() {
    let _guard = SCAN_TEST_LOCK.lock().unwrap();
    let dir = os_font_dir();
    let (channel, collected, batch_count) = collecting_channel();
    if let Err(e) = tauri::async_runtime::block_on(scan_font_directory(
        dir.clone(),
        channel,
        next_test_scan_id(),
    )) {
        panic!("scan_font_directory failed for '{dir}': {e}");
    }
    // Move the collected entries out so subsequent assertions don't hold
    // the mutex; LocalFontEntry isn't Clone, so we can't snapshot via copy.
    let entries: Vec<LocalFontEntry> = std::mem::take(&mut *collected.lock().unwrap());

    assert!(
        !entries.is_empty(),
        "expected at least one font in {dir}, got zero"
    );

    for e in &entries {
        assert!(!e.path.is_empty(), "empty path in scan result");
        assert!(!e.families.is_empty(), "empty families list for {}", e.path);
        for family in &e.families {
            assert!(
                !family.trim().is_empty(),
                "blank family name in list for {}: {:?}",
                e.path,
                e.families
            );
        }
        assert!(e.size_bytes > 0, "zero size for {}", e.path);
        // Scanned paths should have survived canonicalization and not carry
        // the Win32 extended-length prefix.
        assert!(
            !e.path.starts_with("\\\\?\\"),
            "path should be normalized, got {}",
            e.path
        );
    }

    // Streaming-actually-streamed assertion. SCAN_BATCH_SIZE = 40 (see
    // fonts.rs), so any directory with > 40 faces should produce ≥ 2
    // batches. A regression to a single end-of-scan emit would silently
    // pass without this check.
    let batches = batch_count.load(Ordering::Relaxed);
    if entries.len() > 40 {
        assert!(
            batches >= 2,
            "expected ≥ 2 batches for {} entries, got {}",
            entries.len(),
            batches
        );
    }

    let unique_files = entries
        .iter()
        .map(|e| e.path.as_str())
        .collect::<std::collections::HashSet<_>>()
        .len();
    let total_variants: usize = entries.iter().map(|e| e.families.len()).sum();
    println!(
        "Scanned '{}' → {} faces in {} batches, {} unique files, {} family-name variants",
        dir,
        entries.len(),
        batches,
        unique_files,
        total_variants,
    );
}

#[test]
fn rejects_invalid_directory_inputs() {
    let _guard = SCAN_TEST_LOCK.lock().unwrap();
    assert!(tauri::async_runtime::block_on(scan_font_directory(
        String::new(),
        discard_channel(),
        next_test_scan_id()
    ))
    .is_err());
    assert!(tauri::async_runtime::block_on(scan_font_directory(
        "\0path\0with\0nulls".to_string(),
        discard_channel(),
        next_test_scan_id()
    ))
    .is_err());
    assert!(tauri::async_runtime::block_on(scan_font_directory(
        "Z:\\definitely\\does\\not\\exist".to_string(),
        discard_channel(),
        next_test_scan_id()
    ))
    .is_err());
}

/// Public-API stale cancel exercise: a cancel for an old scan id must not
/// abort a fresh scan using a different id.
#[test]
fn stale_cancel_id_does_not_abort_fresh_scan() {
    let _guard = SCAN_TEST_LOCK.lock().unwrap();
    let old_scan_id = next_test_scan_id();
    let fresh_scan_id = next_test_scan_id();
    cancel_font_scan(old_scan_id);
    let dir = os_font_dir();
    let (channel, collected, _batches) = collecting_channel();
    if let Err(e) =
        tauri::async_runtime::block_on(scan_font_directory(dir.clone(), channel, fresh_scan_id))
    {
        panic!("scan_font_directory failed for '{dir}': {e}");
    }
    let entries: Vec<LocalFontEntry> = std::mem::take(&mut *collected.lock().unwrap());
    assert!(
        !entries.is_empty(),
        "stale cancel id should not affect a fresh scan"
    );
}

#[test]
fn mid_scan_cancel_keeps_partial_results_when_directory_is_large_enough() {
    let _guard = SCAN_TEST_LOCK.lock().unwrap();
    let dir = os_font_dir();

    let (full_channel, full_collected, _full_batches) = collecting_channel();
    if let Err(e) = tauri::async_runtime::block_on(scan_font_directory(
        dir.clone(),
        full_channel,
        next_test_scan_id(),
    )) {
        panic!("baseline scan_font_directory failed for '{dir}': {e}");
    }
    let full_entries: Vec<LocalFontEntry> = std::mem::take(&mut *full_collected.lock().unwrap());

    if full_entries.len() <= 40 {
        println!(
            "Skipping mid-scan cancel strength assertion: only {} faces in '{}'",
            full_entries.len(),
            dir
        );
        return;
    }

    let scan_id = next_test_scan_id();
    let (cancel_channel, cancel_collected, cancel_batches) = cancelling_channel(scan_id);
    if let Err(e) =
        tauri::async_runtime::block_on(scan_font_directory(dir.clone(), cancel_channel, scan_id))
    {
        panic!("cancel scan_font_directory failed for '{dir}': {e}");
    }
    let partial_entries: Vec<LocalFontEntry> =
        std::mem::take(&mut *cancel_collected.lock().unwrap());

    assert!(
        !partial_entries.is_empty(),
        "mid-scan cancel should preserve the first emitted batch"
    );
    assert!(
        partial_entries.len() < full_entries.len(),
        "cancel should stop before the full directory is loaded: partial={}, full={}",
        partial_entries.len(),
        full_entries.len()
    );
    assert!(
        cancel_batches.load(Ordering::Relaxed) >= 1,
        "expected at least one streamed batch before cancellation"
    );
}
