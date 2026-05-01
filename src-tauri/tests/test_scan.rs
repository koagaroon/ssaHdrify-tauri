//! Smoke test for the directory-scan path.
//!
//! Scans the OS font directory (a guaranteed-present folder with real
//! fonts) and asserts that `scan_font_directory` streams `Batch` events
//! containing canonicalized entries. Path-validation tests use a no-op
//! channel since the rejection happens before any batch could be emitted.
//!
//! Run with:
//!     cargo test --manifest-path src-tauri/Cargo.toml --test test_scan

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use app_lib::fonts::{cancel_font_scan, scan_font_directory, LocalFontEntry, ScanProgress};
use tauri::ipc::{Channel, InvokeResponseBody};

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
/// SCAN_CANCEL_FLAG is process-global: in-source unit tests in fonts.rs
/// flip it for cancel-path coverage. The integration tests below rely on
/// `scan_font_directory`'s `store(false)` reset at command entry to remain
/// independent of any concurrent unit test that may have left the flag
/// set. If a future change ever moves that reset later in the function,
/// this integration test starts intermittently failing — keep the reset
/// at the very top of the public command.
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
        // `Done` sentinel carries no payload — ignore here; the test
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
    let dir = os_font_dir();
    let (channel, collected, batch_count) = collecting_channel();
    if let Err(e) = scan_font_directory(dir.clone(), channel) {
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

    // Streaming-actually-streamed assertion. SCAN_BATCH_SIZE = 20 (see
    // fonts.rs), so any directory with > 20 faces should produce ≥ 2
    // batches. A regression to a single end-of-scan emit would silently
    // pass without this check.
    let batches = batch_count.load(Ordering::Relaxed);
    if entries.len() > 20 {
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
    assert!(scan_font_directory(String::new(), discard_channel()).is_err());
    assert!(scan_font_directory("\0path\0with\0nulls".to_string(), discard_channel()).is_err());
    assert!(scan_font_directory(
        "Z:\\definitely\\does\\not\\exist".to_string(),
        discard_channel()
    )
    .is_err());
}

/// Public-API cancel exercise: invoke cancel_font_scan() right before
/// scan_font_directory() to seed the shared SCAN_CANCEL_FLAG. The
/// stale-signal guard added in F-2 should treat the cancel as stale
/// (no batches yet emitted) and continue the scan rather than aborting
/// with zero entries. Without that guard, a previous-scan cancel landing
/// after the new scan's flag reset would surface as a misleading "no
/// fonts found" error to the user.
#[test]
fn stale_cancel_flag_does_not_abort_fresh_scan() {
    cancel_font_scan();
    let dir = os_font_dir();
    let (channel, collected, _batches) = collecting_channel();
    if let Err(e) = scan_font_directory(dir.clone(), channel) {
        panic!("scan_font_directory failed for '{dir}': {e}");
    }
    let entries: Vec<LocalFontEntry> = std::mem::take(&mut *collected.lock().unwrap());
    assert!(
        !entries.is_empty(),
        "stale cancel signal should have been swallowed by the entry-time \
         reset + first-iteration guard, but scan returned 0 entries"
    );
}
