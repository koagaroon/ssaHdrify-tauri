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
const ALLOWED_FONT_EXTENSIONS: &[&str] = &["ttf", "otf", "ttc", "otc"];

/// Cap on fonts parsed from a single directory or file-list scan. Bounds the
/// IPC payload and prevents a malicious/mistaken pick of a huge directory
/// from blocking the UI thread.
const MAX_FONTS_PER_SCAN: usize = 500;

/// Maximum TTC face count we will enumerate before bailing out. Real fonts
/// stay well under this; the cap is only defense-in-depth against malformed
/// headers that might claim an absurd number of faces.
const MAX_TTC_FACES: u32 = 64;

/// Cap on raw font data read for subsetting — prevents OOM with large CJK
/// fonts and mirrors the front-end guard in `ass-uuencode.ts`.
const MAX_FONT_DATA_SIZE: u64 = 50 * 1024 * 1024;

/// Cap on the unmodified font emitted by the subset fallback path. Lower
/// than `MAX_FONT_DATA_SIZE` because the fallback sends the full font through
/// IPC → JS heap → ASS string.
const MAX_FONT_FALLBACK_SIZE: usize = 10 * 1024 * 1024;

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
/// Never evicted — the set is bounded by the number of unique system fonts
/// (typically < 1000), and eviction would introduce TOCTOU windows.
static ALLOWED_FONT_PATHS: Lazy<Mutex<HashSet<String>>> = Lazy::new(|| Mutex::new(HashSet::new()));

/// Sibling provenance cache for paths that came from a user-picked directory
/// or file list (via `scan_font_directory` / `scan_font_files`). Paths here
/// skip the system-fonts-directory whitelist in `subset_font`, but still must
/// be registered first — arbitrary IPC-supplied paths are still rejected.
static ALLOWED_USER_FONT_PATHS: Lazy<Mutex<HashSet<String>>> =
    Lazy::new(|| Mutex::new(HashSet::new()));

/// Result of font lookup — includes path and face index for TTC files.
#[derive(serde::Serialize)]
pub struct FontLookupResult {
    /// Absolute path to the font file
    pub path: String,
    /// Face index within the file (0 for single-font files, >0 for TTC faces)
    pub index: u32,
}

/// One font face discovered in a user-picked directory or file.
///
/// `families` holds **all** localized family-name variants pulled from the
/// face's name table — a single CJK font typically declares an English name
/// (nameID=1 in en) plus a Chinese name (nameID=1 in zh-CN), and sometimes a
/// separate Typographic Family (nameID=16). Any of these may be what an ASS
/// script chose to reference, so the matcher indexes the face under every
/// variant.
///
/// The entry count reported to users reflects font files/faces (not variants),
/// so a folder with 3 TTFs shows as "3 fonts" even if we pulled 8 matchable
/// name variants from them.
#[derive(serde::Serialize)]
pub struct LocalFontEntry {
    /// Canonical path to the font file (may be shared across entries for TTC)
    pub path: String,
    /// Face index within the file (0 for TTF/OTF, 0..n for TTC/OTC)
    pub index: u32,
    /// All localized family-name variants for this face. The primary (the one
    /// shown in the UI) is `families[0]`; the rest exist for matching only.
    pub families: Vec<String>,
    /// True when OS/2 weight >= 600 (SemiBold+). Matches ASS \b1 semantics.
    pub bold: bool,
    /// True for Italic or Oblique styles.
    pub italic: bool,
    /// File size on disk — useful for UI display.
    pub size_bytes: u64,
}

/// Find a system font file path by family name, bold, and italic flags.
/// Returns the path + face index. Prefers TTF/TTC over OTF/OTC for subtitle
/// renderer compatibility (libass/VSFilter don't support OTF bold).
#[tauri::command]
pub fn find_system_font(
    family: String,
    bold: bool,
    italic: bool,
) -> Result<FontLookupResult, String> {
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
        .map_err(|e| {
            format!(
                "Font not found: {} (bold={}, italic={}): {}",
                family, bold, italic, e
            )
        })?;

    match handle {
        Handle::Path { path, font_index } => {
            let ext = path
                .extension()
                .and_then(|e| e.to_str())
                .map(|e| e.to_lowercase())
                .unwrap_or_default();

            // OTF/OTC warning: libass/VSFilter don't support OTF bold rendering.
            // font-kit's select_best_match returns the system's preferred match
            // and has no API to filter by format. Enumerating the family and
            // loading each face to check style properties is too expensive for
            // a font lookup hot path. Accept the OTF and warn.
            if ext == "otf" || ext == "otc" {
                log::warn!(
                    "Using OTF font for '{}' — bold may not render in libass/VSFilter",
                    family
                );
            }

            register_font_path(&path, font_index)
        }
        Handle::Memory { .. } => Err("Font is memory-only (no file path available)".to_string()),
    }
}

/// Cap on distinct family-name variants pulled from one font face. Real fonts
/// have 2–4 (English + localized); 32 is a generous safety ceiling against a
/// pathological name table.
const MAX_FAMILY_VARIANTS_PER_FACE: usize = 32;

/// Parse one font file (TTF/OTF/TTC/OTC) and return a `LocalFontEntry` per
/// face **and per distinct localized family name** in the face's name table.
///
/// A single TTF can declare its family under multiple languages (common with
/// CJK fonts that ship both an English and a Chinese name). We emit one entry
/// per variant so the frontend matcher finds the font no matter which name the
/// ASS script happens to reference. This was the root cause of the "font not
/// recognized" symptom: font-kit's `family_name()` returns only the
/// locale-preferred name, which on zh-CN Windows silently shadowed English
/// family names that subtitle scripts typically use.
///
/// `canonical` must already be canonicalized by the caller — this function
/// registers the resolved path in `ALLOWED_USER_FONT_PATHS`.
fn parse_local_font_file(canonical: &Path) -> Vec<LocalFontEntry> {
    use fontcull_skrifa::string::StringId;
    use fontcull_skrifa::{FontRef, MetadataProvider};

    // Extension check is intentionally case-insensitive (.TTF vs .ttf are the
    // same file format). The ASCII-lowercase conversion is correct here — all
    // ALLOWED_FONT_EXTENSIONS entries are ASCII.
    let ext = canonical
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase())
        .unwrap_or_default();
    if !ALLOWED_FONT_EXTENSIONS.contains(&ext.as_str()) {
        return Vec::new();
    }

    let size_bytes = fs::metadata(canonical).map(|m| m.len()).unwrap_or(0);
    let is_collection = ext == "ttc" || ext == "otc";
    let max_faces = if is_collection { MAX_TTC_FACES } else { 1 };

    let canonical_string = normalize_canonical_path(&canonical.to_string_lossy());

    // Read the file once; share the bytes between font-kit (weight/style
    // detection) via Arc and skrifa (name-table enumeration) via a slice.
    let data = match fs::read(canonical) {
        Ok(d) => d,
        Err(_) => return Vec::new(),
    };
    let arc_data = std::sync::Arc::new(data);

    let mut entries = Vec::new();
    for i in 0..max_faces {
        // font-kit for weight/style — its enum API is simpler than reading
        // OS/2 directly through skrifa.
        let fk_font = match font_kit::font::Font::from_bytes(arc_data.clone(), i) {
            Ok(f) => f,
            Err(_) => break,
        };
        let props = fk_font.properties();
        let bold = props.weight.0 >= 600.0;
        let italic = !matches!(props.style, Style::Normal);

        // skrifa for ALL localized family names — this is the key fix.
        let font_ref = match FontRef::from_index(&arc_data, i) {
            Ok(f) => f,
            Err(_) => break,
        };

        let mut family_variants: HashSet<String> = HashSet::new();
        for id in [StringId::FAMILY_NAME, StringId::TYPOGRAPHIC_FAMILY_NAME] {
            for localized in font_ref.localized_strings(id) {
                let name: String = localized.chars().collect();
                let trimmed = name.trim();
                if !trimmed.is_empty() && trimmed.len() <= 256 {
                    family_variants.insert(trimmed.to_string());
                    if family_variants.len() >= MAX_FAMILY_VARIANTS_PER_FACE {
                        break;
                    }
                }
            }
            if family_variants.len() >= MAX_FAMILY_VARIANTS_PER_FACE {
                break;
            }
        }

        // Fallback: if the name table produced nothing, emit one entry using
        // font-kit's single-name API so the font isn't silently dropped.
        if family_variants.is_empty() {
            let fallback = fk_font.family_name();
            if !fallback.trim().is_empty() {
                family_variants.insert(fallback);
            }
        }

        if family_variants.is_empty() {
            continue;
        }

        // Register the path once per face — the allow-set is a HashSet so
        // repeated inserts are cheap no-ops.
        if let Ok(mut cache) = ALLOWED_USER_FONT_PATHS.lock() {
            cache.insert(canonical_string.clone());
        }

        // Stabilize the primary-name pick: prefer font-kit's family_name if
        // it's among the variants, else fall back to a sorted order so UI
        // listings stay deterministic across runs (HashSet iteration order
        // is not guaranteed). family_variants is a HashSet, so no duplicates
        // can leak into the sorted list.
        let primary = fk_font.family_name();
        let mut families: Vec<String> = family_variants.into_iter().collect();
        families.sort();
        if let Some(pos) = families.iter().position(|v| v == &primary) {
            // rotate_right(1) moves families[pos] to index 0 while keeping
            // the elements before it in alphabetical order — swap(0, pos)
            // would displace the element at 0 to pos, breaking sort order.
            families[..=pos].rotate_right(1);
        }

        entries.push(LocalFontEntry {
            path: canonical_string.clone(),
            index: i,
            families,
            bold,
            italic,
            size_bytes,
        });
    }
    entries
}

/// Scan a user-picked directory (one level deep) for font files.
/// Does NOT recurse — the `Fonts/` convention is flat by tradition, and
/// limiting recursion keeps the "only files under the picked directory"
/// security reasoning straightforward.
#[tauri::command]
pub fn scan_font_directory(dir: String) -> Result<Vec<LocalFontEntry>, String> {
    if dir.is_empty() || dir.len() > 4096 {
        return Err("Directory path must be 1-4096 characters".to_string());
    }
    if dir.chars().any(|c| c.is_control()) {
        return Err("Directory path contains invalid characters".to_string());
    }

    let canonical_dir = Path::new(&dir)
        .canonicalize()
        .map_err(|e| format!("Cannot resolve directory path: {e}"))?;
    if !canonical_dir.is_dir() {
        return Err(format!("Not a directory: {dir}"));
    }

    let read = fs::read_dir(&canonical_dir).map_err(|e| format!("Cannot read directory: {e}"))?;

    let mut result = Vec::new();
    for entry in read {
        let entry = match entry {
            Ok(e) => e,
            Err(_) => continue,
        };
        let path = entry.path();
        if !path.is_file() {
            continue;
        }

        // Canonicalize per-entry to follow symlinks/reparse points, then
        // verify the resolved file is still under the picked directory.
        // This is what blocks a symlink inside the chosen Fonts/ folder
        // from pointing at /etc/shadow or similar.
        let canonical = match path.canonicalize() {
            Ok(c) => c,
            Err(_) => continue,
        };
        if !canonical.starts_with(&canonical_dir) {
            continue;
        }

        for font_entry in parse_local_font_file(&canonical) {
            result.push(font_entry);
            if result.len() > MAX_FONTS_PER_SCAN {
                return Err(format!(
                    "Too many fonts in directory (> {MAX_FONTS_PER_SCAN}). \
                     Pick a more specific folder or split into multiple."
                ));
            }
        }
    }

    log_scan_summary(
        &format!("font directory '{}'", canonical_dir.display()),
        &result,
    );
    Ok(result)
}

/// Log a scan-result summary with face/file/variant counts.
///
/// Entries map 1:1 to faces. Files ≤ faces (TTC collections hold multiple
/// faces per file); variants per face are folded inside each entry.
fn log_scan_summary(source: &str, result: &[LocalFontEntry]) {
    let file_count = result.iter().map(|e| &e.path).collect::<HashSet<_>>().len();
    let variant_count: usize = result.iter().map(|e| e.families.len()).sum();
    log::info!(
        "Scanned {}: {} faces / {} files / {} name variants",
        source,
        result.len(),
        file_count,
        variant_count,
    );
}

/// Scan a user-picked list of individual font files. Same per-file logic as
/// `scan_font_directory`, but the caller supplies paths directly.
#[tauri::command]
pub fn scan_font_files(paths: Vec<String>) -> Result<Vec<LocalFontEntry>, String> {
    if paths.len() > MAX_FONTS_PER_SCAN {
        return Err(format!(
            "Too many files ({}, max {MAX_FONTS_PER_SCAN})",
            paths.len()
        ));
    }

    let mut result = Vec::new();
    for p in paths {
        if p.is_empty() || p.len() > 4096 {
            continue;
        }
        if p.chars().any(|c| c.is_control()) {
            continue;
        }

        let canonical = match Path::new(&p).canonicalize() {
            Ok(c) => c,
            Err(_) => continue,
        };
        if !canonical.is_file() {
            continue;
        }

        for font_entry in parse_local_font_file(&canonical) {
            result.push(font_entry);
            if result.len() > MAX_FONTS_PER_SCAN {
                return Err(format!(
                    "Too many font faces across files (> {MAX_FONTS_PER_SCAN})"
                ));
            }
        }
    }

    log_scan_summary("local font files", &result);
    Ok(result)
}

/// Register a font path in the provenance cache and return the lookup result.
fn register_font_path(path: &Path, font_index: u32) -> Result<FontLookupResult, String> {
    let canonical = path
        .canonicalize()
        .map_err(|e| format!("Cannot resolve font path: {}", e))?;
    let canonical_string = normalize_canonical_path(&canonical.to_string_lossy());
    ALLOWED_FONT_PATHS
        .lock()
        .map_err(|e| format!("Internal error: font path cache corrupted: {}", e))?
        .insert(canonical_string.clone());

    Ok(FontLookupResult {
        path: canonical_string,
        index: font_index,
    })
}

/// True when `path` equals `dir` or lives under it (using `sep` as the
/// separator). Matched via `starts_with` only — no `contains` — so that
/// directories whose names merely include "fonts" never leak through.
fn path_under_dir(path: &str, dir: &str, sep: &str) -> bool {
    path == dir || path.starts_with(&format!("{dir}{sep}"))
}

/// Check whether a canonicalized path is under a known system fonts directory.
fn is_in_system_fonts_dir(canonical: &Path) -> bool {
    let canonical_str = normalize_canonical_path(&canonical.to_string_lossy());

    if cfg!(windows) {
        let lower = canonical_str.to_lowercase().replace("/", "\\");
        let under = |dir: &str| path_under_dir(&lower, dir, "\\");

        // System fonts directory — use SYSTEMROOT to support non-C: installs
        let sys_root = std::env::var("SYSTEMROOT")
            .unwrap_or_else(|_| "C:\\Windows".to_string())
            .to_lowercase()
            .replace("/", "\\");
        if under(&format!("{sys_root}\\fonts")) {
            return true;
        }
        // Per-user fonts directory (Windows 10 1809+)
        if let Ok(local_app_data) = std::env::var("LOCALAPPDATA") {
            let user_font_dir = format!(
                "{}\\microsoft\\windows\\fonts",
                local_app_data.to_lowercase().replace("/", "\\")
            );
            if under(&user_font_dir) {
                return true;
            }
        }
        false
    } else if cfg!(target_os = "macos") {
        let under = |dir: &str| path_under_dir(&canonical_str, dir, "/");
        const MAC_DIRS: &[&str] = &[
            "/Library/Fonts",
            "/System/Library/Fonts",
            "/System/Library/AssetsV2",
            "/Library/Application Support",
            "/opt/homebrew/share/fonts",
            "/usr/local/share/fonts",
        ];
        if MAC_DIRS.iter().any(|d| under(d)) {
            return true;
        }
        // Per-user fonts: ~/Library/Fonts/
        if let Some(home) = std::env::var_os("HOME") {
            let user_font_dir = format!("{}/Library/Fonts", home.to_string_lossy());
            if under(&user_font_dir) {
                return true;
            }
        }
        false
    } else {
        // Linux
        let under = |dir: &str| path_under_dir(&canonical_str, dir, "/");
        if under("/usr/share/fonts") || under("/usr/local/share/fonts") {
            return true;
        }
        if let Some(home) = std::env::var_os("HOME") {
            let home_str = home.to_string_lossy();
            if under(&format!("{home_str}/.fonts"))
                || under(&format!("{home_str}/.local/share/fonts"))
            {
                return true;
            }
        }
        false
    }
}

/// Subset a font file to only include the specified codepoints.
///
/// Uses fontcull (Google's klippa engine) for pure-Rust subsetting.
/// For TTC files with face index > 0, uses fontcull's internal crates directly
/// to select the correct face. Always includes ASCII printable (0x0020–0x007E)
/// and CJK fullwidth forms (0xFF01–0xFF5E) as safety padding.
/// Falls back to full font on error.
#[tauri::command]
pub fn subset_font(
    font_path: String,
    font_index: u32,
    codepoints: Vec<u32>,
) -> Result<Vec<u8>, String> {
    // IPC boundary validation: font_index and codepoints come from untrusted JS
    if font_index > 255 {
        return Err(format!("Invalid font face index: {font_index} (max 255)"));
    }
    if codepoints.len() > 200_000 {
        return Err(format!(
            "Too many codepoints: {} (max 200,000)",
            codepoints.len()
        ));
    }
    // Reject out-of-range codepoints. Unicode tops out at U+10FFFF; anything
    // larger is a JS-side bug or a crafted IPC payload and must not reach
    // fontcull's IntSet, which would happily allocate for absurd values.
    if let Some(&bad) = codepoints.iter().find(|&&cp| cp > 0x10FFFF) {
        return Err(format!("Invalid codepoint: U+{bad:X} (max U+10FFFF)"));
    }

    let path = Path::new(&font_path);
    let filename = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("<unknown>");

    // Validate file extension against allowed font types
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_lowercase())
        .unwrap_or_default();
    if !ALLOWED_FONT_EXTENSIONS.contains(&ext.as_str()) {
        return Err(format!(
            "Invalid font file type '{}' for '{}'. Allowed extensions: {}",
            ext,
            filename,
            ALLOWED_FONT_EXTENSIONS.join(", ")
        ));
    }

    // Canonicalize to resolve symlinks, "..", and normalize the path
    let canonical = path
        .canonicalize()
        .map_err(|e| format!("Cannot resolve font path: {}", e))?;

    // Primary guard: the path must have been discovered by one of the scan
    // commands (find_system_font OR scan_font_directory / scan_font_files).
    // Arbitrary IPC-supplied paths are rejected.
    let canonical_string = normalize_canonical_path(&canonical.to_string_lossy());
    let is_system = ALLOWED_FONT_PATHS
        .lock()
        .map_err(|e| format!("Internal error: font path cache corrupted: {e}"))?
        .contains(&canonical_string);
    let is_user = ALLOWED_USER_FONT_PATHS
        .lock()
        .map_err(|e| format!("Internal error: user font path cache corrupted: {e}"))?
        .contains(&canonical_string);
    if !is_system && !is_user {
        return Err("Font path was not discovered by a scan command".to_string());
    }

    // Defense-in-depth: system-discovered paths must live under a known
    // system fonts directory. User-picked paths skip this check by design
    // — the whole point is to accept a user-chosen directory — but they
    // still had to pass the provenance cache above, so random file reads
    // via IPC are still blocked.
    if is_system && !is_in_system_fonts_dir(&canonical) {
        return Err("System font path is not in a system fonts directory".to_string());
    }

    // Pre-read size check — rejects obvious oversize before allocating the Vec.
    let metadata = fs::metadata(&canonical).map_err(|e| format!("Cannot stat font file: {}", e))?;
    if metadata.len() > MAX_FONT_DATA_SIZE {
        return Err(format!(
            "Font file too large ({} MB, max {} MB)",
            metadata.len() / 1024 / 1024,
            MAX_FONT_DATA_SIZE / 1024 / 1024
        ));
    }

    let font_data = fs::read(&canonical)
        .map_err(|e| format!("Failed to read font file '{}': {}", filename, e))?;

    // Post-read size check (TOCTOU mitigation — file could grow between stat and read)
    if font_data.len() as u64 > MAX_FONT_DATA_SIZE {
        return Err(format!(
            "Font file too large after read ({} MB, max {} MB)",
            font_data.len() / 1024 / 1024,
            MAX_FONT_DATA_SIZE / 1024 / 1024
        ));
    }

    // Build codepoint set: caller's codepoints + safety padding
    let mut all_codepoints = codepoints;
    // ASCII printable — always needed for punctuation, numbers, basic latin
    all_codepoints.extend(0x0020u32..=0x007Eu32);
    // CJK fullwidth forms — common in CJK subtitle typesetting (，。！？etc.)
    all_codepoints.extend(0xFF01u32..=0xFF5Eu32);
    all_codepoints.sort();
    all_codepoints.dedup();

    // Attempt subsetting; fall back to full font if it fails
    let subset_result = if font_index == 0 {
        // Common path: single font or first face in TTC
        fontcull::subset_font_data_unicode(&font_data, &all_codepoints, &[])
            .map_err(|e| format!("{e:?}"))
    } else {
        // TTC with face index > 0: use internal crates with from_index
        subset_with_index(&font_data, font_index, &all_codepoints)
    };

    match subset_result {
        Ok(subsetted) => {
            log::info!(
                "Subsetted '{}' (face {}): {} → {} bytes ({} codepoints)",
                filename,
                font_index,
                font_data.len(),
                subsetted.len(),
                all_codepoints.len()
            );
            Ok(subsetted)
        }
        Err(e) => {
            log::warn!(
                "Subsetting failed for '{}' (face {}): {}, falling back to full font",
                filename,
                font_index,
                e
            );
            // Cap fallback size — the full font goes through IPC → JS heap → ASS string,
            // so a large font would cause excessive memory use in the frontend.
            if font_data.len() > MAX_FONT_FALLBACK_SIZE {
                return Err(format!(
                    "Subsetting failed and full font too large ({} MB, max {} MB for fallback)",
                    font_data.len() / 1024 / 1024,
                    MAX_FONT_FALLBACK_SIZE / 1024 / 1024
                ));
            }
            Ok(font_data)
        }
    }
}

/// Subset a specific face from a TTC/OTC collection file.
/// Uses fontcull's internal crates directly for `FontRef::from_index`.
fn subset_with_index(font_data: &[u8], index: u32, codepoints: &[u32]) -> Result<Vec<u8>, String> {
    use fontcull_klippa::{subset_font, Plan, SubsetFlags};
    use fontcull_read_fonts::collections::IntSet;
    use fontcull_skrifa::{FontRef, GlyphId, Tag};
    use fontcull_write_fonts::types::NameId;

    let font = FontRef::from_index(font_data, index)
        .map_err(|e| format!("Cannot parse font face {index}: {e:?}"))?;

    let mut unicode_set: IntSet<u32> = IntSet::empty();
    for &cp in codepoints {
        unicode_set.insert(cp);
    }

    let empty_gids: IntSet<GlyphId> = IntSet::empty();
    let empty_tags: IntSet<Tag> = IntSet::empty();
    let empty_name_ids: IntSet<NameId> = IntSet::empty();
    let empty_langs: IntSet<u16> = IntSet::empty();
    let layout_scripts: IntSet<Tag> = IntSet::all();
    let layout_features: IntSet<Tag> = IntSet::empty();

    let plan = Plan::new(
        &empty_gids,
        &unicode_set,
        &font,
        SubsetFlags::default(),
        &empty_tags,
        &layout_scripts,
        &layout_features,
        &empty_name_ids,
        &empty_langs,
    );

    subset_font(&font, &plan).map_err(|e| format!("Subset failed for face {index}: {e:?}"))
}
