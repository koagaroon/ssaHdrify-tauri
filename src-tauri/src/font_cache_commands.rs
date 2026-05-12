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

use crate::font_cache::{CacheError, FontCache, FontMetadata};
use crate::fonts::entries_to_cache_metadata;

/// Sentinel set true while any cache-mutating IPC command
/// (`rescan_font_cache_drift` or `clear_font_cache`) is mid-flight, so
/// the other one can refuse rather than race. The frontend modal
/// already gates the buttons, but the IPC layer is the actual security
/// boundary — a misbehaving / out-of-band caller could otherwise
/// interleave the two and either (a) have rescan's Phase 3 apply
/// resurrect rows clear just wiped (clear-during-rescan window) or
/// (b) have clear drop+recreate the handle between rescan's Phase 1
/// snapshot and Phase 3 apply (rescan-during-clear window). One CAS-
/// gated flag covers both directions.
static CACHE_MUTATION_IN_PROGRESS: AtomicBool = AtomicBool::new(false);

/// RAII guard that owns the CACHE_MUTATION_IN_PROGRESS flag for the
/// lifetime of one cache-mutating operation. CAS happens inside
/// `try_acquire` and the flag is only set when the guard is
/// constructed — there's no "flag set but guard not yet bound" window
/// for a panic to leak the flag.
struct CacheMutationGuard;

impl CacheMutationGuard {
    fn try_acquire() -> Result<Self, String> {
        CACHE_MUTATION_IN_PROGRESS
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .map(|_| CacheMutationGuard)
            .map_err(|_| "Another cache operation is already in progress".to_string())
    }
}

impl Drop for CacheMutationGuard {
    fn drop(&mut self) {
        CACHE_MUTATION_IN_PROGRESS.store(false, Ordering::Release);
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

/// Read a folder's mtime as Unix seconds, returning None when either
/// the metadata stat or the `modified()` call fails. Single decision
/// point for the rescan flow: `detect_font_cache_drift`, rescan
/// Phase 1's snapshot build, and rescan Phase 3's re-stat all gate on
/// this so a folder is classified consistently regardless of which
/// failure mode it hits (truly gone vs. permission-denied vs. network
/// share offline vs. filesystem with no mtime support).
///
/// Without this symmetry (Codex round-2 N-R2-3 / N-R2-14): Phase 1
/// used `metadata().and_then(modified())` while Phase 3 used only
/// `metadata().is_ok()`. A folder whose metadata stat succeeded but
/// whose `modified()` call failed would be omitted from Phase 1's
/// snapshot (reported as `removed`) yet Phase 3's re-stat would still
/// see `is_ok()` and skip the eviction as "reappeared" — UI claims
/// eviction happened, DB still has the stale rows.
fn try_modified_at(path: &Path) -> Option<i64> {
    let modified = std::fs::metadata(path).ok()?.modified().ok()?;
    Some(
        modified
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0),
    )
}

/// Convenience for callers that want a "best-effort" mtime with a
/// zero fallback (Phase 2's scanned-folder mtime recorded into cache).
/// Composes on `try_modified_at` so all stat semantics agree.
fn stat_mtime_or_zero(path: &Path) -> i64 {
    try_modified_at(path).unwrap_or(0)
}

// `entries_to_cache_metadata` (in `crate::fonts`) is the shared helper —
// `try_record_folder_in_gui_cache` and the rescan-apply path here both
// route through it, and the CLI's `run_refresh_fonts` loop does too.
// The previous local `entries_to_metadata` duplicated that conversion
// AND lacked the per-file mtime dedup needed for TTC files
// (Round 1 A2.N-R1-15 / A2.N-R1-17 / A2.N-R1-18).

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

/// One folder whose Phase-2 scan failed in `rescan_font_cache_drift`.
/// Surfaces what the user needs to know (which folder, why) so the
/// drift modal can show a partial-success state instead of pretending
/// rescan was a clean win.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SkippedFolder {
    /// Cached folder path that triggered the skip.
    pub folder: String,
    /// User-facing reason from the failing scan (already includes the
    /// folder path in some cases; the frontend renders the pair as
    /// `folder — reason`).
    pub reason: String,
}

/// Outcome of a `rescan_font_cache_drift` call.
#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RescanResult {
    /// Count of folders successfully re-scanned and replaced in cache.
    pub modified_rescanned: usize,
    /// Count of folders evicted from cache (includes both the
    /// `report.removed` folders that truly disappeared AND the Phase-2-
    /// skipped folders whose stale rows are dropped — see
    /// `apply_rescan_to_cache`).
    pub removed_evicted: usize,
    /// Folders whose Phase-2 scan failed. Their cache rows were evicted
    /// (so later lookups fall through to fresh sources / system fonts
    /// rather than returning stale data, closing Codex ccac42fe). The
    /// frontend keeps the drift modal in a partial-success state when
    /// this is non-empty so the user knows which folders need attention.
    pub skipped: Vec<SkippedFolder>,
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
        // `try_modified_at` failures (folder gone, permission denied,
        // network share offline, no-mtime FS) all route to "omit from
        // snapshot" → diff_against reports the folder as removed.
        // Permission-denied is a slight false positive (folder exists,
        // we just can't see it) but the user wants to know either way —
        // the cache rows are about-to-be-stale.
        if let Some(mtime) = try_modified_at(Path::new(&folder.folder_path)) {
            snapshot.push((folder.folder_path.clone(), mtime));
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
    // apply resurrect the cleared rows. CacheMutationGuard's CAS-inside-
    // new pattern makes "flag set but no guard yet" structurally
    // impossible; Drop releases on every exit path (Ok / Err / panic-
    // unwind). Same guard also blocks a concurrent rescan if clear is
    // already running.
    let _mutation_guard = CacheMutationGuard::try_acquire()?;

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
            if let Some(mtime) = try_modified_at(Path::new(&folder.folder_path)) {
                snapshot.push((folder.folder_path.clone(), mtime));
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
    //
    // Per-folder error catch (mirrors `run_refresh_fonts` in
    // bin/cli/main.rs:609): one folder hitting MAX_CACHE_POPULATE_FACES
    // or a transient I/O error must not abort the whole rescan — that
    // would let one oversized font pack DoS the user's entire cache
    // refresh. Log WARN with folder context, push to `skipped`, continue.
    // Phase 3's eviction of skipped folders' stale rows closes the
    // silent-stale-cache shortcut Codex ccac42fe flagged.
    let mut scanned: Vec<(String, i64, Vec<FontMetadata>)> =
        Vec::with_capacity(report.modified.len());
    let mut skipped: Vec<SkippedFolder> = Vec::new();
    for folder in &report.modified {
        let folder_path = Path::new(folder);
        let folder_mtime = stat_mtime_or_zero(folder_path);
        match crate::fonts::scan_directory_collecting(folder_path) {
            Ok(entries) => {
                scanned.push((
                    folder.clone(),
                    folder_mtime,
                    entries_to_cache_metadata(&entries),
                ));
            }
            Err(err) => {
                log::warn!("rescan: skipping {folder} — {err}");
                skipped.push(SkippedFolder {
                    folder: folder.clone(),
                    reason: err,
                });
            }
        }
    }

    // Phase 3 — under lock: apply scan results + evict removed and
    // skipped folders. Pure DB work, short hold time. See
    // `apply_rescan_to_cache` for the per-list semantics.
    let (modified_rescanned, removed_evicted) = {
        let mut slot = GUI_FONT_CACHE
            .lock()
            .map_err(|_| "GUI cache mutex poisoned".to_string())?;
        let cache = slot
            .as_mut()
            .ok_or_else(|| "Cache became unavailable between drift detect and apply".to_string())?;
        apply_rescan_to_cache(cache, &scanned, &report.removed, &skipped)?
    };

    Ok(RescanResult {
        modified_rescanned,
        removed_evicted,
        skipped,
    })
}

/// Apply Phase-2 scan outcomes to the cache. Three input lists, three
/// behaviors:
///
/// - `scanned` — folders whose Phase-2 scan succeeded. Each gets its
///   row replaced with the fresh face metadata + new mtime.
/// - `removed` — folders Phase 1 reported as gone. Re-stat first: if
///   the folder is back on disk (another command populated it between
///   Phase 1 and Phase 3, or the user re-added the source), skip the
///   eviction so we don't clobber a concurrent populate. The Phase-2
///   snapshot is older than any such write so replace_folder above is
///   safe without this dance, but eviction isn't.
/// - `skipped` — folders whose Phase-2 scan failed. Their stale cache
///   rows MUST go: without this, a failed-scan folder kept old rows
///   while `rescan_font_cache_drift` still returned `Ok` and the
///   frontend cleared drift state, leaving `lookup_font_family` to
///   serve wrong-font results silently (Codex ccac42fe). Eviction is
///   the structural defense; UI handling of `RescanResult.skipped` is
///   the user-visible defense on top. No re-stat dance — a folder we
///   couldn't scan is a folder we can't trust.
///
/// Returns `(modified_rescanned, removed_evicted)`. `removed_evicted`
/// counts skipped-folder evictions too because they're the same DB
/// operation; the caller's user-facing tally is just "rows we dropped".
fn apply_rescan_to_cache(
    cache: &mut FontCache,
    scanned: &[(String, i64, Vec<FontMetadata>)],
    removed: &[String],
    skipped: &[SkippedFolder],
) -> Result<(usize, usize), String> {
    let mut modified_rescanned = 0usize;
    let mut removed_evicted = 0usize;

    for (folder, folder_mtime, metadata) in scanned {
        cache
            .replace_folder(folder, *folder_mtime, metadata)
            .map_err(|e| format!("replace_folder({folder}): {e}"))?;
        modified_rescanned += 1;
    }
    for folder in removed {
        // Same stat bar as Phase 1 / detect_drift: only treat the folder
        // as "reappeared" when it gives us a real mtime now. A folder
        // whose `metadata().is_ok()` but whose `modified()` still fails
        // matches the same "barely visible" state Phase 1 omitted from
        // the snapshot — proceed with eviction so the UI claim and DB
        // state stay aligned (N-R2-3 / N-R2-14).
        if try_modified_at(Path::new(folder)).is_some() {
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
    for sk in skipped {
        cache
            .remove_folder(&sk.folder)
            .map_err(|e| format!("remove_folder({}): {e}", sk.folder))?;
        removed_evicted += 1;
    }
    Ok((modified_rescanned, removed_evicted))
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
    // Refuse mid-rescan AND block a concurrent rescan from starting
    // while clear is running. Acquiring the guard via CAS (not just
    // a load) closes the rescan-after-load window: without the CAS,
    // a rescan could start between our check and our slot-lock-take,
    // then have Phase 3 apply rows on top of our freshly-recreated
    // cache. The frontend modal already gates the buttons; this is
    // the IPC-layer enforcement that out-of-band callers can't bypass.
    let _mutation_guard = CacheMutationGuard::try_acquire()?;
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
                "GUI cache busy (rescan or clear in progress); skipping populate \
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
    let metadata: Vec<FontMetadata> = entries_to_cache_metadata(entries);
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
            log::warn!(
                "GUI cache busy (rescan or clear in progress); skipping evict for {folder_path}"
            );
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

/// Best-effort eviction of every folder from the GUI cache. Called
/// from `fonts::clear_font_sources` so the persistent cache stays in
/// step with the user's "Clear all sources" intent on the session-DB
/// side (Round 2 N-R2-2). Without this, clear_font_sources wiped the
/// session DB but left `cached_folders` / `cached_fonts` rows intact —
/// the next embed pass resolved a family via the cache to a path
/// whose session-DB provenance had been cleared, and `subset_font`
/// rejected it with "Font path was not discovered by a scan command."
///
/// Same posture as `try_remove_folder_from_gui_cache`:
/// - `try_lock` (not `lock`) so a long rescan doesn't stall the
///   user-visible clear.
/// - Cache unavailable → silent no-op (nothing to evict).
/// - Per-folder `remove_folder` errors log WARN but don't abort the
///   iteration; best-effort.
///
/// A concurrent `try_record_folder_in_gui_cache` that races between
/// `list_folders` and the iteration's `remove_folder` would leave a
/// fresh row behind — acceptable because the racing populate is
/// post-intent ("Clear all" was issued before the new populate).
pub fn try_clear_all_folders_in_gui_cache() {
    let mut slot = match GUI_FONT_CACHE.try_lock() {
        Ok(s) => s,
        Err(std::sync::TryLockError::Poisoned(_)) => {
            log::warn!("GUI cache mutex poisoned; skipping clear-all");
            return;
        }
        Err(std::sync::TryLockError::WouldBlock) => {
            log::warn!("GUI cache busy (rescan or clear in progress); skipping clear-all");
            return;
        }
    };
    let cache = match slot.as_mut() {
        Some(c) => c,
        None => return,
    };
    let folders = match cache.list_folders() {
        Ok(fs) => fs,
        Err(e) => {
            log::warn!("GUI cache list_folders failed during clear-all: {e}");
            return;
        }
    };
    let total = folders.len();
    for f in folders {
        if let Err(e) = cache.remove_folder(&f.folder_path) {
            log::warn!(
                "GUI cache remove_folder({}) during clear-all: {e}",
                f.folder_path
            );
        }
    }
    log::info!("GUI cache clear-all evicted {total} folder rows");
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

// ── Tests ────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    struct TempCacheDir(std::path::PathBuf);

    impl TempCacheDir {
        fn new(name: &str) -> Self {
            let mut dir = std::env::temp_dir();
            dir.push(format!(
                "ssahdrify_font_cache_cmds_test_{}_{}",
                name,
                std::process::id()
            ));
            let _ = fs::remove_dir_all(&dir);
            fs::create_dir_all(&dir).unwrap();
            Self(dir)
        }
    }

    impl Drop for TempCacheDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn temp_cache(name: &str) -> (TempCacheDir, FontCache) {
        let guard = TempCacheDir::new(name);
        let cache_path = guard.0.join("cache.sqlite3");
        let cache = FontCache::open_or_create(&cache_path).expect("open cache");
        (guard, cache)
    }

    #[test]
    fn apply_rescan_evicts_skipped_folder_rows() {
        // Codex ccac42fe regression: a Phase-2 scan failure must drop
        // the stale rows so later lookup_font_family can't short-circuit
        // through them. Without this fix, `skipped` was silent and the
        // command returned Ok; the rows lingered for the rest of the
        // session.
        let (_guard, mut cache) = temp_cache("skipped_evict");
        cache
            .replace_folder("/bogus/skipped/folder", 12345, &[])
            .unwrap();
        assert!(
            cache
                .list_folders()
                .unwrap()
                .iter()
                .any(|f| f.folder_path == "/bogus/skipped/folder"),
            "seed row missing"
        );

        let skipped = vec![SkippedFolder {
            folder: "/bogus/skipped/folder".to_string(),
            reason: "Not a directory".to_string(),
        }];
        let (modified, evicted) = apply_rescan_to_cache(&mut cache, &[], &[], &skipped).unwrap();
        assert_eq!(modified, 0);
        assert_eq!(evicted, 1);
        assert!(
            cache
                .list_folders()
                .unwrap()
                .iter()
                .all(|f| f.folder_path != "/bogus/skipped/folder"),
            "stale row still present after skip eviction"
        );
    }

    #[test]
    fn apply_rescan_replaces_modified_and_leaves_others() {
        let (_guard, mut cache) = temp_cache("replace_keep");
        cache.replace_folder("/folder/a", 100, &[]).unwrap();
        cache.replace_folder("/folder/b", 200, &[]).unwrap();

        let scanned = vec![("/folder/a".to_string(), 999, vec![])];
        let (modified, evicted) = apply_rescan_to_cache(&mut cache, &scanned, &[], &[]).unwrap();
        assert_eq!(modified, 1);
        assert_eq!(evicted, 0);

        let folders = cache.list_folders().unwrap();
        let a = folders
            .iter()
            .find(|f| f.folder_path == "/folder/a")
            .expect("a present");
        assert_eq!(a.folder_mtime, 999, "a's mtime not updated");
        assert!(
            folders.iter().any(|f| f.folder_path == "/folder/b"),
            "b should not be touched"
        );
    }

    #[test]
    fn apply_rescan_does_not_evict_removed_that_reappeared() {
        // Existing re-stat dance: a folder reported as removed in
        // Phase 1 may have been re-populated by a concurrent command
        // by the time Phase 3 runs. Eviction must skip when the
        // folder is back on disk.
        let (guard, mut cache) = temp_cache("removed_reappeared");
        let real_path = guard.0.to_string_lossy().to_string();
        cache.replace_folder(&real_path, 100, &[]).unwrap();

        let removed = vec![real_path.clone()];
        let (_, evicted) = apply_rescan_to_cache(&mut cache, &[], &removed, &[]).unwrap();
        assert_eq!(evicted, 0, "reappeared folder should be left alone");
        assert!(
            cache
                .list_folders()
                .unwrap()
                .iter()
                .any(|f| f.folder_path == real_path),
            "row dropped despite re-stat"
        );
    }

    #[test]
    fn try_modified_at_returns_none_for_missing_path() {
        // Symmetry contract: Phase 1 / Phase 3 / detect_drift all
        // gate on this helper, so a missing path must consistently
        // produce "not statable" (None) across every site.
        let mut dir = std::env::temp_dir();
        dir.push(format!(
            "ssahdrify_try_modified_missing_{}",
            std::process::id()
        ));
        // Don't create dir — we want a definitely-absent path.
        assert!(try_modified_at(&dir).is_none());
    }

    #[test]
    fn try_modified_at_returns_some_for_existing_folder() {
        let (_guard, _) = temp_cache("try_modified_exists");
        let mut dir = std::env::temp_dir();
        dir.push(format!(
            "ssahdrify_try_modified_present_{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let mtime = try_modified_at(&dir);
        assert!(mtime.is_some(), "existing folder should yield a Some mtime");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn apply_rescan_evicts_removed_that_no_longer_resolves() {
        // Pinning the N-R2-3 fix: a folder that doesn't pass the same
        // stat bar Phase 1 used (no real mtime now) must still be
        // evicted — Phase 3 must NOT short-circuit to "reappeared".
        let (_guard, mut cache) = temp_cache("removed_actually_gone");
        let bogus = "/bogus/definitely-not-a-real-folder/round-2";
        cache.replace_folder(bogus, 100, &[]).unwrap();
        let removed = vec![bogus.to_string()];
        let (_, evicted) = apply_rescan_to_cache(&mut cache, &[], &removed, &[]).unwrap();
        assert_eq!(evicted, 1);
        assert!(cache
            .list_folders()
            .unwrap()
            .iter()
            .all(|f| f.folder_path != bogus));
    }
}
