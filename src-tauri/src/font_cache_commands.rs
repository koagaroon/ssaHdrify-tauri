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
///
/// `pub(crate)` since Round 4 Codex finding 3: `clear_font_sources`
/// in `fonts.rs` now acquires the guard upfront so its session-DB
/// clear and persistent-cache clear commit atomically. The earlier
/// scheme (helper acquires guard internally) silently no-op'd cache
/// clear when a concurrent rescan held the guard — leaving session-
/// DB cleared but cache rows behind. Atomic acquire + pass-by-
/// reference to `clear_all_folders_in_gui_cache_locked` ties the
/// two halves to the same guard token.
pub(crate) struct CacheMutationGuard;

impl CacheMutationGuard {
    pub(crate) fn try_acquire() -> Result<Self, String> {
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

/// One-shot migration of the legacy GUI font cache file from a prior
/// Tauri-managed `app_data_dir` (typically the bundle-identifier path
/// `%APPDATA%/com.koagaroon.ssahdrify/`) to the unified `ssahdrify/`
/// data dir introduced in Round 11 W11.4b. Best-effort: every failure
/// is logged at WARN and the function returns — the worst case is a
/// stale ~4 KB orphan at the legacy location, and the GUI continues
/// with a fresh empty cache (no different from a first-run user).
///
/// Migrates the main `gui_font_cache.sqlite3` file plus any SQLite
/// sidecars (`-journal` / `-wal` / `-shm`) so a dirty-close state from
/// a previous v1.4.x run carries over intact. Skips silently when:
///   - legacy_dir == new_dir (already on unified path)
///   - legacy main file is missing (nothing to migrate)
///   - new main file already exists (user already on new path —
///     overwriting would clobber their current state)
///
/// `fs::rename` is atomic on same-filesystem moves (both paths live
/// under the same per-user data root, so this holds in practice).
pub fn migrate_legacy_gui_cache(legacy_dir: &Path, new_dir: &Path) {
    if legacy_dir == new_dir {
        return;
    }
    let legacy_main = legacy_dir.join(GUI_CACHE_FILE_NAME);
    let new_main = new_dir.join(GUI_CACHE_FILE_NAME);
    if !legacy_main.exists() {
        return;
    }
    if new_main.exists() {
        log::debug!(
            "Legacy GUI cache exists at {} but new location {} already has one; \
             leaving legacy in place as orphan.",
            legacy_main.display(),
            new_main.display()
        );
        return;
    }
    if let Err(e) = std::fs::create_dir_all(new_dir) {
        log::warn!(
            "GUI cache migration: cannot create new dir {}: {e}. \
             Skipping migration; cache will start fresh at new location.",
            new_dir.display()
        );
        return;
    }
    // R12 A-R12-1: refuse migration if the legacy main file (or any
    // sidecar) is a reparse point. Codebase posture across every other
    // fs op site (safe_io, encoding, fonts) is "refuse on reparse";
    // this migration was the lone outlier. Even though rename is
    // generally atomic and on Windows doesn't fall back to follow-link
    // copy-then-delete the way Linux does, posture consistency matters
    // — and if a future stdlib change altered rename semantics the
    // codebase should already be defended. Skip the whole migration on
    // a suspicious legacy path; cache starts fresh at new location.
    if crate::util::is_reparse_point(&legacy_main) {
        log::warn!(
            "GUI cache migration: legacy main file {} is a reparse point. \
             Refusing to migrate; cache will start fresh at new location.",
            legacy_main.display()
        );
        return;
    }
    // Migrate main file first; sidecars follow only if main rename
    // succeeded. If sidecar rename fails after main succeeded, the
    // sidecar is left at the legacy location — SQLite at the new
    // location will treat the missing sidecar as a clean state, which
    // is the right fallback (dirty-state recovery is best-effort).
    match std::fs::rename(&legacy_main, &new_main) {
        Ok(()) => {
            log::info!(
                "GUI font cache migrated: {} → {}",
                legacy_main.display(),
                new_main.display()
            );
        }
        Err(e) => {
            log::warn!(
                "GUI cache migration: rename {} → {} failed: {e}. \
                 Leaving legacy file in place; new location starts fresh.",
                legacy_main.display(),
                new_main.display()
            );
            return;
        }
    }
    for suffix in ["-journal", "-wal", "-shm"] {
        let mut legacy_side = legacy_main.clone().into_os_string();
        legacy_side.push(suffix);
        let legacy_side = PathBuf::from(legacy_side);
        if !legacy_side.exists() {
            continue;
        }
        // Same reparse-point posture as the main file above. Sidecar
        // is best-effort, so a suspicious sidecar just gets left at
        // the legacy location.
        if crate::util::is_reparse_point(&legacy_side) {
            log::warn!(
                "GUI cache migration: legacy sidecar {} is a reparse point. \
                 Leaving in place; SQLite treats new location as clean-close.",
                legacy_side.display()
            );
            continue;
        }
        let mut new_side = new_main.clone().into_os_string();
        new_side.push(suffix);
        let new_side = PathBuf::from(new_side);
        if let Err(e) = std::fs::rename(&legacy_side, &new_side) {
            log::warn!(
                "GUI cache migration: sidecar rename {} → {} failed: {e}. \
                 SQLite will treat the new location as a clean-close state.",
                legacy_side.display(),
                new_side.display()
            );
        }
    }
}

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

// Re-export so test code can reach it via this module without a
// crate-path qualifier. The canonical home is `font_cache.rs` so the
// CLI binary can use the same helper — Round 3 N-R3-15 consolidation.
use crate::font_cache::try_modified_at;

/// Convenience for callers that want a "best-effort" mtime with a
/// Returns the folder's modified-at unix-seconds, OR `None` when stat
/// fails. The previous form silently substituted 0 on failure, which
/// marked the cache row with the Unix epoch; the next drift-detect
/// then compared 0 vs the live folder's real positive mtime and
/// reported `modified`, prompting another doomed rescan that hit the
/// same failure mode — a loop bug (N-R5-RUSTGUI-03). Callers MUST
/// handle `None` by skipping the populate/replace for that folder so
/// no row gets a bogus epoch stamp.
fn stat_mtime(path: &Path) -> Option<i64> {
    try_modified_at(path)
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

/// One folder that didn't make it through a clean rescan.
/// `kind` distinguishes Phase-2 scan failure (couldn't read the folder)
/// from Phase-3 apply failure (couldn't write the cache row); the
/// frontend renders both kinds in the same partial-success block.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SkippedFolder {
    /// Cached folder path that triggered the skip. Field name
    /// `folder` (not `folder_path`) is intentional and paired with
    /// TS `FontCacheSkippedFolder.folder` in `tauri-api.ts`; the
    /// shorter form jars against `FolderRecord.folder_path` in
    /// `font_cache.rs` but the trade is "shorter UI-facing field
    /// name vs internal-storage descriptor" — keep the TS pairing.
    pub folder: String,
    /// User-facing reason — the error message from the failing op
    /// (already includes the folder path in some cases; the frontend
    /// renders the pair as `folder — reason`).
    pub reason: String,
    /// Which phase failed. ScanFailed: filesystem walk / name-table
    /// read errored; cache rows for the folder were evicted as a
    /// fall-through-to-fresh guard. ApplyFailed: SQLite write errored
    /// mid-rescan; the cache row state for this folder is whatever
    /// the previous successful operation left (Round 3 N-R3-2 — was
    /// previously a hard Err return that wiped the partial-success
    /// signal for ALL folders).
    pub kind: SkipKind,
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum SkipKind {
    ScanFailed,
    ApplyFailed,
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
    /// Folders that didn't apply cleanly — both Phase-2 scan failures
    /// (ScanFailed) and Phase-3 apply failures (ApplyFailed). The
    /// frontend keeps the drift modal in a partial-success state when
    /// this is non-empty so the user knows which folders need attention
    /// (Codex ccac42fe + Round 3 N-R3-2).
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
    // schema_mismatch ⇔ path published but handle absent. Two states
    // leave the slot None: (1) `init_gui_font_cache` on
    // SchemaVersionMismatch (see `init_gui_font_cache` in this same
    // file for the symmetric slot-cleanup that publishes this state),
    // and (2) `clear_font_cache` transiently between dropping the old
    // handle and re-creating. Clear holds the slot lock throughout
    // step (2), so `path.exists()` is false in that window and
    // `schema_mismatch` stays false — only state (1) actually surfaces
    // as schema_mismatch=true.
    //
    // Round 6 Wave 6.5 #22: switched to `try_exists()` to distinguish
    // NotFound from permission-denied. Pre-W6.5 the comment here
    // recommended this switch (chmod-000 cache misclassifying as "no
    // file"); the loop is now closed. `try_exists()` returns Err on
    // genuine IO failure (we propagate via `?`) and Ok(false) only on
    // confirmed NotFound. The Ok(true) branch is the same "path
    // present but handle absent → schema mismatch" signal as before.
    let schema_mismatch = !available
        && path
            .try_exists()
            .map_err(|e| format!("Failed to stat cache path: {e}"))?;
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
///
/// Does NOT take `CacheMutationGuard` (Round 2 N-R2-16): this is a
/// read-only command and the slot mutex is held throughout the
/// `cached_folders` enumeration + `diff_against` call, so a parallel
/// `clear_font_cache` can't drop the slot mid-iteration. The guard is
/// load-bearing only when a command both reads AND writes the cache
/// across multiple lock acquisitions (rescan's Phase 1 / Phase 3
/// split; clear's drop + recreate); detect_drift does neither.
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
    // `bin/cli/main.rs`): one folder hitting MAX_CACHE_POPULATE_FACES
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
        // None → skip the populate (see `stat_mtime` doc): without
        // this, a transient stat failure would write an epoch-zero
        // mtime that drift-detect re-flags forever.
        let Some(folder_mtime) = stat_mtime(folder_path) else {
            log::warn!("rescan: skipping {folder} — folder mtime unreadable");
            skipped.push(SkippedFolder {
                folder: folder.clone(),
                reason: "folder mtime unreadable".to_string(),
                kind: SkipKind::ScanFailed,
            });
            continue;
        };
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
                    kind: SkipKind::ScanFailed,
                });
            }
        }
    }

    // Phase 3 — under lock: apply scan results + evict removed and
    // skipped folders. Pure DB work, short hold time. Per-folder
    // ApplyFailed errors aggregate into `skipped` alongside the
    // Phase-2 ScanFailed entries; the helper no longer short-circuits
    // on the first SQLite error so an N-th folder failure doesn't
    // hide the success of folders 0..N (Round 3 N-R3-2).
    let (modified_rescanned, removed_evicted) = {
        let mut slot = GUI_FONT_CACHE
            .lock()
            .map_err(|_| "GUI cache mutex poisoned".to_string())?;
        let cache = slot
            .as_mut()
            .ok_or_else(|| "Cache became unavailable between drift detect and apply".to_string())?;
        apply_rescan_to_cache(cache, &scanned, &report.removed, &mut skipped)
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
///
/// Per-folder ApplyFailed errors push into `skipped` rather than
/// short-circuiting via `?` (Round 3 N-R3-2). Each `replace_folder` /
/// `remove_folder` is its own SQLite transaction, so committed rows
/// 0..N stay committed even if row N+1 fails — propagating the
/// failure as a hard Err to the frontend would discard that
/// information and prompt the user to re-run the rescan, doing the
/// same work twice. Aggregating into `skipped` lets the modal show
/// "N folders refreshed, M failed to write — see list" partial-
/// success state.
fn apply_rescan_to_cache(
    cache: &mut FontCache,
    scanned: &[(String, i64, Vec<FontMetadata>)],
    removed: &[String],
    skipped: &mut Vec<SkippedFolder>,
) -> (usize, usize) {
    let mut modified_rescanned = 0usize;
    let mut removed_evicted = 0usize;

    for (folder, folder_mtime, metadata) in scanned {
        match cache.replace_folder(folder, *folder_mtime, metadata) {
            Ok(()) => modified_rescanned += 1,
            Err(e) => {
                let reason = format!("replace_folder failed: {e}");
                log::warn!("apply_rescan_to_cache {folder} — {reason}");
                skipped.push(SkippedFolder {
                    folder: folder.clone(),
                    reason,
                    kind: SkipKind::ApplyFailed,
                });
            }
        }
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
        match cache.remove_folder(folder) {
            Ok(()) => removed_evicted += 1,
            Err(e) => {
                let reason = format!("remove_folder failed: {e}");
                log::warn!("apply_rescan_to_cache {folder} — {reason}");
                skipped.push(SkippedFolder {
                    folder: folder.clone(),
                    reason,
                    kind: SkipKind::ApplyFailed,
                });
            }
        }
    }
    // Evict the Phase-2 scan failures. Iterate over a snapshot of
    // current ScanFailed entries so we don't mutate `skipped` while
    // borrowing it — also lets ApplyFailed entries from a Phase-2
    // eviction failure get appended without re-evicting them.
    let scan_failed_folders: Vec<String> = skipped
        .iter()
        .filter(|s| s.kind == SkipKind::ScanFailed)
        .map(|s| s.folder.clone())
        .collect();
    for folder in scan_failed_folders {
        match cache.remove_folder(&folder) {
            Ok(()) => removed_evicted += 1,
            Err(e) => {
                let reason = format!("remove_folder (scan-failed eviction) failed: {e}");
                log::warn!("apply_rescan_to_cache {folder} — {reason}");
                skipped.push(SkippedFolder {
                    folder: folder.clone(),
                    reason,
                    kind: SkipKind::ApplyFailed,
                });
            }
        }
    }
    (modified_rescanned, removed_evicted)
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

    // Two-lock pattern (W6.7 Round 6): the slot lock is taken,
    // released, then re-taken — bracketed by the CacheMutationGuard
    // above. The interleaved drop is intentional so the SQLite file
    // handle is released before `std::fs::remove_file` runs (Windows
    // file locks prevent removing an open file). During the brief
    // window between the two lock acquisitions, observers see
    // `slot.is_some() == false` AND the file may or may not exist
    // depending on which remove_file calls have completed — `open_font_cache`
    // would derive `schema_mismatch = !available && path.try_exists()`,
    // which is the same wire-shape it would emit during legitimate
    // schema-mismatch state. User-visible consequence in the
    // sub-millisecond window: the cache status reads as "unavailable"
    // (correct) and possibly transiently "schema_mismatch=true"
    // (acceptable — the next `open_font_cache` poll after recreate
    // settles to the correct state). The CacheMutationGuard above
    // serializes the whole clear vs any concurrent rescan, so the
    // window is locked against the operation that would matter most.
    //
    // Drop handle first so SQLite releases the file lock before we
    // try to delete. Holding the slot lock through the close is fine —
    // it's the SQLite-level file handle drop we care about.
    //
    // Round 11 W11.4 (A4-R11-02): also clear provenance HERE, inside
    // the same lock scope that sets slot=None. Pre-R11 the
    // `clear_cache_provenance()` call sat at the END of this function
    // — after the fresh empty cache had already been published to
    // the slot. Between `*slot = Some(fresh)` and the trailing
    // `clear_cache_provenance()`, subset_font on another thread (which
    // locks ALLOWED_CACHE_FONT_PATHS only, NOT the slot) could pass
    // its provenance check against a stale (path, face_index) entry
    // registered by an earlier `lookup_family` hit — even though the
    // newly-published cache no longer referenced that path. Hoisting
    // the clear into the shutdown scope means the moment the old slot
    // dies, the old trust set dies with it: any subset_font call
    // arriving after this point either sees `slot = None` (cache
    // unavailable, no work) or, once `*slot = Some(fresh)` lands
    // below, the fresh empty cache + already-cleared trust set.
    // Lock order slot → provenance matches the existing GUI
    // lookup_font_family path (slot → register_cache_provenance).
    //
    // Round 10 N-R10-002: symmetric with `clear_font_sources` — the
    // user's "fresh slate" signal must drop in-process provenance rows
    // alongside the SQLite rebuild. ALLOWED_FONT_PATHS (system fonts)
    // stays — system discovery is cache-independent.
    {
        let mut slot = GUI_FONT_CACHE
            .lock()
            .map_err(|_| "GUI cache mutex poisoned".to_string())?;
        *slot = None;
        crate::fonts::clear_cache_provenance();
    }

    // Best-effort cleanup of main file + journal sidecars. Same suffix
    // set as init_user_font_db so a partially-cleared state from an
    // earlier crash gets fully wiped here.
    //
    // R12 A-R12-2: reparse-point check before remove_file. On Windows
    // `fs::remove_file` of a symlink removes the link, not the target,
    // so the security delta is small — but the codebase posture
    // everywhere else (safe_io, encoding, fonts) refuses on reparse,
    // and the cache file under per-user AppData should never be a
    // symlink in the normal case. Skip the remove on suspicion;
    // re-create below will then fail and surface the issue.
    for suffix in ["", "-journal", "-wal", "-shm"] {
        let mut p = path.clone().into_os_string();
        p.push(suffix);
        let p = PathBuf::from(p);
        if crate::util::is_reparse_point(&p) {
            log::warn!(
                "clear_font_cache: refusing to remove reparse-point {}; leaving in place.",
                p.display()
            );
            continue;
        }
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
    // None → skip the populate so the cache doesn't acquire an
    // epoch-zero row that drift-detect re-flags forever (see
    // `stat_mtime` doc).
    let Some(folder_mtime) = stat_mtime(folder_path) else {
        log::warn!(
            "GUI cache populate skipped (folder mtime unreadable): {}",
            folder_path.display()
        );
        return;
    };
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

/// Eviction of every folder from the GUI cache, called from
/// `fonts::clear_font_sources` so the persistent cache stays in step
/// with the user's "Clear all sources" intent on the session-DB side
/// (Round 2 N-R2-2). Without this, clear_font_sources wiped the
/// session DB but left `cached_folders` / `cached_fonts` rows intact —
/// the next embed pass resolved a family via the cache to a path
/// whose session-DB provenance had been cleared, and `subset_font`
/// rejected it with "Font path was not discovered by a scan command."
///
/// CALLER MUST already hold `CacheMutationGuard` (Round 4 Codex
/// finding 3): `clear_font_sources` clears the session DB AND evicts
/// the persistent cache as one atomic mutation, so the guard wraps
/// both steps. Re-acquiring inside this fn would either deadlock
/// (reentrancy-unsafe CAS) or fail and silently skip the eviction.
///
/// (A prior `try_clear_all_folders_in_gui_cache` wrapper that
/// acquired the guard itself was removed in Wave 5.3b N-R5-RUSTGUI-02
/// — every caller already held the guard for atomic-mutation reasons,
/// the wrapper was dead.)
///
/// Mutation-guard + slot-lock interaction (Round 3 N-R3-3 / A-R3-1):
/// - The `&CacheMutationGuard` arg proves the caller blocked /
///   blocks `rescan_font_cache_drift`. Without it, a clear landing
///   between rescan's Phase 2 (long scan outside slot lock) and
///   Phase 3 (apply scan results inside slot lock) would wipe rows
///   just before Phase 3 re-inserts the freshly scanned ones — end
///   state: cache holds rows whose session-DB provenance was just
///   cleared, UI claim and DB state disagree.
/// - `try_lock` on the SLOT mutex stays — that protects against
///   handle drop in the clear_font_cache recovery path.
/// - Cache unavailable → silent no-op (nothing to evict).
/// - Per-folder `remove_folder` errors log WARN but don't abort the
///   iteration; best-effort.
///
/// A concurrent `try_record_folder_in_gui_cache` that races between
/// `list_folders` and the iteration's `remove_folder` would leave a
/// fresh row behind — acceptable because the racing populate is
/// post-intent ("Clear all" was issued before the new populate).
pub(crate) fn clear_all_folders_in_gui_cache_locked(_guard: &CacheMutationGuard) {
    let mut slot = match GUI_FONT_CACHE.try_lock() {
        Ok(s) => s,
        Err(std::sync::TryLockError::Poisoned(_)) => {
            log::warn!("GUI cache mutex poisoned; skipping clear-all");
            return;
        }
        Err(std::sync::TryLockError::WouldBlock) => {
            log::warn!("GUI cache slot busy; skipping clear-all");
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
    // Shared `validate_font_family` (Round 3 N-R3-20): bounds family
    // length + rejects control characters before the SQL bind, same
    // as find_system_font and resolve_user_font.
    crate::util::validate_font_family(&family)?;
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
    // Round 6 Wave 6.3 D1: register the cache hit in the in-process
    // provenance set so `subset_font`'s gate accepts the returned
    // path. Without this, the GUI's lookup tier 2 (embed-time cache
    // hit) goes through the IPC roundtrip and then trips the gate
    // as "Font path was not discovered by a scan command". See
    // `register_cache_provenance` for the threat-model rationale.
    //
    // Round 10 N-R10-003: registration failure → treat as a cache
    // miss (`Ok(None)`) rather than returning the unsafe path.
    // `register_cache_provenance` calls `validate_ipc_path`, so a
    // hostile cache row carrying BiDi / control / `..` segments
    // surfaces here as Err. Previously this branch logged WARN but
    // still returned `Ok(Some(result))` — the unscrubbed path then
    // flowed into IPC response → frontend display surfaces (status
    // panel, log lines) BEFORE `subset_font`'s re-validation could
    // reject it (P1b disclosure surface). Returning None forces the
    // caller into the next lookup tier (system fonts) and keeps the
    // crafted path off the wire.
    if let Some(ref r) = result {
        if let Err(e) = crate::fonts::register_cache_provenance(r) {
            log::warn!(
                "Font '{family}' cache lookup hit a path that failed provenance validation; \
                 treating as miss: {e}"
            );
            return Ok(None);
        }
    }
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
            // PID + nanos (Round 6 Wave 6.5 #21) — `font_cache.rs`'s
            // equivalent TempCacheDir uses the same shape. PID alone
            // collides when two tests with the same `name` argument
            // run in the same process (parallel test threads or a
            // future test that reuses the same fixture name).
            let nanos = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.subsec_nanos())
                .unwrap_or(0);
            let mut dir = std::env::temp_dir();
            dir.push(format!(
                "ssahdrify_font_cache_cmds_test_{}_{}_{}",
                name,
                std::process::id(),
                nanos
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

        let mut skipped = vec![SkippedFolder {
            folder: "/bogus/skipped/folder".to_string(),
            reason: "Not a directory".to_string(),
            kind: SkipKind::ScanFailed,
        }];
        let (modified, evicted) = apply_rescan_to_cache(&mut cache, &[], &[], &mut skipped);
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
        // No new ApplyFailed entries when the eviction succeeded.
        assert!(skipped.iter().all(|s| s.kind == SkipKind::ScanFailed));
    }

    #[test]
    fn apply_rescan_replaces_modified_and_leaves_others() {
        let (_guard, mut cache) = temp_cache("replace_keep");
        cache.replace_folder("/folder/a", 100, &[]).unwrap();
        cache.replace_folder("/folder/b", 200, &[]).unwrap();

        let scanned = vec![("/folder/a".to_string(), 999, vec![])];
        let mut skipped: Vec<SkippedFolder> = Vec::new();
        let (modified, evicted) = apply_rescan_to_cache(&mut cache, &scanned, &[], &mut skipped);
        assert_eq!(modified, 1);
        assert_eq!(evicted, 0);
        assert!(skipped.is_empty(), "no errors expected");

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
        let mut skipped: Vec<SkippedFolder> = Vec::new();
        let (_, evicted) = apply_rescan_to_cache(&mut cache, &[], &removed, &mut skipped);
        assert_eq!(evicted, 0, "reappeared folder should be left alone");
        assert!(skipped.is_empty());
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
        let mut skipped: Vec<SkippedFolder> = Vec::new();
        let (_, evicted) = apply_rescan_to_cache(&mut cache, &[], &removed, &mut skipped);
        assert_eq!(evicted, 1);
        assert!(cache
            .list_folders()
            .unwrap()
            .iter()
            .all(|f| f.folder_path != bogus));
        assert!(skipped.is_empty());
    }

    #[test]
    fn apply_rescan_preserves_pre_existing_failed_entry_alongside_success() {
        // Round 3 N-R3-2 partial coverage: the Phase-3 ApplyFailed
        // push path (Err arm of `cache.replace_folder` /
        // `remove_folder`) requires real SQLite-write injection to
        // exercise and is not covered by this test. What we DO pin
        // here: a pre-existing `SkippedFolder { kind: ApplyFailed }`
        // in the input vec survives alongside successful operations —
        // i.e., the helper doesn't accidentally wipe or rewrite the
        // input vec. Real mid-loop failure coverage deferred until
        // the repo gains a FontCache fault-injection seam (Round 4
        // N-R4-05 — original title "continues_after_per_folder_failure"
        // over-promised).
        let (_guard, mut cache) = temp_cache("preserves_pre_existing_apply_failed");
        cache.replace_folder("/folder/x", 100, &[]).unwrap();

        let scanned = vec![("/folder/x".to_string(), 999, vec![])];
        let mut skipped = vec![SkippedFolder {
            folder: "/already/failed".to_string(),
            reason: "previously failed".to_string(),
            kind: SkipKind::ApplyFailed,
        }];
        let (modified, _evicted) = apply_rescan_to_cache(&mut cache, &scanned, &[], &mut skipped);
        assert_eq!(modified, 1, "successful folder still counted");
        assert!(
            skipped
                .iter()
                .any(|s| s.folder == "/already/failed" && s.kind == SkipKind::ApplyFailed),
            "pre-existing ApplyFailed entry preserved"
        );
    }

    // ── Round 11 W11.4b (R10 N-R10-036): migrate_legacy_gui_cache ──

    fn make_legacy_pair(name: &str) -> (TempCacheDir, TempCacheDir) {
        // Two disjoint tempdirs simulate the legacy (Tauri-given) and
        // new (unified) data dirs. Each has its own cleanup guard so
        // a panic mid-test still removes both.
        (
            TempCacheDir::new(&format!("{name}_legacy")),
            TempCacheDir::new(&format!("{name}_new")),
        )
    }

    #[test]
    fn migrate_legacy_gui_cache_moves_main_file_and_sidecars() {
        let (legacy, new) = make_legacy_pair("happy");
        let legacy_main = legacy.0.join(GUI_CACHE_FILE_NAME);
        fs::write(&legacy_main, b"sqlite-bytes").unwrap();
        let legacy_wal = {
            let mut p = legacy_main.clone().into_os_string();
            p.push("-wal");
            PathBuf::from(p)
        };
        fs::write(&legacy_wal, b"wal-bytes").unwrap();

        migrate_legacy_gui_cache(&legacy.0, &new.0);

        let new_main = new.0.join(GUI_CACHE_FILE_NAME);
        let new_wal = {
            let mut p = new_main.clone().into_os_string();
            p.push("-wal");
            PathBuf::from(p)
        };
        assert!(new_main.exists(), "main file should move to new location");
        assert!(new_wal.exists(), "sidecar should follow main");
        assert!(!legacy_main.exists(), "main file should leave legacy");
        assert!(!legacy_wal.exists(), "sidecar should leave legacy");
        assert_eq!(fs::read(&new_main).unwrap(), b"sqlite-bytes");
    }

    #[test]
    fn migrate_legacy_gui_cache_skips_when_new_already_exists() {
        // Don't clobber: if the user already has data at the new path,
        // leave it alone and let the legacy file stay as orphan.
        let (legacy, new) = make_legacy_pair("no_clobber");
        let legacy_main = legacy.0.join(GUI_CACHE_FILE_NAME);
        let new_main = new.0.join(GUI_CACHE_FILE_NAME);
        fs::write(&legacy_main, b"legacy-bytes").unwrap();
        fs::write(&new_main, b"new-bytes").unwrap();

        migrate_legacy_gui_cache(&legacy.0, &new.0);

        assert!(legacy_main.exists(), "legacy left in place");
        assert_eq!(
            fs::read(&new_main).unwrap(),
            b"new-bytes",
            "new file must NOT be overwritten"
        );
    }

    #[test]
    fn migrate_legacy_gui_cache_skips_when_legacy_missing() {
        // No-op when nothing to migrate (fresh-install user case).
        let (legacy, new) = make_legacy_pair("nothing_to_do");
        let new_main = new.0.join(GUI_CACHE_FILE_NAME);
        migrate_legacy_gui_cache(&legacy.0, &new.0);
        assert!(!new_main.exists(), "no new file synthesized");
    }

    #[test]
    fn migrate_legacy_gui_cache_skips_when_paths_equal() {
        // Safety: callers shouldn't pass the same path on both sides,
        // but if they do, the helper must not attempt a self-rename.
        let dir = TempCacheDir::new("same_path");
        let main = dir.0.join(GUI_CACHE_FILE_NAME);
        fs::write(&main, b"x").unwrap();
        migrate_legacy_gui_cache(&dir.0, &dir.0);
        assert!(main.exists(), "self-rename must not destroy the file");
    }
}
