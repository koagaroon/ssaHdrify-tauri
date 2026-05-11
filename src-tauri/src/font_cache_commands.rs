//! Tauri command wrappers for the persistent font cache.
//!
//! `font_cache.rs` itself stays Tauri-free so the CLI binary can use it
//! without pulling in the GUI's IPC layer. This module is the GUI-only
//! IPC surface: a static `Mutex<Option<FontCache>>` initialized once
//! during Tauri setup, plus the five commands the React drift modal +
//! embed-time lookup tier call into.
//!
//! See `docs/architecture/ssahdrify_cli_design.md` § "v1.4.1 stable
//! 后续用户反馈" #5 for the locked design (5 commands + 3-button modal +
//! lookup tier ordering).

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

use once_cell::sync::Lazy;
use serde::Serialize;

use crate::font_cache::{CacheError, FamilyKey, FontCache, FontMetadata};

/// Sentinel set true while `rescan_font_cache_drift` is mid-flight
/// (Phase 1 → Phase 3) so `clear_font_cache` can refuse rather than
/// race the rescan's apply phase. The frontend modal already gates
/// the buttons, but the IPC layer is the actual security boundary —
/// a misbehaving / out-of-band caller could otherwise interleave the
/// two and resurrect just-cleared data via Phase 3's apply.
static RESCAN_IN_PROGRESS: AtomicBool = AtomicBool::new(false);

/// RAII guard that owns the RESCAN_IN_PROGRESS flag for the lifetime
/// of one rescan. CAS happens inside `try_acquire` and the flag is
/// only set when the guard is constructed — there's no "flag set but
/// guard not yet bound" window for a panic to leak the flag.
struct RescanGuard;

impl RescanGuard {
    fn try_acquire() -> Result<Self, String> {
        RESCAN_IN_PROGRESS
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .map(|_| RescanGuard)
            .map_err(|_| "Another rescan is already in progress".to_string())
    }
}

impl Drop for RescanGuard {
    fn drop(&mut self) {
        RESCAN_IN_PROGRESS.store(false, Ordering::Release);
    }
}

// Note: `crate::font_cache` also defines its own `FontLookupResult`
// (font_path / face_index, i32). Not imported here — IPC commands
// return `crate::fonts::FontLookupResult` (path / index, u32) so the
// frontend uses one TS type across all three resolution tiers.

/// File name placed under Tauri's `app_data_dir`. The CLI uses
/// `cli_font_cache.sqlite3` (sibling); per-binary names prevent SQLite
/// lock contention when both binaries run at once.
const GUI_CACHE_FILE_NAME: &str = "gui_font_cache.sqlite3";

/// Live cache handle, populated by `init_gui_font_cache` during Tauri
/// setup and consumed by the five commands. `None` when init hit a
/// schema mismatch or other recoverable error — in that state the
/// frontend's drift modal renders the "rebuild required" path so the
/// user can clear and re-init explicitly.
static GUI_FONT_CACHE: Lazy<Mutex<Option<FontCache>>> = Lazy::new(|| Mutex::new(None));

/// Cache file path published separately from the live handle so
/// `clear_font_cache` can drop the connection AND wipe the file even
/// when `GUI_FONT_CACHE` is `None` (schema-mismatch recovery path).
static GUI_FONT_CACHE_PATH: Lazy<Mutex<Option<PathBuf>>> = Lazy::new(|| Mutex::new(None));

/// Initialize the GUI font cache. Called once from Tauri's `setup`
/// closure with the same `app_data_dir` used by `init_user_font_db`.
///
/// Failure modes split:
/// - I/O / open errors are returned as `Err` so the rfd MessageBox in
///   `lib.rs::run` can surface them (mirrors the session DB's posture).
/// - `SchemaVersionMismatch` is logged at WARN and returns `Ok(())` —
///   the user can still launch the app, the cache just stays
///   unavailable until they hit "Clear cache" in the drift modal. This
///   matches the locked "no auto-migrate" decision: never silently
///   delete a cache file the user might want to inspect.
pub fn init_gui_font_cache(app_data_dir: &Path) -> Result<(), String> {
    std::fs::create_dir_all(app_data_dir).map_err(|e| {
        format!(
            "Cannot create app data dir '{}': {e}",
            app_data_dir.display()
        )
    })?;
    let cache_path = app_data_dir.join(GUI_CACHE_FILE_NAME);

    // Publish the path before attempting open so `clear_font_cache`
    // works in the schema-mismatch recovery path (which leaves the
    // handle slot empty but still needs to know which file to wipe).
    {
        let mut path_slot = GUI_FONT_CACHE_PATH
            .lock()
            .map_err(|_| "GUI cache path mutex poisoned".to_string())?;
        *path_slot = Some(cache_path.clone());
    }

    match FontCache::open_or_create(&cache_path) {
        Ok(cache) => {
            let mut slot = GUI_FONT_CACHE
                .lock()
                .map_err(|_| "GUI cache mutex poisoned".to_string())?;
            *slot = Some(cache);
            Ok(())
        }
        Err(CacheError::SchemaVersionMismatch { found, expected }) => {
            log::warn!(
                "GUI font cache at {} has schema version {found}; expected {expected}. \
                 Cache unavailable until user clears via drift modal.",
                cache_path.display()
            );
            Ok(())
        }
        Err(e) => {
            // Clear the path slot too so `open_font_cache`'s
            // `schema_mismatch = !available && path.exists()` derivation
            // doesn't false-report schema_mismatch for a non-schema I/O
            // failure (which would route the user to "rebuild" when
            // recreate also fails).
            if let Ok(mut path_slot) = GUI_FONT_CACHE_PATH.lock() {
                *path_slot = None;
            }
            Err(format!(
                "Cannot open GUI font cache at {}: {e}",
                cache_path.display()
            ))
        }
    }
}

// ---- Helpers -----------------------------------------------------------

fn stat_mtime_or_zero(path: &Path) -> i64 {
    std::fs::metadata(path)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// Convert scan output (`fonts::LocalFontEntry`) to cache rows
/// (`font_cache::FontMetadata`). Mirrors the conversion in CLI
/// `run_refresh_fonts`; deliberately not extracted to a shared helper
/// because the two sites are the only callers and extracting drags
/// `LocalFontEntry` into `font_cache.rs` (currently independent).
fn entries_to_metadata(entries: Vec<crate::fonts::LocalFontEntry>) -> Vec<FontMetadata> {
    entries
        .into_iter()
        .map(|e| {
            let file_mtime = stat_mtime_or_zero(Path::new(&e.path));
            FontMetadata {
                file_path: e.path,
                // u64 → i64 saturating conversion. A font file >
                // i64::MAX bytes is impossible in practice (8.4 EB)
                // but try_from + saturate matches the broader cast-
                // discipline pattern in the codebase and avoids the
                // implicit `as` truncation if reality ever shifts.
                file_size: i64::try_from(e.size_bytes).unwrap_or(i64::MAX),
                file_mtime,
                face_index: e.index as i32,
                family_keys: e
                    .families
                    .into_iter()
                    .map(|family_name| FamilyKey {
                        family_name,
                        bold: e.bold,
                        italic: e.italic,
                    })
                    .collect(),
            }
        })
        .collect()
}

// ---- IPC types ---------------------------------------------------------

/// Status of the font cache after init / on demand. Returned by
/// `open_font_cache` so the frontend can decide between "ready",
/// "needs rebuild", or "missing" without a separate probe command.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CacheStatus {
    /// True if a working cache handle is loaded and queries will work.
    pub available: bool,
    /// True if the file on disk has a schema version different from
    /// this build's `SCHEMA_VERSION`. Mutually exclusive with `available`
    /// (mismatch leaves the handle `None`).
    pub schema_mismatch: bool,
    /// Absolute path to the cache file on disk. Always populated once
    /// init has run, even if the handle is `None`.
    pub path: String,
}

/// Drift report exposed over IPC. Mirrors `font_cache::DriftReport`
/// with serde derived; `added` is always empty in the GUI flow because
/// the GUI doesn't walk source roots from this command (matches the
/// CLI's `check_cache_drift` semantic: drift = filesystem changes to
/// folders the cache already tracks).
#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DriftReport {
    pub added: Vec<String>,
    pub modified: Vec<String>,
    pub removed: Vec<String>,
}

/// Outcome of a `rescan_font_cache_drift` call.
#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RescanResult {
    /// Count of folders successfully re-scanned and replaced in cache.
    pub modified_rescanned: usize,
    /// Count of folders evicted from cache (no longer on disk).
    pub removed_evicted: usize,
}

// ---- Tauri commands ----------------------------------------------------

/// Report the current cache status. Useful for the launch-time check
/// (frontend asks "is cache ready?" before calling detect_drift) and
/// for re-checking after `clear_font_cache`.
#[tauri::command]
pub fn open_font_cache() -> Result<CacheStatus, String> {
    let path = GUI_FONT_CACHE_PATH
        .lock()
        .map_err(|_| "GUI cache path mutex poisoned".to_string())?
        .clone()
        .ok_or_else(|| "Cache path not initialized; setup did not run".to_string())?;
    let available = GUI_FONT_CACHE
        .lock()
        .map_err(|_| "GUI cache mutex poisoned".to_string())?
        .is_some();
    // schema_mismatch ⇔ path published but handle absent. init_gui_font_cache
    // only leaves the slot empty in that specific recovery state.
    let schema_mismatch = !available && path.exists();
    Ok(CacheStatus {
        available,
        schema_mismatch,
        path: path.display().to_string(),
    })
}

/// Detect drift between the cached folder set and the live filesystem.
/// For each cached folder, stat the directory; folders that no longer
/// exist (or that we can't stat) are reported as `removed`, folders
/// whose mtime differs from the cached value are reported as `modified`.
/// `added` is always empty — the GUI doesn't walk source roots here
/// (mirrors the CLI's `check_cache_drift` decision).
///
/// Returns an empty report when the cache is unavailable (init failed
/// or schema mismatch); the frontend treats empty + unavailable as
/// "no modal needed" while `open_font_cache` separately surfaces the
/// schema-mismatch state for the rebuild path.
#[tauri::command]
pub fn detect_font_cache_drift() -> Result<DriftReport, String> {
    let slot = GUI_FONT_CACHE
        .lock()
        .map_err(|_| "GUI cache mutex poisoned".to_string())?;
    let cache = match slot.as_ref() {
        Some(c) => c,
        None => return Ok(DriftReport::default()),
    };
    let cached_folders = cache
        .list_folders()
        .map_err(|e| format!("list cached folders: {e}"))?;
    let mut snapshot: Vec<(String, i64)> = Vec::with_capacity(cached_folders.len());
    for folder in &cached_folders {
        let folder_path = Path::new(&folder.folder_path);
        // metadata() / modified() can fail for many reasons (folder
        // gone, permission denied, network share offline). All route
        // to "omit from snapshot" → diff_against reports the folder
        // as removed. Permission-denied is a slight false positive
        // (folder exists, we just can't see it) but the user wants
        // to know either way — the cache rows are about-to-be-stale.
        if let Ok(meta) = std::fs::metadata(folder_path) {
            if let Ok(modified) = meta.modified() {
                let mtime = modified
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_secs() as i64)
                    .unwrap_or(0);
                snapshot.push((folder.folder_path.clone(), mtime));
            }
        }
    }
    let report = cache
        .diff_against(&snapshot)
        .map_err(|e| format!("compute drift: {e}"))?;
    Ok(DriftReport {
        added: report.added,
        modified: report.modified,
        removed: report.removed,
    })
}

/// Bring the cache back into sync with the filesystem: re-scan every
/// folder reported as `modified`, evict every folder reported as
/// `removed`. Computes drift fresh inside the same lock so the report
/// can't get stale between detect and rescan. `added` is empty by
/// design (see `detect_font_cache_drift`) so this command does not
/// scan new folders — those come into the cache via the existing
/// FontSourceModal scan flow (which best-effort writes to the cache
/// after each successful scan via `try_record_folder_in_gui_cache`)
/// or the CLI's `refresh-fonts` subcommand.
#[tauri::command]
pub fn rescan_font_cache_drift() -> Result<RescanResult, String> {
    // Block parallel `clear_font_cache` between Phase 1 and Phase 3 so
    // Clear can't drop+recreate the cache mid-rescan and have Phase 3's
    // apply resurrect the cleared rows. RescanGuard's CAS-inside-new
    // pattern makes "flag set but no guard yet" structurally impossible;
    // Drop releases on every exit path (Ok / Err / panic-unwind).
    let _rescan_guard = RescanGuard::try_acquire()?;

    // Phase 1 — under lock: list cached folders + compute the
    // filesystem-snapshot drift report. list_folders, the per-folder
    // metadata stat, and diff_against are all cheap (small in-DB sets +
    // one stat per cached folder).
    let report = {
        let slot = GUI_FONT_CACHE
            .lock()
            .map_err(|_| "GUI cache mutex poisoned".to_string())?;
        let cache = slot.as_ref().ok_or_else(|| {
            "Cache not available (init failed or schema mismatch). \
             Use clear_font_cache to rebuild."
                .to_string()
        })?;
        let cached_folders = cache
            .list_folders()
            .map_err(|e| format!("list cached folders: {e}"))?;
        let mut snapshot: Vec<(String, i64)> = Vec::with_capacity(cached_folders.len());
        for folder in &cached_folders {
            if let Ok(meta) = std::fs::metadata(&folder.folder_path) {
                if let Ok(modified) = meta.modified() {
                    let mtime = modified
                        .duration_since(std::time::UNIX_EPOCH)
                        .map(|d| d.as_secs() as i64)
                        .unwrap_or(0);
                    snapshot.push((folder.folder_path.clone(), mtime));
                }
            }
        }
        cache
            .diff_against(&snapshot)
            .map_err(|e| format!("compute drift: {e}"))?
    };

    // Phase 2 — outside lock: scan each modified folder. This is the
    // long step (full directory walk + name-table reads); concurrent
    // lookup_font_family / try_record_folder_in_gui_cache calls run in
    // parallel instead of waiting on a multi-second to multi-minute
    // scan that used to be inside the lock.
    let mut scanned: Vec<(String, i64, Vec<FontMetadata>)> =
        Vec::with_capacity(report.modified.len());
    for folder in &report.modified {
        let folder_path = Path::new(folder);
        let folder_mtime = stat_mtime_or_zero(folder_path);
        let entries = crate::fonts::scan_directory_collecting(folder_path)?;
        scanned.push((folder.clone(), folder_mtime, entries_to_metadata(entries)));
    }

    // Phase 3 — under lock: apply the scan results + evict removed
    // folders. Pure DB work, short hold time.
    //
    // Removed-list re-stat: Phase 1's `report.removed` is a snapshot
    // from before Phase 2's I/O. Between Phase 1 and Phase 3, another
    // command can have called `try_record_folder_in_gui_cache` for X,
    // freshly populating it (or the user can have re-added the source
    // through the source modal). Blindly applying `remove_folder("X")`
    // would clobber that fresh write. Re-stat each `removed` folder
    // here; if it now exists on disk, skip — the drift report was
    // stale. Modified-list doesn't need this dance: Phase 2's scan
    // captured a snapshot strictly newer than any concurrent populate,
    // so replace_folder's idempotent overwrite is safe.
    let mut modified_rescanned = 0usize;
    let mut removed_evicted = 0usize;
    {
        let mut slot = GUI_FONT_CACHE
            .lock()
            .map_err(|_| "GUI cache mutex poisoned".to_string())?;
        let cache = slot
            .as_mut()
            .ok_or_else(|| "Cache became unavailable between drift detect and apply".to_string())?;
        for (folder, folder_mtime, metadata) in &scanned {
            cache
                .replace_folder(folder, *folder_mtime, metadata)
                .map_err(|e| format!("replace_folder({folder}): {e}"))?;
            modified_rescanned += 1;
        }
        for folder in &report.removed {
            if std::fs::metadata(folder).is_ok() {
                log::info!(
                    "Skipping cache eviction for {folder}: folder reappeared between drift detect and apply"
                );
                continue;
            }
            cache
                .remove_folder(folder)
                .map_err(|e| format!("remove_folder({folder}): {e}"))?;
            removed_evicted += 1;
        }
    }

    Ok(RescanResult {
        modified_rescanned,
        removed_evicted,
    })
}

/// Drop the SQLite connection, delete the cache file (and its WAL
/// sidecars), then re-create a fresh empty cache. After this command
/// the handle is ready again with version-current schema; subsequent
/// scans (CLI `refresh-fonts`, future GUI populate path) repopulate.
///
/// Used as the "Clear cache" button in the drift modal AND as the
/// rebuild path when `open_font_cache` reports `schema_mismatch`.
#[tauri::command]
pub fn clear_font_cache() -> Result<(), String> {
    // Refuse mid-rescan: rescan_font_cache_drift's Phase 2 runs
    // outside the cache mutex; if Clear succeeds during that window,
    // Phase 3's apply re-creates rows the user just asked to wipe.
    // The frontend modal already gates the buttons; this is the IPC-
    // layer enforcement that out-of-band callers can't bypass.
    if RESCAN_IN_PROGRESS.load(Ordering::Acquire) {
        return Err("Cannot clear cache: rescan in progress".to_string());
    }
    let path = GUI_FONT_CACHE_PATH
        .lock()
        .map_err(|_| "GUI cache path mutex poisoned".to_string())?
        .clone()
        .ok_or_else(|| "Cache path not initialized; setup did not run".to_string())?;

    // Drop handle first so SQLite releases the file lock before we
    // try to delete. Holding the slot lock through the close is fine —
    // it's the SQLite-level file handle drop we care about.
    {
        let mut slot = GUI_FONT_CACHE
            .lock()
            .map_err(|_| "GUI cache mutex poisoned".to_string())?;
        *slot = None;
    }

    // Best-effort cleanup of main file + journal sidecars. Same suffix
    // set as init_user_font_db so a partially-cleared state from an
    // earlier crash gets fully wiped here.
    for suffix in ["", "-journal", "-wal", "-shm"] {
        let mut p = path.clone().into_os_string();
        p.push(suffix);
        let p = PathBuf::from(p);
        match std::fs::remove_file(&p) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => log::warn!("clear_font_cache: removing {} failed: {e}", p.display()),
        }
    }

    let fresh = FontCache::open_or_create(&path)
        .map_err(|e| format!("re-create cache at {}: {e}", path.display()))?;
    let mut slot = GUI_FONT_CACHE
        .lock()
        .map_err(|_| "GUI cache mutex poisoned".to_string())?;
    *slot = Some(fresh);
    Ok(())
}

/// Best-effort populate of the GUI cache from a directory scan that has
/// just succeeded against the session DB. Called from
/// `fonts::scan_font_directory` after its transaction commits — the
/// scan is the user-visible operation, this is a piggyback for next
/// launch's lookup tier and drift detection.
///
/// Failures here MUST NOT propagate: the scan was already a success
/// from the user's perspective; cache is a perf overlay, not part of
/// the contract. Errors log at WARN with the folder context.
///
/// Called only for directory-scan sources (`kind="dir"` in the JS
/// FontSource model). File-list scans (`kind="files"`) have no folder
/// anchor for the cache's drift model and stay session-only.
pub fn try_record_folder_in_gui_cache(
    folder_path: &Path,
    entries: &[crate::fonts::LocalFontEntry],
) {
    // try_lock (not lock) so a long-running rescan_font_cache_drift in
    // another command doesn't make this scan's user-visible completion
    // wait on cache write. If the lock is contended, skip with a WARN —
    // the user just doesn't get cache acceleration for this folder
    // until next time the folder is scanned (or next launch's drift
    // detection picks up the difference).
    let mut slot = match GUI_FONT_CACHE.try_lock() {
        Ok(s) => s,
        Err(std::sync::TryLockError::Poisoned(_)) => {
            log::warn!("GUI cache mutex poisoned; skipping populate");
            return;
        }
        Err(std::sync::TryLockError::WouldBlock) => {
            log::warn!(
                "GUI cache busy (rescan in progress); skipping populate \
                 for {} this scan — will populate on next add",
                folder_path.display()
            );
            return;
        }
    };
    let cache = match slot.as_mut() {
        Some(c) => c,
        None => {
            log::warn!(
                "GUI cache unavailable (init failed or schema mismatch); \
                 skipping populate for {}",
                folder_path.display()
            );
            return;
        }
    };
    let folder_mtime = stat_mtime_or_zero(folder_path);
    let metadata: Vec<FontMetadata> = entries
        .iter()
        .map(|e| FontMetadata {
            file_path: e.path.clone(),
            // Saturating u64 → i64 — matches entries_to_metadata's
            // pattern; impossible in practice (8.4 EB font file) but
            // keeps cast discipline consistent.
            file_size: i64::try_from(e.size_bytes).unwrap_or(i64::MAX),
            file_mtime: stat_mtime_or_zero(Path::new(&e.path)),
            face_index: e.index as i32,
            family_keys: e
                .families
                .iter()
                .map(|family_name| FamilyKey {
                    family_name: family_name.clone(),
                    bold: e.bold,
                    italic: e.italic,
                })
                .collect(),
        })
        .collect();
    // Normalize the canonical path BEFORE storing it as the cache key.
    // `font_faces.path` (session DB, source for the eviction key in
    // try_remove_folder_from_gui_cache's caller) is normalized at scan
    // time via fonts::normalize_canonical_path — without matching that
    // here, the Windows extended-prefix form `\\?\C:\...` written here
    // would never match the prefix-stripped form supplied to evict,
    // and remove_font_source's cache eviction would silently no-op
    // every dir-mode source.
    let folder_path_str = crate::fonts::normalize_canonical_path(&folder_path.to_string_lossy());
    let face_count = metadata.len();
    match cache.replace_folder(&folder_path_str, folder_mtime, &metadata) {
        Ok(()) => {
            log::info!(
                "GUI cache populated: {} ({} faces)",
                folder_path_str,
                face_count
            );
        }
        Err(e) => {
            log::warn!("GUI cache populate for {folder_path_str} failed: {e}");
        }
    }
}

/// Best-effort eviction of a folder from the GUI cache. Called from
/// `fonts::remove_font_source` after the session DB delete commits —
/// the user's "remove this source" action is the user-visible
/// operation; cache eviction is a side-effect that keeps cache state
/// aligned with user intent ("I no longer want this folder").
///
/// Same posture as `try_record_folder_in_gui_cache`:
/// - `try_lock` (not `lock`) so a long rescan doesn't stall the
///   user-visible remove.
/// - Cache unavailable → silent no-op (nothing to evict).
/// - `cache.remove_folder` on a folder the cache doesn't track still
///   runs the 3-statement transaction but each DELETE matches zero
///   rows. Acceptable for files-mode sources whose parent folder
///   happens to coincide with a tracked dir — the wasted txn round-
///   trip is negligible vs. adding a pre-check.
///
/// Pairs with `try_record_folder_in_gui_cache` (auto-populate on scan)
/// for symmetric add/remove cache hygiene.
pub fn try_remove_folder_from_gui_cache(folder_path: &str) {
    let mut slot = match GUI_FONT_CACHE.try_lock() {
        Ok(s) => s,
        Err(std::sync::TryLockError::Poisoned(_)) => {
            log::warn!("GUI cache mutex poisoned; skipping evict for {folder_path}");
            return;
        }
        Err(std::sync::TryLockError::WouldBlock) => {
            log::warn!("GUI cache busy (rescan in progress); skipping evict for {folder_path}");
            return;
        }
    };
    let cache = match slot.as_mut() {
        Some(c) => c,
        None => return,
    };
    match cache.remove_folder(folder_path) {
        Ok(()) => log::info!("GUI cache evicted folder: {folder_path}"),
        Err(e) => log::warn!("GUI cache evict {folder_path} failed: {e}"),
    }
}

/// Look up a (family_name, bold, italic) tuple in the cache. Returns
/// `Some(FontLookupResult)` matching the existing `find_system_font`
/// shape (path + index) so the frontend can use one TS type across the
/// session-DB / cache / system-font tiers; returns `None` when the
/// family isn't in the cache OR when the cache is unavailable.
///
/// Result type intentionally aliases `crate::fonts::FontLookupResult`
/// (already serde-derived for IPC) instead of wrapping cache's
/// internal `FontLookupResult` (different field names: font_path/face_index
/// vs path/index, different int types).
#[tauri::command]
pub fn lookup_font_family(
    family: String,
    bold: bool,
    italic: bool,
) -> Result<Option<crate::fonts::FontLookupResult>, String> {
    // IPC boundary validation matching find_system_font / resolve_user_font:
    // bound family length and reject control characters before the SQL
    // bind. Without this, a misbehaving frontend could pin a multi-MB
    // string in a transient allocation per query.
    if family.is_empty() {
        return Err("Font family name is empty".to_string());
    }
    if family.chars().count() > 256 {
        return Err("Font family name exceeds 256 characters".to_string());
    }
    if family.chars().any(|c| c.is_control()) {
        return Err("Font family name contains control characters".to_string());
    }
    let slot = GUI_FONT_CACHE
        .lock()
        .map_err(|_| "GUI cache mutex poisoned".to_string())?;
    let cache = match slot.as_ref() {
        Some(c) => c,
        None => return Ok(None),
    };
    let result = cache
        .lookup_family(&family, bold, italic)
        .map_err(|e| format!("lookup_family: {e}"))?;
    Ok(result.map(|r| crate::fonts::FontLookupResult {
        path: r.font_path,
        index: r.face_index as u32,
    }))
}
