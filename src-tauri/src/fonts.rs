use font_kit::family_name::FamilyName;
use font_kit::handle::Handle;
use font_kit::properties::{Properties, Style, Weight};
use font_kit::source::SystemSource;
use once_cell::sync::Lazy;
use std::collections::HashSet;
use std::fs;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// Allowed font file extensions (lowercase).
const ALLOWED_FONT_EXTENSIONS: &[&str] = &["ttf", "otf", "ttc", "otc"];

/// Defense-in-depth ceiling on faces emitted from a single scan. Not a UX
/// limit — real font-collection users with thousands of files should never
/// hit this. Caps malicious/runaway directories whose IPC payload would
/// otherwise pin the JS heap (each LocalFontEntry serializes to ~500 bytes,
/// so 100k entries ≈ 50 MB end-to-end). Above this the channel-streaming
/// architecture is the wrong tool — partial results are preserved and the
/// scan stops. The check fires when `total > MAX_FONTS_PER_SCAN`, so the
/// permitted-then-rejected entry is the (MAX + 1)th — cosmetic off-by-one
/// kept as-is so the buffer flush emits everything that was parseable.
const MAX_FONTS_PER_SCAN: usize = 100_000;

/// Cap on the path-list length accepted by `scan_font_files`. Mirrors
/// `dropzone::MAX_INPUT_PATHS` — the OS file picker can't realistically
/// deliver more than a handful of thousand files in one selection, so 1000
/// is generous for the user-facing flow while bounding worst-case CPU/IO
/// if a future code path or compromised frontend supplies a huge vector.
/// The per-entry MAX_FONTS_PER_SCAN ceiling still applies inside the loop.
const MAX_INPUT_PATHS: usize = 1000;

/// Number of faces accumulated before flushing a `ScanProgress::Batch`.
///
/// This value is a UX choice, NOT a correctness gate. Correctness lives in
/// the `ScanProgress::Done` sentinel — the frontend awaits Done before
/// reading its accumulator, so any batch that happens to slip onto Tauri's
/// async `plugin:__TAURI_CHANNEL__|fetch` path (8192-byte threshold) is
/// still delivered in order before Done fires. Do NOT remove the Done
/// sentinel as "redundant" because batches are sized small here — that
/// would reintroduce a silent zero-result for any oversize batch.
///
/// What this size DOES buy: keeping typical batches under the 8 KB
/// threshold makes them travel via Tauri's synchronous direct-eval path,
/// so they fire the JS callback during the scan rather than in a single
/// burst at the end. Concretely: progress UI climbs visibly, cancel clicks
/// land on a responsive JS thread, and React renders the count
/// incrementally. With typical entry sizes (~150–220 bytes JSON, varies
/// with path length and family-variant count), 20 faces yields roughly
/// 3–5 KB per batch. Pathological entries (32 family variants × 256-cp
/// CJK names × 32K UNC path) can blow this past 8 KB — that's fine,
/// correctness still holds via Done.
const SCAN_BATCH_SIZE: usize = 20;

/// Maximum wall-clock interval between progress emits during a slow scan.
/// Forces a flush even when the per-batch threshold hasn't been hit yet,
/// so the UI keeps rolling on a folder whose files happen to parse slowly
/// (large CJK fonts, network-mounted drives, etc.).
const SCAN_BATCH_INTERVAL: Duration = Duration::from_millis(100);

/// Maximum TTC face count we will enumerate before bailing out. Real
/// production fonts ship with 2–8 faces; 16 is generous while capping the
/// work a crafted TTC can force us to do (e.g., a malicious file declaring
/// 256 faces with every other one malformed would otherwise drive the
/// per-file parse cost linearly).
const MAX_TTC_FACES: u32 = 16;

/// Cap on raw font data read for subsetting — prevents OOM with large CJK
/// fonts and mirrors the front-end guard in `ass-uuencode.ts`.
const MAX_FONT_DATA_SIZE: u64 = 50 * 1024 * 1024;

/// Cap on the unmodified font emitted by the subset fallback path. Lower
/// than `MAX_FONT_DATA_SIZE` because the fallback sends the full font through
/// IPC → JS heap → ASS string.
const MAX_FONT_FALLBACK_SIZE: usize = 10 * 1024 * 1024;

/// Overall cap on the size of each provenance cache, as a defense against a
/// pathological long-running session that accumulates tens of thousands of
/// scanned directories. A typical desktop has < 1000 installed fonts; 100k
/// leaves plenty of headroom for power users who scan many folders while
/// still bounding worst-case memory (each entry averages ≤200 bytes, so the
/// cap is ~20 MB per cache).
const MAX_PROVENANCE_CACHE_SIZE: usize = 100_000;

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

/// Cooperative cancel flag for the active font scan. The UI exposes only a
/// single scan at a time (FontSourceModal disables both pickers while one
/// is running), so a single global is sufficient. `scan_font_directory` and
/// `scan_font_files` clear the flag on entry — if the user clicks cancel
/// before a new scan starts, the stale signal is harmlessly reset.
static SCAN_CANCEL_FLAG: AtomicBool = AtomicBool::new(false);

/// Streaming progress event for the font scan commands. The `Batch` variant
/// carries newly-parsed faces in chunks; `Done` is the end-of-stream
/// sentinel the frontend awaits before returning the accumulator.
///
/// **Why Done is required**: Tauri's `Channel` uses two delivery paths
/// internally. Payloads under 8 KB go via direct `webview.eval()` and fire
/// the JS callback synchronously *during* the command execution. Payloads
/// ≥ 8 KB use an async `plugin:__TAURI_CHANNEL__|fetch` round-trip — those
/// callbacks fire *after* the command's invoke promise has already
/// resolved. Without a sentinel the frontend would `return accumulated`
/// while large batches were still in flight, producing an empty list.
/// `Done` is small (under the threshold), travels via direct eval, but the
/// Channel layer enforces in-order delivery — so the frontend's `Done`
/// handler only fires *after* every preceding `Batch` has been processed.
/// See A-bug-1 in the v1.3.1 design doc for the diagnostic data.
#[derive(serde::Serialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum ScanProgress {
    /// One chunk of newly-parsed faces. Multiple `Batch` events are emitted
    /// per scan; the frontend appends them to its accumulator.
    Batch { entries: Vec<LocalFontEntry> },
    /// End-of-stream sentinel. Always emitted on the `Ok` path (success or
    /// cancel). NOT emitted on the `Err` path — the invoke rejection
    /// already signals failure and the frontend must not block waiting for
    /// a `Done` that will never arrive.
    Done,
}

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
///
/// `Deserialize` is derived for the integration tests in `tests/test_scan.rs`,
/// which round-trip the streamed `ScanProgress::Batch` JSON back into entries
/// for assertion. The frontend never deserializes this on the Rust side, so
/// it is a benign addition.
#[derive(serde::Serialize, serde::Deserialize)]
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
            // Log the detailed error server-side; return a generic message
            // to the frontend so OS-level paths never surface in user toasts.
            // INFO not WARN: a missed system lookup is normal flow when the
            // user hasn't picked local font sources yet, and the frontend
            // surfaces "Missing" badges per font anyway. Bumping to warn
            // would spam dev logs every time a batch is analyzed before
            // sources are added; release builds (Warn+) hide info entirely.
            log::info!("font lookup failed for '{family}' (bold={bold}, italic={italic}): {e}");
            format!("Font not found: {family} (bold={bold}, italic={italic})")
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

    // Stat + size-cap guard before fs::read — a malicious user-picked
    // directory could otherwise OOM the process by containing a .ttf
    // that's actually a hundred-gigabyte impostor file. Aligns with
    // subset_font's own MAX_FONT_DATA_SIZE cap.
    let size_bytes = match fs::metadata(canonical) {
        Ok(m) => {
            if m.len() > MAX_FONT_DATA_SIZE {
                return Vec::new();
            }
            m.len()
        }
        Err(_) => return Vec::new(),
    };
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
    // Permit a single consecutive parse failure before giving up. In practice
    // font_kit returns an error once `i` exceeds the real face count, so one
    // tolerance catches that natural end-of-collection while keeping the
    // per-file parse cost bounded at 2 × face_count rather than 3 ×. Crafted
    // TTCs cannot force us to parse all 64 slots just by salting every other
    // face with bad data.
    const MAX_CONSECUTIVE_FAILURES: u32 = 1;
    let mut consecutive_failures: u32 = 0;
    for i in 0..max_faces {
        // font-kit for weight/style — its enum API is simpler than reading
        // OS/2 directly through skrifa.
        let fk_font = match font_kit::font::Font::from_bytes(arc_data.clone(), i) {
            Ok(f) => {
                consecutive_failures = 0;
                f
            }
            Err(_) => {
                consecutive_failures += 1;
                if consecutive_failures > MAX_CONSECUTIVE_FAILURES {
                    break;
                }
                continue;
            }
        };
        let props = fk_font.properties();
        let bold = props.weight.0 >= 600.0;
        let italic = !matches!(props.style, Style::Normal);

        // skrifa for ALL localized family names — this is the key fix.
        let font_ref = match FontRef::from_index(&arc_data, i) {
            Ok(f) => f,
            Err(_) => {
                consecutive_failures += 1;
                if consecutive_failures > MAX_CONSECUTIVE_FAILURES {
                    break;
                }
                continue;
            }
        };

        let mut family_variants: HashSet<String> = HashSet::new();
        for id in [StringId::FAMILY_NAME, StringId::TYPOGRAPHIC_FAMILY_NAME] {
            for localized in font_ref.localized_strings(id) {
                // Take chars lazily with a short ceiling so a malformed
                // font with a 2GB name-table entry can't OOM the process
                // before the length guard fires. 257 chars is enough to
                // detect ">256 chars" overflow in the guard below.
                let name: String = localized.chars().take(257).collect();
                let trimmed = name.trim();
                // Guard counts CODEPOINTS, not bytes — a 100-char CJK
                // family name (300+ UTF-8 bytes) is perfectly legitimate,
                // and the previous byte-length gate silently dropped such
                // names on non-Latin fonts.
                let char_count = trimmed.chars().count();
                if !trimmed.is_empty() && char_count <= 256 {
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
        // repeated inserts are cheap no-ops. Honor MAX_PROVENANCE_CACHE_SIZE
        // (mirrors the system-side `register_font_path` enforcement): drop
        // the whole face when the cache is full rather than registering a
        // path that subset_font would later reject. `continue` takes us to
        // the next face index, leaving partial results intact.
        {
            let cache_full = match ALLOWED_USER_FONT_PATHS.lock() {
                Ok(mut cache) => {
                    if !cache.contains(&canonical_string)
                        && cache.len() >= MAX_PROVENANCE_CACHE_SIZE
                    {
                        true
                    } else {
                        cache.insert(canonical_string.clone());
                        false
                    }
                }
                Err(_) => true,
            };
            if cache_full {
                log::warn!(
                    "user font provenance cache full ({MAX_PROVENANCE_CACHE_SIZE}); dropping face"
                );
                continue;
            }
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

/// Streaming scan of a user-picked directory (one level deep). Faces are
/// emitted to `emit_batch` in chunks of `SCAN_BATCH_SIZE` (or every
/// `SCAN_BATCH_INTERVAL` when parsing is slower than batching). Returns the
/// total face count on success, or an error if the directory is unreadable.
/// Cancellation via `SCAN_CANCEL_FLAG` returns `Ok(total_so_far)` with all
/// already-emitted batches retained by the caller.
///
/// Does NOT recurse — the `Fonts/` convention is flat by tradition, and
/// limiting recursion keeps the "only files under the picked directory"
/// security reasoning straightforward.
fn scan_directory_inner<F: FnMut(Vec<LocalFontEntry>)>(
    canonical_dir: &Path,
    mut emit_batch: F,
) -> Result<usize, String> {
    let read = fs::read_dir(canonical_dir).map_err(|e| {
        log::warn!("read_dir failed for '{}': {e}", canonical_dir.display());
        "Cannot read directory".to_string()
    })?;

    let mut buffer: Vec<LocalFontEntry> = Vec::new();
    let mut total: usize = 0;
    let mut last_emit = Instant::now();

    for entry in read {
        if SCAN_CANCEL_FLAG.load(Ordering::Relaxed) {
            // Stale-signal guard: if cancel fires before we've parsed any
            // face, treat it as a leftover from a previous scan whose
            // cancel command was still in Tauri's dispatch pool when this
            // scan reset the flag at entry. Clear and continue rather
            // than aborting an empty-result scan, which would surface to
            // the user as a misleading "no fonts found" error.
            if total == 0 {
                SCAN_CANCEL_FLAG.store(false, Ordering::Relaxed);
            } else {
                // Flush any in-flight batch before returning so the
                // frontend sees every face we parsed before cancellation.
                if !buffer.is_empty() {
                    emit_batch(std::mem::take(&mut buffer));
                }
                log::info!(
                    "font scan cancelled in directory '{}' after {} faces",
                    canonical_dir.display(),
                    total
                );
                return Ok(total);
            }
        }

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
        if !canonical.starts_with(canonical_dir) {
            continue;
        }

        for font_entry in parse_local_font_file(&canonical) {
            buffer.push(font_entry);
            total += 1;
            if total > MAX_FONTS_PER_SCAN {
                if !buffer.is_empty() {
                    emit_batch(std::mem::take(&mut buffer));
                }
                return Err(format!(
                    "Too many fonts in directory (> {MAX_FONTS_PER_SCAN}). \
                     The defense-in-depth ceiling was exceeded — split the \
                     folder into smaller batches."
                ));
            }
        }

        if buffer.len() >= SCAN_BATCH_SIZE || last_emit.elapsed() >= SCAN_BATCH_INTERVAL {
            emit_batch(std::mem::take(&mut buffer));
            last_emit = Instant::now();
        }
    }

    if !buffer.is_empty() {
        emit_batch(buffer);
    }

    Ok(total)
}

/// Tauri command wrapping `scan_directory_inner` with a typed progress
/// channel. Frontend creates a `Channel<ScanProgress>`, passes it as
/// `progress`, and receives `Batch` events as faces are parsed.
#[tauri::command]
pub fn scan_font_directory(
    dir: String,
    progress: tauri::ipc::Channel<ScanProgress>,
) -> Result<(), String> {
    if dir.is_empty() || dir.len() > 4096 {
        return Err("Directory path must be 1-4096 characters".to_string());
    }
    if dir.chars().any(|c| c.is_control()) {
        return Err("Directory path contains invalid characters".to_string());
    }

    let canonical_dir = Path::new(&dir).canonicalize().map_err(|e| {
        log::warn!("canonicalize directory failed: {e}");
        "Cannot resolve directory path".to_string()
    })?;
    if !canonical_dir.is_dir() {
        return Err("Not a directory".to_string());
    }

    // Reset the cancel flag at scan start. A cancel intent that arrived
    // between scans (after the previous one finished, before this one
    // started) gets cleared — there's no scan to cancel at that moment,
    // so dropping the stale signal is correct.
    SCAN_CANCEL_FLAG.store(false, Ordering::Relaxed);

    let total = scan_directory_inner(&canonical_dir, |batch| {
        // Channel send fails when the frontend has dropped the channel
        // (e.g., modal closed mid-scan). Subsequent sends are harmless
        // no-ops on the same dropped channel — we keep parsing because
        // the caller may still consume the final Ok return, and the
        // wasted CPU is bounded by MAX_FONTS_PER_SCAN.
        let _ = progress.send(ScanProgress::Batch { entries: batch });
    })?;

    // End-of-stream sentinel; see ScanProgress::Done. MUST be the last
    // send on the Ok path so the frontend's accumulator is full when
    // its Done handler fires. Skipped on the Err path above (the `?`
    // short-circuits before reaching here).
    let _ = progress.send(ScanProgress::Done);

    log::info!(
        "Scanned font directory '{}': {} faces total",
        canonical_dir.display(),
        total
    );
    Ok(())
}

/// Streaming scan of a user-picked file list. Mirrors
/// `scan_directory_inner`, with cancel checks between files and the same
/// batching cadence.
fn scan_files_inner<F: FnMut(Vec<LocalFontEntry>)>(
    paths: Vec<String>,
    mut emit_batch: F,
) -> Result<usize, String> {
    let mut buffer: Vec<LocalFontEntry> = Vec::new();
    let mut total: usize = 0;
    let mut last_emit = Instant::now();

    for p in paths {
        if SCAN_CANCEL_FLAG.load(Ordering::Relaxed) {
            // See scan_directory_inner for the stale-signal rationale.
            if total == 0 {
                SCAN_CANCEL_FLAG.store(false, Ordering::Relaxed);
            } else {
                if !buffer.is_empty() {
                    emit_batch(std::mem::take(&mut buffer));
                }
                log::info!("font scan cancelled in file list after {} faces", total);
                return Ok(total);
            }
        }

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
            buffer.push(font_entry);
            total += 1;
            if total > MAX_FONTS_PER_SCAN {
                if !buffer.is_empty() {
                    emit_batch(std::mem::take(&mut buffer));
                }
                return Err(format!(
                    "Too many font faces across files (> {MAX_FONTS_PER_SCAN})"
                ));
            }
        }

        if buffer.len() >= SCAN_BATCH_SIZE || last_emit.elapsed() >= SCAN_BATCH_INTERVAL {
            emit_batch(std::mem::take(&mut buffer));
            last_emit = Instant::now();
        }
    }

    if !buffer.is_empty() {
        emit_batch(buffer);
    }

    Ok(total)
}

/// Tauri command wrapping `scan_files_inner` with a typed progress channel.
/// Same shape as `scan_font_directory` — frontend supplies the list of
/// paths and a `Channel<ScanProgress>` for incremental delivery.
#[tauri::command]
pub fn scan_font_files(
    paths: Vec<String>,
    progress: tauri::ipc::Channel<ScanProgress>,
) -> Result<(), String> {
    if paths.len() > MAX_INPUT_PATHS {
        return Err(format!(
            "Too many file paths ({}, max {MAX_INPUT_PATHS})",
            paths.len()
        ));
    }

    SCAN_CANCEL_FLAG.store(false, Ordering::Relaxed);

    let total = scan_files_inner(paths, |batch| {
        let _ = progress.send(ScanProgress::Batch { entries: batch });
    })?;

    // See scan_font_directory for why Done is mandatory on the Ok path.
    let _ = progress.send(ScanProgress::Done);

    log::info!("Scanned local font files: {} faces total", total);
    Ok(())
}

/// Cooperative cancel for an active font scan. Sets the global flag; the
/// running scan checks it between files and returns early with all
/// already-emitted batches retained on the frontend. Idempotent — calling
/// when no scan is active just leaves a stale flag, which the next scan
/// resets on entry.
#[tauri::command]
pub fn cancel_font_scan() {
    SCAN_CANCEL_FLAG.store(true, Ordering::Relaxed);
}

/// Register a font path in the provenance cache and return the lookup result.
fn register_font_path(path: &Path, font_index: u32) -> Result<FontLookupResult, String> {
    let canonical = path.canonicalize().map_err(|e| {
        log::warn!("canonicalize font path failed: {e}");
        "Cannot resolve font path".to_string()
    })?;
    let canonical_string = normalize_canonical_path(&canonical.to_string_lossy());
    let mut cache = ALLOWED_FONT_PATHS
        .lock()
        .map_err(|_| "Internal error: font path cache corrupted".to_string())?;
    // Reject only when the cache is full AND we'd be adding a new entry;
    // re-registering an existing path is always cheap. An explicit error
    // beats silently succeeding here then failing later in subset_font.
    if !cache.contains(&canonical_string) && cache.len() >= MAX_PROVENANCE_CACHE_SIZE {
        return Err(format!(
            "Too many registered font paths (> {MAX_PROVENANCE_CACHE_SIZE}). \
             Restart the app to clear the cache."
        ));
    }
    cache.insert(canonical_string.clone());

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

/// Cached, normalized system-fonts dir derived from `SYSTEMROOT` + `\Fonts`.
///
/// Captured eagerly at app startup via `init_system_dirs` (see `lib.rs`) so
/// the value is locked in before any user action could indirectly trigger
/// an `std::env::set_var` call. The cache is `Lazy` rather than a plain
/// `OnceCell` so that any code path which somehow runs before
/// `init_system_dirs` (e.g., a unit test in this module) still gets a
/// well-defined value rather than a panic.
///
/// Defense-in-depth note: a deeper hardening would call
/// `GetSystemWindowsDirectoryW` directly, which reads from a kernel-set
/// process buffer immune to env-var manipulation. We rely on the eager
/// init plus the project's no-`set_var` policy instead, since adding Win32
/// FFI for one path read is not justified at the current threat model
/// (attacker would already need code execution to mutate env vars).
#[cfg(windows)]
static WINDOWS_SYSTEM_FONTS_DIR: Lazy<String> = Lazy::new(|| {
    let sys_root = std::env::var("SYSTEMROOT")
        .unwrap_or_else(|_| "C:\\Windows".to_string())
        .to_lowercase()
        .replace("/", "\\");
    format!("{sys_root}\\fonts")
});

/// Cached, normalized per-user fonts dir (Windows 10 1809+). `None` if
/// `LOCALAPPDATA` was unset at startup. Same caching rationale as
/// `WINDOWS_SYSTEM_FONTS_DIR`.
#[cfg(windows)]
static WINDOWS_USER_FONTS_DIR: Lazy<Option<String>> = Lazy::new(|| {
    std::env::var("LOCALAPPDATA").ok().map(|p| {
        format!(
            "{}\\microsoft\\windows\\fonts",
            p.to_lowercase().replace("/", "\\")
        )
    })
});

/// Force-initialize the cached system-fonts directory paths. Called from
/// `lib.rs::run` at app startup so the env-var snapshot is taken before
/// any user action could indirectly mutate the process environment.
pub fn init_system_dirs() {
    #[cfg(windows)]
    {
        Lazy::force(&WINDOWS_SYSTEM_FONTS_DIR);
        Lazy::force(&WINDOWS_USER_FONTS_DIR);
    }
}

/// Check whether a canonicalized path is under a known system fonts directory.
fn is_in_system_fonts_dir(canonical: &Path) -> bool {
    let canonical_str = normalize_canonical_path(&canonical.to_string_lossy());

    if cfg!(windows) {
        #[cfg(windows)]
        {
            let lower = canonical_str.to_lowercase().replace("/", "\\");
            let under = |dir: &str| path_under_dir(&lower, dir, "\\");

            if under(&WINDOWS_SYSTEM_FONTS_DIR) {
                return true;
            }
            if let Some(user_dir) = WINDOWS_USER_FONTS_DIR.as_ref() {
                if under(user_dir) {
                    return true;
                }
            }
            false
        }
        #[cfg(not(windows))]
        {
            let _ = canonical_str;
            false
        }
    } else if cfg!(target_os = "macos") {
        // APFS is case-insensitive by default; compare in lowercase so symlink
        // chains that surface mixed-case paths still match canonical targets.
        let lower = canonical_str.to_lowercase();
        let under = |dir: &str| path_under_dir(&lower, &dir.to_lowercase(), "/");
        const MAC_DIRS: &[&str] = &[
            "/Library/Fonts",
            "/System/Library/Fonts",
            "/System/Library/AssetsV2",
            // Narrow to Adobe/Fonts — the wider /Library/Application Support
            // tree holds every app's data, not just fonts, so allowing the
            // whole tree weakens the "system font directory" gate.
            "/Library/Application Support/Adobe/Fonts",
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
    let canonical = path.canonicalize().map_err(|e| {
        log::warn!("canonicalize font path failed for '{filename}': {e}");
        "Cannot resolve font path".to_string()
    })?;

    // Primary guard: the path must have been discovered by one of the scan
    // commands (find_system_font OR scan_font_directory / scan_font_files).
    // Arbitrary IPC-supplied paths are rejected.
    let canonical_string = normalize_canonical_path(&canonical.to_string_lossy());
    let is_system = ALLOWED_FONT_PATHS
        .lock()
        .map_err(|_| "Internal error: font path cache corrupted".to_string())?
        .contains(&canonical_string);
    let is_user = ALLOWED_USER_FONT_PATHS
        .lock()
        .map_err(|_| "Internal error: user font path cache corrupted".to_string())?
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
    let metadata = fs::metadata(&canonical).map_err(|e| {
        log::warn!("stat font file failed for '{filename}': {e}");
        "Cannot stat font file".to_string()
    })?;
    if metadata.len() > MAX_FONT_DATA_SIZE {
        return Err(format!(
            "Font file too large ({:.1} MB, max {} MB)",
            metadata.len() as f64 / (1024.0 * 1024.0),
            MAX_FONT_DATA_SIZE / 1024 / 1024
        ));
    }

    let font_data = fs::read(&canonical).map_err(|e| {
        log::warn!("read font file failed for '{filename}': {e}");
        format!("Failed to read font file '{filename}'")
    })?;

    // Post-read size check (TOCTOU mitigation — file could grow between stat and read)
    if font_data.len() as u64 > MAX_FONT_DATA_SIZE {
        return Err(format!(
            "Font file too large after read ({:.1} MB, max {} MB)",
            font_data.len() as f64 / (1024.0 * 1024.0),
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
                    "Subsetting failed and full font too large ({:.1} MB, max {} MB for fallback)",
                    font_data.len() as f64 / (1024.0 * 1024.0),
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

#[cfg(test)]
mod tests {
    use super::*;

    /// `scan_directory_inner` on a non-existent path surfaces the read_dir
    /// error as the user-facing string. The closure is never called.
    #[test]
    fn directory_inner_rejects_missing_dir() {
        let mut emitted: Vec<Vec<LocalFontEntry>> = Vec::new();
        let bogus = Path::new("Z:\\absolutely-not-a-real-directory\\for-testing");
        let result = scan_directory_inner(bogus, |batch| emitted.push(batch));
        assert!(result.is_err());
        assert!(emitted.is_empty());
    }

    /// `scan_files_inner` skips invalid entries (empty / oversized / control
    /// chars) silently and emits nothing when none of the inputs resolve to
    /// a real font file. The streaming contract holds for the empty case:
    /// the closure receives zero batches.
    #[test]
    fn files_inner_skips_invalid_paths_without_emitting() {
        SCAN_CANCEL_FLAG.store(false, Ordering::Relaxed);
        let mut emitted: Vec<Vec<LocalFontEntry>> = Vec::new();
        let bad_paths = vec![
            String::new(),                    // empty
            "x".repeat(5000),                 // oversized
            "has\u{0000}control".to_string(), // control char
            "Z:\\does-not-exist.ttf".to_string(),
        ];
        let total = scan_files_inner(bad_paths, |batch| emitted.push(batch))
            .expect("invalid paths should be skipped, not error");
        assert_eq!(total, 0);
        assert!(emitted.is_empty());
    }

    /// Cancel flag set before scan entry causes an immediate return on the
    /// first iteration. Validates the cancel-poll path without depending on
    /// real font files. Buffer is empty so no batch is emitted.
    #[test]
    fn files_inner_honors_pre_set_cancel_flag() {
        SCAN_CANCEL_FLAG.store(true, Ordering::Relaxed);
        let mut emitted: Vec<Vec<LocalFontEntry>> = Vec::new();
        let total = scan_files_inner(vec!["irrelevant.ttf".to_string()], |batch| {
            emitted.push(batch)
        })
        .expect("cancel returns Ok with partial results");
        assert_eq!(total, 0);
        assert!(emitted.is_empty());
        // Reset for any subsequent tests in the same process.
        SCAN_CANCEL_FLAG.store(false, Ordering::Relaxed);
    }

    /// `cancel_font_scan` flips the global flag. Smoke test — the command
    /// is otherwise a one-liner.
    #[test]
    fn cancel_command_sets_flag() {
        SCAN_CANCEL_FLAG.store(false, Ordering::Relaxed);
        cancel_font_scan();
        assert!(SCAN_CANCEL_FLAG.load(Ordering::Relaxed));
        SCAN_CANCEL_FLAG.store(false, Ordering::Relaxed);
    }
}
