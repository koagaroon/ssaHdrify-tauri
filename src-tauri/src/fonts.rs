use font_kit::family_name::FamilyName;
use font_kit::handle::Handle;
use font_kit::properties::{Properties, Style, Weight};
use font_kit::source::SystemSource;
use once_cell::sync::Lazy;
use std::collections::HashSet;
use std::fs;
use std::path::Path;
use std::sync::Mutex;

/// Allowed font file extensions (lowercase).
const ALLOWED_FONT_EXTENSIONS: &[&str] = &["ttf", "otf", "ttc", "woff", "woff2"];

/// Strip the Win32 extended-length UNC prefix (`\\?\`) that `canonicalize()`
/// adds on Windows, so paths compare consistently across insert and lookup.
fn normalize_canonical_path(canonical_str: &str) -> String {
    if let Some(stripped) = canonical_str.strip_prefix("\\\\?\\") {
        stripped.to_string()
    } else {
        canonical_str.to_string()
    }
}

/// Provenance cache: tracks font paths returned by `find_system_font`.
/// Only paths that were discovered through the font lookup API are allowed
/// to be read by `subset_font`, preventing arbitrary file reads via IPC.
// Font paths discovered by find_system_font are cached for the session.
// The cache grows unbounded but is bounded by the number of unique system fonts
// (typically < 1000). Paths are never evicted to avoid TOCTOU issues.
static ALLOWED_FONT_PATHS: Lazy<Mutex<HashSet<String>>> =
    Lazy::new(|| Mutex::new(HashSet::new()));

/// Find a system font file path by family name, bold, and italic flags.
/// Returns the absolute path to the font file on disk.
#[tauri::command]
pub fn find_system_font(family: String, bold: bool, italic: bool) -> Result<String, String> {
    // Input validation: reject empty, oversized, or control-char-containing names
    if family.is_empty() || family.len() > 256 {
        return Err("Font family name must be 1-256 characters".to_string());
    }
    if family.chars().any(|c| c.is_control() || c == '\x7f') {
        return Err("Font family name contains invalid characters".to_string());
    }

    let source = SystemSource::new();

    let mut props = Properties::new();
    if bold {
        props.weight = Weight::BOLD;
    }
    if italic {
        props.style = Style::Italic;
    }

    let handle = source
        .select_best_match(&[FamilyName::Title(family.clone())], &props)
        .map_err(|e| format!("Font not found: {} (bold={}, italic={}): {}", family, bold, italic, e))?;

    match handle {
        Handle::Path { path, .. } => {
            // Register the canonicalized path as allowed for subset_font.
            // subset_font checks the canonical form, so we must store the
            // same form here to ensure the provenance check passes.
            let canonical = path.canonicalize()
                .map_err(|e| format!("Cannot resolve font path: {}", e))?;
            let canonical_string = normalize_canonical_path(&canonical.to_string_lossy());
            ALLOWED_FONT_PATHS.lock()
                .map_err(|e| format!("Internal error: font path cache corrupted: {}", e))?
                .insert(canonical_string.clone());

            // Return the normalized canonical path so the frontend uses the
            // same value stored in the provenance cache.
            Ok(canonical_string)
        }
        Handle::Memory { .. } => {
            Err("Font is memory-only (no file path available)".to_string())
        }
    }
}

/// Check whether a canonicalized path is under a known system fonts directory.
/// Uses `starts_with` only — no `contains` patterns — to prevent matching
/// arbitrary directories that happen to include "fonts" in the path.
fn is_in_system_fonts_dir(canonical: &Path) -> bool {
    let canonical_str = normalize_canonical_path(&canonical.to_string_lossy());

    if cfg!(windows) {
        let lower = canonical_str.to_lowercase().replace("/", "\\");
        // System fonts directory — use SYSTEMROOT to support non-C: installs
        let sys_root = std::env::var("SYSTEMROOT")
            .unwrap_or_else(|_| "C:\\Windows".to_string())
            .to_lowercase()
            .replace("/", "\\");
        let sys_fonts_prefix = format!("{}\\fonts\\", sys_root);
        let sys_fonts_exact = format!("{}\\fonts", sys_root);
        let sys_fonts = lower.starts_with(&sys_fonts_prefix)
            || lower == sys_fonts_exact;
        // Per-user fonts directory (Windows 10 1809+)
        let user_fonts = if let Ok(local_app_data) = std::env::var("LOCALAPPDATA") {
            let user_font_dir = format!("{}\\microsoft\\windows\\fonts",
                local_app_data.to_lowercase().replace("/", "\\"));
            lower.starts_with(&format!("{}\\", user_font_dir))
                || lower == user_font_dir
        } else {
            false
        };
        sys_fonts || user_fonts
    } else if cfg!(target_os = "macos") {
        canonical_str.starts_with("/Library/Fonts/")
            || canonical_str == "/Library/Fonts"
            || canonical_str.starts_with("/System/Library/Fonts/")
            || canonical_str == "/System/Library/Fonts"
            || {
                // Per-user fonts: ~/Library/Fonts/
                if let Some(home) = std::env::var_os("HOME") {
                    let home_str = home.to_string_lossy();
                    let user_font_dir = format!("{}/Library/Fonts", home_str);
                    canonical_str.starts_with(&format!("{}/", user_font_dir))
                        || canonical_str == user_font_dir.as_str()
                } else {
                    false
                }
            }
    } else {
        // Linux
        let user_fonts = if let Some(home) = std::env::var_os("HOME") {
            let home_str = home.to_string_lossy();
            let dot_fonts = format!("{}/.fonts", home_str);
            let local_fonts = format!("{}/.local/share/fonts", home_str);
            canonical_str.starts_with(&format!("{}/", dot_fonts))
                || canonical_str == dot_fonts.as_str()
                || canonical_str.starts_with(&format!("{}/", local_fonts))
                || canonical_str == local_fonts.as_str()
        } else {
            false
        };
        canonical_str.starts_with("/usr/share/fonts/")
            || canonical_str == "/usr/share/fonts"
            || canonical_str.starts_with("/usr/local/share/fonts/")
            || canonical_str == "/usr/local/share/fonts"
            || user_fonts
    }
}

/// Subset a font file to only include the specified codepoints.
/// Currently returns the full font file — true subsetting will be added
/// with a dedicated Rust crate in a future update.
///
/// NOTE: The frontend (FontEmbed.tsx) displays a glyph count derived from
/// the `_codepoints` parameter, which may imply the font has been subsetted.
/// Until true subsetting is implemented, the full font is always embedded
/// regardless of the displayed count. TODO: update UI to clarify this.
#[tauri::command]
pub fn subset_font(font_path: String, _codepoints: Vec<u32>) -> Result<Vec<u8>, String> {
    let path = Path::new(&font_path);
    let filename = path.file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("<unknown>");

    // Validate file extension against allowed font types
    let ext = path.extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_lowercase())
        .unwrap_or_default();
    if !ALLOWED_FONT_EXTENSIONS.contains(&ext.as_str()) {
        return Err(format!(
            "Invalid font file type '{}' for '{}'. Allowed extensions: {}",
            ext, filename, ALLOWED_FONT_EXTENSIONS.join(", ")
        ));
    }

    // Canonicalize to resolve symlinks, "..", and normalize the path
    let canonical = path.canonicalize()
        .map_err(|e| format!("Cannot resolve font path: {}", e))?;

    // Primary guard: verify the path was discovered by find_system_font
    let canonical_string = normalize_canonical_path(&canonical.to_string_lossy());
    let is_allowed = ALLOWED_FONT_PATHS.lock()
        .map_err(|e| format!("Internal error: font path cache corrupted: {}", e))?
        .contains(&canonical_string);
    if !is_allowed {
        return Err("Font path was not discovered by find_system_font".to_string());
    }

    // Defense-in-depth: verify canonical path is under a known system fonts directory
    if !is_in_system_fonts_dir(&canonical) {
        return Err("Font path is not in a system fonts directory".to_string());
    }

    // Reject font files larger than 50 MB to prevent OOM with large CJK fonts
    let metadata = fs::metadata(&canonical)
        .map_err(|e| format!("Cannot stat font file: {}", e))?;
    if metadata.len() > 50 * 1024 * 1024 {
        return Err(format!(
            "Font file too large ({} MB, max 50 MB)",
            metadata.len() / 1024 / 1024
        ));
    }

    // TODO: implement actual subsetting with a Rust font crate
    // For now, return the full font file — embedding still works,
    // just with larger file sizes
    fs::read(&canonical)
        .map_err(|e| format!("Failed to read font file '{}': {}", filename, e))
}
