//! Native font subsystem facade.
//!
//! Submodules divide the font pipeline by responsibility while keeping the
//! `crate::fonts::*` surface stable for GUI IPC commands, CLI workflows, and
//! integration tests:
//! - `session_db`: session SQLite index and `USER_FONT_LOOKUP_SQL` resolution
//! - `name_table`: OpenType/TTC name decoding and `cmap` sanitization
//! - `scan`: bounded directory/file discovery, preflight, and cancellation
//! - `subset`: system font lookup, provenance gating, and `fontcull` subsetting

mod name_table;
mod scan;
mod session_db;
mod subset;

use std::path::Path;
#[cfg(test)]
use std::sync::Mutex;
use std::time::Duration;

pub use crate::font_cache::FontDirectoryScope;

pub(crate) use scan::scan_directory_collecting_with_snapshot;
pub use scan::{
    import_font_directory_for_cli, import_font_directory_for_cli_with_scope,
    import_font_files_for_cli, scan_directory_collecting, scan_directory_collecting_with_scope,
    FontDirectoryScanCommandResult, FontDirectoryScanPreflight, FontScanPreflight, ScanProgress,
    ScanStopReason,
};
pub use session_db::{
    clear_font_sources, clear_user_font_db_path, init_user_font_db, remove_font_source,
    resolve_user_font,
};
#[cfg(test)]
pub(crate) use subset::cache_provenance_contains;
pub use subset::{
    clear_cache_provenance, find_system_font, init_system_dirs, register_cache_provenance,
    subset_font,
};

#[allow(unused_imports)]
use name_table::*;
#[allow(unused_imports)]
use scan::*;
#[allow(unused_imports)]
use session_db::*;
#[allow(unused_imports)]
use subset::*;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(i32)]
enum FontSourceKind {
    RecursiveDirectory = 0,
    ShallowDirectory = 1,
    Files = 2,
}

impl FontSourceKind {
    fn for_directory(scope: FontDirectoryScope) -> Self {
        match scope {
            FontDirectoryScope::Shallow => Self::ShallowDirectory,
            FontDirectoryScope::Recursive => Self::RecursiveDirectory,
        }
    }

    fn as_i32(self) -> i32 {
        self as i32
    }
}

fn scan_reason_code(reason: ScanStopReason) -> i32 {
    match reason {
        ScanStopReason::Natural => 0,
        ScanStopReason::UserCancel => 1,
        ScanStopReason::CeilingHit => 2,
        ScanStopReason::IncompleteIo => 3,
    }
}

/// Allowed font file extensions (lowercase).
// Exposed `pub` so integration tests (test_scan.rs) can pattern-match
// against the same canonical list instead of re-enumerating a sibling
// literal that could drift.
pub const ALLOWED_FONT_EXTENSIONS: &[&str] = &["ttf", "otf", "ttc", "otc"];

/// Defense-in-depth ceiling on faces emitted from a single scan. Not a UX
/// limit — real font-collection users with thousands of files should never
/// hit this. Caps malicious/runaway directories whose IPC and SQLite work
/// would otherwise grow without bound. Above this, partial results are
/// preserved and the scan stops.
///
/// Off-by-one note: the check `if total > MAX_FONTS_PER_SCAN` runs INSIDE
/// the per-face inner loop, AFTER the entry was pushed and `total`
/// incremented. The check fires on the SAME iteration as the push that
/// crossed the threshold — the (cap+1)th push sets `total = cap+1`,
/// the inline check sees `cap+1 > cap` and breaks immediately. Final
/// buffer size = `MAX_FONTS_PER_SCAN + 1`, one over the cap. Kept this
/// way deliberately so the final emit carries the entry that tripped
/// the gate (rather than discarding it post-push). An earlier wording
/// claimed `MAX_FONTS_PER_SCAN + MAX_TTC_FACES - 1`, which would have
/// been true if the check ran once per FILE; it runs once per FACE, so
/// the `MAX_TTC_FACES` term doesn't apply. A prior wording also said
/// "fires on the very next iteration once total crosses", which
/// suggested a one-iteration delay; the check fires same-iteration.
const MAX_FONTS_PER_SCAN: usize = 100_000;

// MAX_INPUT_PATHS lives in `util` so dropzone and fonts share a single
// definition. `MAX_FONTS_PER_SCAN` (the per-entry face cap) still
// applies independently inside the scan loop.

/// Number of faces accumulated before flushing a `ScanProgress::Batch`.
///
/// This value is a UX choice, NOT a correctness gate. Correctness lives in
/// the `ScanProgress::Done` sentinel — the frontend awaits Done before
/// reporting final registration counts. Do NOT remove the Done sentinel as
/// "redundant" — it carries the load-bearing `reason` + `added` +
/// `duplicated` payload AND signals end-of-stream so the frontend's
/// donePromise resolves; Channel delivery in-order also lets the frontend
/// safely report registered counts only after every preceding Batch has
/// drained. (Pre-SQLite, Done was additionally needed to dodge Tauri
/// Channel's 8 KB sync/async split when batches contained full font
/// entries; post-SQLite the payload is constant-tiny so that motivation
/// is gone, but the four reasons above remain.)
///
/// Channel-budget context: since the SQLite migration, `ScanProgress::Batch`
/// payload is constant-tiny (one `usize` count). The 8 KB direct-eval
/// threshold no longer applies — every batch goes via the synchronous
/// direct-eval path. The only budget this size needs to respect is event rate: too
/// many events too fast saturate the WebView2 main thread. Aim for ≤ ~10
/// emits per second visible to the UI; combined with `SCAN_BATCH_INTERVAL`
/// below, batch=40 sits well inside that envelope.
///
/// The flush check lives inside the per-file face loop, so actual emitted
/// batches are capped at this face count even when one TTC/OTC file expands
/// into many faces.
const SCAN_BATCH_SIZE: usize = 40;

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
///
/// `u32` (not `usize`) because the loop uses it as the face index passed
/// to `fontcull_skrifa::FontRef::from_index(_, u32)` — matching the parser
/// API avoids casts in the hot loop.
const MAX_TTC_FACES: u32 = 16;

/// Cap on raw font data read for scanning/subsetting — prevents OOM with large
/// CJK fonts while still accepting known real-world CJK collections just under
/// 60 MiB.
const MAX_FONT_DATA_SIZE: u64 = 64 * 1024 * 1024;
const UNICODE_SCALAR_MAX: u32 = 0x10FFFF;

/// Cap on each in-memory font-provenance cache (`ALLOWED_FONT_PATHS`
/// and `ALLOWED_CACHE_FONT_PATHS`), as a defense against a pathological
/// long-running session. User-picked font provenance is stored in the
/// session SQLite index instead of an in-memory set, so XL source folders
/// do not pin tens of gigabytes of path/name metadata.
///
/// Cap applies independently to each provenance set (up to 100k entries per set).
const MAX_PROVENANCE_CACHE_SIZE: usize = 100_000;

/// AppData filename for the session-only user font index. It is cleared at
/// app startup; persistence across restarts is intentionally deferred.
///
/// Exposed `pub` so the GUI session index and CLI `TempFontDbDir` cleanup
/// share a single filename definition.
pub const USER_FONT_DB_FILENAME: &str = "user-font-sources.session.sqlite3";

/// Cap on directory entries the preflight pass will canonicalize before
/// bailing out. Real font folders top out around 20–30k entries even in
/// the XL bucket; a directory exceeding this is either a misclick onto
/// a system root or a hostile fixture, and either way the user wants
/// "directory too large to preview" feedback rather than a frozen UI
/// while millions of canonicalize calls run.
pub(crate) const MAX_PREFLIGHT_ENTRIES: usize = 200_000;

/// Maximum number of real directories a recursive font-library scan may
/// retain in its bounded traversal/cache manifest. The representative large
/// library used by this project has 157 leaf folders, so 4,096 leaves ample
/// headroom without allowing a picked drive root to grow memory without bound.
pub(crate) const MAX_SCAN_DIRECTORIES: usize = 4_096;

/// Maximum nesting beneath a selected recursive library root. Font packs are
/// normally one to four levels deep; 64 prevents pathological directory trees
/// from turning the iterative walker into an unbounded path-retention task.
pub(crate) const MAX_SCAN_DEPTH: usize = 64;

/// Global candidate-file budget for one directory scan. This is independent
/// of the face ceiling because TTC/OTC files can contain multiple faces.
pub(crate) const MAX_SCAN_FONT_FILES: usize = 50_000;

/// Global amount of candidate font data a scan may inspect. The known large
/// corpus is about 54 GiB; 128 GiB accommodates it while bounding total I/O
/// even though each individual file also has the 64 MiB cap.
pub(crate) const MAX_SCAN_TOTAL_FONT_BYTES: u64 = 128 * 1024 * 1024 * 1024;

/// Bound on path bytes retained by the recursive DFS stack and per-directory
/// sorted entry vectors. This is a separate memory budget from entry count.
pub(crate) const MAX_SCAN_PATH_BYTES: usize = 64 * 1024 * 1024;

/// Maximum codepoint count per `subset_font` call. Bounds fontcull's
/// IntSet allocation against attacker-influenced IPC payloads. A
/// realistic CJK subset is in the low tens of thousands; 200k covers
/// every legitimate single-font usage with headroom while rejecting
/// crafted megabyte-class codepoint arrays before any allocation.
/// Extracted from an inline `200_000` literal in `subset_font` to match
/// the named-const convention every other cap in this module follows.
///
/// `pub` so the CLI bin's `MAX_SUBSET_CODEPOINTS_FOR_DEDUP`
/// (which MUST equal this value — the dedup-merge cap is bounded by
/// what subset_font itself accepts) can both reference it AND assert
/// equality at test time. The TS sibling in
/// `src/features/font-embed/font-embedder.ts` mirrors the same value;
/// its WHY comment names this constant as the source of truth.
pub const MAX_SUBSET_CODEPOINTS: usize = 200_000;

/// Maximum font-collection face index accepted at `subset_font`'s IPC
/// boundary. TTC files practically cap at 16 faces (per
/// `MAX_TTC_FACES`); 255 is the defense-in-depth ceiling, far past
/// any legitimate font collection and inside u8 range so the value
/// fits the OpenType `numFonts` field shape. Extracted from an inline
/// `255` literal in `subset_font`.
pub(crate) const MAX_SUBSET_FONT_INDEX: u32 = 255;

/// Strip the Win32 extended-length prefix (`\\?\` / `\\?\UNC\`) that
/// `canonicalize()` adds on Windows, so paths compare consistently
/// across insert and lookup. UNC form `\\?\UNC\server\share\…` rewrites
/// to `\\server\share\…` (the standard UNC representation); the local
/// form `\\?\C:\…` rewrites to `C:\…`. Without the UNC branch, network-
/// share fonts would land in the dedup HashSet under a different prefix
/// than their non-prefixed equivalents and fail equivalence dedup.
pub(crate) fn normalize_canonical_path(canonical_str: &str) -> String {
    if let Some(unc) = canonical_str.strip_prefix("\\\\?\\UNC\\") {
        format!("\\\\{unc}")
    } else if let Some(stripped) = canonical_str.strip_prefix("\\\\?\\") {
        stripped.to_string()
    } else {
        canonical_str.to_string()
    }
}

/// Cap on the number of font faces a single directory scan will
/// snapshot into the GUI / CLI persistent cache. Above this threshold
/// the cache populate is skipped with a WARN log — session-DB import
/// still succeeds, the user just doesn't get cross-launch acceleration
/// for that source. Defense-in-depth against GUI cache OOM and
/// refresh-fonts OOM: real font libraries top out at a few thousand
/// faces; 20k is 5× margin over anything legitimate.
pub const MAX_CACHE_POPULATE_FACES: usize = 32_768;

/// Serializes unit tests that mutate scan/session/provenance globals across
/// both this module and the GUI font-cache command module.
#[cfg(test)]
static SCAN_TEST_LOCK: Mutex<()> = Mutex::new(());

#[cfg(test)]
pub(crate) fn lock_font_state_for_test() -> std::sync::MutexGuard<'static, ()> {
    SCAN_TEST_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

#[derive(Debug, Clone)]
pub struct FontSourceImportSummary {
    pub total: usize,
    pub added: usize,
    pub duplicated: usize,
    pub reason: ScanStopReason,
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
/// `face_names` holds full-face aliases such as `Dream Han Serif SC W22` or a
/// PostScript name. These identify a specific face/weight, so lookup indexes
/// them style-insensitively: an ASS file can name the W22 face directly even
/// when its separate Bold column is `0`. Some fonts also repeat a full face
/// name in their family-name table; keep that full-name alias anyway so the
/// concrete face remains directly addressable.
///
/// The entry count reported to users reflects font files/faces (not variants),
/// so a folder with 3 TTFs shows as "3 fonts" even if we pulled 8 matchable
/// name variants from them.
///
/// `Deserialize` is derived only under `#[cfg(test)]` — production code
/// never deserializes these (frontend gets source summaries, scan
/// pipeline produces them in-process). Gating keeps the
/// no-untrusted-deser invariant explicit at the type level.
#[derive(Clone, serde::Serialize)]
#[cfg_attr(test, derive(serde::Deserialize))]
pub struct LocalFontEntry {
    /// Canonical path to the font file (may be shared across entries for TTC)
    pub path: String,
    /// Face index within the file (0 for TTF/OTF, 0..n for TTC/OTC)
    pub index: u32,
    /// All localized family-name variants for this face. The primary (the one
    /// shown in the UI) is `families[0]`; the rest exist for matching only.
    pub families: Vec<String>,
    /// Full-face / PostScript aliases that identify this exact face.
    pub face_names: Vec<String>,
    /// True when OS/2 weight >= 600 (SemiBold+). Matches ASS \b1 semantics.
    pub bold: bool,
    /// True for Italic or Oblique styles.
    pub italic: bool,
    /// File size on disk — useful for UI display.
    pub size_bytes: u64,
}

/// Convert scan entries to persistent-cache rows, dedup-statting each
/// distinct file path once (TTC files contribute N entries with the
/// same `path`, so a naive per-entry stat was N+1 syscalls per TTC).
/// Shared by the GUI's locked cache publisher, drift rescan, and the CLI's
/// `run_refresh_fonts` loop.
///
/// Saturating u64→i64 on `size_bytes` and u32→i32 on `face_index`
/// keeps with the codebase's cast-discipline pattern; the limits are
/// impossible in practice (8.4 EB / 2.1 G faces) but the explicit
/// saturate makes intent visible.
pub fn entries_to_cache_metadata(
    entries: &[LocalFontEntry],
) -> Result<Vec<crate::font_cache::FontMetadata>, String> {
    // (lifetime tie WHY): `mtime_cache`'s key is
    // `&str` borrowed from `entries[i].path` — the same borrow that
    // `iter().map()` walks below. The borrow lives only as long as
    // `entries`, which is bound to the function-parameter lifetime,
    // so the HashMap never outlives its keys. A future refactor that
    // changes `entries.iter()` to `entries.into_iter()` (taking
    // ownership / consuming each LocalFontEntry) would invalidate
    // these borrows mid-loop — the `String` clone of `e.path` for
    // `FontMetadata.file_path` below would no longer be the same
    // memory the HashMap key points to. Switch the HashMap key to
    // `String` at that point.
    let mut mtime_cache: std::collections::HashMap<&str, i64> = std::collections::HashMap::new();
    let mut metadata = Vec::with_capacity(entries.len());
    for e in entries {
        // Route through the shared `try_modified_at` helper in
        // `font_cache.rs` so every stat-time extraction site stays
        // single-source. An earlier version reproduced the helper
        // body inline; a future change to the helper (e.g., its
        // `.ok()?` for pre-epoch mtime safety) would have left this
        // site behind.
        //
        // Fail the whole publication when a scanned face becomes unreadable.
        // A source-level cache row must describe one complete, stable scan;
        // silently dropping a face would make the row look authoritative
        // until some later drift happens to invalidate it.
        let mtime = *mtime_cache.entry(e.path.as_str()).or_insert_with(|| {
            crate::font_cache::try_modified_at(Path::new(e.path.as_str())).unwrap_or(-1)
        });
        if mtime < 0 {
            return Err(format!(
                "Font metadata changed or became unreadable before caching: {}",
                e.path
            ));
        }
        let mut family_keys: Vec<crate::font_cache::FamilyKey> = Vec::new();
        for family_name in &e.families {
            family_keys.push(crate::font_cache::FamilyKey {
                family_name: family_name.clone(),
                bold: e.bold,
                italic: e.italic,
            });
        }

        metadata.push(crate::font_cache::FontMetadata {
            file_path: e.path.clone(),
            file_size: i64::try_from(e.size_bytes).unwrap_or(i64::MAX),
            file_mtime: mtime,
            face_index: i32::try_from(e.index).unwrap_or(i32::MAX),
            family_keys,
            face_name_aliases: e.face_names.clone(),
        });
    }
    Ok(metadata)
}

pub(crate) fn has_allowed_font_extension(path: &Path) -> bool {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase())
        .unwrap_or_default();
    ALLOWED_FONT_EXTENSIONS.contains(&ext.as_str())
}

pub async fn preflight_font_directory(
    dir: String,
    scope: Option<FontDirectoryScope>,
    scan_id: u64,
) -> Result<FontDirectoryScanPreflight, String> {
    scan::preflight_font_directory_cmd(dir, scope, scan_id, |_| true).await
}

pub async fn preflight_font_directory_with_policy<P>(
    dir: String,
    scope: Option<FontDirectoryScope>,
    scan_id: u64,
    is_allowed: P,
) -> Result<FontDirectoryScanPreflight, String>
where
    P: Fn(&Path) -> bool + Send + Sync + 'static,
{
    scan::preflight_font_directory_cmd(dir, scope, scan_id, is_allowed).await
}

pub async fn preflight_font_files(paths: Vec<String>) -> Result<FontScanPreflight, String> {
    scan::preflight_font_files_cmd(paths, |_| true).await
}

pub async fn preflight_font_files_with_policy<P>(
    paths: Vec<String>,
    is_allowed: P,
) -> Result<FontScanPreflight, String>
where
    P: Fn(&Path) -> bool + Send + Sync + 'static,
{
    scan::preflight_font_files_cmd(paths, is_allowed).await
}

pub async fn scan_font_directory(
    dir: String,
    progress: tauri::ipc::Channel<ScanProgress>,
    scan_id: u64,
    source_id: String,
    scope: Option<FontDirectoryScope>,
) -> Result<FontDirectoryScanCommandResult, String> {
    scan::scan_font_directory_cmd(dir, progress, scan_id, source_id, scope, |_| true).await
}

pub async fn scan_font_directory_with_policy<P>(
    dir: String,
    progress: tauri::ipc::Channel<ScanProgress>,
    scan_id: u64,
    source_id: String,
    scope: Option<FontDirectoryScope>,
    is_allowed: P,
) -> Result<FontDirectoryScanCommandResult, String>
where
    P: Fn(&Path) -> bool + Send + Sync + 'static,
{
    scan::scan_font_directory_cmd(dir, progress, scan_id, source_id, scope, is_allowed).await
}

pub async fn scan_font_files(
    paths: Vec<String>,
    progress: tauri::ipc::Channel<ScanProgress>,
    scan_id: u64,
    source_id: String,
) -> Result<(), String> {
    scan::scan_font_files_cmd(paths, progress, scan_id, source_id, |_| true).await
}

pub async fn scan_font_files_with_policy<P>(
    paths: Vec<String>,
    progress: tauri::ipc::Channel<ScanProgress>,
    scan_id: u64,
    source_id: String,
    is_allowed: P,
) -> Result<(), String>
where
    P: Fn(&Path) -> bool + Send + Sync + 'static,
{
    scan::scan_font_files_cmd(paths, progress, scan_id, source_id, is_allowed).await
}

/// Cooperative cancel for an active font scan. Targeted by scan id so stale
/// requests from older scans cannot cancel newer work.
#[tauri::command]
pub fn cancel_font_scan(scan_id: u64) {
    scan::cancel_font_scan_cmd(scan_id);
}

#[cfg(test)]
mod layout_tests;

#[cfg(test)]
mod tests;
