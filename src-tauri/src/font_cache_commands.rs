//! Blocking implementations and shared state for the persistent font cache.
//!
//! `font_cache.rs` itself stays Tauri-free so the CLI binary can use it
//! without pulling in the GUI's IPC layer. The async Tauri wrappers live in
//! `ipc_commands`; this module owns the static `Mutex<Option<FontCache>>`
//! initialized during Tauri setup plus the synchronous operations used by the
//! React drift modal and embed-time lookup tier:
//! - `lifecycle`: cache migration, setup initialization, status/clear/rebuild,
//!   session-source sync hooks, and family lookup
//! - `drift_rescan`: non-blocking three-phase drift detection and rescan apply

mod drift_rescan;
mod lifecycle;
#[cfg(test)]
mod tests;

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Mutex;

use once_cell::sync::Lazy;
use serde::Serialize;

use crate::font_cache::{CacheSourceKey, FontCache, FontDirectoryScope};

pub use drift_rescan::{detect_font_cache_drift, rescan_font_cache_drift};
pub(crate) use lifecycle::{
    clear_all_sources_in_gui_cache_locked, record_source_in_gui_cache_locked,
    remove_source_from_gui_cache_locked,
};
pub use lifecycle::{
    clear_font_cache, init_gui_font_cache, lookup_font_family, migrate_legacy_gui_cache,
    open_font_cache,
};

#[allow(unused_imports)]
use drift_rescan::*;
#[allow(unused_imports)]
use lifecycle::*;

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
/// setup and consumed by the five commands. `None` after a schema mismatch or
/// another non-fatal initialization error. Only schema mismatch is represented
/// as rebuild-required status; ordinary failures are returned from
/// `open_font_cache` through the separate stored-error slot below.
static GUI_FONT_CACHE: Lazy<Mutex<Option<FontCache>>> = Lazy::new(|| Mutex::new(None));

/// Cache file path published separately from the live handle so
/// `clear_font_cache` can drop the connection AND wipe the file even
/// when `GUI_FONT_CACHE` is `None` (schema-mismatch recovery path).
static GUI_FONT_CACHE_PATH: Lazy<Mutex<Option<PathBuf>>> = Lazy::new(|| Mutex::new(None));

/// User-safe reason for the most recent non-schema initialization failure.
/// The path slot deliberately remains empty for those failures so an ordinary
/// permission or open error cannot be mistaken for a rebuildable schema
/// mismatch. `open_font_cache` reads this slot before inferring status.
static GUI_FONT_CACHE_INIT_FAILURE: Lazy<Mutex<Option<String>>> = Lazy::new(|| Mutex::new(None));

const MAX_GUI_CACHE_INIT_FAILURE_CHARS: usize = 1_024;

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
    /// shorter form differs from the internal source-record field, but
    /// the trade is "shorter UI-facing field name vs internal-storage
    /// descriptor" — keep the TS pairing.
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
