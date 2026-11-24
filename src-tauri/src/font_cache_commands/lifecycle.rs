use std::path::{Path, PathBuf};

use crate::font_cache::{
    validate_cache_source_stability, CacheError, CacheSourceSnapshot, FontCache,
    FontDirectoryScope, FontMetadata,
};
use crate::fonts::entries_to_cache_metadata;

use super::*;

pub(super) fn sanitize_gui_cache_init_failure(message: &str) -> String {
    let mut sanitized = String::with_capacity(message.len().min(MAX_GUI_CACHE_INIT_FAILURE_CHARS));
    let mut chars = message.chars();
    for _ in 0..MAX_GUI_CACHE_INIT_FAILURE_CHARS {
        let Some(ch) = chars.next() else {
            return sanitized;
        };
        if ch.is_control()
            || matches!(
                ch,
                '\u{2028}' | '\u{2029}'
                    | '\u{200B}'..='\u{200D}'
                    | '\u{2060}'
                    | '\u{180E}'
                    | '\u{FEFF}'
                    | '\u{061C}'
                    | '\u{200E}'
                    | '\u{200F}'
                    | '\u{202A}'..='\u{202E}'
                    | '\u{2066}'..='\u{2069}'
            )
        {
            sanitized.push(' ');
        } else {
            sanitized.push(ch);
        }
    }
    if chars.next().is_some() {
        sanitized.push('…');
    }
    sanitized
}

pub(super) fn store_gui_cache_init_failure(message: String) -> Result<(), String> {
    let mut failure_slot = GUI_FONT_CACHE_INIT_FAILURE
        .lock()
        .map_err(|_| "GUI cache initialization-error mutex poisoned".to_string())?;
    *failure_slot = Some(sanitize_gui_cache_init_failure(&message));
    Ok(())
}

pub(super) fn clear_gui_cache_init_failure() -> Result<(), String> {
    let mut failure_slot = GUI_FONT_CACHE_INIT_FAILURE
        .lock()
        .map_err(|_| "GUI cache initialization-error mutex poisoned".to_string())?;
    *failure_slot = None;
    Ok(())
}

pub(super) fn gui_cache_init_failure() -> Result<Option<String>, String> {
    GUI_FONT_CACHE_INIT_FAILURE
        .lock()
        .map_err(|_| "GUI cache initialization-error mutex poisoned".to_string())
        .map(|failure| failure.clone())
}

pub(super) fn reset_gui_cache_state_for_init() -> Result<(), String> {
    {
        let mut cache_slot = GUI_FONT_CACHE
            .lock()
            .map_err(|_| "GUI cache mutex poisoned".to_string())?;
        *cache_slot = None;
        finish_gui_cache_mutation();
    }
    {
        let mut path_slot = GUI_FONT_CACHE_PATH
            .lock()
            .map_err(|_| "GUI cache path mutex poisoned".to_string())?;
        *path_slot = None;
    }
    clear_gui_cache_init_failure()
}

pub(super) fn gui_cache_directory_failure_for_ipc(error: &std::io::Error) -> String {
    let kind = match error.kind() {
        std::io::ErrorKind::NotFound => "path not found",
        std::io::ErrorKind::PermissionDenied => "permission denied",
        std::io::ErrorKind::AlreadyExists | std::io::ErrorKind::NotADirectory => {
            "a non-directory entry already exists there"
        }
        std::io::ErrorKind::InvalidInput => "invalid path",
        _ => "filesystem error",
    };
    format!("GUI font cache initialization failed while creating its data directory: {kind}")
}

pub(super) fn gui_cache_open_failure_for_ipc(cache_path: &Path, error: &CacheError) -> String {
    let path = cache_path.display().to_string();
    let redacted_detail = error.to_string().replace(&path, "<font-cache path>");
    format!("GUI font cache initialization failed: {redacted_detail}")
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
    // Setup normally calls this exactly once. Reset all three slots anyway so a
    // future explicit retry cannot retain a stale live handle, path, or error.
    reset_gui_cache_state_for_init()?;
    // `app_data_dir` here is resolved via the caller in `lib.rs`
    // which passes `font_cache::unified_app_data_dir()` — chain is
    // `std::env::var("APPDATA")`; an alternative resolution exists via Tauri's
    // `app.path().app_data_dir()` (the `$DATA` capability scope
    // variable resolution), used at a different layer. Both
    // chains land inside the user's own AppData —
    // planting a reparse-point in the parent walk requires AppData
    // write access. Same local-user class as parent-walk reparse on AppData.
    // Defending here would mean a parent-walk reparse scan on every
    // startup, duplicating the FontCache::open_or_create boundary
    // check — and contradicting the locked single-user-desktop threat
    // model. Revisit if the project ships in a multi-user /
    // MDM-managed deployment.
    if let Err(error) = std::fs::create_dir_all(app_data_dir) {
        let detailed = format!(
            "Cannot create app data dir '{}': {error}",
            app_data_dir.display()
        );
        store_gui_cache_init_failure(gui_cache_directory_failure_for_ipc(&error))?;
        return Err(detailed);
    }
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
            clear_gui_cache_init_failure()?;
            let mut slot = GUI_FONT_CACHE
                .lock()
                .map_err(|_| "GUI cache mutex poisoned".to_string())?;
            *slot = Some(cache);
            Ok(())
        }
        Err(CacheError::SchemaVersionMismatch { found, expected }) => {
            // Schema mismatch has its own structured status and recovery path;
            // never let a stale ordinary-init error take precedence over it.
            clear_gui_cache_init_failure()?;
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
            store_gui_cache_init_failure(gui_cache_open_failure_for_ipc(&cache_path, &e))?;
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

// ---- Blocking cache operations -----------------------------------------

/// Report the current cache status. Useful for the launch-time check
/// (frontend asks "is cache ready?" before calling detect_drift) and
/// for re-checking after `clear_font_cache`.
pub fn open_font_cache() -> Result<CacheStatus, String> {
    // A stored non-schema failure is more precise than the absence-derived
    // "setup did not run" fallback and must win over schema-mismatch inference.
    if let Some(failure) = gui_cache_init_failure()? {
        return Err(failure);
    }
    let path = GUI_FONT_CACHE_PATH
        .lock()
        .map_err(|_| "GUI cache path mutex poisoned".to_string())?
        .clone();
    let path = match path {
        Some(path) => path,
        None => {
            return Err(gui_cache_init_failure()?
                .unwrap_or_else(|| "Cache path not initialized; setup did not run".to_string()));
        }
    };
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
        .clone();
    let path = match path {
        Some(path) => path,
        None => {
            return Err(gui_cache_init_failure()?
                .unwrap_or_else(|| "Cache path not initialized; setup did not run".to_string()));
        }
    };

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
    let cleared_live_cache = {
        let mut slot = GUI_FONT_CACHE
            .lock()
            .map_err(|_| "GUI cache mutex poisoned".to_string())?;
        if let Some(cache) = slot.as_mut() {
            cache
                .clear_sources()
                .map_err(|e| format!("clear cached font sources: {e}"))?;
            finish_gui_cache_mutation();
            true
        } else {
            false
        }
    };
    if cleared_live_cache {
        clear_gui_cache_init_failure()?;
        return Ok(());
    }

    // No live handle means initialization rejected the file (normally a
    // schema mismatch). Rebuilding requires deleting the incompatible main
    // database. Main-file deletion is mandatory; sidecar cleanup remains
    // best-effort because a fresh SQLite open can safely decide whether any
    // surviving journal is usable.
    remove_cache_files_for_rebuild(&paths)?;
    let fresh = match FontCache::open_or_create(&path) {
        Ok(cache) => cache,
        Err(error) => {
            let detailed = format!("re-create cache at {}: {error}", path.display());
            store_gui_cache_init_failure(gui_cache_open_failure_for_ipc(&path, &error))?;
            return Err(detailed);
        }
    };
    clear_gui_cache_init_failure()?;
    let mut slot = GUI_FONT_CACHE
        .lock()
        .map_err(|_| "GUI cache mutex poisoned".to_string())?;
    finish_gui_cache_mutation();
    *slot = Some(fresh);
    Ok(())
}

pub(super) fn remove_cache_files_for_rebuild(paths: &[PathBuf]) -> Result<(), String> {
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
