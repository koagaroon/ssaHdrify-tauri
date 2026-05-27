//! Tauri command wrappers for the persistent font cache.
//!
//! `font_cache.rs` itself stays Tauri-free so the CLI binary can use it
//! without pulling in the GUI's IPC layer. This module is the GUI-only
//! IPC surface: a static `Mutex<Option<FontCache>>` initialized once
//! during Tauri setup, plus the five commands the React drift modal +
//! embed-time lookup tier call into.
//!
//! The GUI command surface stays deliberately small: cache status,
//! drift detection, drift rescan, clear/rebuild, and lookup. The
//! frontend owns presentation; this layer owns cache mutation ordering.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
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
/// `pub(crate)` because `clear_font_sources` in `fonts.rs` acquires
/// the guard upfront so its session-DB clear and persistent-cache
/// clear commit atomically. The earlier
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

/// Outcome of attempting to acquire the GUI font-cache slot. Returned by
/// [`with_cache_slot`] so each caller decides what to do about a busy
/// or unavailable cache without sharing a forced policy.
///
/// extracted from three near-identical try_lock + as_mut
/// blocks across `try_record_folder_in_gui_cache`,
/// `try_remove_folder_from_gui_cache`, and
/// `clear_all_folders_in_gui_cache_locked`. The shared shape was the
/// reviewer's actual concern; the three sites disagree on WouldBlock
/// log level (DEBUG for the auto-populate / auto-evict best-effort
/// helpers; WARN for clear-all because the guard is supposed to
/// prevent contention there) and on whether None-slot deserves a WARN,
/// so the helper isolates only the locking + cache-handle access and
/// returns this enum for each call site to dispatch on.
pub(crate) enum CacheSlotOutcome<R> {
    /// Closure ran on a live `FontCache`.
    Ran(R),
    /// Slot lock was contended (`TryLockError::WouldBlock`). The helper
    /// did NOT log; the caller logs at the level appropriate to its
    /// scope (DEBUG = success-of-degradation, WARN = guard-discipline
    /// regression).
    Busy,
    /// Mutex was poisoned. Already logged at WARN by the helper.
    Poisoned,
    /// Slot was `None` (init failed or schema mismatch). The helper
    /// did NOT log; the caller decides whether the situation warrants
    /// a WARN message or is acceptable as silent no-op.
    Unavailable,
}

/// Run `f` against the GUI font-cache handle if available, returning a
/// [`CacheSlotOutcome`] that distinguishes "ran" from each non-running
/// shape so the call site can log appropriately. The Poisoned arm logs
/// here because that's the same message every caller wants; every
/// other arm is the caller's policy.
fn with_cache_slot<F, R>(f: F) -> CacheSlotOutcome<R>
where
    F: FnOnce(&mut FontCache) -> R,
{
    let mut slot = match GUI_FONT_CACHE.try_lock() {
        Ok(s) => s,
        Err(std::sync::TryLockError::Poisoned(_)) => {
            log::warn!("GUI cache mutex poisoned");
            return CacheSlotOutcome::Poisoned;
        }
        Err(std::sync::TryLockError::WouldBlock) => return CacheSlotOutcome::Busy,
    };
    match slot.as_mut() {
        Some(c) => CacheSlotOutcome::Ran(f(c)),
        None => CacheSlotOutcome::Unavailable,
    }
}

/// Cache file path published separately from the live handle so
/// `clear_font_cache` can drop the connection AND wipe the file even
/// when `GUI_FONT_CACHE` is `None` (schema-mismatch recovery path).
static GUI_FONT_CACHE_PATH: Lazy<Mutex<Option<PathBuf>>> = Lazy::new(|| Mutex::new(None));

/// Monotonic generation counter bumped every time `clear_font_cache`
/// publishes a fresh `FontCache` into `GUI_FONT_CACHE`. The counter is
/// the synchronization primitive that makes `detect_font_cache_drift`'s
/// Phase 1 / Phase 3 lock split safe against a concurrent
/// clear-and-republish: Phase 1 captures the generation under the slot
/// lock alongside the folder snapshot; Phase 3 re-acquires the slot
/// lock and verifies the generation matches before calling
/// `diff_against`. A mismatch means the cache was rebuilt between
/// phases, so the Phase-1 snapshot is stale and the only correct
/// answer is `DriftReport::default()`. The bump MUST live inside the
/// same slot-lock scope as `*slot = Some(fresh)` so detect can't
/// observe the new handle without also observing the new generation.
static GUI_FONT_CACHE_GENERATION: AtomicU64 = AtomicU64::new(0);

/// One-shot migration of the legacy GUI font cache file from a prior
/// Tauri-managed `app_data_dir` (typically the bundle-identifier path
/// `%APPDATA%/com.koagaroon.ssahdrify/`) to the unified `ssahdrify/`
/// data dir introduced in the unified dir migration. Best-effort: every failure
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
    // PathBuf::eq is byte-level equality, not canonical.
    // On the current shape (`<data_dir>/com.koagaroon.ssahdrify` vs
    // `<data_dir>/ssahdrify`) the two strings are distinct, so this
    // check correctly fires "different dirs → proceed with migration".
    // The trap would be if the two ever resolved to the same physical
    // path through symlinks / case-folding / 8.3 short names — in
    // which case migration would copy a file onto itself. Today's
    // setup precludes that: legacy uses the Tauri bundle identifier
    // (compile-time string) and new uses `ssahdrify` (also compile-
    // time), and both are joined onto the same `data_dir()` base. If
    // the unified path ever changes to share a segment with the
    // bundle ID, swap this for a canonicalize-then-compare.
    if legacy_dir == new_dir {
        return;
    }
    let legacy_main = legacy_dir.join(GUI_CACHE_FILE_NAME);
    let new_main = new_dir.join(GUI_CACHE_FILE_NAME);
    if !legacy_main.exists() {
        return;
    }
    // probe the unified-path destination with
    // `symlink_metadata` (lstat-equivalent), NOT `Path::exists()`
    // which follows symlinks. A planted symlink at new_main pointing
    // at some sensitive target would otherwise make exists() return
    // true and trigger the "orphan" early-return — but
    // FontCache::open_or_create called later from init_gui_font_cache
    // would happily follow the symlink and land the SQLite file on
    // the attacker's chosen target. lstat-based check sees the
    // symlink itself; we treat the path as "occupied" (correct
    // outcome) without ever following it.
    if std::fs::symlink_metadata(&new_main).is_ok() {
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
    // Refuse migration if the legacy main file is a reparse point.
    // The check is positioned immediately
    // before fs::rename (there is no intermediate validation work in
    // this function between the check and the rename); on Windows
    // fs::rename of a same-volume reparse point moves the link
    // itself, but a CROSS-VOLUME rename falls back to copy-then-
    // delete which DOES follow the link. The reparse-check + rename
    // remain stat-then-act on Windows — an attacker who can swap the
    // file between syscalls still wins — but the window is single-
    // syscall narrow and the codebase posture (safe_io.rs:266 / :313
    // also stat-then-act with similar narrowness) is "accept the
    // residual TOCTOU under P1a, close everything wider". A true
    // race-free fix would need NtSetInformationFile on a handle
    // opened with FILE_FLAG_OPEN_REPARSE_POINT; not worth the Win32
    // interop for the single-user threat model.
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
    // Sidecar loop is independently best-effort: each sidecar's
    // rename can succeed or fail on its own, so a partial-failure
    // shape like "main + -journal moved, -wal stuck at legacy" is
    // possible. SQLite at the new location
    // recovers from a missing sidecar as a clean-close state, which
    // is correct fallback semantics; the orphan sidecar at the
    // legacy location is invisible to the running app and gets
    // cleaned up the next time Tauri's bundle-namespaced dir is
    // pruned (or by the user inspecting the legacy dir). The cost
    // of a precise cleanup-on-failure rollback (move sidecars back
    // to legacy if any later sidecar fails) isn't worth it — the
    // orphan is harmless and "clean app launch" is the priority.
    for suffix in ["-journal", "-wal", "-shm"] {
        let mut legacy_side = legacy_main.clone().into_os_string();
        legacy_side.push(suffix);
        let legacy_side = PathBuf::from(legacy_side);
        // check reparse-point BEFORE exists(). The
        // original order (exists then reparse) was fragile — a
        // dangling symlink returns false from exists() so the loop
        // continues without reaching the reparse check; benign here
        // (loop body never runs), but a future refactor that dropped
        // the exists() short-circuit would reintroduce a window. By
        // running is_reparse_point first (which uses symlink_metadata
        // and so handles dangling symlinks), the loop's structural
        // invariant becomes "reparse never reaches fs::rename"
        // regardless of subsequent re-arrangement.
        if crate::util::is_reparse_point(&legacy_side) {
            log::warn!(
                "GUI cache migration: legacy sidecar {} is a reparse point. \
                 Leaving in place; SQLite treats new location as clean-close.",
                legacy_side.display()
            );
            continue;
        }
        if !legacy_side.exists() {
            continue;
        }
        let mut new_side = new_main.clone().into_os_string();
        new_side.push(suffix);
        let new_side = PathBuf::from(new_side);
        // also check the DESTINATION for reparse-point
        // before rename. The legacy side check above guards against
        // following a planted symlink at the source; the destination
        // check guards against the (rarer but real) shape where the
        // new unified `app_data_dir` already contains a reparse-pointed
        // sidecar entry that fs::rename would resolve through. SQLite
        // would later open WAL at the resolved target. Cost: one
        // symlink_metadata syscall per sidecar (3 sidecars iterated
        // here — `-journal` / `-wal` / `-shm`; the main file is
        // handled above the loop and gets its own pair of reparse
        // checks). At most once per app launch on the migration
        // path. Bounded P1a per single-user-desktop AppData reparse
        // class, but symmetry with the source-side check is cheap
        // enough to keep.
        if crate::util::is_reparse_point(&new_side) {
            log::warn!(
                "GUI cache migration: new-location sidecar {} is a reparse point. \
                 Leaving legacy sidecar in place; SQLite treats new location as clean-close.",
                new_side.display()
            );
            continue;
        }
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
    // `app_data_dir` here is resolved via the caller in `lib.rs`
    // which passes `font_cache::unified_app_data_dir()` — chain is
    // `std::env::var("APPDATA")` (Windows) / `$XDG_DATA_HOME` (POSIX)
    // / `~/Library/Application Support` (macOS) per the unified dir
    // migration; an alternative resolution exists via Tauri's
    // `app.path().app_data_dir()` (the `$DATA` capability scope
    // variable resolution; see design doc § fs:scope policy
    // "Resolution divergence note"), used at a different layer. Both
    // chains land inside the user's own AppData / XDG_DATA_HOME —
    // planting a reparse-point in the parent walk requires AppData
    // write access. Same P1a class as parent-walk reparse on AppData.
    // Defending here would mean a parent-walk reparse scan on every
    // startup, duplicating the FontCache::open_or_create boundary
    // check — and contradicting the locked single-user-desktop threat
    // model. Revisit if the project ships in a multi-user /
    // MDM-managed deployment.
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
// CLI binary can use the same helper.
use crate::font_cache::try_modified_at;

// Earlier rounds had a `stat_mtime` wrapper here; it was a one-line
// forward to `try_modified_at` and got deleted. Caller contract
// preserved across the refactor ("None means skip the populate /
// replace; epoch-zero must
// never reach SQLite") lives on the canonical helper's doc
// (`font_cache.rs::try_modified_at`); duplicating it on a wrapper
// just decayed (the wrapper's doc became grammatically broken across
// edits without re-reading). Callers now use `try_modified_at`
// directly.

// `entries_to_cache_metadata` (in `crate::fonts`) is the shared helper —
// `try_record_folder_in_gui_cache` and the rescan-apply path here both
// route through it, and the CLI's `run_refresh_fonts` loop does too.
// The previous local `entries_to_metadata` duplicated that conversion
// AND lacked the per-file mtime dedup needed for TTC files.

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
    /// the previous successful operation left — this was previously
    /// a hard Err return that wiped the partial-success signal for
    /// ALL folders.
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
    // switched to `try_exists()` to distinguish
    // NotFound from permission-denied (chmod-000 cache previously
    // misclassified as "no file"). `try_exists()` returns Err on
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
/// Does NOT take `CacheMutationGuard` : this is a
/// read-only command. Lock hold is split so the per-folder
/// `try_modified_at` syscall loop runs WITHOUT the slot
/// lock held — the per-folder stat can stall for seconds per call on
/// a slow network share with hundreds of cached folders, and holding
/// the slot through that loop blocked concurrent `lookup_font_family`
/// calls. The split mirrors `rescan_font_cache_drift`'s Phase 1 /
/// Phase 3 pattern: snapshot folder list under lock, drop lock, stat
/// loop unlocked, re-acquire lock to call `diff_against` (which needs
/// the cache handle).
///
/// A parallel `clear_font_cache` interleaving between Phase 1 and
/// Phase 3 has TWO failure shapes that must both be handled, not one:
///   1. **slot == None mid-clear** — Phase 3 acquires the lock while
///      clear is still between `*slot = None` and `*slot = Some(fresh)`.
///      The `None` arm below returns `DriftReport::default()`.
///   2. **slot == Some(fresh empty cache) post-clear** — Phase 3
///      acquires the lock after clear completed and republished a
///      fresh empty cache. The `Some(c)` arm sees the new handle, and
///      `diff_against` against an empty cache would push every
///      snapshot folder into `added`, violating the documented contract
///      that GUI drift detection's `added` is always empty.
///
/// An earlier version of this docstring claimed shape (1) was the only
/// failure mode — wrong. Shape (2) is the more common one because clear is fast
/// and Phase 2's stat loop on a slow network share is the long step.
/// The fix is `GUI_FONT_CACHE_GENERATION`: Phase 1 captures the
/// generation alongside the folder snapshot under the slot lock,
/// Phase 3 verifies the generation under the slot lock before calling
/// `diff_against`. `clear_font_cache` bumps the generation in the
/// same slot-lock scope as `*slot = Some(fresh)`, so any detect that
/// acquires the slot lock after clear's republish observes both the
/// new handle AND the new generation atomically with respect to that
/// lock release. Generation mismatch ⇒ Phase 1's snapshot is stale ⇒
/// return `DriftReport::default()`.
#[tauri::command]
pub fn detect_font_cache_drift() -> Result<DriftReport, String> {
    // Phase 1: snapshot the cached folder list + capture the cache
    // generation under the lock. Capturing the generation INSIDE the
    // lock pairs it with the folder list we observed: the generation
    // reflects "the handle this list came from".
    let (cached_folders, captured_generation) = {
        let slot = GUI_FONT_CACHE
            .lock()
            .map_err(|_| "GUI cache mutex poisoned".to_string())?;
        let cache = match slot.as_ref() {
            Some(c) => c,
            None => return Ok(DriftReport::default()),
        };
        let folders = cache
            .list_folders()
            .map_err(|e| format!("list cached folders: {e}"))?;
        // `gen` is reserved in Rust edition 2024 (generator
        // syntax); rename pre-empts a forced edit on the next edition bump.
        let generation = GUI_FONT_CACHE_GENERATION.load(Ordering::Acquire);
        (folders, generation)
        // slot dropped at end of block
    };

    // Phase 2: per-folder stat loop OUTSIDE the lock. Slow-network /
    // permission-denied / folder-gone all route to "omit from
    // snapshot" → Phase 3's diff_against reports them as removed.
    //
    // mtime granularity : `try_modified_at` returns
    // Unix seconds (1 s resolution). On NTFS / APFS / ext4 / Btrfs
    // (≤1 ms underlying resolution) sub-second mtime bumps round to
    // distinct integer seconds so drift detection is reliable. On
    // FAT / exFAT (2 s native granularity) two writes inside the
    // same 2 s window can collapse to the same i64 and read as
    // "no drift". Out of scope: the GUI cache lives under AppData,
    // which is never FAT/exFAT in a normal Windows install. The
    // companion test at test_font_cache.rs:308 sleeps 2100 ms
    // between writes precisely so that test fixture sets a
    // hardware-floor-safe interval regardless of underlying FS.
    let mut snapshot: Vec<(String, i64)> = Vec::with_capacity(cached_folders.len());
    for folder in &cached_folders {
        if let Some(mtime) = try_modified_at(Path::new(&folder.folder_path)) {
            snapshot.push((folder.folder_path.clone(), mtime));
        }
    }

    // Phase 3: re-acquire the lock and route through `finalize_drift`,
    // which handles both interleaving shapes (cache cleared mid-detect
    // / cache rebuilt mid-detect) before reaching `diff_against`.
    let slot = GUI_FONT_CACHE
        .lock()
        .map_err(|_| "GUI cache mutex poisoned".to_string())?;
    let current_generation = GUI_FONT_CACHE_GENERATION.load(Ordering::Acquire);
    finalize_drift(
        slot.as_ref(),
        &snapshot,
        captured_generation,
        current_generation,
    )
}

/// Phase-3 finalizer for `detect_font_cache_drift`. Pure function so the
/// generation-mismatch and cache-unavailable shapes are unit-testable
/// without standing up the global `GUI_FONT_CACHE` state. Callers must
/// hold the slot lock for the duration of this call.
fn finalize_drift(
    cache: Option<&FontCache>,
    snapshot: &[(String, i64)],
    captured_generation: u64,
    current_generation: u64,
) -> Result<DriftReport, String> {
    // Shape (2): cache was cleared AND a fresh empty cache republished
    // between Phase 1 and Phase 3. The snapshot we built describes a
    // cache that no longer exists; the only correct response is
    // "no drift to report — caller should re-detect against the new
    // generation if they still care".
    if captured_generation != current_generation {
        return Ok(DriftReport::default());
    }
    // Shape (1): cache was cleared and the new handle hasn't landed yet
    // (`*slot = None` between clear's two slot-lock scopes). Same
    // user-visible answer as shape (2).
    let Some(cache) = cache else {
        return Ok(DriftReport::default());
    };
    let report = cache
        .diff_against(snapshot)
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
    // silent-stale-cache shortcut.
    let mut scanned: Vec<(String, i64, Vec<FontMetadata>)> =
        Vec::with_capacity(report.modified.len());
    let mut skipped: Vec<SkippedFolder> = Vec::new();
    for folder in &report.modified {
        let folder_path = Path::new(folder);
        // None → skip the populate (see `try_modified_at` doc):
        // without this, a transient stat failure would write an
        // epoch-zero mtime that drift-detect re-flags forever.
        let Some(folder_mtime) = try_modified_at(folder_path) else {
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
    // hide the success of folders 0..N.
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
///   serve wrong-font results silently . Eviction is
///   the structural defense; UI handling of `RescanResult.skipped` is
///   the user-visible defense on top. No re-stat dance — a folder we
///   couldn't scan is a folder we can't trust.
///
/// Returns `(modified_rescanned, removed_evicted)`. `removed_evicted`
/// counts skipped-folder evictions too because they're the same DB
/// operation; the caller's user-facing tally is just "rows we dropped".
///
/// Per-folder ApplyFailed errors push into `skipped` rather than
/// short-circuiting via `?` . Each `replace_folder` /
/// `remove_folder` is its own SQLite transaction, so committed rows
/// 0..N stay committed even if row N+1 fails — propagating the
/// failure as a hard Err to the frontend would discard that
/// information and prompt the user to re-run the rescan, doing the
/// same work twice. Aggregating into `skipped` lets the modal show
/// "N folders refreshed, M failed to write — see list" partial-
/// success state.
///
/// **Intentional double-surfacing of ScanFailed folders**: a
/// Phase-1 ScanFailed folder appears in BOTH the
/// returned `skipped[].kind == ScanFailed` list AND the
/// `removed_evicted` count, because Phase-2 evicts its stale cache
/// rows via `cache.remove_folder` (incrementing `removed_evicted`)
/// while the `skipped` entry stays for the UI to render. The two
/// surfaces measure different things: `skipped` = "what failed to
/// rescan, surface to the user"; `removed_evicted` = "DB rows we
/// dropped this run, for the summary tally". A future refactor
/// that "deduplicates" by removing the ScanFailed entries from
/// `skipped` after eviction would silently break the modal's
/// user-facing failure report.
fn apply_rescan_to_cache(
    cache: &mut FontCache,
    scanned: &[(String, i64, Vec<FontMetadata>)],
    removed: &[String],
    skipped: &mut Vec<SkippedFolder>,
) -> (usize, usize) {
    let mut modified_rescanned = 0usize;
    let mut removed_evicted = 0usize;

    for (folder, folder_mtime, metadata) in scanned {
        // (Pattern 2 racing-replace defense):
        // re-stat the folder mtime IMMEDIATELY before replace_folder.
        // Phase 2 ran outside the lock, so a parallel
        // `try_record_folder_in_gui_cache` (FontSourceModal's
        // best-effort populate) could have written a fresher row for
        // this folder while we held only the scanned snapshot. Without
        // this re-check, Phase 3 would overwrite the racing populate
        // with our Phase-2 data — low-impact (next drift detect would
        // pick it up) but a real race. Compare current mtime against
        // the Phase-2-captured value; if it ticked forward, skip the
        // replace and surface the race in `skipped` so the user knows
        // the folder is fresh-elsewhere. Stat-fail at this point
        // routes to "trust the Phase-2 mtime" (same fail-open posture
        // as the Phase-1 collection loop).
        let current_mtime = try_modified_at(Path::new(folder)).unwrap_or(*folder_mtime);
        if current_mtime > *folder_mtime {
            log::info!(
                "apply_rescan_to_cache {folder} — folder mtime advanced \
                 ({} → {current_mtime}) between Phase 2 scan and Phase 3 \
                 apply; skipping replace to preserve concurrent fresh row",
                *folder_mtime
            );
            skipped.push(SkippedFolder {
                folder: folder.clone(),
                reason: "concurrent fresh write detected; skipped to avoid \
                         overwriting newer data"
                    .to_string(),
                kind: SkipKind::ApplyFailed,
            });
            continue;
        }
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
        // state stay aligned.
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

    // Build the main-file + sidecar set and reject reparse points
    // BEFORE dropping the live cache handle. If a planted sidecar is
    // found, clear must fail without making the current in-memory
    // cache unavailable or clearing the provenance set.
    let paths: Vec<PathBuf> = ["", "-journal", "-wal", "-shm"]
        .iter()
        .map(|suffix| {
            let mut p = path.clone().into_os_string();
            p.push(suffix);
            PathBuf::from(p)
        })
        .collect();
    let reparse_skipped: Vec<String> = paths
        .iter()
        .filter(|p| crate::util::is_reparse_point(p))
        .map(|p| {
            log::warn!(
                "clear_font_cache: refusing to remove reparse-point {}; aborting clear.",
                p.display()
            );
            p.display().to_string()
        })
        .collect();
    if !reparse_skipped.is_empty() {
        return Err(format!(
            "Refusing to clear font cache: the following path(s) are reparse points \
             (symlinks / junctions) and were left in place to avoid following the link. \
             Inspect and remove manually: {}",
            reparse_skipped.join(", ")
        ));
    }

    // Two-lock pattern: the slot lock is taken, released, then
    // re-taken — bracketed by the CacheMutationGuard
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
    // also clear provenance HERE, inside
    // the same lock scope that sets slot=None. An earlier version had
    // the `clear_cache_provenance()` call at the END of this function
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
    // symmetric with `clear_font_sources` — the
    // user's "fresh slate" signal must drop in-process provenance rows
    // alongside the SQLite rebuild. ALLOWED_FONT_PATHS (system fonts)
    // stays — system discovery is cache-independent.
    //
    // the generation counter is NOT bumped here,
    // even though `slot` transitions Some → None. The second scope
    // (after the on-disk rebuild succeeds) does `*slot = Some(fresh)`
    // AND `fetch_add(Release)` together — that's where the new
    // generation labels the NEW handle's identity. Between these two
    // scopes, a concurrent `detect_font_cache_drift` Phase 3 that
    // observes `slot=None` falls through `finalize_drift`'s
    // None arm (`cache: None` → `DriftReport::default()`) without
    // needing a generation check. The generation only matters once a
    // new handle has been published; tagging the empty transition
    // would be a no-op that any future fourth-state transition
    // (`Some(stale)` → `Some(fresh)` directly) would need anyway.
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
    // Two-phase pre-scan + remove for atomic semantics. The pre-scan
    // above runs before the live handle/provenance are dropped; this
    // loop is Phase 2 and only runs once every path is known clean.
    // An earlier
    // version of the loop detected reparse and continued — but ALSO
    // continued removing any subsequent non-reparse sidecars, leaving
    // partial state (e.g., [main, JOURNAL-reparse, wal, shm] → main +
    // wal + shm wiped, journal symlink left, Err returned). The Err
    // message promised atomicity ("none removed and the user sees the
    // reparse-path message") that the implementation didn't honor.
    // Phase 1 detects ANY reparse upfront; if found, abort with Err
    // before any remove_file. Phase 2 (only if all clean) removes
    // all four files. Atomicity is bounded to the reparse-point
    // pre-check (all-or-none on "encountered a reparse-point
    // sidecar"). Individual `remove_file` failures mid-Phase-2
    // (drive eject, permission flip, antivirus lock) log WARN and
    // continue, leaving the surviving sidecars in place — an earlier
    // doc claim "either all removed or none" read broader than the
    // implementation delivered.
    //
    // **TOCTOU between Phase 1 and Phase 2 (P1a-accepted)**: between
    // the pre-scan reparse check below and the per-file remove_file
    // loop, a P1a actor with filesystem
    // access could plant a symlink at any of the four sidecar paths
    // — the remove_file would then act on the planted link instead
    // of the file we lstat'd. Bounded by P1a (single-user desktop,
    // AppData-local — defender controls the parent directory). The
    // CacheMutationGuard above serializes against rescan / clear-
    // re-entry but not against an attacker with filesystem-level
    // access; closing that window would require atomic open-and-
    // unlink primitives (Linux-specific) or a wider lock scope that
    // doesn't exist on Windows. Revisit if the project deploys in
    // multi-user / MDM-managed shapes (same revisit trigger as the
    // design doc § fs:scope resolution divergence note).
    //
    // P1a vs P1b note : these sidecar paths look
    // filesystem-resident, which superficially suggests P1b (content
    // source under attacker influence). The distinction: P1b applies
    // to user-influenced *content* (subtitle files, font packs) — the
    // attacker chooses the bytes/path. Here the paths are computed
    // entirely from `init_gui_font_cache(&app_data_dir)` outputs;
    // there is no user-content tributary. The threat is purely the
    // P1a actor model (process with filesystem write access to the
    // defender's AppData parent), not a content-tainted input.
    for p in &paths {
        match std::fs::remove_file(p) {
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
    // bump the generation INSIDE the slot-
    // lock scope, after publishing the new handle, so any concurrent
    // `detect_font_cache_drift` that acquires the slot lock after this
    // release observes both the new cache AND the bumped generation
    // atomically with respect to this lock — closing the window where
    // detect's Phase 3 could see slot=Some(fresh empty) but
    // generation=old, run diff_against, and leak snapshot folders into
    // `added`. Release ordering pairs with detect's Acquire load.
    GUI_FONT_CACHE_GENERATION.fetch_add(1, Ordering::Release);
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
    // (sibling): mtime-unreadable is a
    // success-of-degradation — scan succeeded, cache populate skipped —
    // so DEBUG, not WARN. Done before locking the slot so we don't
    // bother contending for the cache when there's nothing to write.
    let Some(folder_mtime) = try_modified_at(folder_path) else {
        log::debug!(
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
    // locking + Poisoned handling moved into `with_cache_slot`;
    // the WARN-on-busy-vs-DEBUG-on-busy policy lives at the call site so
    // this helper (best-effort populate after a successful scan) keeps
    // its DEBUG level and clear_all_folders_in_gui_cache_locked stays at
    // WARN. See `with_cache_slot` docstring for the outcome semantics.
    match with_cache_slot(|cache| cache.replace_folder(&folder_path_str, folder_mtime, &metadata)) {
        CacheSlotOutcome::Ran(Ok(())) => {
            log::info!(
                "GUI cache populated: {} ({} faces)",
                folder_path_str,
                face_count
            );
        }
        CacheSlotOutcome::Ran(Err(e)) => {
            log::warn!("GUI cache populate for {folder_path_str} failed: {e}");
        }
        CacheSlotOutcome::Busy => {
            // success-of-degradation — user's scan
            // completed; populate skipped because a rescan / clear holds
            // the slot lock. DEBUG, not WARN (vibe-coding § Log-level).
            log::debug!(
                "GUI cache busy (rescan or clear in progress); skipping populate \
                 for {} this scan — will populate on next add",
                folder_path.display()
            );
        }
        CacheSlotOutcome::Poisoned => {
            // Helper already logged the WARN; nothing more to do.
        }
        CacheSlotOutcome::Unavailable => {
            log::warn!(
                "GUI cache unavailable (init failed or schema mismatch); \
                 skipping populate for {}",
                folder_path.display()
            );
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
    // see `try_record_folder_in_gui_cache` for the helper
    // rationale. Unavailable arm stays silent here (eviction of a
    // folder we never indexed in the first place is by definition a
    // no-op) — that's the divergence from the populate sibling.
    match with_cache_slot(|cache| cache.remove_folder(folder_path)) {
        CacheSlotOutcome::Ran(Ok(())) => log::info!("GUI cache evicted folder: {folder_path}"),
        CacheSlotOutcome::Ran(Err(e)) => {
            log::warn!("GUI cache evict {folder_path} failed: {e}");
        }
        CacheSlotOutcome::Busy => {
            // success-of-degradation — user's
            // remove-source action completed on the session DB; cache
            // eviction skipped because a rescan / clear holds the
            // lock. Stays consistent on the next launch's drift
            // detect. DEBUG, not WARN.
            log::debug!(
                "GUI cache busy (rescan or clear in progress); skipping evict for {folder_path}"
            );
        }
        CacheSlotOutcome::Poisoned => {
            // Helper already logged the WARN; nothing more to do.
        }
        CacheSlotOutcome::Unavailable => {} // silent: nothing to evict
    }
}

/// Eviction of every folder from the GUI cache, called from
/// `fonts::clear_font_sources` so the persistent cache stays in step
/// with the user's "Clear all sources" intent on the session-DB side
/// . Without this, clear_font_sources wiped the
/// session DB but left `cached_folders` / `cached_fonts` rows intact —
/// the next embed pass resolved a family via the cache to a path
/// whose session-DB provenance had been cleared, and `subset_font`
/// rejected it with "Font path was not discovered by a scan command."
///
/// CALLER MUST already hold `CacheMutationGuard`: `clear_font_sources`
/// clears the session DB AND evicts the persistent cache as one
/// atomic mutation, so the guard wraps both steps. Re-acquiring
/// inside this fn would either deadlock (reentrancy-unsafe CAS) or
/// fail and silently skip the eviction.
///
/// (A prior `try_clear_all_folders_in_gui_cache` wrapper that
/// acquired the guard itself was removed — every caller already held
/// the guard for atomic-mutation reasons, the wrapper was dead.)
///
/// Mutation-guard + slot-lock interaction:
/// - The `&CacheMutationGuard` arg proves the caller blocked /
///   blocks `rescan_font_cache_drift`. Without it, a clear landing
///   between rescan's Phase 2 (long scan outside slot lock) and
///   Phase 3 (apply scan results inside slot lock) would wipe rows
///   just before Phase 3 re-inserts the freshly scanned ones — end
///   state: cache holds rows whose session-DB provenance was just
///   cleared, UI claim and DB state disagree.
/// - `try_lock` on the SLOT mutex stays — that protects against
///   handle drop in the clear_font_cache recovery path. In practice
///   `try_lock` here is guaranteed-success because
///   `CacheMutationGuard` (held by every caller per the
///   contract above) already serializes against `rescan_font_cache_drift`
///   and `clear_font_cache` — the only paths that hold the slot
///   lock for any meaningful duration. The `WouldBlock` arm exists
///   only as a defensive fallback for an unanticipated future
///   slot-holder; if a real caller hits it, that's a guard-discipline
///   regression, not normal contention. WARN log is therefore the
///   right level (failure-to-degrade), not DEBUG.
/// - Cache unavailable → silent no-op (nothing to evict).
/// - Per-folder `remove_folder` errors log WARN but don't abort the
///   iteration; best-effort.
///
/// A concurrent `try_record_folder_in_gui_cache` that races between
/// `list_folders` and the iteration's `remove_folder` would leave a
/// fresh row behind — acceptable because the racing populate is
/// post-intent ("Clear all" was issued before the new populate).
pub(crate) fn clear_all_folders_in_gui_cache_locked(_guard: &CacheMutationGuard) {
    // see `try_record_folder_in_gui_cache` for the helper
    // rationale. The Busy arm logs WARN here (NOT debug like the
    // best-effort sibling helpers) because `CacheMutationGuard` is
    // supposed to have already serialized this against
    // `rescan_font_cache_drift` / `clear_font_cache` — the only paths
    // that hold the slot lock for any meaningful duration.
    // WouldBlock therefore signals a guard-
    // discipline regression worth surfacing, not normal contention.
    match with_cache_slot(|cache| {
        let folders = cache.list_folders()?;
        let total = folders.len();
        for f in folders {
            if let Err(e) = cache.remove_folder(&f.folder_path) {
                log::warn!(
                    "GUI cache remove_folder({}) during clear-all: {e}",
                    f.folder_path
                );
            }
        }
        Ok::<usize, CacheError>(total)
    }) {
        CacheSlotOutcome::Ran(Ok(total)) => {
            log::info!("GUI cache clear-all evicted {total} folder rows");
        }
        CacheSlotOutcome::Ran(Err(e)) => {
            log::warn!("GUI cache list_folders failed during clear-all: {e}");
        }
        CacheSlotOutcome::Busy => log::warn!("GUI cache slot busy; skipping clear-all"),
        CacheSlotOutcome::Poisoned => {
            // Helper already logged the WARN; nothing more to do.
        }
        CacheSlotOutcome::Unavailable => {} // silent: nothing to clear
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
    // Shared `validate_font_family` : bounds family
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
    // Register the cache hit in the in-process provenance set so
    // `subset_font`'s gate accepts the returned
    // path. Without this, the GUI's lookup tier 2 (embed-time cache
    // hit) goes through the IPC roundtrip and then trips the gate
    // as "Font path was not discovered by a scan command". See
    // `register_cache_provenance` for the threat-model rationale.
    //
    // registration failure → treat as a cache
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
            // (Pattern 1 census parity): `{family}`
            // is interpolated raw here, no `sanitize_for_display` /
            // `stripUnicodeControls` wrap. Safe today because
            // `validate_font_family` (invoked at the top of `lookup_font_family`) already rejected
            // BiDi / zero-width / control characters before reaching
            // this site — `family` is a sanitized substring of the IPC
            // input. If `validate_font_family`'s rejection set is ever
            // relaxed, this log site silently re-opens as a leak; the
            // pin lives here so a future relaxation reviewer notices
            // the dependency. `{e}` is the
            // `register_cache_provenance` error string which carries
            // no path bytes (provenance Err strings are generic
            // refusal messages).
            log::warn!(
                "Font '{family}' cache lookup hit a path that failed provenance validation; \
                 treating as miss: {e}"
            );
            return Ok(None);
        }
    }
    // (Pattern 3 cross-helper coupling): the
    // `register_cache_provenance(r)` call above routes through
    // `u32::try_from(hit.face_index())` and returns Ok(None) on
    // negative values (font_cache.rs:298) — so the cast on line below
    // is safe today only via that sibling check. A future refactor
    // that weakens / moves / splits provenance's negativity guard
    // would silently re-introduce wrap-to-huge-u32 here. `try_from`
    // + unreachable!() makes the negativity guarantee local to this
    // site; the unreachable arm fires only if provenance contract
    // breaks, in which case loud panic >> silent wrap to ~4 G face
    // index.
    Ok(result.map(|r| crate::fonts::FontLookupResult {
        path: r.font_path,
        index: u32::try_from(r.face_index)
            .expect("face_index negativity guaranteed by register_cache_provenance above"),
    }))
}

// ── Tests ────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    /// RAII guard mirroring `font_cache.rs::tests::TempCacheDir` —
    /// the canonical-shape comment on that sibling enumerates every
    /// other temp-dir construction in the workspace (dropzone.rs /
    /// safe_io.rs / fonts.rs test modules) and explains why
    /// consolidation hasn't landed.
    ///
    /// Same posture, NOT identical: this version takes a `name:
    /// &str` argument (the lib-side one is no-args) and the seed
    /// uses `subsec_nanos` (the lib-side uses `as_nanos`, which is
    /// wider entropy). The difference hasn't surfaced as a collision
    /// in practice. Keep this struct in sync with its sibling for
    /// Drop semantics / suffix shape; if the seed-strength gap ever
    /// becomes a parallel-test issue, port the `as_nanos` form here
    /// rather than the other way around.
    struct TempCacheDir(std::path::PathBuf);

    impl TempCacheDir {
        fn new(name: &str) -> Self {
            // PID + nanos — `font_cache.rs`'s
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
        // Regression pin: a Phase-2 scan failure must drop
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
        // A folder that doesn't pass the same stat bar Phase 1 used
        // (no real mtime now) must still be evicted — Phase 3 must
        // NOT short-circuit to "reappeared".
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
        // Partial coverage: the Phase-3 ApplyFailed push path (Err arm
        // of `cache.replace_folder` / `remove_folder`) requires real
        // SQLite-write injection to exercise and is not covered by
        // this test. What we DO pin here: a pre-existing
        // `SkippedFolder { kind: ApplyFailed }` in the input vec
        // survives alongside successful operations — i.e., the helper
        // doesn't accidentally wipe or rewrite the input vec. Real
        // mid-loop failure coverage is not yet done — it needs the
        // repo to gain a FontCache fault-injection seam.
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

    // ── migrate_legacy_gui_cache ──

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

    #[cfg(unix)]
    #[test]
    fn clear_font_cache_reparse_error_preserves_live_handle() {
        use std::os::unix::fs::symlink;

        let dir = TempCacheDir::new("clear_reparse_preserve");
        let cache_path = dir.0.join(GUI_CACHE_FILE_NAME);
        let cache = FontCache::open_or_create(&cache_path).expect("open cache");
        let target = dir.0.join("wal-target");
        fs::write(&target, b"not sqlite").unwrap();
        let wal = {
            let mut p = cache_path.clone().into_os_string();
            p.push("-wal");
            PathBuf::from(p)
        };
        symlink(&target, &wal).unwrap();

        {
            let mut path_slot = GUI_FONT_CACHE_PATH.lock().unwrap();
            *path_slot = Some(cache_path.clone());
            let mut cache_slot = GUI_FONT_CACHE.lock().unwrap();
            *cache_slot = Some(cache);
        }

        let err = clear_font_cache().unwrap_err();
        assert!(err.contains("reparse points"), "got: {err}");
        assert!(
            GUI_FONT_CACHE.lock().unwrap().is_some(),
            "failed clear must leave the old cache handle available"
        );

        *GUI_FONT_CACHE.lock().unwrap() = None;
        *GUI_FONT_CACHE_PATH.lock().unwrap() = None;
        crate::fonts::clear_cache_provenance();
    }

    // ── finalize_drift generation check ──

    #[test]
    fn finalize_drift_returns_default_when_generation_changed() {
        // Simulates `detect_font_cache_drift` Phase 1 capturing the
        // cached folders + generation, then
        // `clear_font_cache` republishing a fresh empty cache (which
        // bumps the generation), then Phase 3 calling finalize_drift
        // with a cache reference that no longer matches the snapshot.
        // Without the generation check, the snapshot's folders would
        // leak into `added`, violating the documented "added is always
        // empty for the GUI path" contract. With the check, Phase 3
        // returns DriftReport::default().
        let (_guard, cache) = temp_cache("fin_drift_gen_changed");
        // Pre-clear snapshot: two folders the user previously had in
        // their cache. The fresh post-clear `cache` we pass in does
        // NOT contain them.
        let snapshot = vec![
            ("/legacy/folder/a".to_string(), 100),
            ("/legacy/folder/b".to_string(), 200),
        ];
        let report = finalize_drift(Some(&cache), &snapshot, 5, 6).unwrap();
        assert!(
            report.added.is_empty(),
            "stale snapshot must NOT leak into added[]; got {:?}",
            report.added
        );
        assert!(report.modified.is_empty(), "modified must also be empty");
        assert!(report.removed.is_empty(), "removed must also be empty");
    }

    #[test]
    fn finalize_drift_returns_default_when_cache_unavailable() {
        // Pins shape (1): cache slot is None (clear is mid-flight,
        // between `*slot = None` and `*slot = Some(fresh)`).
        // Generation check still happens first, but None is the
        // independent reason for the default return.
        let snapshot = vec![("/folder/a".to_string(), 100)];
        let report = finalize_drift(None, &snapshot, 0, 0).unwrap();
        assert!(report.added.is_empty());
        assert!(report.modified.is_empty());
        assert!(report.removed.is_empty());
    }

    #[test]
    fn finalize_drift_returns_diff_when_generation_matches() {
        // Counter-test: when the generation didn't change between
        // Phase 1 and Phase 3 (no clear interleaved), diff_against
        // runs and reports real drift. Seeds /folder/a with mtime
        // 100; passes a snapshot with mtime 999 (mtime mismatch
        // → reported as modified).
        let (_guard, mut cache) = temp_cache("fin_drift_gen_matches");
        cache.replace_folder("/folder/a", 100, &[]).unwrap();
        let snapshot = vec![("/folder/a".to_string(), 999)];
        let report = finalize_drift(Some(&cache), &snapshot, 42, 42).unwrap();
        assert_eq!(
            report.modified,
            vec!["/folder/a".to_string()],
            "mtime mismatch should classify as modified"
        );
        assert!(
            report.added.is_empty(),
            "snapshot path is in cache → not added"
        );
        assert!(
            report.removed.is_empty(),
            "all cache rows present in snapshot"
        );
    }
}
