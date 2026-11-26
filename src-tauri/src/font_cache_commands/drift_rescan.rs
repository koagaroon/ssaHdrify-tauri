use std::path::Path;
use std::sync::atomic::Ordering;

use crate::font_cache::{
    snapshot_source_directories, validate_cache_source_stability, CacheError, CacheSourceKey,
    CacheSourceSnapshot, FontCache, FontDirectoryScope, FontMetadata,
};
use crate::fonts::entries_to_cache_metadata;

use super::*;

pub(super) fn collect_live_source_snapshots(
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

pub(super) fn classify_unreadable_existing_as_modified(
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
pub(super) fn finalize_drift(
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

pub(super) fn validate_scanned_sources_before_apply(
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

pub(super) fn confirm_removed_sources_before_apply(
    removed: &[CacheSourceKey],
) -> Vec<CacheSourceKey> {
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

pub(super) trait RescanCacheWriter {
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
pub(super) fn apply_rescan_to_cache<C: RescanCacheWriter>(
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
