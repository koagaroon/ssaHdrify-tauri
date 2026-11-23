//! Bounded directory discovery, preflight checks, streaming/blocking font
//! scans, and scan-id cancellation state.

use super::*;
use crate::util::{validate_ipc_path, MAX_INPUT_PATHS};
use rusqlite::{params, OptionalExtension};
use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

/// No frontend-created scan may use this id. Keeping zero reserved lets the
/// Rust side distinguish "no active/cancelled scan" from real work.
pub(super) const NO_SCAN_ID: u64 = 0;

/// Scan id currently owned by the blocking scan worker. The UI only starts
/// one scan at a time, but this guard also prevents a compromised frontend
/// from launching overlapping scans that would race the shared provenance
/// cache and cancellation state.
pub(super) static ACTIVE_SCAN_ID: AtomicU64 = AtomicU64::new(NO_SCAN_ID);

/// Highest scan id that has received a cancel request. Cancel requests are
/// targeted by id instead of using a process-wide boolean, so a late cancel
/// from an older scan cannot abort a fresh one. `fetch_max` also preserves a
/// real current cancel if an older stale command arrives afterward.
pub(super) static CANCEL_SCAN_ID: AtomicU64 = AtomicU64::new(NO_SCAN_ID);

/// Streaming progress event for the font scan commands. The `Batch` variant
/// carries only a cumulative parsed-face count; `Done` is the end-of-stream
/// sentinel the frontend awaits before trusting the registered source count.
///
/// **Why Done is required**: Tauri's `Channel` uses two delivery paths
/// internally. Payloads under 8 KB go via direct `webview.eval()` and fire
/// the JS callback synchronously *during* the command execution. Payloads
/// ≥ 8 KB use an async `plugin:__TAURI_CHANNEL__|fetch` round-trip — those
/// callbacks fire *after* the command's invoke promise has already
/// resolved. Without a sentinel the frontend could report completion before
/// every progress callback had drained.
/// `Done` is small (under the threshold), travels via direct eval, but the
/// Channel layer enforces in-order delivery — so the frontend's `Done`
/// handler only fires *after* every preceding `Batch` has been processed.
/// The async-after-invoke-resolve case is pinned by the frontend
/// `runStreamingScan` tests.
///
/// Wire-format mirror lives in `src/lib/tauri-api.ts` as
/// `RawScanProgress`. The two definitions are NOT generated from each
/// other — renaming a variant or adding a field on this side without
/// updating the TS union will silently break the frontend's channel
/// callback. Keep both sides in lockstep within one commit.
#[derive(serde::Serialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum ScanProgress {
    /// Progress after one chunk of newly-parsed faces. Multiple `Batch`
    /// events are emitted per scan; the frontend only needs the cumulative
    /// count because the heavy source index stays in Rust.
    Batch { total: usize },
    /// End-of-stream sentinel. Always emitted on the `Ok` path (natural
    /// completion, user cancel, or defense-in-depth cap stop). NOT
    /// emitted on the `Err` path — the invoke rejection already signals
    /// failure and the frontend must not block waiting for a `Done` that
    /// will never arrive.
    ///
    /// `reason` carries why the scan stopped — see `ScanStopReason`.
    ///
    /// Payload-size invariant: this variant serializes to roughly
    /// 80 bytes JSON (a short tag string + three small ints), well
    /// under the 8 KB Tauri Channel direct-eval threshold. As long as
    /// `reason` stays a single discriminant and the count fields stay
    /// `usize`, Done always travels via the synchronous direct-eval
    /// path. Future fields must keep the serialized size strictly
    /// under 8 KB (Tauri Channel's direct-eval threshold) — string
    /// fields are the risk. Bound them at the API boundary OR
    /// aggregate into counts; never let a Vec<String> or unbounded
    /// String slip in. Keep this invariant mirrored in the TS
    /// `RawScanProgress` contract.
    Done {
        reason: ScanStopReason,
        added: usize,
        duplicated: usize,
    },
}

/// Why a font scan stopped. Single-variant enum lets frontend and test code
/// pattern-match all four terminal states exhaustively.
///
/// Wire format: serializes as a bare lowercased camelCase string
/// (`"natural"`, `"userCancel"`, `"ceilingHit"`) because the variants
/// have no payload.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub enum ScanStopReason {
    /// Scan walked the entire input and finished naturally.
    Natural,
    /// User pressed Cancel during the scan.
    UserCancel,
    /// A defense-in-depth ceiling fired. Two ceilings collapse into
    /// this variant — the per-scan log line distinguishes which:
    /// - `MAX_FONTS_PER_SCAN` face ceiling
    /// - `MAX_PREFLIGHT_ENTRIES` dedup-set ceiling (previously the
    ///   dedup break reported `Natural`, silently truncating to the
    ///   user)
    ///
    /// Partial results are preserved on the way out in both cases.
    CeilingHit,
    /// One or more descendant paths became unreadable or unstable during a
    /// directory walk. Already-imported faces remain available for this
    /// session, but this outcome is never eligible for persistent caching.
    IncompleteIo,
}

#[derive(Debug, Clone, Copy)]
pub(super) struct ScanOutcome {
    pub(super) total: usize,
    /// Why this scan stopped. Forwarded into `ScanProgress::Done.reason`
    /// after the SQLite import completes.
    pub(super) reason: ScanStopReason,
}

#[derive(Debug, Clone, Copy)]
pub(super) struct StreamingScanCommandResult {
    pub(super) outcome: ScanOutcome,
    pub(super) import: ImportOutcome,
    pub(super) cache_truncated: bool,
}

pub(super) fn is_cache_publication_eligible(result: &StreamingScanCommandResult) -> bool {
    result.outcome.reason == ScanStopReason::Natural
        && !result.cache_truncated
        && result.import.added > 0
}

pub(super) struct ActiveScanGuard {
    scan_id: u64,
}

impl Drop for ActiveScanGuard {
    fn drop(&mut self) {
        let _ = ACTIVE_SCAN_ID.compare_exchange(
            self.scan_id,
            NO_SCAN_ID,
            Ordering::SeqCst,
            Ordering::SeqCst,
        );
        // Release CANCEL_SCAN_ID's claim on this scan's id, if any.
        // CANCEL_SCAN_ID is a monotonic high-water
        // mark via fetch_max, so a finished scan whose id was the
        // current max would leave the cancel state "poisoned" — every
        // subsequent scan with a lower id (Date.now()-seeded ids are
        // ~1.7e12, far below u64::MAX) would silently fail to cancel
        // because fetch_max(lower) cannot reduce CANCEL_SCAN_ID. Reset
        // back to NO_SCAN_ID atomically when this scan was the high
        // mark; any newer scan that won the slot will reset it again
        // on its own Drop, so the eventual-state invariant holds.
        let _ = CANCEL_SCAN_ID.compare_exchange(
            self.scan_id,
            NO_SCAN_ID,
            Ordering::SeqCst,
            Ordering::SeqCst,
        );
    }
}

/// Reserve `scan_id` as the active scan, returning a Drop-guarded handle
/// that releases it on exit. Called BEFORE `spawn_blocking` so that:
/// (a) the IPC command can fail synchronously with "Another font scan is
/// already running" instead of spawning a thread that immediately exits,
/// and (b) the guard's Drop (clearing ACTIVE_SCAN_ID via
/// `compare_exchange(scan_id, NO_SCAN_ID)`) runs on whichever thread
/// owns the guard at exit time, regardless of whether the closure in
/// `spawn_blocking` returned Ok or Err.
///
/// SeqCst on the CAS pairs with the SeqCst load in `cancel_font_scan`
/// to give a total order across all ACTIVE_SCAN_ID accesses — needed
/// so cancel_font_scan's range check can never see a stale NO_SCAN_ID
/// while a fresh scan has already won the slot.
pub(super) fn begin_font_scan(scan_id: u64) -> Result<ActiveScanGuard, String> {
    // Public IPC commands now validate scan_id != NO_SCAN_ID at the
    // boundary; debug_assert catches any future internal caller that
    // bypasses that gate. Release builds skip the check entirely so
    // we don't pay for it on the spawn path of every legitimate scan.
    debug_assert!(
        scan_id != NO_SCAN_ID,
        "begin_font_scan called with NO_SCAN_ID"
    );

    ACTIVE_SCAN_ID
        .compare_exchange(NO_SCAN_ID, scan_id, Ordering::SeqCst, Ordering::SeqCst)
        .map_err(|_| "Another font scan is already running".to_string())?;

    let guard = ActiveScanGuard { scan_id };

    // After winning the slot, the only legitimate way `CANCEL_SCAN_ID
    // == scan_id` is that this scan's owner already issued a cancel for
    // it — either pre-armed (frontend wrote CANCEL_SCAN_ID before
    // begin_font_scan ran) or post-CAS-pre-load (cancel_font_scan
    // raced past our CAS, observed ACTIVE_SCAN_ID == scan_id, and
    // wrote CANCEL_SCAN_ID = scan_id before this load fired). Both
    // shapes mean "user wants this scan cancelled"; bail.
    //
    // The previous design unconditionally cleared CANCEL_SCAN_ID here,
    // which closed the pre-arm case but silently overwrote the post-
    // CAS race — a real cancel click arriving in that window was lost.
    // Check-and-bail closes both at once. Drop the guard explicitly so
    // ACTIVE_SCAN_ID is released before the caller sees the error.
    if CANCEL_SCAN_ID.load(Ordering::SeqCst) == scan_id {
        drop(guard);
        return Err("Font scan was cancelled".to_string());
    }

    Ok(guard)
}

pub(super) fn font_scan_cancelled(scan_id: u64) -> bool {
    // Acquire load pairs with the Release fetch_max in `cancel_font_scan`.
    scan_id != NO_SCAN_ID && CANCEL_SCAN_ID.load(Ordering::Acquire) == scan_id
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FontScanPreflight {
    pub(super) font_files: usize,
    pub(super) total_bytes: u64,
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FontDirectoryScanPreflight {
    pub(super) font_files: usize,
    pub(super) total_bytes: u64,
    pub(super) reason: ScanStopReason,
}

#[derive(Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FontDirectoryScanCommandResult {
    pub(super) cache_disposition: FontCacheDisposition,
}

#[derive(Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) enum FontCacheDisposition {
    Cached,
    SessionOnly,
}

#[derive(Debug, Clone)]
pub(super) struct DiscoveredFontFile {
    pub(super) path: PathBuf,
    pub(super) size_bytes: u64,
    pub(super) modified_at: i64,
}

#[derive(Debug, Clone)]
pub(super) struct DirectoryDiscovery {
    pub(super) files: Vec<DiscoveredFontFile>,
    pub(super) directories: Vec<crate::font_cache::FolderSnapshot>,
    pub(super) candidate_files: Vec<crate::font_cache::FileSnapshot>,
    pub(super) reason: ScanStopReason,
}

pub(super) fn checked_canonical_path(path: &Path, root: &Path) -> Result<PathBuf, String> {
    let canonical = path
        .canonicalize()
        .map_err(|_| "A font-library entry became unreadable".to_string())?;
    if !canonical.starts_with(root) {
        return Err("A font-library entry resolved outside the selected root".to_string());
    }
    let normalized = normalize_canonical_path(
        canonical
            .to_str()
            .ok_or_else(|| "A font-library path is not valid UTF-8".to_string())?,
    );
    validate_ipc_path(&normalized, "Discovered font")?;
    Ok(canonical)
}

/// Deterministic, iterative depth-first discovery shared by recursive
/// preflight and parsing. It never follows symlinks/junctions/reparse points,
/// applies global budgets across the selected root, and polls cancellation
/// during metadata walking rather than waiting for font parsing to begin.
pub(super) fn discover_font_files(
    canonical_root: &Path,
    scope: FontDirectoryScope,
    scan_id: u64,
) -> Result<DirectoryDiscovery, String> {
    match crate::util::try_is_reparse_point(canonical_root) {
        Ok(false) => {}
        Ok(true) => return Err("Font library root cannot be a symlink or junction".to_string()),
        Err(_) => return Err("Cannot inspect font library root".to_string()),
    }

    let mut stack = vec![(canonical_root.to_path_buf(), 0usize)];
    let mut files = Vec::new();
    let mut seen_font_paths = HashSet::new();
    let mut directories = Vec::new();
    let mut candidate_files = Vec::new();
    let mut visited_entries = 0usize;
    let mut visited_directories = 0usize;
    let mut retained_path_bytes = canonical_root.as_os_str().len();
    let mut total_font_bytes = 0u64;
    let mut incomplete_io = false;

    macro_rules! discovery_result {
        ($reason:expr) => {
            DirectoryDiscovery {
                files,
                directories,
                candidate_files,
                reason: $reason,
            }
        };
    }

    while let Some((directory, depth)) = stack.pop() {
        if font_scan_cancelled(scan_id) {
            return Ok(discovery_result!(ScanStopReason::UserCancel));
        }
        if visited_directories >= MAX_SCAN_DIRECTORIES || depth > MAX_SCAN_DEPTH {
            return Ok(discovery_result!(ScanStopReason::CeilingHit));
        }
        visited_directories += 1;

        let normalized_directory = normalize_canonical_path(
            directory
                .to_str()
                .ok_or_else(|| "A font-library directory is not valid UTF-8".to_string())?,
        );
        if validate_ipc_path(&normalized_directory, "Discovered directory").is_err() {
            if directory == canonical_root {
                return Err("Font library root cannot be represented safely".to_string());
            }
            incomplete_io = true;
            continue;
        }
        match crate::font_cache::try_modified_at(&directory) {
            Some(folder_mtime) => directories.push(crate::font_cache::FolderSnapshot {
                folder_path: normalized_directory,
                folder_mtime,
            }),
            None if directory == canonical_root => {
                return Err("Cannot read font library root timestamp".to_string())
            }
            None => {
                incomplete_io = true;
                continue;
            }
        }

        let read = match fs::read_dir(&directory) {
            Ok(read) => read,
            Err(error) if directory == canonical_root => {
                log::warn!("read font-library root failed: {error}");
                return Err("Cannot read directory".to_string());
            }
            Err(error) => {
                log::info!("read nested font directory failed: {error}");
                incomplete_io = true;
                continue;
            }
        };

        let mut children = Vec::new();
        for entry in read {
            if font_scan_cancelled(scan_id) {
                return Ok(discovery_result!(ScanStopReason::UserCancel));
            }
            if visited_entries >= MAX_PREFLIGHT_ENTRIES {
                return Ok(discovery_result!(ScanStopReason::CeilingHit));
            }
            visited_entries += 1;
            match entry {
                Ok(entry) => {
                    let path = entry.path();
                    retained_path_bytes =
                        retained_path_bytes.saturating_add(path.as_os_str().len());
                    if retained_path_bytes > MAX_SCAN_PATH_BYTES {
                        return Ok(discovery_result!(ScanStopReason::CeilingHit));
                    }
                    children.push(path);
                }
                Err(_) => incomplete_io = true,
            }
        }
        children.sort();

        let mut nested = Vec::new();
        for path in children {
            if font_scan_cancelled(scan_id) {
                return Ok(discovery_result!(ScanStopReason::UserCancel));
            }
            match crate::util::try_is_reparse_point(&path) {
                Ok(true) => continue,
                Err(_) => {
                    incomplete_io = true;
                    continue;
                }
                Ok(false) => {}
            }
            let metadata = match fs::symlink_metadata(&path) {
                Ok(metadata) => metadata,
                Err(_) => {
                    incomplete_io = true;
                    continue;
                }
            };
            if metadata.is_dir() {
                if scope == FontDirectoryScope::Recursive {
                    match checked_canonical_path(&path, canonical_root) {
                        Ok(canonical) => nested.push(canonical),
                        Err(_) => incomplete_io = true,
                    }
                }
                continue;
            }
            if !metadata.is_file() || !has_allowed_font_extension(&path) {
                continue;
            }
            if files.len() >= MAX_SCAN_FONT_FILES {
                return Ok(discovery_result!(ScanStopReason::CeilingHit));
            }
            let canonical = match checked_canonical_path(&path, canonical_root) {
                Ok(canonical) => canonical,
                Err(_) => {
                    incomplete_io = true;
                    continue;
                }
            };
            let dedup_key = normalize_canonical_path(&canonical.to_string_lossy());
            if !seen_font_paths.insert(dedup_key) {
                continue;
            }
            total_font_bytes = total_font_bytes.saturating_add(metadata.len());
            if total_font_bytes > MAX_SCAN_TOTAL_FONT_BYTES {
                return Ok(discovery_result!(ScanStopReason::CeilingHit));
            }
            let normalized_file = normalize_canonical_path(
                canonical
                    .to_str()
                    .ok_or_else(|| "A font-library file is not valid UTF-8".to_string())?,
            );
            let file_mtime = match crate::font_cache::try_modified_at(&canonical) {
                Some(file_mtime) => {
                    candidate_files.push(crate::font_cache::FileSnapshot {
                        file_path: normalized_file,
                        file_size: i64::try_from(metadata.len()).unwrap_or(i64::MAX),
                        file_mtime,
                    });
                    file_mtime
                }
                None => {
                    incomplete_io = true;
                    -1
                }
            };
            files.push(DiscoveredFontFile {
                path: canonical,
                size_bytes: metadata.len(),
                modified_at: file_mtime,
            });
        }

        // LIFO stack: push in reverse so the alphabetically-first directory
        // is visited first and the same source resolves identically each run.
        for nested_dir in nested.into_iter().rev() {
            stack.push((nested_dir, depth + 1));
        }
    }

    files.sort_by(|a, b| a.path.cmp(&b.path));
    directories.sort_by(|a, b| a.folder_path.cmp(&b.folder_path));
    candidate_files.sort_by(|a, b| a.file_path.cmp(&b.file_path));
    Ok(discovery_result!(if incomplete_io {
        ScanStopReason::IncompleteIo
    } else {
        ScanStopReason::Natural
    }))
}

pub(super) fn add_preflight_file(path: &Path, out: &mut FontScanPreflight) {
    if !has_allowed_font_extension(path) {
        return;
    }
    let Ok(metadata) = fs::metadata(path) else {
        return;
    };
    if !metadata.is_file() {
        return;
    }
    out.font_files += 1;
    out.total_bytes = out.total_bytes.saturating_add(metadata.len());
}

#[derive(Debug, PartialEq, Eq)]
pub(super) enum ExplicitFontPathError {
    Rejected(String),
    IncompleteIo,
}

impl std::fmt::Display for ExplicitFontPathError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Rejected(reason) => formatter.write_str(reason),
            Self::IncompleteIo => {
                formatter.write_str("A selected font file could not be resolved or inspected")
            }
        }
    }
}

/// Resolve one explicitly selected font file and validate both sides of the
/// resolution boundary. The second validation is required even when the raw
/// picker path passed: a symlink / junction / filesystem alias may resolve to
/// a canonical Windows ADS path or another representation the IPC validator
/// rejects. Return the normalized canonical string alongside the `PathBuf` so
/// callers cannot accidentally deduplicate or publish the path before that
/// post-canonicalization check.
pub(super) fn canonicalize_and_validate_explicit_font_path_with<F>(
    path: &str,
    canonicalize: F,
) -> Result<(PathBuf, String), ExplicitFontPathError>
where
    F: FnOnce(&Path) -> std::io::Result<PathBuf>,
{
    validate_ipc_path(path, "File").map_err(ExplicitFontPathError::Rejected)?;
    let canonical =
        canonicalize(Path::new(path)).map_err(|_| ExplicitFontPathError::IncompleteIo)?;
    let canonical_text = canonical.to_str().ok_or_else(|| {
        ExplicitFontPathError::Rejected("A selected font path is not valid UTF-8".to_string())
    })?;
    let normalized = normalize_canonical_path(canonical_text);
    validate_ipc_path(&normalized, "Resolved font").map_err(ExplicitFontPathError::Rejected)?;
    Ok((canonical, normalized))
}

pub(super) fn canonicalize_and_validate_explicit_font_path(
    path: &str,
) -> Result<(PathBuf, String), ExplicitFontPathError> {
    canonicalize_and_validate_explicit_font_path_with(path, Path::canonicalize)
}

pub(super) fn resolve_explicit_font_file_with<C, M>(
    path: &str,
    canonicalize: C,
    metadata: M,
) -> Result<Option<(PathBuf, String)>, ExplicitFontPathError>
where
    C: FnOnce(&Path) -> std::io::Result<PathBuf>,
    M: FnOnce(&Path) -> std::io::Result<fs::Metadata>,
{
    let resolved = canonicalize_and_validate_explicit_font_path_with(path, canonicalize)?;
    if !has_allowed_font_extension(&resolved.0) {
        return Ok(None);
    }
    let metadata = metadata(&resolved.0).map_err(|_| ExplicitFontPathError::IncompleteIo)?;
    Ok(metadata.is_file().then_some(resolved))
}

pub(super) fn resolve_explicit_font_file(
    path: &str,
) -> Result<Option<(PathBuf, String)>, ExplicitFontPathError> {
    resolve_explicit_font_file_with(path, Path::canonicalize, |canonical| {
        fs::metadata(canonical)
    })
}

pub(super) fn preflight_files_inner(paths: Vec<String>) -> FontScanPreflight {
    // The public command enforces MAX_INPUT_PATHS, but the inner
    // helper has no caller-side check. Debug-mode assertion catches
    // any future internal caller that bypasses the public command.
    //
    // (seen-HashSet bound documentation): the
    // assert below also bounds the `seen: HashSet` allocation
    // implicitly — `seen` grows at most one entry per loop iteration,
    // and the loop runs `paths.len()` times. With MAX_INPUT_PATHS =
    // 1000, the worst-case allocation is ~1000 normalized-path
    // strings (much less than the parallel MAX_PREFLIGHT_ENTRIES =
    // 200_000 ceiling that applies to directory scans). If a future
    // change raises MAX_INPUT_PATHS substantially OR removes the IPC
    // gate, this debug_assert is the structural reminder that `seen`
    // needs its own cap.
    debug_assert!(
        paths.len() <= MAX_INPUT_PATHS,
        "preflight_files_inner: paths.len()={} exceeds MAX_INPUT_PATHS={}",
        paths.len(),
        MAX_INPUT_PATHS
    );
    let mut out = FontScanPreflight {
        font_files: 0,
        total_bytes: 0,
    };
    let mut seen = HashSet::new();
    // mirror dropzone.rs's aggregate-count pattern.
    // Per-path validate / canonicalize failures `continue` silently inside
    // the loop; a single `log::info!` post-loop tells anyone reading the
    // log why the resolved count differs from the input count ("I dragged
    // 50 files but only 30 are in the preflight" used to leave no trail).
    let total_inputs = paths.len();
    let mut rejected = 0usize;
    for p in paths {
        let Ok((canonical, canonical_key)) = canonicalize_and_validate_explicit_font_path(&p)
        else {
            rejected += 1;
            continue;
        };
        if !canonical.is_file() || !seen.insert(canonical_key) {
            continue;
        }
        add_preflight_file(&canonical, &mut out);
    }
    if rejected > 0 {
        log::info!(
            "preflight_files: dropped {rejected} of {total_inputs} input path(s) (validate / canonicalize failure)"
        );
    }
    out
}

pub(super) async fn preflight_font_directory_cmd(
    dir: String,
    scope: Option<FontDirectoryScope>,
    scan_id: u64,
) -> Result<FontDirectoryScanPreflight, String> {
    if scan_id == NO_SCAN_ID {
        return Err("Scan id must be non-zero".to_string());
    }
    validate_ipc_path(&dir, "Directory")?;
    let scope = scope.unwrap_or(FontDirectoryScope::Shallow);
    let active_scan = begin_font_scan(scan_id)?;
    tauri::async_runtime::spawn_blocking(move || {
        let _active_scan = active_scan;
        match crate::util::try_is_reparse_point(Path::new(&dir)) {
            Ok(false) => {}
            Ok(true) => return Err("Font library root cannot be a symlink or junction".to_string()),
            Err(_) => return Err("Cannot inspect font library root".to_string()),
        }
        let canonical_dir = Path::new(&dir).canonicalize().map_err(|e| {
            log::warn!("preflight canonicalize directory failed: {e}");
            "Cannot resolve directory path".to_string()
        })?;
        if !canonical_dir.is_dir() {
            return Err("Not a directory".to_string());
        }
        let discovery = discover_font_files(&canonical_dir, scope, scan_id)?;
        match discovery.reason {
            ScanStopReason::CeilingHit => {
                return Err("Font library is too large to inspect safely".to_string())
            }
            ScanStopReason::IncompleteIo => {
                return Err(
                    "Font library changed or became unreadable during inspection".to_string(),
                )
            }
            ScanStopReason::Natural | ScanStopReason::UserCancel => {}
        }
        let mut total_bytes = 0u64;
        for file in &discovery.files {
            total_bytes = total_bytes.saturating_add(file.size_bytes);
        }
        Ok(FontDirectoryScanPreflight {
            font_files: discovery.files.len(),
            total_bytes,
            reason: discovery.reason,
        })
    })
    .await
    .map_err(|e| format!("Font preflight worker failed: {e}"))?
}

pub(super) async fn preflight_font_files_cmd(
    paths: Vec<String>,
) -> Result<FontScanPreflight, String> {
    if paths.len() > MAX_INPUT_PATHS {
        return Err(format!(
            "Too many file paths ({}, max {MAX_INPUT_PATHS})",
            paths.len()
        ));
    }
    tauri::async_runtime::spawn_blocking(move || Ok(preflight_files_inner(paths)))
        .await
        .map_err(|e| format!("Font preflight worker failed: {e}"))?
}

/// Scope-aware directory scanner used by the new library surface. Discovery
/// is a separate bounded phase so cancellation works while walking a large
/// tree, then files are parsed in deterministic path order and streamed in
/// the same small batches as the legacy shallow scanner.
pub(super) fn scan_directory_with_scope_inner<
    F: FnMut(Vec<LocalFontEntry>) -> Result<(), String>,
>(
    canonical_dir: &Path,
    scope: FontDirectoryScope,
    scan_id: u64,
    captured_snapshot: Option<&mut Option<crate::font_cache::CacheSourceSnapshot>>,
    emit_batch: F,
) -> Result<ScanOutcome, String> {
    scan_directory_with_scope_inner_with_parser(
        canonical_dir,
        scope,
        scan_id,
        captured_snapshot,
        parse_local_font_file,
        emit_batch,
    )
}

pub(super) fn scan_directory_with_scope_inner_with_parser<F, P>(
    canonical_dir: &Path,
    scope: FontDirectoryScope,
    scan_id: u64,
    mut captured_snapshot: Option<&mut Option<crate::font_cache::CacheSourceSnapshot>>,
    mut parse_font: P,
    mut emit_batch: F,
) -> Result<ScanOutcome, String>
where
    F: FnMut(Vec<LocalFontEntry>) -> Result<(), String>,
    P: FnMut(&Path, u64) -> Result<Vec<LocalFontEntry>, FontFileSkipReason>,
{
    let mut discovery = discover_font_files(canonical_dir, scope, scan_id)?;
    if discovery.reason == ScanStopReason::UserCancel {
        return Ok(ScanOutcome {
            total: 0,
            reason: ScanStopReason::UserCancel,
        });
    }

    if discovery.reason == ScanStopReason::Natural {
        if let Some(slot) = captured_snapshot.as_mut() {
            **slot = Some(crate::font_cache::CacheSourceSnapshot {
                source_root: normalize_canonical_path(
                    canonical_dir
                        .to_str()
                        .ok_or_else(|| "Font library root is not valid UTF-8".to_string())?,
                ),
                scope,
                directories: std::mem::take(&mut discovery.directories),
                files: std::mem::take(&mut discovery.candidate_files),
            });
        }
    }

    let discovery_reason = discovery.reason;
    let mut buffer = Vec::new();
    let mut total = 0usize;
    let mut last_emit = Instant::now();
    for discovered in discovery.files {
        if font_scan_cancelled(scan_id) {
            if !buffer.is_empty() {
                emit_batch(std::mem::take(&mut buffer))?;
            }
            return Ok(ScanOutcome {
                total,
                reason: ScanStopReason::UserCancel,
            });
        }
        let live_size = match fs::metadata(&discovered.path) {
            Ok(metadata) if metadata.is_file() => metadata.len(),
            _ => {
                if !buffer.is_empty() {
                    emit_batch(std::mem::take(&mut buffer))?;
                }
                return Ok(ScanOutcome {
                    total,
                    reason: ScanStopReason::IncompleteIo,
                });
            }
        };
        if live_size != discovered.size_bytes {
            if !buffer.is_empty() {
                emit_batch(std::mem::take(&mut buffer))?;
            }
            return Ok(ScanOutcome {
                total,
                reason: ScanStopReason::IncompleteIo,
            });
        }
        let parsed_entries = match parse_font(&discovered.path, scan_id) {
            Ok(entries) => entries,
            Err(reason) if reason.is_incomplete_io() => {
                if !buffer.is_empty() {
                    emit_batch(std::mem::take(&mut buffer))?;
                }
                return Ok(ScanOutcome {
                    total,
                    reason: ScanStopReason::IncompleteIo,
                });
            }
            Err(_) => Vec::new(),
        };
        let stable_after_parse = fs::metadata(&discovered.path)
            .ok()
            .filter(|metadata| metadata.is_file() && metadata.len() == discovered.size_bytes)
            .and_then(|_| crate::font_cache::try_modified_at(&discovered.path))
            .is_some_and(|modified_at| modified_at == discovered.modified_at);
        if !stable_after_parse {
            if !buffer.is_empty() {
                emit_batch(std::mem::take(&mut buffer))?;
            }
            return Ok(ScanOutcome {
                total,
                reason: ScanStopReason::IncompleteIo,
            });
        }
        for font_entry in parsed_entries {
            buffer.push(font_entry);
            total += 1;
            if total > MAX_FONTS_PER_SCAN {
                emit_batch(std::mem::take(&mut buffer))?;
                return Ok(ScanOutcome {
                    total,
                    reason: ScanStopReason::CeilingHit,
                });
            }
            if buffer.len() >= SCAN_BATCH_SIZE || last_emit.elapsed() >= SCAN_BATCH_INTERVAL {
                emit_batch(std::mem::take(&mut buffer))?;
                last_emit = Instant::now();
            }
        }
    }
    if !buffer.is_empty() {
        emit_batch(buffer)?;
    }
    let reason = if font_scan_cancelled(scan_id) {
        ScanStopReason::UserCancel
    } else {
        discovery_reason
    };
    Ok(ScanOutcome { total, reason })
}

/// Shared scan-command body: opens the SQLite transaction, drives the
/// inner scan loop with an emit closure that imports each batch into
/// the source index AND streams a count-only progress event, then sends
/// the Done sentinel on the Ok path.
///
/// Lifted out of `scan_font_directory` and `scan_font_files` once
/// their shared body — open transaction, drive emit closure, send
/// Done sentinel — had accumulated enough duplication to extract.
/// The two commands now differ only in their pre-validation +
/// canonicalize stages and the inner scan they invoke through
/// `scan_body`.
///
/// `log_label` is the human-readable scan target (directory path, or
/// "local font files" for the file-list command) — folded into the
/// completion log line.
pub(super) struct StreamingScanSource<'a> {
    pub(super) source_id: &'a str,
    pub(super) source_kind: FontSourceKind,
    pub(super) root_path: Option<&'a str>,
    pub(super) log_label: &'a str,
}

pub(super) fn run_streaming_scan_command<S>(
    scan_id: u64,
    source: StreamingScanSource<'_>,
    progress: tauri::ipc::Channel<ScanProgress>,
    // When `Some`, every batch is cloned into the supplied Vec before
    // being passed downstream to the session-DB import. The directory
    // scan path uses this to feed the locked source-cache publisher
    // after commit; the file-list scan path passes `None` because it
    // has no directory-source identity for the cache's drift model. Bounded by
    // MAX_CACHE_POPULATE_FACES.
    //
    // Cap-hit policy: when the cap fires mid-scan, the function returns
    // `Ok(true)` to signal the caller that the persistent cache
    // populate MUST be skipped. The Vec is truncated to fit (in-session
    // memory bound) but the caller does not write a row to the
    // persistent cache. An earlier design routed the truncated Vec
    // through the persistent-cache publisher
    // on the theory that partial cache acceleration beats none, but
    // persistent cache rows are folder-anchored by mtime — a truncated
    // folder whose mtime doesn't change later is indistinguishable
    // from a fully cached folder, and drift detection considers it
    // valid forever. Cache lookups for fonts NOT in the truncated set
    // miss → fall through cleanly; cache lookups for fonts WITH a
    // (lookup-key-colliding) early-index face return that face's path,
    // which subset_font then rejects via the session-DB provenance
    // gate. Net: persistent skipped/wrong-font behavior until the
    // user manually clears the cache. Better to never persist the
    // truncated state than to corner the user.
    //
    // OOM defense still holds: peak memory is bounded by the cap.
    mut collected_for_cache: Option<&mut Vec<LocalFontEntry>>,
    scan_body: S,
) -> Result<StreamingScanCommandResult, String>
where
    S: FnOnce(
        u64,
        &mut dyn FnMut(Vec<LocalFontEntry>) -> Result<(), String>,
    ) -> Result<ScanOutcome, String>,
{
    let mut conn = open_user_font_db()?;
    let tx = conn
        .transaction()
        .map_err(|e| db_error("transaction start failed", e))?;
    create_user_font_source_tx(&tx, source.source_id, source.source_kind, source.root_path)?;
    let mut import = ImportOutcome::default();
    let mut progress_total = 0usize;
    // Tracks whether the cap was hit mid-scan and the collected Vec
    // was truncated to fit. Returned to the caller — when true the
    // caller MUST skip persistent cache populate (a truncated row
    // would be indistinguishable from a full row to drift detection,
    // cornering the user; see the collected_for_cache parameter doc
    // for the full reasoning).
    let mut cache_truncated = false;
    let outcome = scan_body(scan_id, &mut |batch| {
        let batch_size = batch.len();
        // For directory scans, snapshot the batch before it's consumed
        // by the session-DB import so we can populate the GUI cache
        // post-commit. Cloning here (vs taking ownership and avoiding
        // import's consume) keeps `import_user_font_batch_tx`'s API
        // stable and the eviction/dedup semantics it owns intact.
        // .as_mut() (not .as_deref_mut) so we hold &mut Vec<T>, not the
        // &mut [T] slice that as_deref_mut would yield — slices can't
        // grow, so .extend wouldn't compile against them.
        //
        // Cache cap: take what fits instead of dropping everything
        // . Previously a 19_999-entry batch followed
        // by a 2-entry batch would clear all 20_001 because the cap
        // tripped on the second batch, losing the well-under-cap work
        // already done. Now: take MAX_CACHE_POPULATE_FACES - c.len()
        // slots from the overflowing batch and mark the cache as
        // truncated. The truncated Vec keeps in-session memory
        // bounded, but the returned `cache_truncated` flag tells the
        // caller to SKIP the persistent cache write entirely —
        // persisting a truncated row would make drift detection
        // consider the folder valid forever and the user would be
        // cornered into "Clear cache" to recover.
        // `cache_truncated` also short-circuits subsequent batches
        // because they can't add more without re-exceeding the cap.
        if !cache_truncated {
            if let Some(c) = collected_for_cache.as_mut() {
                let remaining = MAX_CACHE_POPULATE_FACES.saturating_sub(c.len());
                if batch.len() > remaining {
                    // Wording avoids attack framing ("malicious or
                    // abnormally large font packs") because 17k+ fonts
                    // is a real fan-sub typesetting collection size,
                    // and avoids the inaccurate "all 20000 faces (or
                    // more)" framing (session-DB import is bounded by
                    // MAX_FONTS_PER_SCAN = 100_000, not by
                    // MAX_CACHE_POPULATE_FACES). It states the
                    // operational fact (cache populate skipped,
                    // session-DB lookups remain authoritative).
                    log::warn!(
                        "Persistent font cache populate skipped: scan reached the {}-face \
                         cache defense-in-depth cap. Session-DB lookups remain authoritative \
                         for this scan (bounded by MAX_FONTS_PER_SCAN, not this cache cap). \
                         To enable persistent-cache acceleration, split the folder into \
                         smaller subdirectories and rescan.",
                        MAX_CACHE_POPULATE_FACES,
                    );
                    c.extend(batch.iter().take(remaining).cloned());
                    cache_truncated = true;
                } else {
                    c.extend(batch.iter().cloned());
                }
            }
        }
        // Run the SQLite import FIRST so progress_total only advances on
        // committed work. If the import errors, the closure short-
        // circuits via `?` and the next outer `?` rolls the whole scan
        // back without leaving the user staring at a count that
        // overshoots the registered source.
        let batch_import = import_user_font_batch_tx(&tx, source.source_id, batch)?;
        progress_total += batch_size;
        import.added += batch_import.added;
        import.duplicated += batch_import.duplicated;
        let _ = progress.send(ScanProgress::Batch {
            total: progress_total,
        });
        Ok(())
    })?;
    finish_user_font_source_tx(&tx, source.source_id, outcome.reason)?;
    remove_empty_user_font_source_tx(&tx, source.source_id, import.added)?;
    tx.commit()
        .map_err(|e| db_error("transaction commit failed", e))?;

    // End-of-stream sentinel; see ScanProgress::Done. MUST be the last
    // send on the Ok path so every progress event has drained before
    // the frontend reports the registered source count. NOT emitted on
    // the Err path — the IPC rejection handles that signal, and
    // runStreamingScan never reaches the donePromise await when invoke
    // rejects.
    //
    // Batch-send failures stay swallowed (UX progress is informational —
    // missing a few batches is harmless), but the Done send remains
    // diagnostic because `runStreamingScan` awaits it. Tauri owns the JS
    // callback registration until the Rust Channel is dropped; React renders,
    // modal hides, and `onmessage` replacement do not unregister it. In the
    // installed Wry runtime, this send can therefore fail only after the app
    // event loop/webview is tearing down (or serialization fails), at which
    // point the old JS realm and its promise are no longer user-visible.
    // Returning Err here would be worse: the SQLite transaction has already
    // committed, so an IPC rejection would falsely report a rolled-back scan.
    if let Err(e) = progress.send(ScanProgress::Done {
        reason: outcome.reason,
        added: import.added,
        duplicated: import.duplicated,
    }) {
        log::warn!(
            "scan {scan_id}: Done sentinel send failed after scan commit during app teardown ({e})"
        );
    }

    log::info!(
        "{} with scan {scan_id}: {} faces total, {} added, {} duplicate{}",
        source.log_label,
        outcome.total,
        import.added,
        import.duplicated,
        match outcome.reason {
            ScanStopReason::Natural => "",
            ScanStopReason::UserCancel => " (cancelled)",
            ScanStopReason::CeilingHit => " (ceiling hit)",
            ScanStopReason::IncompleteIo => " (incomplete I/O)",
        }
    );
    Ok(StreamingScanCommandResult {
        outcome,
        import,
        cache_truncated,
    })
}

pub(super) fn run_blocking_scan_import<S>(
    source_id: &str,
    source_kind: FontSourceKind,
    root_path: Option<&str>,
    scan_body: S,
) -> Result<FontSourceImportSummary, String>
where
    S: FnOnce(
        u64,
        &mut dyn FnMut(Vec<LocalFontEntry>) -> Result<(), String>,
    ) -> Result<ScanOutcome, String>,
{
    validate_font_source_id(source_id)?;
    let mut conn = open_user_font_db()?;
    let tx = conn
        .transaction()
        .map_err(|e| db_error("transaction start failed", e))?;
    create_user_font_source_tx(&tx, source_id, source_kind, root_path)?;
    let mut import = ImportOutcome::default();
    let outcome = scan_body(NO_SCAN_ID, &mut |batch| {
        let batch_import = import_user_font_batch_tx(&tx, source_id, batch)?;
        import.added += batch_import.added;
        import.duplicated += batch_import.duplicated;
        Ok(())
    })?;
    finish_user_font_source_tx(&tx, source_id, outcome.reason)?;
    remove_empty_user_font_source_tx(&tx, source_id, import.added)?;
    tx.commit()
        .map_err(|e| db_error("transaction commit failed", e))?;

    Ok(FontSourceImportSummary {
        total: outcome.total,
        added: import.added,
        duplicated: import.duplicated,
        reason: outcome.reason,
    })
}

/// Scan one directory (one level, non-recursive — matching the
/// existing `import_font_directory_for_cli` semantics) and return
/// every font face found, without touching any database.
///
/// Used by the persistent-font-cache `refresh-fonts` flow: the CLI
/// shell calls this to get raw `LocalFontEntry` records, converts
/// them into `font_cache::FontMetadata`, and writes them to the
/// persistent cache (NOT the GUI session DB). Keeps the cache
/// module decoupled from font parsing.
///
/// Uses `NO_SCAN_ID` like `run_blocking_scan_import` does — the
/// scan is non-cancellable, callers must Ctrl+C if they want to
/// abort. Acceptable for refresh-fonts which is a foreground
/// operation under user attention.
pub fn scan_directory_collecting(dir: &Path) -> Result<Vec<LocalFontEntry>, String> {
    scan_directory_collecting_with_scope(dir, FontDirectoryScope::Shallow)
}

pub fn scan_directory_collecting_with_scope(
    dir: &Path,
    scope: FontDirectoryScope,
) -> Result<Vec<LocalFontEntry>, String> {
    scan_directory_collecting_impl(dir, scope, None, parse_local_font_file)
}

pub(crate) fn scan_directory_collecting_with_snapshot(
    dir: &Path,
    scope: FontDirectoryScope,
) -> Result<(Vec<LocalFontEntry>, crate::font_cache::CacheSourceSnapshot), String> {
    let mut snapshot = None;
    let entries =
        scan_directory_collecting_impl(dir, scope, Some(&mut snapshot), parse_local_font_file)?;
    let snapshot = snapshot.ok_or_else(|| {
        "Font source scan completed without a cache snapshot; persistent cache was not changed"
            .to_string()
    })?;
    Ok((entries, snapshot))
}

pub(super) fn scan_directory_collecting_impl<P>(
    dir: &Path,
    scope: FontDirectoryScope,
    captured_snapshot: Option<&mut Option<crate::font_cache::CacheSourceSnapshot>>,
    parse_font: P,
) -> Result<Vec<LocalFontEntry>, String>
where
    P: FnMut(&Path, u64) -> Result<Vec<LocalFontEntry>, FontFileSkipReason>,
{
    // (considered, rejected): GUI's
    // scan_font_directory validates inside because it's an IPC entry
    // point (untrusted JS string). The CLI's scan_directory_collecting
    // is an INTERNAL helper called from run_refresh_fonts after the
    // caller has already canonicalized — adding validate_ipc_path
    // here would reject the caller's `\\?\C:\…` verbatim form, which
    // Windows canonicalize() returns. Validation lives at the CLI
    // entry point (`run_refresh_fonts` validates each --font-dir
    // argv) — the proper boundary. CLI/GUI asymmetry of "where the
    // entry point lives" is intrinsic, not a missing defense.
    match crate::util::try_is_reparse_point(dir) {
        Ok(false) => {}
        Ok(true) => {
            return Err(format!(
                "Font source root cannot be a symlink or junction: {}",
                dir.display()
            ))
        }
        Err(e) => return Err(format!("Cannot inspect directory '{}': {e}", dir.display())),
    }
    let canonical = dir
        .canonicalize()
        .map_err(|e| format!("Cannot resolve directory '{}': {e}", dir.display()))?;
    if !canonical.is_dir() {
        return Err(format!("Not a directory: {}", canonical.display()));
    }
    let mut entries: Vec<LocalFontEntry> = Vec::new();
    let outcome = scan_directory_with_scope_inner_with_parser(
        &canonical,
        scope,
        NO_SCAN_ID,
        captured_snapshot,
        parse_font,
        |batch| {
            // Defense-in-depth against refresh-fonts OOM on crafted font
            // folders: fail fast if a single source would
            // push us past the cache-populate cap. The CLI caller in
            // run_refresh_fonts catches this and continues with the next
            // dir; without the cap, a malicious pack could hold hundreds
            // of MB to multi-GB of font metadata in memory before the
            // `cache.replace_source` write would even start.
            if entries.len() + batch.len() > MAX_CACHE_POPULATE_FACES {
                return Err(format!(
                    "Source has more font faces than the persistent cache safely accepts \
                     ({}+ faces, cap {MAX_CACHE_POPULATE_FACES}). Skipping this source.",
                    entries.len() + batch.len()
                ));
            }
            entries.extend(batch);
            Ok(())
        },
    )?;
    if outcome.reason != ScanStopReason::Natural {
        return Err(format!(
            "Font source scan was incomplete ({:?}); persistent cache was not changed",
            outcome.reason
        ));
    }
    Ok(entries)
}

pub fn import_font_directory_for_cli(
    dir: &Path,
    source_id: &str,
) -> Result<FontSourceImportSummary, String> {
    import_font_directory_for_cli_with_scope(dir, source_id, FontDirectoryScope::Shallow)
}

pub fn import_font_directory_for_cli_with_scope(
    dir: &Path,
    source_id: &str,
    scope: FontDirectoryScope,
) -> Result<FontSourceImportSummary, String> {
    // Canonicalize: the font scanner indexes by the resolved path so
    // sources reached via different symlinks aren't double-imported.
    // (CLI's --output-dir handling deliberately does NOT canonicalize
    // — see absolute_path() in bin/cli/main.rs — because output paths
    // round-trip through user-facing diagnostics where the user-typed
    // form should be preserved. Indexing has no such constraint.)
    match crate::util::try_is_reparse_point(dir) {
        Ok(false) => {}
        Ok(true) => return Err("Font source root cannot be a symlink or junction".to_string()),
        Err(_) => return Err("Cannot inspect font source root".to_string()),
    }
    let canonical_dir = dir.canonicalize().map_err(|e| {
        log::warn!("canonicalize directory failed: {e}");
        "Cannot resolve directory path".to_string()
    })?;
    if !canonical_dir.is_dir() {
        return Err("Not a directory".to_string());
    }
    let root_path = normalize_canonical_path(&canonical_dir.to_string_lossy());
    run_blocking_scan_import(
        source_id,
        FontSourceKind::for_directory(scope),
        Some(&root_path),
        |scan_id, emit_batch| {
            scan_directory_with_scope_inner(&canonical_dir, scope, scan_id, None, emit_batch)
        },
    )
}

pub fn import_font_files_for_cli(
    paths: Vec<String>,
    source_id: &str,
) -> Result<FontSourceImportSummary, String> {
    if paths.len() > MAX_INPUT_PATHS {
        return Err(format!(
            "Too many file paths ({}, max {MAX_INPUT_PATHS})",
            paths.len()
        ));
    }
    run_blocking_scan_import(
        source_id,
        FontSourceKind::Files,
        None,
        |scan_id, emit_batch| scan_files_inner(paths, scan_id, emit_batch),
    )
}

/// Tauri command wrapping `scan_directory_inner` with a typed progress
/// channel. Frontend creates a `Channel<ScanProgress>`, passes it as
/// `progress`, and receives `Batch` events as faces are parsed.
pub(super) async fn scan_font_directory_cmd(
    dir: String,
    progress: tauri::ipc::Channel<ScanProgress>,
    scan_id: u64,
    source_id: String,
    scope: Option<FontDirectoryScope>,
) -> Result<FontDirectoryScanCommandResult, String> {
    if scan_id == NO_SCAN_ID {
        return Err("Scan id must be non-zero".to_string());
    }
    validate_ipc_path(&dir, "Directory")?;
    validate_font_source_id(&source_id)?;
    let scope = scope.unwrap_or(FontDirectoryScope::Shallow);

    let cache_mutation =
        crate::font_cache_commands::CacheMutationGuard::try_acquire().map_err(|_| {
            "A font-cache operation is running; wait for it to finish and retry".to_string()
        })?;
    let active_scan = begin_font_scan(scan_id)?;
    tauri::async_runtime::spawn_blocking(move || {
        // The guard's only job is to clear ACTIVE_SCAN_ID on Drop when
        // the worker thread exits — Ok or Err. Bind it into the
        // closure's scope (the move at the spawn_blocking boundary
        // already transferred ownership; this just keeps it alive
        // through every return path inside).
        let _active_scan = active_scan;
        let cache_mutation = cache_mutation;
        match crate::util::try_is_reparse_point(Path::new(&dir)) {
            Ok(false) => {}
            Ok(true) => return Err("Font library root cannot be a symlink or junction".to_string()),
            Err(_) => return Err("Cannot inspect font library root".to_string()),
        }
        let canonical_dir = Path::new(&dir).canonicalize().map_err(|e| {
            log::warn!("canonicalize directory failed: {e}");
            "Cannot resolve directory path".to_string()
        })?;
        if !canonical_dir.is_dir() {
            return Err("Not a directory".to_string());
        }
        // WHY full path in log_label (vs sanitized / truncated):
        // log::info! is INTERNAL telemetry consumed by
        // `RUST_LOG=info` dev runs and tauri-plugin-log files written
        // under app-data-dir (user-local, no cross-user reach). The
        // path is provided BY the user via picker / drag-drop, so it
        // is content the user already saw. local-user (single-user desktop)
        // does not consider local-file disclosure a leak. If this
        // app later ships a remote-log shipper or a public crash
        // reporter, sanitize at the shipper boundary, not here — the
        // local INFO line is the right diagnostic granularity.
        let log_label = format!("Scanned font directory '{}'", canonical_dir.display());
        let mut entries_for_cache: Vec<LocalFontEntry> = Vec::new();
        let mut cache_snapshot = None;
        let root_path = normalize_canonical_path(
            canonical_dir
                .to_str()
                .ok_or_else(|| "Font directory path is not valid UTF-8".to_string())?,
        );
        let existing_source = open_user_font_db()?
            .query_row(
                "SELECT 1 FROM font_sources
                 WHERE source_id = ?1 OR (root_path = ?2 AND source_kind = ?3)
                 LIMIT 1",
                params![
                    &source_id,
                    &root_path,
                    FontSourceKind::for_directory(scope).as_i32()
                ],
                |_| Ok(()),
            )
            .optional()
            .map_err(|e| db_error("duplicate source lookup failed", e))?
            .is_some();
        if existing_source {
            return Err("That font source is already loaded".to_string());
        }
        // Invalidate an older persistent snapshot before importing this
        // source. A cancelled or incomplete replacement must not leave the
        // old complete cache silently active behind the partial session view.
        crate::font_cache_commands::remove_source_from_gui_cache_locked(
            &cache_mutation,
            &root_path,
            scope,
        )?;
        let scan_result = run_streaming_scan_command(
            scan_id,
            StreamingScanSource {
                source_id: &source_id,
                source_kind: FontSourceKind::for_directory(scope),
                root_path: Some(&root_path),
                log_label: &log_label,
            },
            progress,
            Some(&mut entries_for_cache),
            |scan_id, emit_batch| {
                scan_directory_with_scope_inner(
                    &canonical_dir,
                    scope,
                    scan_id,
                    Some(&mut cache_snapshot),
                    emit_batch,
                )
            },
        )?;

        let eligible = is_cache_publication_eligible(&scan_result);
        let cache_disposition = if eligible {
            if let Some(snapshot) = cache_snapshot {
                match crate::font_cache_commands::record_source_in_gui_cache_locked(
                    &cache_mutation,
                    &snapshot,
                    &entries_for_cache,
                ) {
                    Ok(()) => FontCacheDisposition::Cached,
                    Err(error) => {
                        log::warn!("GUI font-cache publication skipped: {error}");
                        FontCacheDisposition::SessionOnly
                    }
                }
            } else {
                FontCacheDisposition::SessionOnly
            }
        } else {
            FontCacheDisposition::SessionOnly
        };
        Ok(FontDirectoryScanCommandResult { cache_disposition })
    })
    .await
    .map_err(|e| format!("Font scan worker failed: {e}"))?
}

/// Streaming scan of a user-picked file list. Mirrors
/// `scan_directory_inner`, with cancel checks between files and the same
/// per-face batching cadence.
pub(super) fn scan_files_inner<F: FnMut(Vec<LocalFontEntry>) -> Result<(), String>>(
    paths: Vec<String>,
    scan_id: u64,
    emit_batch: F,
) -> Result<ScanOutcome, String> {
    scan_files_inner_with_operations(
        paths,
        scan_id,
        resolve_explicit_font_file,
        parse_local_font_file,
        emit_batch,
    )
}

pub(super) fn scan_files_inner_with_operations<F, P, R>(
    paths: Vec<String>,
    scan_id: u64,
    mut resolve_file: R,
    mut parse_font: P,
    mut emit_batch: F,
) -> Result<ScanOutcome, String>
where
    F: FnMut(Vec<LocalFontEntry>) -> Result<(), String>,
    P: FnMut(&Path, u64) -> Result<Vec<LocalFontEntry>, FontFileSkipReason>,
    R: FnMut(&str) -> Result<Option<(PathBuf, String)>, ExplicitFontPathError>,
{
    // hard cap inside the function instead of relying
    // on caller-trust. Public commands already enforce MAX_INPUT_PATHS
    // at IPC boundary, but this matches `scan_directory_inner`'s shape
    // (visited-entry hard cap) so a future internal caller can't bypass
    // by skipping the IPC layer. Cost: one branch per invocation.
    if paths.len() > MAX_INPUT_PATHS {
        return Err(format!(
            "Too many file paths: {} (max {MAX_INPUT_PATHS})",
            paths.len()
        ));
    }
    let mut buffer: Vec<LocalFontEntry> = Vec::new();
    let mut total: usize = 0;
    let mut last_emit = Instant::now();
    // Mirror the dedup `preflight_files_inner` already applies — a list
    // with duplicate canonical paths would otherwise re-parse each
    // duplicate, then rely on the SQLite `UNIQUE(path, face_index)`
    // constraint to discard them as `duplicated`. Wastes IO/parse time
    // and inflates the cancel-poll budget.
    let mut seen: HashSet<String> = HashSet::new();

    // mirror dropzone.rs's aggregate-count pattern.
    // Invalid paths and non-font targets are intentional skips; a
    // single `log::info!` post-loop tells anyone reading the log why
    // the parsed face count came from fewer than `total_inputs` source
    // files. Emitted unconditionally before any cancel / ceiling
    // early-return below, so the signal survives partial returns.
    let total_inputs = paths.len();
    let mut rejected = 0usize;

    let log_rejected = |rejected: usize| {
        if rejected > 0 {
            log::info!(
                "scan_files: dropped {rejected} of {total_inputs} input path(s) (invalid path or non-font target)"
            );
        }
    };

    for p in paths {
        if font_scan_cancelled(scan_id) {
            if !buffer.is_empty() {
                emit_batch(std::mem::take(&mut buffer))?;
            }
            log::info!(
                "font scan {} cancelled in file list after {} faces",
                scan_id,
                total
            );
            log_rejected(rejected);
            return Ok(ScanOutcome {
                total,
                reason: ScanStopReason::UserCancel,
            });
        }

        // No `is_reparse_point` pre-check here, unlike
        // `scan_directory_inner`. The asymmetry is intentional:
        // this function processes paths the user
        // EXPLICITLY selected one by one via file picker / drag-drop,
        // each entry expressing the user's direct intent to scan that
        // exact file. `scan_directory_inner`'s reject applies to
        // entries DISCOVERED inside a user-chosen folder, where a
        // crafted font pack could plant top-level symlinks to inflate
        // the per-folder count past the XL-safety preflight (untrusted-input). For
        // the file-list path the user picked each entry on its own, so
        // chasing a single symlinked entry doesn't bypass any aggregate
        // count gate. `canonicalize` + `has_allowed_font_extension`
        // in the resolver still bound the exfil surface
        // to font-extension targets.
        let (canonical, canonical_key) = match resolve_file(&p) {
            Ok(Some(resolved)) => resolved,
            Ok(None) | Err(ExplicitFontPathError::Rejected(_)) => {
                rejected += 1;
                continue;
            }
            Err(ExplicitFontPathError::IncompleteIo) => {
                if !buffer.is_empty() {
                    emit_batch(std::mem::take(&mut buffer))?;
                }
                log::warn!(
                    "Selected font file '{}' could not be resolved or inspected",
                    safe_font_file_log_label(Path::new(&p))
                );
                log_rejected(rejected);
                return Ok(ScanOutcome {
                    total,
                    reason: ScanStopReason::IncompleteIo,
                });
            }
        };
        if !seen.insert(canonical_key) {
            continue;
        }

        let parsed_entries = match parse_font(&canonical, scan_id) {
            Ok(entries) => entries,
            Err(reason) if reason.is_incomplete_io() => {
                if !buffer.is_empty() {
                    emit_batch(std::mem::take(&mut buffer))?;
                }
                log_rejected(rejected);
                return Ok(ScanOutcome {
                    total,
                    reason: ScanStopReason::IncompleteIo,
                });
            }
            Err(_) => Vec::new(),
        };
        for font_entry in parsed_entries {
            buffer.push(font_entry);
            total += 1;
            if total > MAX_FONTS_PER_SCAN {
                if !buffer.is_empty() {
                    emit_batch(std::mem::take(&mut buffer))?;
                }
                log::info!(
                    "font scan {} hit the {MAX_FONTS_PER_SCAN}-face ceiling in file list",
                    scan_id
                );
                log_rejected(rejected);
                return Ok(ScanOutcome {
                    total,
                    reason: ScanStopReason::CeilingHit,
                });
            }

            if buffer.len() >= SCAN_BATCH_SIZE || last_emit.elapsed() >= SCAN_BATCH_INTERVAL {
                emit_batch(std::mem::take(&mut buffer))?;
                last_emit = Instant::now();
            }
        }
    }

    if !buffer.is_empty() {
        emit_batch(buffer)?;
    }

    // Post-loop cancellation re-check . The top-of-loop
    // `font_scan_cancelled` only fires on the NEXT iteration; when
    // `parse_local_font_file`'s per-face cancel poll fires inside the
    // FINAL directory entry / file, the loop exits naturally and the
    // outer reason would otherwise read as Natural — UI sees "completed
    // normally" while the partial buffer is silently kept.
    //
    // the post-loop classification is intentionally
    // 2-way (cancel / natural) rather than the 3-way (cancel / ceiling /
    // natural) in the scoped directory scanner. The reason is structural —
    // directory discovery has a `visited >= MAX_PREFLIGHT_ENTRIES`
    // bound that sets `dedup_ceiling_hit = true` and `break`s, falling
    // through to the post-loop where the flag selects `CeilingHit`. This
    // function takes a pre-validated `paths` vector (`MAX_INPUT_PATHS`
    // bound checked by the caller), so there is no analogous visited
    // cap. The MAX_FONTS_PER_SCAN ceiling above returns `CeilingHit`
    // eagerly via early return, never reaching this point. If a future
    // refactor adds a visited-style bound here, mirror the
    // `dedup_ceiling_hit` flag and add the third arm.
    let reason = if font_scan_cancelled(scan_id) {
        ScanStopReason::UserCancel
    } else {
        ScanStopReason::Natural
    };
    log_rejected(rejected);
    Ok(ScanOutcome { total, reason })
}

/// Tauri command wrapping `scan_files_inner` with a typed progress channel.
/// Same shape as `scan_font_directory` — frontend supplies the list of
/// paths and a `Channel<ScanProgress>` for incremental delivery.
pub(super) async fn scan_font_files_cmd(
    paths: Vec<String>,
    progress: tauri::ipc::Channel<ScanProgress>,
    scan_id: u64,
    source_id: String,
) -> Result<(), String> {
    if scan_id == NO_SCAN_ID {
        return Err("Scan id must be non-zero".to_string());
    }
    if paths.len() > MAX_INPUT_PATHS {
        return Err(format!(
            "Too many file paths ({}, max {MAX_INPUT_PATHS})",
            paths.len()
        ));
    }
    validate_font_source_id(&source_id)?;

    let active_scan = begin_font_scan(scan_id)?;
    tauri::async_runtime::spawn_blocking(move || {
        let _active_scan = active_scan; // see scan_font_directory for the WHY
        run_streaming_scan_command(
            scan_id,
            StreamingScanSource {
                source_id: &source_id,
                source_kind: FontSourceKind::Files,
                root_path: None,
                log_label: "Scanned local font files",
            },
            progress,
            // No GUI cache populate for file-list scans: the cache's
            // drift model is folder-anchored (folder mtime vs cached
            // mtime), and an arbitrary file list has no single folder
            // to anchor against. Files-mode sources stay session-only.
            None,
            |scan_id, emit_batch| scan_files_inner(paths, scan_id, emit_batch),
        )
        // File-list scans are intentionally session-only; discard the
        // directory-cache publication details.
        .map(|_| ())
    })
    .await
    .map_err(|e| format!("Font scan worker failed: {e}"))?
}

/// Cooperative cancel for an active font scan. The request is targeted by
/// scan id; stale commands from older scans cannot cancel newer work. The
/// running scan checks between files and returns early with all
/// already-emitted batches retained on the frontend.
pub(super) fn cancel_font_scan_cmd(scan_id: u64) {
    if scan_id == NO_SCAN_ID {
        return;
    }
    // Range-check against the currently-active scan id. Three cases:
    //
    //   - `scan_id < active` (stale-low): a cancel arriving for an old
    //     scan id while a newer one runs. fetch_max below is a no-op
    //     because CANCEL_SCAN_ID has already been bumped past it (or
    //     stays at its current value). Accepted-but-harmless.
    //   - `scan_id == active` (legitimate): cancel the currently-
    //     running scan. fetch_max writes CANCEL_SCAN_ID = scan_id; the
    //     scan worker's `font_scan_cancelled` poll observes equality
    //     and bails. This is the normal cancel path.
    //   - `scan_id > active` (out-of-band): rejected. Without this
    //     guard, a misbehaving frontend calling `cancel_font_scan(
    //     u64::MAX)` once would permanently set CANCEL_SCAN_ID to MAX
    //     and silently disable cancellation for the rest of the
    //     session — every legitimate future scan id would compare
    //     unequal in `font_scan_cancelled` and the cancel button would
    //     stop working with no log signal.
    //
    // `active == NO_SCAN_ID` collapses with the out-of-band case (any
    // scan_id > 0 is "future" relative to no scan) — same rejection
    // path. Don't touch CANCEL_SCAN_ID even with fetch_max in that
    // case; the invariant "we only write CANCEL_SCAN_ID while an
    // active scan exists" is easier to reason about.
    //
    // Project is trust-the-frontend, so this guard is defense-in-depth
    // matching the IPC-validation discipline in `validate_font_source_id`
    // and `validate_ipc_path`.
    let active = ACTIVE_SCAN_ID.load(Ordering::SeqCst);
    if active == NO_SCAN_ID || scan_id > active {
        return;
    }
    // Mixed-ordering note: the load above is SeqCst because it reads
    // the same atomic that `begin_font_scan` writes via SeqCst CAS —
    // we want a consistent total order across all ACTIVE_SCAN_ID
    // operations. The Release fetch_max pairs with the worker's Acquire
    // load in `font_scan_cancelled`; keep that synchronization independent
    // of the per-scan-id equality check.
    //
    // `fetch_max` ensures a stale cancel for an OLDER (smaller-id) scan
    // arriving after a newer cancel cannot regress CANCEL_SCAN_ID. The
    // returned prior max is intentionally discarded — caller has no
    // useful action either way.
    CANCEL_SCAN_ID.fetch_max(scan_id, Ordering::Release);
}

/// Refuse a mutation when a font scan is mid-flight. The PRIMARY guard
/// is the modal scrim UX — `clear_font_sources` and friends only fire
/// when the FontSourceModal is open, and the modal disables those
/// buttons during scan. This server-side check is defense-in-depth
/// against a misbehaving frontend AND against the degraded-WAL
/// scenario flagged in `set_user_font_db_journal_mode_once` (network /
/// tmpfs / read-only mounts where SQLite silently keeps DELETE
/// journaling — busy_timeout still applies but contention windows are
/// wider).
///
/// Functional rationale: a `DELETE FROM font_sources` issued mid-scan
/// would block until the scan's transaction commits and then
/// immediately delete everything just inserted — surprising the user
/// who probably wanted "clear before the scan starts" or "after it
/// completes".
///
/// `Acquire` ordering pairs with the SeqCst CAS inside `begin_font_
/// scan`. SeqCst there is stronger than Release-Acquire, so the
/// pairing holds. Do NOT downgrade `begin_font_scan`'s CAS to Release:
/// `cancel_font_scan`'s SeqCst load on ACTIVE_SCAN_ID requires a total
/// order across all accesses to that atomic.
pub(super) fn reject_during_active_scan(message: &str) -> Result<(), String> {
    if ACTIVE_SCAN_ID.load(Ordering::Acquire) != NO_SCAN_ID {
        return Err(message.to_string());
    }
    Ok(())
}
