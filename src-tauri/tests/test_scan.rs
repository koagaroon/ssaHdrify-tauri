//! Smoke test for the directory-scan path.
//!
//! Scans the OS font directory (a guaranteed-present folder with real
//! fonts) and asserts that `scan_font_directory` streams `Batch` events
//! containing canonicalized entries. Path-validation tests use a no-op
//! channel since the rejection happens before any batch could be emitted.
//!
//! Run with:
//!     cargo test --manifest-path src-tauri/Cargo.toml --test test_scan

use std::sync::{Arc, Mutex};

use app_lib::fonts::{scan_font_directory, LocalFontEntry, ScanProgress};
use tauri::ipc::{Channel, InvokeResponseBody};

/// A `Channel<ScanProgress>` that drops every event. Used for path-validation
/// tests where the scan errors out before any batch is emitted.
fn discard_channel() -> Channel<ScanProgress> {
    Channel::new(|_: InvokeResponseBody| Ok(()))
}

/// A `Channel<ScanProgress>` that decodes each batch back into entries and
/// pushes them onto a shared `Vec`. Returned alongside the buffer so the
/// test can assert on the streamed payload after the scan completes.
fn collecting_channel() -> (Channel<ScanProgress>, Arc<Mutex<Vec<LocalFontEntry>>>) {
    let collected: Arc<Mutex<Vec<LocalFontEntry>>> = Arc::new(Mutex::new(Vec::new()));
    let sink = collected.clone();
    let channel = Channel::new(move |body: InvokeResponseBody| {
        let json = match body {
            InvokeResponseBody::Json(s) => s,
            // The Rust side only emits JSON for ScanProgress; raw bodies
            // would indicate an unrelated message and can be ignored.
            InvokeResponseBody::Raw(_) => return Ok(()),
        };
        let event: serde_json::Value = serde_json::from_str(&json).unwrap();
        if event.get("kind").and_then(|v| v.as_str()) != Some("batch") {
            return Ok(());
        }
        let entries = event.get("entries").cloned().unwrap_or(serde_json::Value::Null);
        let parsed: Vec<LocalFontEntry> = serde_json::from_value(entries).unwrap_or_default();
        sink.lock().unwrap().extend(parsed);
        Ok(())
    });
    (channel, collected)
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
    let (channel, collected) = collecting_channel();
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

    let unique_files = entries
        .iter()
        .map(|e| e.path.as_str())
        .collect::<std::collections::HashSet<_>>()
        .len();
    let total_variants: usize = entries.iter().map(|e| e.families.len()).sum();
    println!(
        "Scanned '{}' → {} faces, {} unique files, {} family-name variants",
        dir,
        entries.len(),
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
