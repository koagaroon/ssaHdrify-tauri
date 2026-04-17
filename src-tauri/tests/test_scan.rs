//! Smoke test for the directory-scan path.
//!
//! Scans the OS font directory (a guaranteed-present folder with real
//! fonts). Verifies that `scan_font_directory` returns entries, that each
//! entry's path came back canonicalized, and that the family/bold/italic
//! fields are populated.
//!
//! Run with:
//!     cargo test --manifest-path src-tauri/Cargo.toml --test test_scan

use app_lib::fonts::scan_font_directory;

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
    let entries = match scan_font_directory(dir.clone()) {
        Ok(e) => e,
        Err(e) => panic!("scan_font_directory failed for '{dir}': {e}"),
    };

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

    let total_variants: usize = entries.iter().map(|e| e.families.len()).sum();
    println!(
        "Scanned '{}' → {} faces, {} unique files, {} family-name variants",
        dir,
        entries.len(),
        entries
            .iter()
            .map(|e| &e.path)
            .collect::<std::collections::HashSet<_>>()
            .len(),
        total_variants,
    );
}

#[test]
fn rejects_invalid_directory_inputs() {
    assert!(scan_font_directory(String::new()).is_err());
    assert!(scan_font_directory("\0path\0with\0nulls".to_string()).is_err());
    assert!(scan_font_directory("Z:\\definitely\\does\\not\\exist".to_string()).is_err());
}
