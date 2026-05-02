use font_kit::family_name::FamilyName;
use font_kit::handle::Handle;
use font_kit::properties::{Properties, Style, Weight};
use font_kit::source::SystemSource;
use once_cell::sync::Lazy;
use rusqlite::{params, Connection, OptionalExtension, Transaction};
use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use crate::util::validate_ipc_path;

/// Allowed font file extensions (lowercase).
const ALLOWED_FONT_EXTENSIONS: &[&str] = &["ttf", "otf", "ttc", "otc"];

/// Defense-in-depth ceiling on faces emitted from a single scan. Not a UX
/// limit — real font-collection users with thousands of files should never
/// hit this. Caps malicious/runaway directories whose IPC and SQLite work
/// would otherwise grow without bound. Above this, partial results are
/// preserved and the scan stops.
///
/// Off-by-one note: the check `if total > MAX_FONTS_PER_SCAN` runs AFTER
/// the entry has already been pushed and `total` incremented, so the cap
/// is a soft ceiling — up to `MAX + 1` entries can land in the buffer
/// before the early-return fires. Kept this way deliberately so the final
/// flush emits everything that was already parseable.
const MAX_FONTS_PER_SCAN: usize = 100_000;

/// Cap on the path-list length accepted by `scan_font_files`. Mirrors
/// `dropzone::MAX_INPUT_PATHS` — the OS file picker can't realistically
/// deliver more than a handful of thousand files in one selection, so 1000
/// is generous for the user-facing flow while bounding worst-case CPU/IO
/// if a future code path or compromised frontend supplies a huge vector.
/// The per-entry MAX_FONTS_PER_SCAN ceiling still applies inside the loop.
const MAX_INPUT_PATHS: usize = 1000;

/// Number of faces accumulated before flushing a `ScanProgress::Batch`.
///
/// This value is a UX choice, NOT a correctness gate. Correctness lives in
/// the `ScanProgress::Done` sentinel — the frontend awaits Done before
/// reporting final registration counts. Do NOT remove the Done sentinel as
/// "redundant" because it is still the end-of-stream marker for Channel
/// delivery and Rust-side source registration.
///
/// Channel-budget context: since the SQLite migration, `ScanProgress::Batch`
/// payload is constant-tiny (one `usize` count). The 8 KB direct-eval
/// threshold (Budget 1 in `reference_tauri_channel_perf.md`) no longer
/// applies — every batch goes via the synchronous direct-eval path. The
/// only budget this size needs to respect is event rate (Budget 2): too
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
const MAX_TTC_FACES: u32 = 16;

/// Cap on raw font data read for subsetting — prevents OOM with large CJK
/// fonts and mirrors the front-end guard in `ass-uuencode.ts`.
const MAX_FONT_DATA_SIZE: u64 = 50 * 1024 * 1024;

/// Cap on the unmodified font emitted by the subset fallback path. Lower
/// than `MAX_FONT_DATA_SIZE` because the fallback sends the full font through
/// IPC → JS heap → ASS string.
const MAX_FONT_FALLBACK_SIZE: usize = 10 * 1024 * 1024;

/// Cap on the system-font provenance cache, as a defense against a
/// pathological long-running session. User-picked font provenance is stored
/// in the session SQLite index instead of an in-memory set, so XL source
/// folders do not pin tens of gigabytes of path/name metadata.
const MAX_PROVENANCE_CACHE_SIZE: usize = 100_000;

/// AppData filename for the session-only user font index. It is cleared at
/// app startup; persistence across restarts is intentionally deferred.
const USER_FONT_DB_FILENAME: &str = "user-font-sources.session.sqlite3";

/// Cap on directory entries the preflight pass will canonicalize before
/// bailing out. Real font folders top out around 20–30k entries even in
/// the XL bucket; a directory exceeding this is either a misclick onto
/// a system root or a hostile fixture, and either way the user wants
/// "directory too large to preview" feedback rather than a frozen UI
/// while millions of canonicalize calls run.
const MAX_PREFLIGHT_ENTRIES: usize = 200_000;

/// Strip the Win32 extended-length UNC prefix (`\\?\`) that `canonicalize()`
/// adds on Windows, so paths compare consistently across insert and lookup.
fn normalize_canonical_path(canonical_str: &str) -> String {
    if let Some(stripped) = canonical_str.strip_prefix("\\\\?\\") {
        stripped.to_string()
    } else {
        canonical_str.to_string()
    }
}

/// Provenance cache: tracks font paths returned by `find_system_font`.
/// Only paths that were discovered through the font lookup API are allowed
/// to be read by `subset_font`, preventing arbitrary file reads via IPC.
/// Never evicted — the set is bounded by the number of unique system fonts
/// (typically < 1000), and eviction would introduce TOCTOU windows.
static ALLOWED_FONT_PATHS: Lazy<Mutex<HashSet<String>>> = Lazy::new(|| Mutex::new(HashSet::new()));

/// Session SQLite path for user-picked font sources. Commands open short-lived
/// connections to this path instead of sharing a global Connection, which keeps
/// the static state simple and avoids holding SQLite page caches for longer
/// than one operation.
static USER_FONT_DB_PATH: Lazy<Mutex<Option<PathBuf>>> = Lazy::new(|| Mutex::new(None));

#[derive(Debug, Clone, Copy, Default)]
struct ImportOutcome {
    added: usize,
    duplicated: usize,
}

fn db_error(context: &str, error: rusqlite::Error) -> String {
    log::warn!("user font index {context}: {error}");
    format!("Internal error: user font index {context}")
}

fn user_font_db_path() -> Result<PathBuf, String> {
    USER_FONT_DB_PATH
        .lock()
        .map_err(|_| "Internal error: user font index path corrupted".to_string())?
        .clone()
        .ok_or_else(|| "User font index is not initialized".to_string())
}

fn open_user_font_db() -> Result<Connection, String> {
    let path = user_font_db_path()?;
    let conn = Connection::open(path).map_err(|e| db_error("open failed", e))?;
    conn.pragma_update(None, "foreign_keys", "ON")
        .map_err(|e| db_error("foreign_keys setup failed", e))?;
    // WAL + 5 s busy_timeout: today the modal-scrim UX prevents two
    // commands from contending on the DB, but `is_user_font_path_registered`
    // (called from `subset_font`), `resolve_user_font` (called from
    // `analyzeFonts`), and `remove_font_source` are all reachable
    // independently of the modal. A future refactor that decouples scan
    // from the modal would surface intermittent SQLITE_BUSY as silent
    // miss/fail under default DELETE journal + busy_timeout=0. Set both
    // up-front so the database stays well-behaved across that refactor.
    conn.pragma_update(None, "journal_mode", "WAL")
        .map_err(|e| db_error("journal_mode setup failed", e))?;
    conn.pragma_update(None, "busy_timeout", 5000)
        .map_err(|e| db_error("busy_timeout setup failed", e))?;
    Ok(conn)
}

fn init_user_font_schema(conn: &Connection) -> Result<(), String> {
    conn.execute_batch(
        "
        PRAGMA foreign_keys = ON;
        CREATE TABLE IF NOT EXISTS font_sources (
            source_id TEXT PRIMARY KEY,
            source_order INTEGER NOT NULL UNIQUE
        );
        CREATE TABLE IF NOT EXISTS font_faces (
            face_id INTEGER PRIMARY KEY,
            source_id TEXT NOT NULL,
            path TEXT NOT NULL,
            face_index INTEGER NOT NULL,
            bold INTEGER NOT NULL,
            italic INTEGER NOT NULL,
            size_bytes INTEGER NOT NULL,
            FOREIGN KEY(source_id) REFERENCES font_sources(source_id) ON DELETE CASCADE,
            UNIQUE(path, face_index)
        );
        CREATE TABLE IF NOT EXISTS font_family_keys (
            key TEXT NOT NULL,
            face_id INTEGER NOT NULL,
            source_order INTEGER NOT NULL,
            FOREIGN KEY(face_id) REFERENCES font_faces(face_id) ON DELETE CASCADE
        );
        CREATE INDEX IF NOT EXISTS idx_font_family_lookup
            ON font_family_keys(key, source_order DESC, face_id DESC);
        CREATE INDEX IF NOT EXISTS idx_font_family_face
            ON font_family_keys(face_id);
        CREATE INDEX IF NOT EXISTS idx_font_faces_source
            ON font_faces(source_id);
        CREATE INDEX IF NOT EXISTS idx_font_faces_path_index
            ON font_faces(path, face_index);
        ",
    )
    .map_err(|e| db_error("schema setup failed", e))
}

/// Initialize the session-only user font index under Tauri AppData. Called
/// from app setup before any IPC command can scan or resolve user fonts.
pub fn init_user_font_db(app_data_dir: &Path) -> Result<(), String> {
    fs::create_dir_all(app_data_dir).map_err(|e| {
        log::warn!(
            "create user font index directory '{}' failed: {e}",
            app_data_dir.display()
        );
        "Cannot create user font index directory".to_string()
    })?;
    let db_path = app_data_dir.join(USER_FONT_DB_FILENAME);
    match fs::remove_file(&db_path) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => {
            log::warn!(
                "remove stale user font index '{}' failed: {e}",
                db_path.display()
            );
            return Err("Cannot reset user font index".to_string());
        }
    }
    // Best-effort cleanup of SQLite sidecars from a prior run that may have
    // crashed mid-transaction. SQLite recovers correctly when the main file
    // is reborn, so functional impact of leftover sidecars is nil — but
    // they accumulate over time and complicate forensics. Suffixes per
    // SQLite docs: -journal (rollback), -wal / -shm (write-ahead log).
    for suffix in ["-journal", "-wal", "-shm"] {
        let mut sidecar = db_path.clone().into_os_string();
        sidecar.push(suffix);
        let _ = fs::remove_file(PathBuf::from(sidecar));
    }
    {
        let mut path_slot = USER_FONT_DB_PATH
            .lock()
            .map_err(|_| "Internal error: user font index path corrupted".to_string())?;
        *path_slot = Some(db_path);
    }
    let conn = open_user_font_db()?;
    init_user_font_schema(&conn)
}

/// No frontend-created scan may use this id. Keeping zero reserved lets the
/// Rust side distinguish "no active/cancelled scan" from real work.
const NO_SCAN_ID: u64 = 0;

/// Scan id currently owned by the blocking scan worker. The UI only starts
/// one scan at a time, but this guard also prevents a compromised frontend
/// from launching overlapping scans that would race the shared provenance
/// cache and cancellation state.
static ACTIVE_SCAN_ID: AtomicU64 = AtomicU64::new(NO_SCAN_ID);

/// Highest scan id that has received a cancel request. Cancel requests are
/// targeted by id instead of using a process-wide boolean, so a late cancel
/// from an older scan cannot abort a fresh one. `fetch_max` also preserves a
/// real current cancel if an older stale command arrives afterward.
static CANCEL_SCAN_ID: AtomicU64 = AtomicU64::new(NO_SCAN_ID);

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
/// See A-bug-1 in the v1.3.1 design doc for the diagnostic data.
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
    /// End-of-stream sentinel. Always emitted on the `Ok` path (success,
    /// cancel, or defense-in-depth cap stop). NOT emitted on the `Err` path — the invoke rejection
    /// already signals failure and the frontend must not block waiting for
    /// a `Done` that will never arrive.
    Done {
        cancelled: bool,
        added: usize,
        duplicated: usize,
    },
}

#[derive(Debug, Clone, Copy)]
struct ScanOutcome {
    total: usize,
    /// True when the scan stopped early but should keep already-imported
    /// partial results. This includes user cancellation and the defensive
    /// MAX_FONTS_PER_SCAN ceiling.
    cancelled: bool,
}

struct ActiveScanGuard {
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
    }
}

fn begin_font_scan(scan_id: u64) -> Result<ActiveScanGuard, String> {
    if scan_id == NO_SCAN_ID {
        return Err("Scan id must be non-zero".to_string());
    }

    ACTIVE_SCAN_ID
        .compare_exchange(NO_SCAN_ID, scan_id, Ordering::SeqCst, Ordering::SeqCst)
        .map_err(|_| "Another font scan is already running".to_string())?;

    Ok(ActiveScanGuard { scan_id })
}

fn font_scan_cancelled(scan_id: u64) -> bool {
    scan_id != NO_SCAN_ID && CANCEL_SCAN_ID.load(Ordering::Relaxed) == scan_id
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
/// The entry count reported to users reflects font files/faces (not variants),
/// so a folder with 3 TTFs shows as "3 fonts" even if we pulled 8 matchable
/// name variants from them.
///
/// `Deserialize` is derived for focused Rust tests. The production frontend
/// no longer receives these entries; it keeps only source summaries while the
/// heavy index stays in Rust.
#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub struct LocalFontEntry {
    /// Canonical path to the font file (may be shared across entries for TTC)
    pub path: String,
    /// Face index within the file (0 for TTF/OTF, 0..n for TTC/OTC)
    pub index: u32,
    /// All localized family-name variants for this face. The primary (the one
    /// shown in the UI) is `families[0]`; the rest exist for matching only.
    pub families: Vec<String>,
    /// True when OS/2 weight >= 600 (SemiBold+). Matches ASS \b1 semantics.
    pub bold: bool,
    /// True for Italic or Oblique styles.
    pub italic: bool,
    /// File size on disk — useful for UI display.
    pub size_bytes: u64,
}

fn user_font_key(family: &str, bold: bool, italic: bool) -> String {
    format!(
        "{}|{}|{}",
        family.to_lowercase(),
        if bold { "1" } else { "0" },
        if italic { "1" } else { "0" }
    )
}

fn validate_font_source_id(source_id: &str) -> Result<(), String> {
    if source_id.is_empty() || source_id.len() > 128 {
        return Err("Font source id must be 1-128 characters".to_string());
    }
    if source_id.chars().any(|c| c.is_control() || c == '\x7f') {
        return Err("Font source id contains invalid characters".to_string());
    }
    Ok(())
}

fn has_allowed_font_extension(path: &Path) -> bool {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase())
        .unwrap_or_default();
    ALLOWED_FONT_EXTENSIONS.contains(&ext.as_str())
}

fn create_user_font_source_tx(tx: &Transaction<'_>, source_id: &str) -> Result<i64, String> {
    validate_font_source_id(source_id)?;
    let source_order: i64 = tx
        .query_row(
            "SELECT COALESCE(MAX(source_order), 0) + 1 FROM font_sources",
            [],
            |row| row.get(0),
        )
        .map_err(|e| db_error("source order query failed", e))?;
    tx.execute(
        "INSERT INTO font_sources(source_id, source_order) VALUES (?1, ?2)",
        params![source_id, source_order],
    )
    .map_err(|e| {
        if matches!(
            e,
            rusqlite::Error::SqliteFailure(
                rusqlite::ffi::Error {
                    code: rusqlite::ErrorCode::ConstraintViolation,
                    ..
                },
                _
            )
        ) {
            "Font source id already exists".to_string()
        } else {
            db_error("source insert failed", e)
        }
    })?;
    Ok(source_order)
}

fn import_user_font_batch_tx(
    tx: &Transaction<'_>,
    source_id: &str,
    source_order: i64,
    entries: Vec<LocalFontEntry>,
) -> Result<ImportOutcome, String> {
    let mut added = 0;
    let mut duplicated = 0;
    let mut insert_face = tx
        .prepare(
            "
            INSERT OR IGNORE INTO font_faces(
                source_id, path, face_index, bold, italic, size_bytes
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)
            ",
        )
        .map_err(|e| db_error("face insert prepare failed", e))?;
    let mut insert_key = tx
        .prepare(
            "
            INSERT INTO font_family_keys(key, face_id, source_order)
            VALUES (?1, ?2, ?3)
            ",
        )
        .map_err(|e| db_error("family-key insert prepare failed", e))?;

    for entry in entries {
        let size_bytes = i64::try_from(entry.size_bytes)
            .map_err(|_| "Font file size is too large for the source index".to_string())?;
        let changed = insert_face
            .execute(params![
                source_id,
                entry.path,
                entry.index,
                if entry.bold { 1 } else { 0 },
                if entry.italic { 1 } else { 0 },
                size_bytes
            ])
            .map_err(|e| db_error("face insert failed", e))?;
        if changed == 0 {
            duplicated += 1;
            continue;
        }
        added += 1;
        let face_id = tx.last_insert_rowid();
        for family in entry.families {
            insert_key
                .execute(params![
                    user_font_key(&family, entry.bold, entry.italic),
                    face_id,
                    source_order
                ])
                .map_err(|e| db_error("family-key insert failed", e))?;
        }
    }

    Ok(ImportOutcome { added, duplicated })
}

fn remove_empty_user_font_source_tx(
    tx: &Transaction<'_>,
    source_id: &str,
    added: usize,
) -> Result<(), String> {
    if added > 0 {
        return Ok(());
    }
    tx.execute(
        "DELETE FROM font_sources WHERE source_id = ?1",
        params![source_id],
    )
    .map_err(|e| db_error("empty source cleanup failed", e))?;
    Ok(())
}

fn is_user_font_path_registered(canonical_path: &str) -> Result<bool, String> {
    let conn = open_user_font_db()?;
    conn.query_row(
        "SELECT 1 FROM font_faces WHERE path = ?1 LIMIT 1",
        params![canonical_path],
        |_| Ok(()),
    )
    .optional()
    .map(|v| v.is_some())
    .map_err(|e| db_error("path lookup failed", e))
}

/// Find a system font file path by family name, bold, and italic flags.
/// Returns the path + face index. Prefers TTF/TTC over OTF/OTC for subtitle
/// renderer compatibility (libass/VSFilter don't support OTF bold).
#[tauri::command]
pub fn find_system_font(
    family: String,
    bold: bool,
    italic: bool,
) -> Result<FontLookupResult, String> {
    // Input validation: reject empty, oversized, or control-char-containing names
    if family.is_empty() || family.len() > 256 {
        return Err("Font family name must be 1-256 characters".to_string());
    }
    if family.chars().any(|c| c.is_control() || c == '\x7f') {
        return Err("Font family name contains invalid characters".to_string());
    }

    let source = SystemSource::new();

    let mut props = Properties::new();
    if bold {
        props.weight = Weight::BOLD;
    }
    if italic {
        props.style = Style::Italic;
    }

    let handle = source
        .select_best_match(&[FamilyName::Title(family.clone())], &props)
        .map_err(|e| {
            // Log the detailed error server-side; return a generic message
            // to the frontend so OS-level paths never surface in user toasts.
            // INFO not WARN: a missed system lookup is normal flow when the
            // user hasn't picked local font sources yet, and the frontend
            // surfaces "Missing" badges per font anyway. Bumping to warn
            // would spam dev logs every time a batch is analyzed before
            // sources are added; release builds (Warn+) hide info entirely.
            log::info!("font lookup failed for '{family}' (bold={bold}, italic={italic}): {e}");
            format!("Font not found: {family} (bold={bold}, italic={italic})")
        })?;

    match handle {
        Handle::Path { path, font_index } => {
            let ext = path
                .extension()
                .and_then(|e| e.to_str())
                .map(|e| e.to_lowercase())
                .unwrap_or_default();

            // OTF/OTC warning: libass/VSFilter don't support OTF bold rendering.
            // font-kit's select_best_match returns the system's preferred match
            // and has no API to filter by format. Enumerating the family and
            // loading each face to check style properties is too expensive for
            // a font lookup hot path. Accept the OTF and warn.
            if ext == "otf" || ext == "otc" {
                log::warn!(
                    "Using OTF font for '{}' — bold may not render in libass/VSFilter",
                    family
                );
            }

            register_font_path(&path, font_index)
        }
        Handle::Memory { .. } => Err("Font is memory-only (no file path available)".to_string()),
    }
}

/// Cap on distinct family-name variants pulled from one font face. Real fonts
/// have 2–4 (English + localized); 32 is a generous safety ceiling against a
/// pathological name table.
const MAX_FAMILY_VARIANTS_PER_FACE: usize = 32;

/// Parse one font file (TTF/OTF/TTC/OTC) and return a `LocalFontEntry` per
/// face **and per distinct localized family name** in the face's name table.
///
/// A single TTF can declare its family under multiple languages (common with
/// CJK fonts that ship both an English and a Chinese name). We emit one entry
/// per variant so the frontend matcher finds the font no matter which name the
/// ASS script happens to reference. This was the root cause of the "font not
/// recognized" symptom: font-kit's `family_name()` returns only the
/// locale-preferred name, which on zh-CN Windows silently shadowed English
/// family names that subtitle scripts typically use.
///
/// `canonical` must already be canonicalized by the caller. User provenance
/// is registered later when the emitted batch is committed to the session
/// SQLite index.
///
/// `scan_id` lets the per-face inner loop poll cancellation BETWEEN faces.
/// Without this, a single TTC with up to `MAX_TTC_FACES` slow-to-parse
/// faces could stall the cancel-acknowledge loop for several seconds (the
/// outer scan only polls between FILES). Pass `NO_SCAN_ID` from contexts
/// without an active scan (e.g., direct unit tests).
fn parse_local_font_file(canonical: &Path, scan_id: u64) -> Vec<LocalFontEntry> {
    use fontcull_skrifa::string::StringId;
    use fontcull_skrifa::{FontRef, MetadataProvider};

    // Extension check is intentionally case-insensitive (.TTF vs .ttf are the
    // same file format). The ASCII-lowercase conversion is correct here — all
    // ALLOWED_FONT_EXTENSIONS entries are ASCII.
    let ext = canonical
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase())
        .unwrap_or_default();
    if !has_allowed_font_extension(canonical) {
        return Vec::new();
    }

    // Stat + size-cap guard before fs::read — a malicious user-picked
    // directory could otherwise OOM the process by containing a .ttf
    // that's actually a hundred-gigabyte impostor file. Aligns with
    // subset_font's own MAX_FONT_DATA_SIZE cap.
    let size_bytes = match fs::metadata(canonical) {
        Ok(m) => {
            if m.len() > MAX_FONT_DATA_SIZE {
                return Vec::new();
            }
            m.len()
        }
        Err(_) => return Vec::new(),
    };
    let is_collection = ext == "ttc" || ext == "otc";
    let max_faces = if is_collection { MAX_TTC_FACES } else { 1 };

    let canonical_string = normalize_canonical_path(&canonical.to_string_lossy());

    // Read the file once; share the bytes between font-kit (weight/style
    // detection) via Arc and skrifa (name-table enumeration) via a slice.
    let data = match fs::read(canonical) {
        Ok(d) => d,
        Err(_) => return Vec::new(),
    };
    let arc_data = std::sync::Arc::new(data);

    let mut entries = Vec::new();
    // Permit a single consecutive parse failure before giving up. In practice
    // font_kit returns an error once `i` exceeds the real face count, so one
    // tolerance catches that natural end-of-collection while keeping the
    // per-file parse cost bounded at 2 × face_count rather than 3 ×. Crafted
    // TTCs cannot force us to parse all 64 slots just by salting every other
    // face with bad data.
    const MAX_CONSECUTIVE_FAILURES: u32 = 1;
    let mut consecutive_failures: u32 = 0;
    for i in 0..max_faces {
        // Per-face cancel poll. The outer scan_*_inner loops only check
        // between files; a 16-face TTC where each face triggers expensive
        // skrifa name-table walks can otherwise eat several seconds of
        // unresponsive Cancel button. NO_SCAN_ID is the "no active scan"
        // sentinel and must never trigger cancellation.
        if font_scan_cancelled(scan_id) {
            break;
        }
        // font-kit for weight/style — its enum API is simpler than reading
        // OS/2 directly through skrifa.
        let fk_font = match font_kit::font::Font::from_bytes(arc_data.clone(), i) {
            Ok(f) => {
                consecutive_failures = 0;
                f
            }
            Err(_) => {
                consecutive_failures += 1;
                if consecutive_failures > MAX_CONSECUTIVE_FAILURES {
                    break;
                }
                continue;
            }
        };
        let props = fk_font.properties();
        let bold = props.weight.0 >= 600.0;
        let italic = !matches!(props.style, Style::Normal);

        // skrifa for ALL localized family names — this is the key fix.
        let font_ref = match FontRef::from_index(&arc_data, i) {
            Ok(f) => f,
            Err(_) => {
                consecutive_failures += 1;
                if consecutive_failures > MAX_CONSECUTIVE_FAILURES {
                    break;
                }
                continue;
            }
        };

        let mut family_variants: HashSet<String> = HashSet::new();
        for id in [StringId::FAMILY_NAME, StringId::TYPOGRAPHIC_FAMILY_NAME] {
            for localized in font_ref.localized_strings(id) {
                // Take chars lazily with a short ceiling so a malformed
                // font with a 2GB name-table entry can't OOM the process
                // before the length guard fires. 257 chars is enough to
                // detect ">256 chars" overflow in the guard below.
                let name: String = localized.chars().take(257).collect();
                let trimmed = name.trim();
                // Guard counts CODEPOINTS, not bytes — a 100-char CJK
                // family name (300+ UTF-8 bytes) is perfectly legitimate,
                // and the previous byte-length gate silently dropped such
                // names on non-Latin fonts.
                let char_count = trimmed.chars().count();
                if !trimmed.is_empty() && char_count <= 256 {
                    family_variants.insert(trimmed.to_string());
                    if family_variants.len() >= MAX_FAMILY_VARIANTS_PER_FACE {
                        break;
                    }
                }
            }
            if family_variants.len() >= MAX_FAMILY_VARIANTS_PER_FACE {
                break;
            }
        }

        // Fallback: if the name table produced nothing, emit one entry using
        // font-kit's single-name API so the font isn't silently dropped.
        if family_variants.is_empty() {
            let fallback = fk_font.family_name();
            if !fallback.trim().is_empty() {
                family_variants.insert(fallback);
            }
        }

        if family_variants.is_empty() {
            continue;
        }

        // Stabilize the primary-name pick: prefer font-kit's family_name if
        // it's among the variants, else fall back to a sorted order so UI
        // listings stay deterministic across runs (HashSet iteration order
        // is not guaranteed). family_variants is a HashSet, so no duplicates
        // can leak into the sorted list.
        let primary = fk_font.family_name();
        let mut families: Vec<String> = family_variants.into_iter().collect();
        families.sort();
        if let Some(pos) = families.iter().position(|v| v == &primary) {
            // rotate_right(1) moves families[pos] to index 0 while keeping
            // the elements before it in alphabetical order — swap(0, pos)
            // would displace the element at 0 to pos, breaking sort order.
            families[..=pos].rotate_right(1);
        }

        entries.push(LocalFontEntry {
            path: canonical_string.clone(),
            index: i,
            families,
            bold,
            italic,
            size_bytes,
        });
    }
    entries
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FontScanPreflight {
    font_files: usize,
    total_bytes: u64,
}

fn add_preflight_file(path: &Path, out: &mut FontScanPreflight) {
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

fn preflight_directory_inner(canonical_dir: &Path) -> Result<FontScanPreflight, String> {
    let read = fs::read_dir(canonical_dir).map_err(|e| {
        log::warn!(
            "preflight read_dir failed for '{}': {e}",
            canonical_dir.display()
        );
        "Cannot read directory".to_string()
    })?;
    let mut out = FontScanPreflight {
        font_files: 0,
        total_bytes: 0,
    };
    let mut visited: usize = 0;
    for entry in read {
        visited += 1;
        if visited > MAX_PREFLIGHT_ENTRIES {
            return Err(format!(
                "Directory has too many entries to preview (>{MAX_PREFLIGHT_ENTRIES})"
            ));
        }
        let Ok(entry) = entry else {
            continue;
        };
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let Ok(canonical) = path.canonicalize() else {
            continue;
        };
        if !canonical.starts_with(canonical_dir) {
            continue;
        }
        add_preflight_file(&canonical, &mut out);
    }
    Ok(out)
}

fn preflight_files_inner(paths: Vec<String>) -> FontScanPreflight {
    let mut out = FontScanPreflight {
        font_files: 0,
        total_bytes: 0,
    };
    let mut seen = HashSet::new();
    for p in paths {
        if validate_ipc_path(&p, "File").is_err() {
            continue;
        }
        let Ok(canonical) = Path::new(&p).canonicalize() else {
            continue;
        };
        if !canonical.is_file()
            || !seen.insert(normalize_canonical_path(&canonical.to_string_lossy()))
        {
            continue;
        }
        add_preflight_file(&canonical, &mut out);
    }
    out
}

#[tauri::command]
pub async fn preflight_font_directory(dir: String) -> Result<FontScanPreflight, String> {
    validate_ipc_path(&dir, "Directory")?;
    tauri::async_runtime::spawn_blocking(move || {
        let canonical_dir = Path::new(&dir).canonicalize().map_err(|e| {
            log::warn!("preflight canonicalize directory failed: {e}");
            "Cannot resolve directory path".to_string()
        })?;
        if !canonical_dir.is_dir() {
            return Err("Not a directory".to_string());
        }
        preflight_directory_inner(&canonical_dir)
    })
    .await
    .map_err(|e| format!("Font preflight worker failed: {e}"))?
}

#[tauri::command]
pub async fn preflight_font_files(paths: Vec<String>) -> Result<FontScanPreflight, String> {
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

/// Streaming scan of a user-picked directory (one level deep). Faces are
/// emitted to `emit_batch` in chunks of up to `SCAN_BATCH_SIZE` (or every
/// `SCAN_BATCH_INTERVAL` when parsing is slower than batching). Returns the
/// total face count on success, or an error if the directory is unreadable.
/// Cancellation via `cancel_font_scan(scan_id)` returns a cancelled outcome
/// with all already-emitted batches retained by the caller.
///
/// Does NOT recurse — the `Fonts/` convention is flat by tradition, and
/// limiting recursion keeps the "only files under the picked directory"
/// security reasoning straightforward.
fn scan_directory_inner<F: FnMut(Vec<LocalFontEntry>) -> Result<(), String>>(
    canonical_dir: &Path,
    scan_id: u64,
    mut emit_batch: F,
) -> Result<ScanOutcome, String> {
    let read = fs::read_dir(canonical_dir).map_err(|e| {
        log::warn!("read_dir failed for '{}': {e}", canonical_dir.display());
        "Cannot read directory".to_string()
    })?;

    let mut buffer: Vec<LocalFontEntry> = Vec::new();
    let mut total: usize = 0;
    let mut last_emit = Instant::now();

    for entry in read {
        if font_scan_cancelled(scan_id) {
            // Flush any in-flight batch before returning so the frontend
            // sees every face parsed before cancellation. Cancellation is
            // polled between files; a single large font parse must finish
            // before this branch can run.
            if !buffer.is_empty() {
                emit_batch(std::mem::take(&mut buffer))?;
            }
            log::info!(
                "font scan {} cancelled in directory '{}' after {} faces",
                scan_id,
                canonical_dir.display(),
                total
            );
            return Ok(ScanOutcome {
                total,
                cancelled: true,
            });
        }

        let entry = match entry {
            Ok(e) => e,
            Err(_) => continue,
        };
        let path = entry.path();
        if !path.is_file() {
            continue;
        }

        // Canonicalize per-entry to follow symlinks/reparse points, then
        // verify the resolved file is still under the picked directory.
        // This is what blocks a symlink inside the chosen Fonts/ folder
        // from pointing at /etc/shadow or similar.
        let canonical = match path.canonicalize() {
            Ok(c) => c,
            Err(_) => continue,
        };
        if !canonical.starts_with(canonical_dir) {
            continue;
        }

        for font_entry in parse_local_font_file(&canonical, scan_id) {
            buffer.push(font_entry);
            total += 1;
            if total > MAX_FONTS_PER_SCAN {
                if !buffer.is_empty() {
                    emit_batch(std::mem::take(&mut buffer))?;
                }
                // info, not warn — release builds (level WARN+) would
                // otherwise emit user paths into log files. Most other
                // path-bearing logs in this module already use info for
                // the same reason; this one was the odd one out.
                log::info!(
                    "font scan {} hit the {MAX_FONTS_PER_SCAN}-face ceiling in directory '{}'",
                    scan_id,
                    canonical_dir.display()
                );
                return Ok(ScanOutcome {
                    total,
                    cancelled: true,
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

    Ok(ScanOutcome {
        total,
        cancelled: false,
    })
}

/// Tauri command wrapping `scan_directory_inner` with a typed progress
/// channel. Frontend creates a `Channel<ScanProgress>`, passes it as
/// `progress`, and receives `Batch` events as faces are parsed.
#[tauri::command]
pub async fn scan_font_directory(
    dir: String,
    progress: tauri::ipc::Channel<ScanProgress>,
    scan_id: u64,
    source_id: String,
) -> Result<(), String> {
    validate_ipc_path(&dir, "Directory")?;
    validate_font_source_id(&source_id)?;

    let active_scan = begin_font_scan(scan_id)?;
    tauri::async_runtime::spawn_blocking(move || {
        let _active_scan = active_scan;
        let canonical_dir = Path::new(&dir).canonicalize().map_err(|e| {
            log::warn!("canonicalize directory failed: {e}");
            "Cannot resolve directory path".to_string()
        })?;
        if !canonical_dir.is_dir() {
            return Err("Not a directory".to_string());
        }

        let mut conn = open_user_font_db()?;
        let tx = conn
            .transaction()
            .map_err(|e| db_error("transaction start failed", e))?;
        let source_order = create_user_font_source_tx(&tx, &source_id)?;
        let mut import = ImportOutcome::default();
        let mut progress_total = 0usize;
        let outcome = scan_directory_inner(&canonical_dir, scan_id, |batch| {
            progress_total += batch.len();
            let batch_import = import_user_font_batch_tx(&tx, &source_id, source_order, batch)?;
            import.added += batch_import.added;
            import.duplicated += batch_import.duplicated;
            let _ = progress.send(ScanProgress::Batch {
                total: progress_total,
            });
            Ok(())
        })?;
        remove_empty_user_font_source_tx(&tx, &source_id, import.added)?;
        tx.commit()
            .map_err(|e| db_error("transaction commit failed", e))?;

        // End-of-stream sentinel; see ScanProgress::Done. MUST be the last
        // send on the Ok path so every progress event has drained before
        // the frontend reports the registered source count.
        let _ = progress.send(ScanProgress::Done {
            cancelled: outcome.cancelled,
            added: import.added,
            duplicated: import.duplicated,
        });

        log::info!(
            "Scanned font directory '{}' with scan {}: {} faces total, {} added, {} duplicate{}",
            canonical_dir.display(),
            scan_id,
            outcome.total,
            import.added,
            import.duplicated,
            if outcome.cancelled {
                " (cancelled)"
            } else {
                ""
            }
        );
        Ok(())
    })
    .await
    .map_err(|e| format!("Font scan worker failed: {e}"))?
}

/// Streaming scan of a user-picked file list. Mirrors
/// `scan_directory_inner`, with cancel checks between files and the same
/// per-face batching cadence.
fn scan_files_inner<F: FnMut(Vec<LocalFontEntry>) -> Result<(), String>>(
    paths: Vec<String>,
    scan_id: u64,
    mut emit_batch: F,
) -> Result<ScanOutcome, String> {
    let mut buffer: Vec<LocalFontEntry> = Vec::new();
    let mut total: usize = 0;
    let mut last_emit = Instant::now();
    // Mirror the dedup `preflight_files_inner` already applies — a list
    // with duplicate canonical paths would otherwise re-parse each
    // duplicate, then rely on the SQLite `UNIQUE(path, face_index)`
    // constraint to discard them as `duplicated`. Wastes IO/parse time
    // and inflates the cancel-poll budget.
    let mut seen: HashSet<String> = HashSet::new();

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
            return Ok(ScanOutcome {
                total,
                cancelled: true,
            });
        }

        if validate_ipc_path(&p, "File").is_err() {
            continue;
        }

        let canonical = match Path::new(&p).canonicalize() {
            Ok(c) => c,
            Err(_) => continue,
        };
        if !canonical.is_file() {
            continue;
        }
        if !seen.insert(normalize_canonical_path(&canonical.to_string_lossy())) {
            continue;
        }

        for font_entry in parse_local_font_file(&canonical, scan_id) {
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
                return Ok(ScanOutcome {
                    total,
                    cancelled: true,
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

    Ok(ScanOutcome {
        total,
        cancelled: false,
    })
}

/// Tauri command wrapping `scan_files_inner` with a typed progress channel.
/// Same shape as `scan_font_directory` — frontend supplies the list of
/// paths and a `Channel<ScanProgress>` for incremental delivery.
#[tauri::command]
pub async fn scan_font_files(
    paths: Vec<String>,
    progress: tauri::ipc::Channel<ScanProgress>,
    scan_id: u64,
    source_id: String,
) -> Result<(), String> {
    if paths.len() > MAX_INPUT_PATHS {
        return Err(format!(
            "Too many file paths ({}, max {MAX_INPUT_PATHS})",
            paths.len()
        ));
    }
    validate_font_source_id(&source_id)?;

    let active_scan = begin_font_scan(scan_id)?;
    tauri::async_runtime::spawn_blocking(move || {
        let _active_scan = active_scan;
        let mut conn = open_user_font_db()?;
        let tx = conn
            .transaction()
            .map_err(|e| db_error("transaction start failed", e))?;
        let source_order = create_user_font_source_tx(&tx, &source_id)?;
        let mut import = ImportOutcome::default();
        let mut progress_total = 0usize;
        let outcome = scan_files_inner(paths, scan_id, |batch| {
            progress_total += batch.len();
            let batch_import = import_user_font_batch_tx(&tx, &source_id, source_order, batch)?;
            import.added += batch_import.added;
            import.duplicated += batch_import.duplicated;
            let _ = progress.send(ScanProgress::Batch {
                total: progress_total,
            });
            Ok(())
        })?;
        remove_empty_user_font_source_tx(&tx, &source_id, import.added)?;
        tx.commit()
            .map_err(|e| db_error("transaction commit failed", e))?;

        // See scan_font_directory for why Done is mandatory on the Ok path.
        let _ = progress.send(ScanProgress::Done {
            cancelled: outcome.cancelled,
            added: import.added,
            duplicated: import.duplicated,
        });

        log::info!(
            "Scanned local font files with scan {}: {} faces total, {} added, {} duplicate{}",
            scan_id,
            outcome.total,
            import.added,
            import.duplicated,
            if outcome.cancelled {
                " (cancelled)"
            } else {
                ""
            }
        );
        Ok(())
    })
    .await
    .map_err(|e| format!("Font scan worker failed: {e}"))?
}

/// Cooperative cancel for an active font scan. The request is targeted by
/// scan id; stale commands from older scans cannot cancel newer work. The
/// running scan checks between files and returns early with all
/// already-emitted batches retained on the frontend.
#[tauri::command]
pub fn cancel_font_scan(scan_id: u64) {
    if scan_id == NO_SCAN_ID {
        return;
    }
    // `fetch_max` ensures a stale cancel for an OLDER (smaller-id) scan
    // arriving after a newer cancel cannot regress CANCEL_SCAN_ID. The
    // returned prior max is intentionally discarded — caller has no
    // useful action either way. Relaxed is sufficient: the scan worker
    // re-loads CANCEL_SCAN_ID inside its poll loop and any ordering
    // we'd want is provided by the SeqCst on ACTIVE_SCAN_ID elsewhere.
    CANCEL_SCAN_ID.fetch_max(scan_id, Ordering::Relaxed);
}

#[tauri::command]
pub fn resolve_user_font(
    family: String,
    bold: bool,
    italic: bool,
) -> Result<Option<FontLookupResult>, String> {
    if family.is_empty() || family.len() > 256 {
        return Err("Font family name must be 1-256 characters".to_string());
    }
    if family.chars().any(|c| c.is_control() || c == '\x7f') {
        return Err("Font family name contains invalid characters".to_string());
    }

    let key = user_font_key(&family, bold, italic);
    let conn = open_user_font_db()?;
    conn.query_row(
        "
        SELECT f.path, f.face_index
        FROM font_family_keys k
        JOIN font_faces f ON f.face_id = k.face_id
        WHERE k.key = ?1
        ORDER BY k.source_order DESC, k.face_id DESC
        LIMIT 1
        ",
        params![key],
        |row| {
            Ok(FontLookupResult {
                path: row.get(0)?,
                index: row.get(1)?,
            })
        },
    )
    .optional()
    .map_err(|e| db_error("lookup failed", e))
}

#[tauri::command]
pub fn remove_font_source(source_id: String) -> Result<(), String> {
    validate_font_source_id(&source_id)?;
    let conn = open_user_font_db()?;
    conn.execute(
        "DELETE FROM font_sources WHERE source_id = ?1",
        params![source_id],
    )
    .map_err(|e| db_error("source delete failed", e))?;
    Ok(())
}

#[tauri::command]
pub fn clear_font_sources() -> Result<(), String> {
    let conn = open_user_font_db()?;
    conn.execute("DELETE FROM font_sources", [])
        .map_err(|e| db_error("source clear failed", e))?;
    Ok(())
}

/// Register a font path in the provenance cache and return the lookup result.
fn register_font_path(path: &Path, font_index: u32) -> Result<FontLookupResult, String> {
    let canonical = path.canonicalize().map_err(|e| {
        log::warn!("canonicalize font path failed: {e}");
        "Cannot resolve font path".to_string()
    })?;
    let canonical_string = normalize_canonical_path(&canonical.to_string_lossy());
    let mut cache = ALLOWED_FONT_PATHS
        .lock()
        .map_err(|_| "Internal error: font path cache corrupted".to_string())?;
    // Reject only when the cache is full AND we'd be adding a new entry;
    // re-registering an existing path is always cheap. An explicit error
    // beats silently succeeding here then failing later in subset_font.
    if !cache.contains(&canonical_string) && cache.len() >= MAX_PROVENANCE_CACHE_SIZE {
        return Err(format!(
            "Too many registered font paths (> {MAX_PROVENANCE_CACHE_SIZE}). \
             Restart the app to clear the cache."
        ));
    }
    cache.insert(canonical_string.clone());

    Ok(FontLookupResult {
        path: canonical_string,
        index: font_index,
    })
}

/// True when `path` equals `dir` or lives under it (using `sep` as the
/// separator). Matched via `starts_with` only — no `contains` — so that
/// directories whose names merely include "fonts" never leak through.
fn path_under_dir(path: &str, dir: &str, sep: &str) -> bool {
    path == dir || path.starts_with(&format!("{dir}{sep}"))
}

/// Cached, normalized system-fonts dir derived from `SYSTEMROOT` + `\Fonts`.
///
/// Captured eagerly at app startup via `init_system_dirs` (see `lib.rs`) so
/// the value is locked in before any user action could indirectly trigger
/// an `std::env::set_var` call. The cache is `Lazy` rather than a plain
/// `OnceCell` so that any code path which somehow runs before
/// `init_system_dirs` (e.g., a unit test in this module) still gets a
/// well-defined value rather than a panic.
///
/// Defense-in-depth note: a deeper hardening would call
/// `GetSystemWindowsDirectoryW` directly, which reads from a kernel-set
/// process buffer immune to env-var manipulation. We rely on the eager
/// init plus the project's no-`set_var` policy instead, since adding Win32
/// FFI for one path read is not justified at the current threat model
/// (attacker would already need code execution to mutate env vars).
#[cfg(windows)]
static WINDOWS_SYSTEM_FONTS_DIR: Lazy<String> = Lazy::new(|| {
    let sys_root = std::env::var("SYSTEMROOT")
        .unwrap_or_else(|_| "C:\\Windows".to_string())
        .to_lowercase()
        .replace("/", "\\");
    format!("{sys_root}\\fonts")
});

/// Cached, normalized per-user fonts dir (Windows 10 1809+). `None` if
/// `LOCALAPPDATA` was unset at startup. Same caching rationale as
/// `WINDOWS_SYSTEM_FONTS_DIR`.
#[cfg(windows)]
static WINDOWS_USER_FONTS_DIR: Lazy<Option<String>> = Lazy::new(|| {
    std::env::var("LOCALAPPDATA").ok().map(|p| {
        format!(
            "{}\\microsoft\\windows\\fonts",
            p.to_lowercase().replace("/", "\\")
        )
    })
});

/// Force-initialize the cached system-fonts directory paths. Called from
/// `lib.rs::run` at app startup so the env-var snapshot is taken before
/// any user action could indirectly mutate the process environment.
pub fn init_system_dirs() {
    #[cfg(windows)]
    {
        Lazy::force(&WINDOWS_SYSTEM_FONTS_DIR);
        Lazy::force(&WINDOWS_USER_FONTS_DIR);
    }
}

/// Check whether a canonicalized path is under a known system fonts directory.
fn is_in_system_fonts_dir(canonical: &Path) -> bool {
    let canonical_str = normalize_canonical_path(&canonical.to_string_lossy());

    if cfg!(windows) {
        #[cfg(windows)]
        {
            let lower = canonical_str.to_lowercase().replace("/", "\\");
            let under = |dir: &str| path_under_dir(&lower, dir, "\\");

            if under(&WINDOWS_SYSTEM_FONTS_DIR) {
                return true;
            }
            if let Some(user_dir) = WINDOWS_USER_FONTS_DIR.as_ref() {
                if under(user_dir) {
                    return true;
                }
            }
            false
        }
        #[cfg(not(windows))]
        {
            let _ = canonical_str;
            false
        }
    } else if cfg!(target_os = "macos") {
        // APFS is case-insensitive by default; compare in lowercase so symlink
        // chains that surface mixed-case paths still match canonical targets.
        let lower = canonical_str.to_lowercase();
        let under = |dir: &str| path_under_dir(&lower, &dir.to_lowercase(), "/");
        const MAC_DIRS: &[&str] = &[
            "/Library/Fonts",
            "/System/Library/Fonts",
            "/System/Library/AssetsV2",
            // Narrow to Adobe/Fonts — the wider /Library/Application Support
            // tree holds every app's data, not just fonts, so allowing the
            // whole tree weakens the "system font directory" gate.
            "/Library/Application Support/Adobe/Fonts",
            "/opt/homebrew/share/fonts",
            "/usr/local/share/fonts",
        ];
        if MAC_DIRS.iter().any(|d| under(d)) {
            return true;
        }
        // Per-user fonts: ~/Library/Fonts/
        if let Some(home) = std::env::var_os("HOME") {
            let user_font_dir = format!("{}/Library/Fonts", home.to_string_lossy());
            if under(&user_font_dir) {
                return true;
            }
        }
        false
    } else {
        // Linux
        let under = |dir: &str| path_under_dir(&canonical_str, dir, "/");
        if under("/usr/share/fonts") || under("/usr/local/share/fonts") {
            return true;
        }
        if let Some(home) = std::env::var_os("HOME") {
            let home_str = home.to_string_lossy();
            if under(&format!("{home_str}/.fonts"))
                || under(&format!("{home_str}/.local/share/fonts"))
            {
                return true;
            }
        }
        false
    }
}

/// Subset a font file to only include the specified codepoints.
///
/// Uses fontcull (Google's klippa engine) for pure-Rust subsetting.
/// For TTC files with face index > 0, uses fontcull's internal crates directly
/// to select the correct face. Always includes ASCII printable (0x0020–0x007E)
/// and CJK fullwidth forms (0xFF01–0xFF5E) as safety padding.
/// Falls back to full font on error.
#[tauri::command]
pub fn subset_font(
    font_path: String,
    font_index: u32,
    codepoints: Vec<u32>,
) -> Result<Vec<u8>, String> {
    // IPC boundary validation: font_index and codepoints come from untrusted JS
    if font_index > 255 {
        return Err(format!("Invalid font face index: {font_index} (max 255)"));
    }
    if codepoints.len() > 200_000 {
        return Err(format!(
            "Too many codepoints: {} (max 200,000)",
            codepoints.len()
        ));
    }
    // Reject out-of-range codepoints. Unicode tops out at U+10FFFF; anything
    // larger is a JS-side bug or a crafted IPC payload and must not reach
    // fontcull's IntSet, which would happily allocate for absurd values.
    if let Some(&bad) = codepoints.iter().find(|&&cp| cp > 0x10FFFF) {
        return Err(format!("Invalid codepoint: U+{bad:X} (max U+10FFFF)"));
    }

    let path = Path::new(&font_path);
    let filename = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("<unknown>");

    // Validate file extension against allowed font types
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_lowercase())
        .unwrap_or_default();
    if !ALLOWED_FONT_EXTENSIONS.contains(&ext.as_str()) {
        return Err(format!(
            "Invalid font file type '{}' for '{}'. Allowed extensions: {}",
            ext,
            filename,
            ALLOWED_FONT_EXTENSIONS.join(", ")
        ));
    }

    // Canonicalize to resolve symlinks, "..", and normalize the path
    let canonical = path.canonicalize().map_err(|e| {
        log::warn!("canonicalize font path failed for '{filename}': {e}");
        "Cannot resolve font path".to_string()
    })?;

    // Primary guard: the path must have been discovered by one of the scan
    // commands (find_system_font OR scan_font_directory / scan_font_files).
    // System paths use the small in-memory provenance set; user-picked paths
    // are checked against the session SQLite source index so XL folders do
    // not pin a huge path HashSet in RAM.
    let canonical_string = normalize_canonical_path(&canonical.to_string_lossy());
    let is_system = ALLOWED_FONT_PATHS
        .lock()
        .map_err(|_| "Internal error: font path cache corrupted".to_string())?
        .contains(&canonical_string);
    let is_user = if is_system {
        false
    } else {
        is_user_font_path_registered(&canonical_string)?
    };
    if !is_system && !is_user {
        return Err("Font path was not discovered by a scan command".to_string());
    }

    // Defense-in-depth: system-discovered paths must live under a known
    // system fonts directory. User-picked paths skip this check by design
    // — the whole point is to accept a user-chosen directory — but they
    // still had to pass the DB-backed provenance check above, so random
    // file reads via IPC are still blocked.
    if is_system && !is_in_system_fonts_dir(&canonical) {
        return Err("System font path is not in a system fonts directory".to_string());
    }

    // Pre-read size check — rejects obvious oversize before allocating the Vec.
    let metadata = fs::metadata(&canonical).map_err(|e| {
        log::warn!("stat font file failed for '{filename}': {e}");
        "Cannot stat font file".to_string()
    })?;
    if metadata.len() > MAX_FONT_DATA_SIZE {
        return Err(format!(
            "Font file too large ({:.1} MB, max {} MB)",
            metadata.len() as f64 / (1024.0 * 1024.0),
            MAX_FONT_DATA_SIZE / 1024 / 1024
        ));
    }

    let font_data = fs::read(&canonical).map_err(|e| {
        log::warn!("read font file failed for '{filename}': {e}");
        format!("Failed to read font file '{filename}'")
    })?;

    // Post-read size check (TOCTOU mitigation — file could grow between stat and read)
    if font_data.len() as u64 > MAX_FONT_DATA_SIZE {
        return Err(format!(
            "Font file too large after read ({:.1} MB, max {} MB)",
            font_data.len() as f64 / (1024.0 * 1024.0),
            MAX_FONT_DATA_SIZE / 1024 / 1024
        ));
    }

    // Build codepoint set: caller's codepoints + safety padding
    let mut all_codepoints = codepoints;
    // ASCII printable — always needed for punctuation, numbers, basic latin
    all_codepoints.extend(0x0020u32..=0x007Eu32);
    // CJK fullwidth forms — common in CJK subtitle typesetting (，。！？etc.)
    all_codepoints.extend(0xFF01u32..=0xFF5Eu32);
    all_codepoints.sort();
    all_codepoints.dedup();

    // Attempt subsetting; fall back to full font if it fails
    let subset_result = if font_index == 0 {
        // Common path: single font or first face in TTC
        // Display, not Debug — Debug repr leaks internal struct fields,
        // table tags, and byte offsets into a frontend-visible error.
        fontcull::subset_font_data_unicode(&font_data, &all_codepoints, &[])
            .map_err(|e| format!("{e}"))
    } else {
        // TTC with face index > 0: use internal crates with from_index
        subset_with_index(&font_data, font_index, &all_codepoints)
    };

    match subset_result {
        Ok(subsetted) => {
            log::info!(
                "Subsetted '{}' (face {}): {} → {} bytes ({} codepoints)",
                filename,
                font_index,
                font_data.len(),
                subsetted.len(),
                all_codepoints.len()
            );
            Ok(subsetted)
        }
        Err(e) => {
            log::warn!(
                "Subsetting failed for '{}' (face {}): {}, falling back to full font",
                filename,
                font_index,
                e
            );
            // Cap fallback size — the full font goes through IPC → JS heap → ASS string,
            // so a large font would cause excessive memory use in the frontend.
            if font_data.len() > MAX_FONT_FALLBACK_SIZE {
                return Err(format!(
                    "Subsetting failed and full font too large ({:.1} MB, max {} MB for fallback)",
                    font_data.len() as f64 / (1024.0 * 1024.0),
                    MAX_FONT_FALLBACK_SIZE / 1024 / 1024
                ));
            }
            Ok(font_data)
        }
    }
}

/// Subset a specific face from a TTC/OTC collection file.
/// Uses fontcull's internal crates directly for `FontRef::from_index`.
fn subset_with_index(font_data: &[u8], index: u32, codepoints: &[u32]) -> Result<Vec<u8>, String> {
    use fontcull_klippa::{subset_font, Plan, SubsetFlags};
    use fontcull_read_fonts::collections::IntSet;
    use fontcull_skrifa::{FontRef, GlyphId, Tag};
    use fontcull_write_fonts::types::NameId;

    let font = FontRef::from_index(font_data, index)
        .map_err(|e| format!("Cannot parse font face {index}: {e:?}"))?;

    let mut unicode_set: IntSet<u32> = IntSet::empty();
    for &cp in codepoints {
        unicode_set.insert(cp);
    }

    let empty_gids: IntSet<GlyphId> = IntSet::empty();
    let empty_tags: IntSet<Tag> = IntSet::empty();
    let empty_name_ids: IntSet<NameId> = IntSet::empty();
    let empty_langs: IntSet<u16> = IntSet::empty();
    let layout_scripts: IntSet<Tag> = IntSet::all();
    let layout_features: IntSet<Tag> = IntSet::empty();

    let plan = Plan::new(
        &empty_gids,
        &unicode_set,
        &font,
        SubsetFlags::default(),
        &empty_tags,
        &layout_scripts,
        &layout_features,
        &empty_name_ids,
        &empty_langs,
    );

    // Display, not Debug — same reasoning as the unicode-subset path.
    subset_font(&font, &plan).map_err(|e| format!("Subset failed for face {index}: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    static DB_TEST_LOCK: Mutex<()> = Mutex::new(());

    fn init_test_user_font_db(name: &str) {
        let dir = std::env::temp_dir().join(format!(
            "ssahdrify-user-font-db-test-{}-{name}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&dir);
        init_user_font_db(&dir).expect("test DB should initialize");
        clear_font_sources().expect("test DB should clear");
    }

    fn sample_entry(path: &str, family: &str, index: u32) -> LocalFontEntry {
        LocalFontEntry {
            path: path.to_string(),
            index,
            families: vec![family.to_string()],
            bold: false,
            italic: false,
            size_bytes: 123,
        }
    }

    fn commit_entries(source_id: &str, entries: Vec<LocalFontEntry>) -> ImportOutcome {
        let mut conn = open_user_font_db().expect("test DB should open");
        let tx = conn.transaction().expect("transaction should start");
        let source_order =
            create_user_font_source_tx(&tx, source_id).expect("source should insert");
        let outcome = import_user_font_batch_tx(&tx, source_id, source_order, entries)
            .expect("batch should import");
        remove_empty_user_font_source_tx(&tx, source_id, outcome.added)
            .expect("empty source cleanup should work");
        tx.commit().expect("transaction should commit");
        outcome
    }

    #[test]
    fn db_schema_indexes_family_keys_for_cascade_delete() {
        let _guard = DB_TEST_LOCK.lock().unwrap();
        init_test_user_font_db("schema-index");
        let conn = open_user_font_db().expect("test DB should open");
        let index_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type = 'index' AND name = 'idx_font_family_face'",
                [],
                |row| row.get(0),
            )
            .expect("schema index query should work");
        assert_eq!(index_count, 1);
    }

    #[test]
    fn db_import_dedupes_faces_and_resolves_family() {
        let _guard = DB_TEST_LOCK.lock().unwrap();
        init_test_user_font_db("dedupe");
        let outcome = commit_entries(
            "source-a",
            vec![
                sample_entry("C:\\Fonts\\A.ttf", "Demo Sans", 0),
                sample_entry("C:\\Fonts\\A.ttf", "Demo Sans Duplicate", 0),
            ],
        );

        assert_eq!(outcome.added, 1);
        assert_eq!(outcome.duplicated, 1);
        let resolved = resolve_user_font("Demo Sans".to_string(), false, false)
            .unwrap()
            .expect("family should resolve");
        assert_eq!(resolved.path, "C:\\Fonts\\A.ttf");
        assert!(is_user_font_path_registered("C:\\Fonts\\A.ttf").unwrap());
    }

    #[test]
    fn db_lookup_prefers_newer_source_for_same_family_key() {
        let _guard = DB_TEST_LOCK.lock().unwrap();
        init_test_user_font_db("source-order");
        commit_entries(
            "source-a",
            vec![sample_entry("C:\\Fonts\\Old.ttf", "Demo Sans", 0)],
        );
        commit_entries(
            "source-b",
            vec![sample_entry("C:\\Fonts\\New.ttf", "Demo Sans", 0)],
        );

        let resolved = resolve_user_font("Demo Sans".to_string(), false, false)
            .unwrap()
            .expect("family should resolve");
        assert_eq!(resolved.path, "C:\\Fonts\\New.ttf");
    }

    #[test]
    fn db_remove_and_clear_update_lookup_and_provenance() {
        let _guard = DB_TEST_LOCK.lock().unwrap();
        init_test_user_font_db("remove-clear");
        commit_entries(
            "source-a",
            vec![sample_entry("C:\\Fonts\\A.ttf", "Demo Sans", 0)],
        );
        commit_entries(
            "source-b",
            vec![sample_entry("C:\\Fonts\\B.ttf", "Demo Sans", 0)],
        );

        remove_font_source("source-b".to_string()).unwrap();
        let resolved = resolve_user_font("Demo Sans".to_string(), false, false)
            .unwrap()
            .expect("older source should become visible again");
        assert_eq!(resolved.path, "C:\\Fonts\\A.ttf");
        assert!(!is_user_font_path_registered("C:\\Fonts\\B.ttf").unwrap());

        clear_font_sources().unwrap();
        assert!(resolve_user_font("Demo Sans".to_string(), false, false)
            .unwrap()
            .is_none());
        assert!(!is_user_font_path_registered("C:\\Fonts\\A.ttf").unwrap());
    }

    /// `scan_directory_inner` on a non-existent path surfaces the read_dir
    /// error as the user-facing string. The closure is never called.
    #[test]
    fn directory_inner_rejects_missing_dir() {
        let mut emitted: Vec<Vec<LocalFontEntry>> = Vec::new();
        let bogus = Path::new("Z:\\absolutely-not-a-real-directory\\for-testing");
        let result = scan_directory_inner(bogus, 1, |batch| {
            emitted.push(batch);
            Ok(())
        });
        assert!(result.is_err());
        assert!(emitted.is_empty());
    }

    /// `scan_files_inner` skips invalid entries (empty / oversized / control
    /// chars) silently and emits nothing when none of the inputs resolve to
    /// a real font file. The streaming contract holds for the empty case:
    /// the closure receives zero batches.
    #[test]
    fn files_inner_skips_invalid_paths_without_emitting() {
        let mut emitted: Vec<Vec<LocalFontEntry>> = Vec::new();
        let bad_paths = vec![
            String::new(),                    // empty
            "x".repeat(5000),                 // oversized
            "has\u{0000}control".to_string(), // control char
            "Z:\\does-not-exist.ttf".to_string(),
        ];
        let outcome = scan_files_inner(bad_paths, 2, |batch| {
            emitted.push(batch);
            Ok(())
        })
        .expect("invalid paths should be skipped, not error");
        assert_eq!(outcome.total, 0);
        assert!(!outcome.cancelled);
        assert!(emitted.is_empty());
    }

    /// A targeted cancel before the first file causes an immediate return on
    /// the first iteration. Validates the cancel-poll path without depending
    /// on real font files. Buffer is empty so no batch is emitted.
    #[test]
    fn files_inner_honors_targeted_cancel_before_first_file() {
        let scan_id = CANCEL_SCAN_ID.load(Ordering::Relaxed).saturating_add(1);
        CANCEL_SCAN_ID.fetch_max(scan_id, Ordering::Relaxed);
        let mut emitted: Vec<Vec<LocalFontEntry>> = Vec::new();
        let outcome = scan_files_inner(vec!["irrelevant.ttf".to_string()], scan_id, |batch| {
            emitted.push(batch);
            Ok(())
        })
        .expect("cancel returns Ok with partial results");
        assert_eq!(outcome.total, 0);
        assert!(outcome.cancelled);
        assert!(emitted.is_empty());
    }

    /// `cancel_font_scan` records the requested id and stale lower ids do not
    /// overwrite a newer cancel request.
    #[test]
    fn cancel_command_records_scan_id_monotonically() {
        let scan_id = CANCEL_SCAN_ID.load(Ordering::Relaxed).saturating_add(10);
        cancel_font_scan(scan_id);
        assert_eq!(CANCEL_SCAN_ID.load(Ordering::Relaxed), scan_id);
        cancel_font_scan(scan_id - 1);
        assert_eq!(CANCEL_SCAN_ID.load(Ordering::Relaxed), scan_id);
    }
}
