//! Blocking implementations and shared state for the persistent font cache.
//!
//! `font_cache.rs` itself stays Tauri-free so the CLI binary can use it
//! without pulling in the GUI's IPC layer. The async Tauri wrappers live in
//! `ipc_commands`; this module owns the static `Mutex<Option<FontCache>>`
//! initialized during Tauri setup plus the synchronous operations used by the
//! React drift modal and embed-time lookup tier.
//!
//! The GUI command surface stays deliberately small: cache status,
//! drift detection, drift rescan, clear/rebuild, and lookup. The
//! frontend owns presentation; this layer owns cache mutation ordering.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Mutex;

use once_cell::sync::Lazy;
use serde::Serialize;

use crate::font_cache::{
    snapshot_source_directories, validate_cache_source_stability, CacheError, CacheSourceKey,
    CacheSourceSnapshot, FontCache, FontDirectoryScope, FontMetadata,
};
use crate::fonts::entries_to_cache_metadata;

/// Sentinel set true while any cache-mutating IPC command
/// is mid-flight, so a second publication, removal, rescan, clear, or rebuild
/// refuses rather than racing it. The frontend gates normal buttons, but the
/// IPC layer is the actual boundary for out-of-band callers. One CAS-gated flag
/// keeps the session database and persistent source ownership synchronized.
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
/// reference to `clear_all_sources_in_gui_cache_locked` ties the
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

/// Monotonic revision counter bumped after every successful cache content or
/// topology mutation. The counter is
/// the synchronization primitive that makes `detect_font_cache_drift`'s
/// Phase 1 / Phase 3 lock split safe against a concurrent
/// mutation: Phase 1 captures the generation under the slot
/// lock alongside the source identities; Phase 3 re-acquires the slot
/// lock and verifies the generation matches before calling
/// `diff_sources`. A mismatch means the cache changed between
/// phases, so the Phase-1 snapshot is stale and the only correct
/// answer is `DriftReport::default()`. The bump MUST live inside the
/// same slot-lock scope as `*slot = Some(fresh)` so detect can't
/// observe the new handle without also observing the new generation.
static GUI_FONT_CACHE_GENERATION: AtomicU64 = AtomicU64::new(0);

/// Complete one successful GUI-cache mutation while the caller still holds
/// `GUI_FONT_CACHE`. Lock order is slot → provenance everywhere. Centralizing
/// the revision bump and provenance invalidation prevents future mutators from
/// updating SQLite while leaving an earlier lookup trusted in-process.
fn finish_gui_cache_mutation() {
    crate::fonts::clear_cache_provenance();
    GUI_FONT_CACHE_GENERATION.fetch_add(1, Ordering::Release);
}

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
    // The check is positioned immediately before `fs::rename`; there
    // is no intermediate validation work in this function between the
    // check and the rename. The operation remains stat-then-act, so an
    // attacker who can swap the file between syscalls can still win,
    // but the window is single-syscall narrow and matches the
    // codebase posture used by the safe I/O helpers. A true race-free
    // fix would need Windows handle-based rename APIs opened with
    // `FILE_FLAG_OPEN_REPARSE_POINT`; not worth the platform interop
    // for the single-user threat model.
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
        // path. Bounded local-user per single-user-desktop AppData reparse
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
    // variable resolution), used at a different layer. Both
    // chains land inside the user's own AppData / XDG_DATA_HOME —
    // planting a reparse-point in the parent walk requires AppData
    // write access. Same local-user class as parent-walk reparse on AppData.
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
// guarded GUI source publication and the rescan-apply path here both
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
    pub added: Vec<CacheSourceKey>,
    pub modified: Vec<CacheSourceKey>,
    pub removed: Vec<CacheSourceKey>,
}

/// One source that didn't make it through a clean rescan.
/// `kind` distinguishes Phase-2 scan failure (couldn't read the source)
/// from Phase-3 apply failure (couldn't write the cache source); the
/// frontend renders both kinds in the same partial-success block.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SkippedFolder {
    /// Cached source root that triggered the skip. Field name
    /// `folder` (not `folder_path`) is intentional and paired with
    /// TS `FontCacheSkippedFolder.folder` in `tauri-api.ts`; the
    /// shorter form jars against `FolderRecord.folder_path` in
    /// `font_cache.rs` but the trade is "shorter UI-facing field
    /// name vs internal-storage descriptor" — keep the TS pairing.
    pub folder: String,
    pub scope: FontDirectoryScope,
    /// User-facing reason — the error message from the failing op
    /// (already includes the source root in some cases; the frontend
    /// renders the pair as `folder — reason`).
    pub reason: String,
    /// Which phase failed. ScanFailed: filesystem walk / name-table
    /// read errored; cache-row eviction was attempted as a
    /// fall-through-to-fresh guard. ApplyFailed: a SQLite replace or eviction
    /// errored mid-rescan; the reason states whether the follow-up fail-closed
    /// eviction also failed.
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
    /// Count of sources successfully re-scanned and replaced in cache.
    pub modified_rescanned: usize,
    /// Count of sources evicted from cache (includes both the
    /// `report.removed` sources that truly disappeared AND the Phase-2-
    /// skipped sources whose stale rows are dropped — see
    /// `apply_rescan_to_cache`).
    pub removed_evicted: usize,
    /// Sources that didn't apply cleanly — both Phase-2 scan failures
    /// (ScanFailed) and Phase-3 apply failures (ApplyFailed). The
    /// frontend keeps the drift modal in a partial-success state when
    /// this is non-empty so the user knows which sources need attention.
    pub skipped: Vec<SkippedFolder>,
}

fn collect_live_source_snapshots(
    cached_sources: &[crate::font_cache::CacheSourceRecord],
) -> (Vec<CacheSourceSnapshot>, Vec<CacheSourceKey>) {
    let mut snapshots = Vec::with_capacity(cached_sources.len());
    let mut unreadable_existing = Vec::new();
    for source in cached_sources {
        match snapshot_source_directories(Path::new(&source.source_root), source.scope) {
            Ok(snapshot) => snapshots.push(snapshot),
            Err(_) => {
                let root = Path::new(&source.source_root);
                let is_clean_real_directory =
                    matches!(crate::util::try_is_reparse_point(root), Ok(false))
                        && std::fs::symlink_metadata(root).is_ok_and(|metadata| metadata.is_dir());
                if is_clean_real_directory {
                    unreadable_existing.push(source.key());
                }
            }
        }
    }
    (snapshots, unreadable_existing)
}

fn classify_unreadable_existing_as_modified(
    report: &mut DriftReport,
    unreadable_existing: &[CacheSourceKey],
) {
    for key in unreadable_existing {
        report.removed.retain(|removed| removed != key);
        if !report.modified.contains(key) {
            report.modified.push(key.clone());
        }
    }
    report.modified.sort();
}

// ---- Blocking cache operations -----------------------------------------

/// Report the current cache status. Useful for the launch-time check
/// (frontend asks "is cache ready?" before calling detect_drift) and
/// for re-checking after `clear_font_cache`.
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

/// Detect drift between cached source snapshots and the live filesystem.
/// Each source is re-walked metadata-only. Any change to a visited real
/// directory or allowed-extension candidate file is reported once at its
/// `(source_root, scope)` owner; missing/unreadable sources are `removed`.
/// `added` remains empty because the GUI begins from persisted owners.
///
/// Returns an empty report when the cache is unavailable (init failed
/// or schema mismatch); the frontend treats empty + unavailable as
/// "no modal needed" while `open_font_cache` separately surfaces the
/// schema-mismatch state for the rebuild path.
///
/// Does NOT take `CacheMutationGuard`: this is read-only. It snapshots source
/// identities under the slot lock, drops the lock for recursive metadata
/// walks, then re-acquires briefly for the database diff. Font lookup therefore
/// remains responsive while a large library is checked.
///
/// A parallel cache mutation interleaving between Phase 1 and Phase 3 has two
/// failure shapes that must both be handled:
///   1. **slot == None mid-clear** — Phase 3 acquires the lock while
///      rebuild is still between `*slot = None` and `*slot = Some(fresh)`.
///      The `None` arm below returns `DriftReport::default()`.
///   2. **slot == Some(fresh empty cache) post-clear** — Phase 3
///      acquires the lock after rebuild completed and republished a
///      fresh empty cache. The `Some(c)` arm sees the new handle, and
///      `diff_sources` against an empty cache would push every
///      snapshot source into `added`, violating the documented contract
///      that GUI drift detection's `added` is always empty.
///
/// The fix is `GUI_FONT_CACHE_GENERATION`: Phase 1 captures the revision
/// alongside the source identities under the slot lock, and Phase 3 verifies it
/// before calling `diff_sources`. Every successful publication, removal,
/// rescan, clear, or rebuild bumps the revision while holding the same slot
/// lock. A mismatch means the filesystem snapshot describes an obsolete source
/// set, so this call returns `DriftReport::default()` and a later check starts
/// from the current cache.
pub fn detect_font_cache_drift() -> Result<DriftReport, String> {
    // Phase 1: snapshot the cached source list + capture the cache
    // generation under the lock. Capturing the generation INSIDE the
    // lock pairs it with the source list we observed: the generation
    // reflects "the handle this list came from".
    let (cached_sources, captured_generation) = {
        let slot = GUI_FONT_CACHE
            .lock()
            .map_err(|_| "GUI cache mutex poisoned".to_string())?;
        let cache = match slot.as_ref() {
            Some(c) => c,
            None => return Ok(DriftReport::default()),
        };
        let sources = cache
            .list_sources()
            .map_err(|e| format!("list cached font sources: {e}"))?;
        // `gen` is reserved in Rust edition 2024 (generator
        // syntax); rename pre-empts a forced edit on the next edition bump.
        let generation = GUI_FONT_CACHE_GENERATION.load(Ordering::Acquire);
        (sources, generation)
        // slot dropped at end of block
    };

    // Phase 2: metadata walks OUTSIDE the slot lock. Timestamps use the
    // filesystem's available precision encoded as Unix nanoseconds; candidate
    // file size independently catches changes on coarse-mtime filesystems.
    let (snapshot, unreadable_existing) = collect_live_source_snapshots(&cached_sources);

    // Phase 3: re-acquire the lock and route through `finalize_drift`,
    // which handles both interleaving shapes (cache cleared mid-detect
    // / cache rebuilt mid-detect) before reaching `diff_sources`.
    let slot = GUI_FONT_CACHE
        .lock()
        .map_err(|_| "GUI cache mutex poisoned".to_string())?;
    let current_generation = GUI_FONT_CACHE_GENERATION.load(Ordering::Acquire);
    finalize_drift(
        slot.as_ref(),
        &snapshot,
        &unreadable_existing,
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
    snapshot: &[CacheSourceSnapshot],
    unreadable_existing: &[CacheSourceKey],
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
        .diff_sources(snapshot)
        .map_err(|e| format!("compute drift: {e}"))?;
    let mut report = DriftReport {
        added: report.added,
        modified: report.modified,
        removed: report.removed,
    };
    // Only merge Phase-1 unreadable roots after confirming that the cache
    // revision still matches. Otherwise a source from the old generation
    // would leak back into an intentionally empty/default report.
    classify_unreadable_existing_as_modified(&mut report, unreadable_existing);
    Ok(report)
}

/// Bring the cache back into sync with the filesystem: re-scan every
/// source reported as `modified`, evict every source reported as
/// `removed`. A mutation guard freezes in-process writers while metadata walks
/// and font parsing run outside the cache mutex; only short database reads and
/// writes hold that mutex. `added` is empty by design (see
/// `detect_font_cache_drift`) so this command does not scan new sources — those
/// enter through guarded publication after a successful directory scan or the
/// CLI's `refresh-fonts` subcommand.
pub fn rescan_font_cache_drift() -> Result<RescanResult, String> {
    // Block parallel `clear_font_cache` between Phase 1 and Phase 3 so
    // Clear can't drop+recreate the cache mid-rescan and have Phase 3's
    // apply resurrect the cleared rows. CacheMutationGuard's CAS-inside-
    // new pattern makes "flag set but no guard yet" structurally
    // impossible; Drop releases on every exit path (Ok / Err / panic-
    // unwind). Same guard also blocks a concurrent rescan if clear is
    // already running.
    let _mutation_guard = CacheMutationGuard::try_acquire()?;

    // Phase 1a — briefly hold the slot only long enough to read persisted
    // source identities. CacheMutationGuard already prevents every mutator.
    let cached_sources = {
        let slot = GUI_FONT_CACHE
            .lock()
            .map_err(|_| "GUI cache mutex poisoned".to_string())?;
        let cache = slot.as_ref().ok_or_else(|| {
            "Cache not available (init failed or schema mismatch). \
             Use clear_font_cache to rebuild."
                .to_string()
        })?;
        cache
            .list_sources()
            .map_err(|e| format!("list cached font sources: {e}"))?
    };
    // Phase 1b — recursive metadata walks run outside the slot, so cache
    // lookups do not wait behind a 20k-file library traversal.
    let (snapshots, unreadable_existing) = collect_live_source_snapshots(&cached_sources);
    let mut report = {
        let slot = GUI_FONT_CACHE
            .lock()
            .map_err(|_| "GUI cache mutex poisoned".to_string())?;
        let cache = slot.as_ref().ok_or_else(|| {
            "Cache became unavailable while computing drift; retry the operation".to_string()
        })?;
        cache
            .diff_sources(&snapshots)
            .map_err(|e| format!("compute drift: {e}"))?
    };
    let mut report = DriftReport {
        added: std::mem::take(&mut report.added),
        modified: std::mem::take(&mut report.modified),
        removed: std::mem::take(&mut report.removed),
    };
    classify_unreadable_existing_as_modified(&mut report, &unreadable_existing);

    // Phase 2 — outside lock: scan each modified source. This is the
    // long step (full directory walk + name-table reads); concurrent
    // lookup_font_family calls run in
    // parallel instead of waiting on a multi-second to multi-minute
    // scan that used to be inside the lock.
    //
    // Per-source error catch (mirrors `run_refresh_fonts` in
    // `bin/cli/main.rs`): one source hitting MAX_CACHE_POPULATE_FACES
    // or a transient I/O error must not abort the whole rescan — that
    // would let one oversized font pack DoS the user's entire cache
    // refresh. Log WARN with source context, push to `skipped`, continue.
    // Phase 3's eviction of skipped sources' stale rows closes the
    // silent-stale-cache shortcut.
    let mut scanned: Vec<(CacheSourceSnapshot, Vec<FontMetadata>)> =
        Vec::with_capacity(report.modified.len());
    let mut skipped: Vec<SkippedFolder> = Vec::new();
    for source in &report.modified {
        let folder_path = Path::new(&source.source_root);
        match crate::fonts::scan_directory_collecting_with_snapshot(folder_path, source.scope) {
            Ok((entries, snapshot)) => {
                let metadata = match entries_to_cache_metadata(&entries) {
                    Ok(metadata) => metadata,
                    Err(err) => {
                        skipped.push(SkippedFolder {
                            folder: source.source_root.clone(),
                            scope: source.scope,
                            reason: err,
                            kind: SkipKind::ScanFailed,
                        });
                        continue;
                    }
                };
                scanned.push((snapshot, metadata));
            }
            Err(err) => {
                log::warn!("rescan: skipping {} — {err}", source.source_root);
                skipped.push(SkippedFolder {
                    folder: source.source_root.clone(),
                    scope: source.scope,
                    reason: err,
                    kind: SkipKind::ScanFailed,
                });
            }
        }
    }

    // Phase 2b — still outside the cache slot, revalidate every completed scan
    // at the final pre-apply point and re-probe every allegedly removed root.
    // The scan already returned the exact snapshot produced by its discovery
    // walk, so a separate pre-scan snapshot and an immediate post-scan rewalk
    // would add two full enumerations without strengthening the final
    // applied-state invariant. This full final rewalk remains necessary:
    // parsed faces are only a subset of the snapshot and cannot reveal added,
    // removed, unreadable, or unparseable candidates or directory changes.
    // The mutation guard freezes in-process cache writers throughout this
    // phase; keeping the filesystem walks out here lets lookups remain
    // responsive.
    let scanned = validate_scanned_sources_before_apply(scanned, &mut skipped);
    let removed = confirm_removed_sources_before_apply(&report.removed);

    // Phase 3 — under lock: apply scan results + evict removed and
    // skipped sources. Pure DB work, short hold time. Per-source
    // ApplyFailed errors aggregate into `skipped` alongside the
    // Phase-2 ScanFailed entries; the helper no longer short-circuits
    // on the first SQLite error so an N-th source failure doesn't
    // hide the success of sources 0..N.
    let (modified_rescanned, removed_evicted) = {
        let mut slot = GUI_FONT_CACHE
            .lock()
            .map_err(|_| "GUI cache mutex poisoned".to_string())?;
        let cache = slot
            .as_mut()
            .ok_or_else(|| "Cache became unavailable between drift detect and apply".to_string())?;
        let outcome = apply_rescan_to_cache(cache, &scanned, &removed, &mut skipped);
        if outcome.0 > 0 || outcome.1 > 0 {
            finish_gui_cache_mutation();
        }
        outcome
    };

    Ok(RescanResult {
        modified_rescanned,
        removed_evicted,
        skipped,
    })
}

fn validate_scanned_sources_before_apply(
    scanned: Vec<(CacheSourceSnapshot, Vec<FontMetadata>)>,
    skipped: &mut Vec<SkippedFolder>,
) -> Vec<(CacheSourceSnapshot, Vec<FontMetadata>)> {
    let mut validated = Vec::with_capacity(scanned.len());
    for (snapshot, metadata) in scanned {
        match validate_cache_source_stability(&snapshot, &metadata) {
            Ok(()) => validated.push((snapshot, metadata)),
            Err(error) => skipped.push(SkippedFolder {
                folder: snapshot.source_root,
                scope: snapshot.scope,
                reason: format!("source changed before cache apply: {error}"),
                kind: SkipKind::ScanFailed,
            }),
        }
    }
    validated
}

fn confirm_removed_sources_before_apply(removed: &[CacheSourceKey]) -> Vec<CacheSourceKey> {
    removed
        .iter()
        .filter_map(|source| {
            if snapshot_source_directories(Path::new(&source.source_root), source.scope).is_ok() {
                log::info!(
                    "Skipping cache eviction for {}: source reappeared between drift detect and apply",
                    source.source_root
                );
                None
            } else {
                Some(source.clone())
            }
        })
        .collect()
}

trait RescanCacheWriter {
    fn replace_source(
        &mut self,
        snapshot: &CacheSourceSnapshot,
        fonts: &[FontMetadata],
    ) -> Result<(), CacheError>;

    fn remove_source(
        &mut self,
        source_root: &str,
        scope: FontDirectoryScope,
    ) -> Result<(), CacheError>;
}

impl RescanCacheWriter for FontCache {
    fn replace_source(
        &mut self,
        snapshot: &CacheSourceSnapshot,
        fonts: &[FontMetadata],
    ) -> Result<(), CacheError> {
        FontCache::replace_source(self, snapshot, fonts)
    }

    fn remove_source(
        &mut self,
        source_root: &str,
        scope: FontDirectoryScope,
    ) -> Result<(), CacheError> {
        FontCache::remove_source(self, source_root, scope)
    }
}

/// Apply Phase-2 scan outcomes to the cache. Three input lists, three
/// behaviors:
///
/// - `scanned` — sources whose Phase-2 scan succeeded. Each gets its
///   owned rows replaced with fresh snapshot + face metadata.
/// - `removed` — sources Phase 1 reported as gone and an outside-the-cache-lock
///   re-probe confirmed absent. Reappeared roots are excluded before this
///   helper is called.
/// - `skipped` — sources whose Phase-2 scan failed. Their stale cache
///   rows MUST go: without this, a failed-scan source kept old rows
///   while `rescan_font_cache_drift` still returned `Ok` and the
///   frontend cleared drift state, leaving `lookup_font_family` to
///   serve wrong-font results silently . Eviction is
///   the structural defense; UI handling of `RescanResult.skipped` is
///   the user-visible defense on top. A source we couldn't scan is a
///   source whose old cache rows cannot be trusted.
///
/// Returns `(modified_rescanned, removed_evicted)`. `removed_evicted`
/// counts skipped-source evictions too because they're the same DB
/// operation; the caller's user-facing tally is just "rows we dropped".
///
/// Per-source ApplyFailed errors push into `skipped` rather than
/// short-circuiting via `?`. Each `replace_source` / `remove_source` is its own
/// SQLite transaction, so committed rows 0..N stay committed even if row N+1
/// fails. A failed replace immediately attempts source eviction; any eviction
/// failure is included in the same user-visible reason. Aggregating the errors
/// preserves the successful-source tally and the exact sources needing action.
///
/// **Intentional double-surfacing of ScanFailed sources**: a
/// Phase-1 ScanFailed source appears in BOTH the
/// returned `skipped[].kind == ScanFailed` list AND the
/// `removed_evicted` count, because Phase-2 evicts its stale cache
/// rows via `cache.remove_source` (incrementing `removed_evicted`)
/// while the `skipped` entry stays for the UI to render. The two
/// surfaces measure different things: `skipped` = "what failed to
/// rescan, surface to the user"; `removed_evicted` = "DB rows we
/// dropped this run, for the summary tally". A future refactor
/// that "deduplicates" by removing the ScanFailed entries from
/// `skipped` after eviction would silently break the modal's
/// user-facing failure report.
fn apply_rescan_to_cache<C: RescanCacheWriter>(
    cache: &mut C,
    scanned: &[(CacheSourceSnapshot, Vec<FontMetadata>)],
    removed: &[CacheSourceKey],
    skipped: &mut Vec<SkippedFolder>,
) -> (usize, usize) {
    let mut modified_rescanned = 0usize;
    let mut removed_evicted = 0usize;

    for (snapshot, metadata) in scanned {
        match cache.replace_source(snapshot, metadata) {
            Ok(()) => modified_rescanned += 1,
            Err(e) => {
                let mut reason = format!("replace_source failed: {e}");
                log::warn!("apply_rescan_to_cache {} — {reason}", snapshot.source_root);
                // A failed replacement transaction leaves the old source rows
                // intact. Evict them immediately so the cache cannot keep
                // serving stale lookups after the UI reports partial success.
                match cache.remove_source(&snapshot.source_root, snapshot.scope) {
                    Ok(()) => removed_evicted += 1,
                    Err(remove_error) => {
                        reason.push_str(&format!(
                            "; fail-closed eviction also failed: {remove_error}"
                        ));
                    }
                }
                skipped.push(SkippedFolder {
                    folder: snapshot.source_root.clone(),
                    scope: snapshot.scope,
                    reason,
                    kind: SkipKind::ApplyFailed,
                });
            }
        }
    }
    for source in removed {
        match cache.remove_source(&source.source_root, source.scope) {
            Ok(()) => removed_evicted += 1,
            Err(e) => {
                let reason = format!("remove_source failed: {e}");
                log::warn!("apply_rescan_to_cache {} — {reason}", source.source_root);
                skipped.push(SkippedFolder {
                    folder: source.source_root.clone(),
                    scope: source.scope,
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
    let scan_failed_sources: Vec<CacheSourceKey> = skipped
        .iter()
        .filter(|s| s.kind == SkipKind::ScanFailed)
        .map(|s| CacheSourceKey {
            source_root: s.folder.clone(),
            scope: s.scope,
        })
        .collect();
    for source in scan_failed_sources {
        match cache.remove_source(&source.source_root, source.scope) {
            Ok(()) => removed_evicted += 1,
            Err(e) => {
                let reason = format!("remove_source (scan-failed eviction) failed: {e}");
                log::warn!("apply_rescan_to_cache {} — {reason}", source.source_root);
                skipped.push(SkippedFolder {
                    folder: source.source_root.clone(),
                    scope: source.scope,
                    reason,
                    kind: SkipKind::ApplyFailed,
                });
            }
        }
    }
    (modified_rescanned, removed_evicted)
}

/// Clear a healthy cache transactionally. When the live handle is unavailable
/// because the on-disk schema is obsolete, delete that incompatible database
/// and re-create a fresh version-current cache instead.
///
/// Used as the "Clear cache" button in the drift modal AND as the
/// rebuild path when `open_font_cache` reports `schema_mismatch`.
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

    // A current, live database does not need file deletion. Clear its source
    // rows in one SQLite transaction so success cannot be reported while an
    // undeletable main file quietly preserves old data.
    {
        let mut slot = GUI_FONT_CACHE
            .lock()
            .map_err(|_| "GUI cache mutex poisoned".to_string())?;
        if let Some(cache) = slot.as_mut() {
            cache
                .clear_sources()
                .map_err(|e| format!("clear cached font sources: {e}"))?;
            finish_gui_cache_mutation();
            return Ok(());
        }
    }

    // No live handle means initialization rejected the file (normally a
    // schema mismatch). Rebuilding requires deleting the incompatible main
    // database. Main-file deletion is mandatory; sidecar cleanup remains
    // best-effort because a fresh SQLite open can safely decide whether any
    // surviving journal is usable.
    remove_cache_files_for_rebuild(&paths)?;
    let fresh = FontCache::open_or_create(&path)
        .map_err(|e| format!("re-create cache at {}: {e}", path.display()))?;
    let mut slot = GUI_FONT_CACHE
        .lock()
        .map_err(|_| "GUI cache mutex poisoned".to_string())?;
    finish_gui_cache_mutation();
    *slot = Some(fresh);
    Ok(())
}

fn remove_cache_files_for_rebuild(paths: &[PathBuf]) -> Result<(), String> {
    for (index, path) in paths.iter().enumerate() {
        match std::fs::remove_file(path) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) if index == 0 => {
                return Err(format!(
                    "Cannot remove the existing font-cache database at {}: {error}",
                    path.display()
                ));
            }
            Err(error) => log::warn!(
                "clear_font_cache: removing sidecar {} failed: {error}",
                path.display()
            ),
        }
    }
    Ok(())
}

/// Publish one completed GUI directory scan into the persistent cache.
/// The caller-held mutation guard serializes publication with rescan, remove,
/// clear, and rebuild. Before taking the SQLite slot, this re-walks directory
/// and candidate-file metadata and refuses already-stale or partial output.
pub(crate) fn record_source_in_gui_cache_locked(
    _guard: &CacheMutationGuard,
    snapshot: &CacheSourceSnapshot,
    entries: &[crate::fonts::LocalFontEntry],
) -> Result<(), String> {
    let metadata: Vec<FontMetadata> = entries_to_cache_metadata(entries)?;
    validate_cache_source_stability(snapshot, &metadata)
        .map_err(|e| format!("font source changed before cache publication: {e}"))?;
    let mut slot = GUI_FONT_CACHE
        .lock()
        .map_err(|_| "GUI cache mutex poisoned".to_string())?;
    let cache = slot.as_mut().ok_or_else(|| {
        "GUI cache unavailable (initialization failed or rebuild is required)".to_string()
    })?;
    cache
        .replace_source(snapshot, &metadata)
        .map_err(|e| format!("publish font source cache: {e}"))?;
    finish_gui_cache_mutation();
    log::info!(
        "GUI cache populated: {} ({:?}, {} faces, {} directories)",
        snapshot.source_root,
        snapshot.scope,
        metadata.len(),
        snapshot.directories.len()
    );
    Ok(())
}

/// Evict exactly one `(source_root, scope)` owner. The caller performs this
/// cache-first while holding the same guard as the session-DB removal, so a
/// busy cache cannot silently leave a source alive across launches.
pub(crate) fn remove_source_from_gui_cache_locked(
    _guard: &CacheMutationGuard,
    source_root: &str,
    scope: FontDirectoryScope,
) -> Result<(), String> {
    let mut slot = GUI_FONT_CACHE
        .lock()
        .map_err(|_| "GUI cache mutex poisoned".to_string())?;
    let Some(cache) = slot.as_mut() else {
        return Ok(());
    };
    cache
        .remove_source(source_root, scope)
        .map_err(|e| format!("remove font source from cache: {e}"))?;
    finish_gui_cache_mutation();
    log::info!("GUI cache evicted source: {source_root} ({scope:?})");
    Ok(())
}

/// Atomically clear every persistent source while the caller holds the same
/// mutation guard as the session-DB clear. SQLite cascade ownership removes
/// directories, candidates, faces, and lookup keys in one transaction.
pub(crate) fn clear_all_sources_in_gui_cache_locked(
    _guard: &CacheMutationGuard,
) -> Result<(), String> {
    let mut slot = GUI_FONT_CACHE
        .lock()
        .map_err(|_| "GUI cache mutex poisoned".to_string())?;
    let Some(cache) = slot.as_mut() else {
        return Ok(());
    };
    let total = cache
        .clear_sources()
        .map_err(|e| format!("clear cached font sources: {e}"))?;
    finish_gui_cache_mutation();
    log::info!("GUI cache clear-all evicted {total} source rows");
    Ok(())
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
    // reject it (untrusted-input disclosure surface). Returning None forces the
    // caller into the next lookup tier (system fonts) and keeps the
    // crafted path off the wire.
    if let Some(ref r) = result {
        if let Err(e) = crate::fonts::register_cache_provenance(r) {
            // `{family}` is interpolated raw here, no `sanitize_for_display` /
            // `stripUnicodeControls` wrap. Safe today because
            // `validate_font_family` (invoked at the top of `lookup_font_family`) already rejected
            // BiDi / zero-width / control characters before reaching
            // this site — `family` is a sanitized substring of the IPC
            // input. If `validate_font_family`'s rejection set is ever
            // relaxed, this log site silently re-opens as a leak; this
            // comment pins the dependency for future audits. `{e}` is the
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
    // Keep the negative-index guarantee local to this cast. The
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
    use crate::font_cache::try_modified_at;
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

    #[derive(Default)]
    struct FakeRescanCache {
        fail_replace_for: Vec<String>,
        fail_remove_for: Vec<String>,
        replace_attempts: Vec<String>,
        remove_attempts: Vec<String>,
    }

    impl RescanCacheWriter for FakeRescanCache {
        fn replace_source(
            &mut self,
            snapshot: &CacheSourceSnapshot,
            _fonts: &[FontMetadata],
        ) -> Result<(), CacheError> {
            let folder_path = &snapshot.source_root;
            self.replace_attempts.push(folder_path.to_string());
            if self
                .fail_replace_for
                .iter()
                .any(|folder| folder == folder_path)
            {
                return Err(CacheError::Io("injected replace failure".to_string()));
            }
            Ok(())
        }

        fn remove_source(
            &mut self,
            folder_path: &str,
            _scope: FontDirectoryScope,
        ) -> Result<(), CacheError> {
            self.remove_attempts.push(folder_path.to_string());
            if self
                .fail_remove_for
                .iter()
                .any(|folder| folder == folder_path)
            {
                return Err(CacheError::Io("injected remove failure".to_string()));
            }
            Ok(())
        }
    }

    fn missing_child_path(guard: &TempCacheDir, name: &str) -> String {
        guard.0.join(name).display().to_string()
    }

    fn shallow_snapshot(root: &str, mtime: i64) -> CacheSourceSnapshot {
        CacheSourceSnapshot {
            source_root: root.to_string(),
            scope: FontDirectoryScope::Shallow,
            directories: vec![crate::font_cache::FolderSnapshot {
                folder_path: root.to_string(),
                folder_mtime: mtime,
            }],
            files: Vec::new(),
        }
    }

    fn shallow_key(root: &str) -> CacheSourceKey {
        CacheSourceKey {
            source_root: root.to_string(),
            scope: FontDirectoryScope::Shallow,
        }
    }

    fn assert_single_apply_failed(skipped: &[SkippedFolder], folder: &str, reason_part: &str) {
        let matches: Vec<&SkippedFolder> = skipped
            .iter()
            .filter(|entry| entry.folder == folder && entry.kind == SkipKind::ApplyFailed)
            .collect();
        assert_eq!(
            matches.len(),
            1,
            "expected one ApplyFailed entry for {folder}, got {skipped:?}"
        );
        assert!(
            matches[0].reason.contains(reason_part),
            "ApplyFailed reason should mention {reason_part:?}, got {:?}",
            matches[0].reason
        );
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
            scope: FontDirectoryScope::Shallow,
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
        let mut cache = FakeRescanCache::default();
        let scanned = vec![(shallow_snapshot("/folder/a", 999), vec![])];
        let mut skipped: Vec<SkippedFolder> = Vec::new();
        let (modified, evicted) = apply_rescan_to_cache(&mut cache, &scanned, &[], &mut skipped);
        assert_eq!(modified, 1);
        assert_eq!(evicted, 0);
        assert!(skipped.is_empty(), "no errors expected");

        assert_eq!(cache.replace_attempts, vec!["/folder/a"]);
        assert!(cache.remove_attempts.is_empty());
    }

    #[test]
    fn apply_rescan_does_not_evict_removed_that_reappeared() {
        // Existing re-walk dance: a source reported as removed in
        // Phase 1 may have been re-populated by a concurrent command
        // by the time Phase 3 runs. Eviction must skip when the
        // source is back on disk.
        let (guard, mut cache) = temp_cache("removed_reappeared");
        let real_path = guard.0.to_string_lossy().to_string();
        cache.replace_folder(&real_path, 100, &[]).unwrap();

        let removed = confirm_removed_sources_before_apply(&[shallow_key(&real_path)]);
        assert!(
            removed.is_empty(),
            "reappeared source must be filtered before DB apply"
        );
        let mut skipped: Vec<SkippedFolder> = Vec::new();
        let (_, evicted) = apply_rescan_to_cache(&mut cache, &[], &removed, &mut skipped);
        assert_eq!(evicted, 0, "reappeared source should be left alone");
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
    fn unreadable_existing_source_is_modified_not_removed() {
        let guard = TempCacheDir::new("unreadable_existing");
        let root = guard.0.join("library");
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join("evil\u{202e}ttf.ttf"), b"candidate").unwrap();
        let root =
            crate::fonts::normalize_canonical_path(root.canonicalize().unwrap().to_str().unwrap());
        let source = crate::font_cache::CacheSourceRecord {
            source_root: root.clone(),
            scope: FontDirectoryScope::Recursive,
            source_order: 1,
            last_scanned_at: 1,
            directories: Vec::new(),
            files: Vec::new(),
        };
        let (snapshots, unreadable_existing) = collect_live_source_snapshots(&[source]);
        assert!(snapshots.is_empty());
        let key = shallow_key(&root);
        let recursive_key = CacheSourceKey {
            scope: FontDirectoryScope::Recursive,
            ..key
        };
        assert_eq!(unreadable_existing, vec![recursive_key.clone()]);

        let mut report = DriftReport {
            removed: vec![recursive_key.clone()],
            ..Default::default()
        };
        classify_unreadable_existing_as_modified(&mut report, &unreadable_existing);
        assert!(report.removed.is_empty());
        assert_eq!(report.modified, vec![recursive_key]);
    }

    #[test]
    fn apply_rescan_evicts_removed_that_no_longer_resolves() {
        // A source that no longer passes the complete metadata walk
        // must still be evicted — Phase 3 must
        // NOT short-circuit to "reappeared".
        let (_guard, mut cache) = temp_cache("removed_actually_gone");
        let bogus = "/bogus/definitely-not-a-real-folder/round-2";
        cache.replace_folder(bogus, 100, &[]).unwrap();
        let removed = confirm_removed_sources_before_apply(&[shallow_key(bogus)]);
        assert_eq!(removed, vec![shallow_key(bogus)]);
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
        // Pins the input-preservation side of ApplyFailed handling:
        // a pre-existing `SkippedFolder { kind: ApplyFailed }`
        // survives alongside successful operations instead of being
        // wiped or rewritten.
        let mut cache = FakeRescanCache::default();

        let scanned = vec![(shallow_snapshot("/folder/x", 999), vec![])];
        let mut skipped = vec![SkippedFolder {
            folder: "/already/failed".to_string(),
            scope: FontDirectoryScope::Shallow,
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

    #[test]
    fn apply_rescan_continues_after_replace_apply_failed() {
        let guard = TempCacheDir::new("replace_apply_failed");
        let first = missing_child_path(&guard, "first");
        let failing = missing_child_path(&guard, "failing");
        let third = missing_child_path(&guard, "third");
        let scanned = vec![
            (shallow_snapshot(&first, 100), Vec::new()),
            (shallow_snapshot(&failing, 200), Vec::new()),
            (shallow_snapshot(&third, 300), Vec::new()),
        ];
        let mut fake = FakeRescanCache {
            fail_replace_for: vec![failing.clone()],
            ..Default::default()
        };
        let mut skipped = Vec::new();

        let (modified, evicted) = apply_rescan_to_cache(&mut fake, &scanned, &[], &mut skipped);

        assert_eq!(modified, 2, "both non-failing replaces should count");
        assert_eq!(evicted, 1, "failed replacement must evict its stale row");
        assert_eq!(
            fake.replace_attempts,
            vec![first, failing.clone(), third],
            "replace failures must not short-circuit later folders"
        );
        assert_eq!(fake.remove_attempts, vec![failing.clone()]);
        assert_single_apply_failed(&skipped, &failing, "replace_source failed");
    }

    #[test]
    fn changed_between_scan_and_apply_is_evicted_fail_closed() {
        let (guard, mut cache) = temp_cache("changed_before_apply");
        let root = guard.0.join("library");
        fs::create_dir_all(&root).unwrap();
        let candidate = root.join("changing.ttf");
        fs::write(&candidate, b"first candidate state").unwrap();
        let snapshot = snapshot_source_directories(&root, FontDirectoryScope::Recursive).unwrap();
        cache.replace_source(&snapshot, &[]).unwrap();

        fs::write(&candidate, b"second, visibly longer candidate state").unwrap();
        let mut skipped = Vec::new();
        let validated =
            validate_scanned_sources_before_apply(vec![(snapshot.clone(), vec![])], &mut skipped);
        assert!(validated.is_empty());
        assert_eq!(skipped.len(), 1);
        assert_eq!(skipped[0].kind, SkipKind::ScanFailed);

        let (_, evicted) = apply_rescan_to_cache(&mut cache, &validated, &[], &mut skipped);
        assert_eq!(evicted, 1);
        assert!(cache.list_sources().unwrap().is_empty());
    }

    #[test]
    fn apply_rescan_continues_after_removed_apply_failed() {
        let guard = TempCacheDir::new("removed_apply_failed");
        let first = missing_child_path(&guard, "first");
        let failing = missing_child_path(&guard, "failing");
        let third = missing_child_path(&guard, "third");
        let removed = vec![
            shallow_key(&first),
            shallow_key(&failing),
            shallow_key(&third),
        ];
        let mut fake = FakeRescanCache {
            fail_remove_for: vec![failing.clone()],
            ..Default::default()
        };
        let mut skipped = Vec::new();

        let (modified, evicted) = apply_rescan_to_cache(&mut fake, &[], &removed, &mut skipped);

        assert_eq!(modified, 0);
        assert_eq!(evicted, 2, "both non-failing evictions should count");
        assert_eq!(
            fake.remove_attempts,
            vec![first, failing.clone(), third],
            "remove failures must not short-circuit later removed sources"
        );
        assert_single_apply_failed(&skipped, &failing, "remove_source failed");
    }

    #[test]
    fn apply_rescan_continues_after_scan_failed_eviction_apply_failed() {
        let guard = TempCacheDir::new("scan_failed_eviction_apply_failed");
        let first = missing_child_path(&guard, "first");
        let failing = missing_child_path(&guard, "failing");
        let third = missing_child_path(&guard, "third");
        let mut fake = FakeRescanCache {
            fail_remove_for: vec![failing.clone()],
            ..Default::default()
        };
        let mut skipped = vec![
            SkippedFolder {
                folder: first.clone(),
                scope: FontDirectoryScope::Shallow,
                reason: "scan failed first".to_string(),
                kind: SkipKind::ScanFailed,
            },
            SkippedFolder {
                folder: failing.clone(),
                scope: FontDirectoryScope::Shallow,
                reason: "scan failed failing".to_string(),
                kind: SkipKind::ScanFailed,
            },
            SkippedFolder {
                folder: third.clone(),
                scope: FontDirectoryScope::Shallow,
                reason: "scan failed third".to_string(),
                kind: SkipKind::ScanFailed,
            },
        ];

        let (modified, evicted) = apply_rescan_to_cache(&mut fake, &[], &[], &mut skipped);

        assert_eq!(modified, 0);
        assert_eq!(
            evicted, 2,
            "both non-failing stale-row evictions should count"
        );
        assert_eq!(
            fake.remove_attempts,
            vec![first.clone(), failing.clone(), third.clone()],
            "scan-failed eviction failures must not short-circuit later folders"
        );
        assert_eq!(
            skipped
                .iter()
                .filter(|entry| entry.kind == SkipKind::ScanFailed)
                .count(),
            3,
            "original ScanFailed entries should stay visible to the UI"
        );
        assert_single_apply_failed(&skipped, &failing, "remove_source");
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

    #[test]
    fn rebuild_refuses_to_report_success_when_main_database_cannot_be_deleted() {
        let guard = TempCacheDir::new("main_delete_failure");
        let main_path = guard.0.join("cache-as-directory.sqlite3");
        fs::create_dir_all(&main_path).unwrap();

        let error = remove_cache_files_for_rebuild(std::slice::from_ref(&main_path))
            .expect_err("a non-deletable main database target must abort rebuild");
        assert!(error.contains("Cannot remove the existing font-cache database"));
        assert!(
            main_path.is_dir(),
            "failed delete must not be reported as clear success"
        );
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
        // cached source identities + generation, then
        // `clear_font_cache` republishing a fresh empty cache (which
        // bumps the generation), then Phase 3 calling finalize_drift
        // with a cache reference that no longer matches the snapshot.
        // Without the generation check, the snapshot's sources would
        // leak into `added`, violating the documented "added is always
        // empty for the GUI path" contract. With the check, Phase 3
        // returns DriftReport::default().
        let (_guard, cache) = temp_cache("fin_drift_gen_changed");
        // Pre-clear snapshot: two sources the user previously had in
        // their cache. The fresh post-clear `cache` we pass in does
        // NOT contain them.
        let snapshot = vec![
            shallow_snapshot("/legacy/folder/a", 100),
            shallow_snapshot("/legacy/folder/b", 200),
        ];
        let unreadable = vec![shallow_key("/legacy/folder/a")];
        let report = finalize_drift(Some(&cache), &snapshot, &unreadable, 5, 6).unwrap();
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
        let snapshot = vec![shallow_snapshot("/folder/a", 100)];
        let report = finalize_drift(None, &snapshot, &[], 0, 0).unwrap();
        assert!(report.added.is_empty());
        assert!(report.modified.is_empty());
        assert!(report.removed.is_empty());
    }

    #[test]
    fn finalize_drift_returns_diff_when_generation_matches() {
        // Counter-test: when the generation didn't change between
        // Phase 1 and Phase 3 (no clear interleaved), diff_sources
        // runs and reports real drift. Seeds /folder/a with mtime
        // 100; passes a snapshot with mtime 999 (mtime mismatch
        // → reported as modified).
        let (_guard, mut cache) = temp_cache("fin_drift_gen_matches");
        cache.replace_folder("/folder/a", 100, &[]).unwrap();
        let snapshot = vec![shallow_snapshot("/folder/a", 999)];
        let report = finalize_drift(Some(&cache), &snapshot, &[], 42, 42).unwrap();
        assert_eq!(
            report.modified,
            vec![shallow_key("/folder/a")],
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
