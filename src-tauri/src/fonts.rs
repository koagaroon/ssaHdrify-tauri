use font_kit::family_name::FamilyName;
use font_kit::handle::Handle;
use font_kit::properties::{Properties, Style as FontKitStyle, Weight as FontKitWeight};
use font_kit::source::SystemSource;
use once_cell::sync::Lazy;
use rusqlite::{params, Connection, OptionalExtension, Transaction};
use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use crate::util::{validate_ipc_path, MAX_INPUT_PATHS};

/// Allowed font file extensions (lowercase).
const ALLOWED_FONT_EXTENSIONS: &[&str] = &["ttf", "otf", "ttc", "otc"];

/// Defense-in-depth ceiling on faces emitted from a single scan. Not a UX
/// limit — real font-collection users with thousands of files should never
/// hit this. Caps malicious/runaway directories whose IPC and SQLite work
/// would otherwise grow without bound. Above this, partial results are
/// preserved and the scan stops.
///
/// Off-by-one note: the check `if total > MAX_FONTS_PER_SCAN` runs INSIDE
/// the per-face inner loop, AFTER the entry was pushed and `total`
/// incremented. A TTC iteration that begins right at `total ==
/// MAX_FONTS_PER_SCAN` may emit up to `MAX_TTC_FACES - 1` more faces
/// before the next post-push check fires, so the actual buffer ceiling
/// is `MAX_FONTS_PER_SCAN + MAX_TTC_FACES - 1`. Kept this way
/// deliberately so the final flush emits everything that was already
/// parseable; the soft excess is bounded by `MAX_TTC_FACES = 16`, well
/// inside the IPC/SQLite envelopes.
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
///
/// `u32` (not `usize`) because the loop uses it as the face index passed
/// to `fontcull_skrifa::FontRef::from_index(_, u32)` — matching the parser
/// API avoids casts in the hot loop.
const MAX_TTC_FACES: u32 = 16;

/// Cap on raw font data read for subsetting — prevents OOM with large CJK
/// fonts and mirrors the front-end guard in `ass-uuencode.ts`.
const MAX_FONT_DATA_SIZE: u64 = 50 * 1024 * 1024;

/// Cap on the unmodified font emitted by the subset fallback path. Lower
/// than `MAX_FONT_DATA_SIZE` because the fallback sends the full font through
/// IPC → JS heap → ASS string.
const MAX_FONT_FALLBACK_SIZE: usize = 10 * 1024 * 1024;

/// Cumulative cap across all fallback emissions in a single app session.
/// Bounds the total bytes flowing through the subset-failure path so a
/// subtitle referencing many corrupt fonts cannot stack-feed the JS heap
/// with N × MAX_FONT_FALLBACK_SIZE worth of full fonts. Per-file 10 MB ×
/// 5 = 50 MB is a generous ceiling for any legitimate workflow; a single-
/// user desktop session that hits this almost certainly has a corrupt
/// font source and should restart + investigate.
const MAX_CUMULATIVE_FALLBACK_BYTES: usize = 50 * 1024 * 1024;
static CUMULATIVE_FALLBACK_BYTES: AtomicUsize = AtomicUsize::new(0);

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
    // foreign_keys and busy_timeout are per-CONNECTION SQLite settings —
    // each freshly-opened connection needs them. Cheap (microseconds).
    // journal_mode (WAL) is per-FILE and persists once set, so it's
    // configured once at init by `set_user_font_db_journal_mode_once`.
    conn.pragma_update(None, "foreign_keys", "ON")
        .map_err(|e| db_error("foreign_keys setup failed", e))?;
    conn.pragma_update(None, "busy_timeout", 5000)
        .map_err(|e| db_error("busy_timeout setup failed", e))?;
    Ok(conn)
}

/// Switch the user font index to WAL mode. Called once from
/// `init_user_font_db`; subsequent connections inherit the file-level
/// mode for free. Hoisted out of `open_user_font_db` so a degraded
/// filesystem (read-only FS, network mount, tmpfs) doesn't trigger the
/// "WAL didn't take" warn once per `is_user_font_path_registered` call
/// — that ran 20+ times per embed pass when the parent did.
fn set_user_font_db_journal_mode_once(conn: &Connection) -> Result<(), String> {
    conn.pragma_update(None, "journal_mode", "WAL")
        .map_err(|e| db_error("journal_mode setup failed", e))?;
    // SQLite silently keeps the previous mode (or falls back to
    // "memory") on filesystems that can't host WAL files. pragma_update
    // returns Ok in both success AND silent-degrade cases — verify the
    // actual mode and warn if WAL didn't take. Not fatal: the per-conn
    // busy_timeout still applies, and the modal-scrim UX prevents the
    // contention this hardening was meant for in the first place.
    let actual_mode: String = conn
        .pragma_query_value(None, "journal_mode", |row| row.get(0))
        .map_err(|e| db_error("journal_mode verify failed", e))?;
    if !actual_mode.eq_ignore_ascii_case("wal") {
        log::warn!(
            "user font index journal_mode is '{actual_mode}', not WAL — \
             SQLITE_BUSY hardening may not apply on this filesystem"
        );
    }
    Ok(())
}

fn init_user_font_schema(conn: &Connection) -> Result<(), String> {
    // foreign_keys is set per-connection in `open_user_font_db`; no need
    // to repeat it here. Schema-only batch.
    conn.execute_batch(
        "
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
    // Mirror the main-file pattern: NotFound is silent, other errors get
    // a forensic warn but never block init.
    for suffix in ["-journal", "-wal", "-shm"] {
        let mut sidecar = db_path.clone().into_os_string();
        sidecar.push(suffix);
        let sidecar = PathBuf::from(sidecar);
        match fs::remove_file(&sidecar) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => {
                log::warn!(
                    "remove stale sqlite sidecar '{}' failed: {e}",
                    sidecar.display()
                );
            }
        }
    }
    // Open a connection BEFORE publishing the path slot so a schema
    // failure doesn't leave the static state half-initialized (slot
    // pointing at a real file but no usable DB behind it). On the Ok
    // path we then publish the slot; on Err the slot stays None and
    // a subsequent open call will surface "User font index is not
    // initialized" instead of a more confusing schema-shape error.
    let conn = Connection::open(&db_path).map_err(|e| db_error("open failed", e))?;
    conn.pragma_update(None, "foreign_keys", "ON")
        .map_err(|e| db_error("foreign_keys setup failed", e))?;
    conn.pragma_update(None, "busy_timeout", 5000)
        .map_err(|e| db_error("busy_timeout setup failed", e))?;
    init_user_font_schema(&conn)?;
    set_user_font_db_journal_mode_once(&conn)?;
    {
        let mut path_slot = USER_FONT_DB_PATH
            .lock()
            .map_err(|_| "Internal error: user font index path corrupted".to_string())?;
        *path_slot = Some(db_path);
    }
    Ok(())
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
    /// End-of-stream sentinel. Always emitted on the `Ok` path (natural
    /// completion, user cancel, or defense-in-depth cap stop). NOT
    /// emitted on the `Err` path — the invoke rejection already signals
    /// failure and the frontend must not block waiting for a `Done` that
    /// will never arrive.
    ///
    /// `reason` carries WHY the scan stopped — see `ScanStopReason`.
    /// Replaces the prior `(cancelled, ceiling_hit)` two-boolean shape,
    /// which encoded only three legitimate states across four flag
    /// combinations.
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
    /// String slip in. See `reference_tauri_channel_perf.md`.
    Done {
        reason: ScanStopReason,
        added: usize,
        duplicated: usize,
    },
}

/// Why a font scan stopped. Three legitimate states; the prior
/// `(cancelled: bool, ceiling_hit: bool)` pair allowed a fourth
/// `(false, true)` combination by construction that the runtime never
/// actually emitted, which the reviewer flagged. Single-variant enum
/// eliminates the impossible state and lets frontend / test code
/// pattern-match exhaustively.
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
    /// MAX_FONTS_PER_SCAN defense-in-depth ceiling fired. Partial
    /// results are preserved on the way out.
    CeilingHit,
}

#[derive(Debug, Clone, Copy)]
struct ScanOutcome {
    total: usize,
    /// Why this scan stopped. Forwarded into `ScanProgress::Done.reason`
    /// after the SQLite import completes.
    reason: ScanStopReason,
}

#[derive(Debug, Clone)]
pub struct FontSourceImportSummary {
    pub total: usize,
    pub added: usize,
    pub duplicated: usize,
    pub reason: ScanStopReason,
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
fn begin_font_scan(scan_id: u64) -> Result<ActiveScanGuard, String> {
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

fn font_scan_cancelled(scan_id: u64) -> bool {
    // Acquire load pairs with the Release fetch_max in `cancel_font_scan`.
    // On weakly-ordered ISAs (ARM64 / Apple Silicon) Relaxed gives no
    // formal cross-thread visibility guarantee; the Acquire here + Release
    // there bounds visibility to the next file iteration's poll. On x86
    // both orderings compile to plain mov / lock cmpxchg, so the upgrade
    // is free for the platform we ship on. Cost on ARM64: one barrier per
    // file iteration in the scan worker — negligible against the actual
    // font-parse cost.
    scan_id != NO_SCAN_ID && CANCEL_SCAN_ID.load(Ordering::Acquire) == scan_id
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
    /// True when OS/2 weight >= 600 (SemiBold+). Matches ASS \b1 semantics.
    pub bold: bool,
    /// True for Italic or Oblique styles.
    pub italic: bool,
    /// File size on disk — useful for UI display.
    pub size_bytes: u64,
}

fn user_font_key(family: &str, bold: bool, italic: bool) -> String {
    // NFC-normalize before lowercase so HFS+ NFD-form filenames and NFC-form
    // font internal names key identically; otherwise precomposed `é` (U+00E9)
    // and decomposed `e + ´` (U+0065 U+0301) produce different keys for the
    // same visual family. Mirrors the TS userFontKey flow.
    // Shared with the persistent cache via `family_lookup_key`.
    let normalized: String = crate::font_cache::family_lookup_key(family);
    // U+001F (Unit Separator) is a control character; real font family names
    // never contain it (would be a malformed font). Matches the TS
    // USER_FONT_KEY_SEP so future cross-layer audits land on the same byte.
    format!(
        "{}\u{001F}{}\u{001F}{}",
        normalized,
        if bold { "1" } else { "0" },
        if italic { "1" } else { "0" }
    )
}

fn validate_font_source_id(source_id: &str) -> Result<(), String> {
    // `len()` is byte count (O(1)). The frontend always mints source
    // ids as UUID v4 strings (32 hex digits + 4 dashes = 36 ASCII bytes).
    // Length cap is 128 to leave headroom for a future format change
    // without IPC contract churn.
    if source_id.is_empty() || source_id.len() > 128 {
        return Err("Font source id must be 1-128 bytes".to_string());
    }
    // ASCII allowlist matching the UUID shape (alphanumerics, dash,
    // underscore). Defense-in-depth: SQL injection is structurally
    // blocked by parameterized queries, but rejecting unexpected bytes
    // at the IPC boundary stops a misbehaving frontend from smuggling
    // file-system separators / log-line breaks / Unicode controls into
    // the id, even though SQL itself wouldn't care. Subsumes the
    // earlier control-char + U+2028/U+2029 check.
    if source_id
        .bytes()
        .any(|b| !matches!(b, b'a'..=b'z' | b'A'..=b'Z' | b'0'..=b'9' | b'-' | b'_'))
    {
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
            // Face already indexed under an earlier source — dedup on
            // canonical (path, face_index). Skipping the family-key inserts
            // below intentionally leaves the original `source_order`
            // authoritative; re-adding the same path under a new source_id
            // does NOT promote the face to a newer lookup priority. Any
            // future change that "promotes on re-add" must also reconcile
            // with `db_lookup_prefers_newer_source_for_same_family_key`.
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

// Perf note: this is invoked once per font during subset_font, so an
// ASS with 30 fonts produces 30 fresh connections. Each `Connection::open`
// is tens of microseconds + the per-conn PRAGMAs (`foreign_keys`,
// `busy_timeout`) — measured-cheap on local disk, but if a future
// benchmark shows measurable embed-pass overhead, switch to a
// `Lazy<Mutex<Connection>>` shared cache. Note that `journal_mode = WAL`
// no longer runs per-connection (hoisted to init), which already
// removed the bulk of the per-call cost.
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
    // Input validation: reject empty, oversized, or control-char-containing
    // names. Use char count (codepoints), NOT byte length — a 100-char CJK
    // family name is 300+ UTF-8 bytes and is perfectly legitimate.
    // `parse_local_font_file` already counts codepoints when ingesting
    // names; the lookup gate must agree, otherwise a font that scans into
    // the index successfully gets rejected at lookup time.
    if family.is_empty() || family.chars().count() > 256 {
        return Err("Font family name must be 1-256 characters".to_string());
    }
    if family.chars().any(|c| c.is_control() || c == '\x7f') {
        return Err("Font family name contains invalid characters".to_string());
    }

    let source = SystemSource::new();

    let mut props = Properties::new();
    if bold {
        props.weight = FontKitWeight::BOLD;
    }
    if italic {
        props.style = FontKitStyle::Italic;
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
            // The "Font not found:" prefix is parsed by the CLI shell
            // (`bin/cli/main.rs` `resolve_embed_font`) to distinguish a
            // benign system-miss from a real error under
            // `--on-missing warn`. Any change to this prefix MUST update
            // the matcher there in lockstep — the GUI doesn't care, but
            // the CLI breaks silently.
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

fn bounded_font_family_name(chars: impl Iterator<Item = char>) -> Option<String> {
    // Take chars lazily with a short ceiling so a malformed font with a
    // huge name-table entry can't OOM the process before the length guard
    // fires. 257 chars is enough to detect ">256 chars" overflow below.
    let name: String = chars.take(257).collect();
    let trimmed = name.trim();
    // Guard counts CODEPOINTS, not bytes — a 100-char CJK family name
    // (300+ UTF-8 bytes) is perfectly legitimate.
    let char_count = trimmed.chars().count();
    if !trimmed.is_empty() && char_count <= 256 {
        Some(trimmed.to_string())
    } else {
        None
    }
}

/// Parse one font file (TTF/OTF/TTC/OTC) and return a `LocalFontEntry` per
/// face **and per distinct localized family name** in the face's name table.
///
/// A single TTF can declare its family under multiple languages (common with
/// CJK fonts that ship both an English and a Chinese name). We emit one entry
/// per variant so the frontend matcher finds the font no matter which name the
/// ASS script happens to reference. Earlier single-name lookup shadowed
/// English family names on zh-CN Windows when the OS preferred a localized
/// name, which caused "font not recognized" reports for valid local sources.
///
/// `canonical` must already be canonicalized by the caller. User provenance
/// is registered later when the emitted batch is committed to the session
/// SQLite index.
///
/// `scan_id` lets the per-face inner loop poll cancellation BETWEEN faces.
/// Without this, a single TTC with up to `MAX_TTC_FACES` slow-to-parse
/// faces could stall the cancel-acknowledge loop for several seconds (the
/// outer scan only polls between FILES). Also bounds the work a crafted
/// TTC can demand by giving cancellation a chance to land between face
/// parses.
///
/// `NO_SCAN_ID` is the no-cancellation sentinel; today every caller is a
/// scan worker with a real id, but the parameter keeps the contract
/// explicit for any future caller that doesn't participate in cancel.
fn parse_local_font_file(canonical: &Path, scan_id: u64) -> Vec<LocalFontEntry> {
    use fontcull_skrifa::string::StringId;
    use fontcull_skrifa::{FontRef, MetadataProvider};

    // Extension check is intentionally case-insensitive (.TTF vs .ttf are the
    // same file format). The ASCII-lowercase conversion is correct here — all
    // ALLOWED_FONT_EXTENSIONS entries are ASCII. Done inline (instead of the
    // `has_allowed_font_extension` helper) so `is_collection` below can reuse
    // the already-computed lowercase extension without a second extension
    // lookup; helper would lower-case once for the bool, then `is_collection`
    // would lower-case again for the TTC/OTC check.
    let ext = canonical
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase())
        .unwrap_or_default();
    if !ALLOWED_FONT_EXTENSIONS.contains(&ext.as_str()) {
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

    // Read the file once and parse it with fontcull/skrifa only. Avoid
    // font-kit here: on Windows its in-memory loader routes through
    // DirectWrite and can retain/copy whole font blobs across huge scans.
    let data = match fs::read(canonical) {
        Ok(d) => d,
        Err(_) => return Vec::new(),
    };

    let mut entries = Vec::new();
    // Permit a single consecutive parse failure before giving up. In practice
    // FontRef::from_index returns an error once `i` exceeds the real face
    // count, so one tolerance catches that natural end-of-collection while
    // keeping the per-file parse cost bounded at 2 × face_count rather than
    // 3 ×. Crafted TTCs cannot force us to parse all 64 slots just by salting
    // every other face with bad data.
    //
    // Single-face files (TTF/OTF) effectively use this as a one-shot:
    // `max_faces = 1`, the loop runs once, and the constant doesn't
    // matter. Only TTC/OTC iteration depends on it.
    const MAX_CONSECUTIVE_FAILURES: u32 = 1;
    let mut consecutive_failures: u32 = 0;
    for i in 0..max_faces {
        // Per-face cancel poll. The outer scan_*_inner loops only check
        // between files; a 16-face TTC where each face triggers expensive
        // skrifa name-table walks can otherwise eat several seconds of
        // unresponsive Cancel button. NO_SCAN_ID is the "no active scan"
        // sentinel and must never trigger cancellation.
        //
        // Returning early with the already-parsed faces is safe — the
        // outer scan's cancel branch flushes the buffer (which now
        // includes our partial faces) before the cancelled outcome is
        // returned, so no parsed work is lost.
        if font_scan_cancelled(scan_id) {
            break;
        }
        let font_ref = match FontRef::from_index(&data, i) {
            Ok(f) => f,
            Err(_) => {
                consecutive_failures += 1;
                if consecutive_failures > MAX_CONSECUTIVE_FAILURES {
                    break;
                }
                continue;
            }
        };
        consecutive_failures = 0;

        let attrs = font_ref.attributes();
        let bold = attrs.weight.value() >= 600.0;
        let italic = !matches!(attrs.style, fontcull_skrifa::attribute::Style::Normal);

        let mut family_variants: HashSet<String> = HashSet::new();
        let mut primary_hint: Option<String> = None;
        for id in [StringId::FAMILY_NAME, StringId::TYPOGRAPHIC_FAMILY_NAME] {
            if primary_hint.is_none() {
                primary_hint = font_ref
                    .localized_strings(id)
                    .english_or_first()
                    .and_then(|localized| bounded_font_family_name(localized.chars()));
            }

            for localized in font_ref.localized_strings(id) {
                if let Some(name) = bounded_font_family_name(localized.chars()) {
                    family_variants.insert(name);
                    if family_variants.len() >= MAX_FAMILY_VARIANTS_PER_FACE {
                        break;
                    }
                }
            }
            if family_variants.len() >= MAX_FAMILY_VARIANTS_PER_FACE {
                break;
            }
        }

        // Last-resort fallback: malformed fonts may have no family IDs but
        // still have a full name. Indexing that is better than silently
        // dropping the face, and it avoids re-entering font-kit/DirectWrite.
        if family_variants.is_empty() {
            for id in [StringId::FULL_NAME, StringId::POSTSCRIPT_NAME] {
                if let Some(name) = font_ref
                    .localized_strings(id)
                    .english_or_first()
                    .and_then(|localized| bounded_font_family_name(localized.chars()))
                {
                    primary_hint = Some(name.clone());
                    family_variants.insert(name);
                    break;
                }
            }
        }

        if family_variants.is_empty() {
            continue;
        }

        // Stabilize the primary-name pick: prefer the best available English
        // family name if it is among the variants, else fall back to sorted
        // order so UI listings stay deterministic across runs.
        let mut families: Vec<String> = family_variants.into_iter().collect();
        families.sort();
        if let Some(pos) = primary_hint
            .as_ref()
            .and_then(|primary| families.iter().position(|v| v == primary))
        {
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
    for (visited, entry) in read.enumerate() {
        // Cap fires BEFORE we touch the entry (canonicalize / metadata),
        // so the worst-case CPU cost is bounded at MAX_PREFLIGHT_ENTRIES
        // canonicalize calls — not MAX+1.
        if visited >= MAX_PREFLIGHT_ENTRIES {
            return Err(format!(
                "Directory has too many entries to preview (>{MAX_PREFLIGHT_ENTRIES})"
            ));
        }
        let Ok(entry) = entry else {
            continue;
        };
        let path = entry.path();
        // Skip reparse points (symlinks / Windows junctions / OneDrive
        // placeholders) BEFORE any metadata or canonicalize call. The
        // earlier ordering called `path.is_file()` first, which goes
        // through `std::fs::metadata` and follows symlinks — for a
        // symlink pointing to a regular file, the kernel resolved the
        // reparse point and opened the target as a side effect even
        // though we then skipped the entry. The starts_with guard at
        // the bottom kept the result correct, but the design intent
        // ("preview never chases symlinks") was not actually upheld in
        // the trace. `scan_directory_inner` intentionally takes a
        // different policy on in-directory symlinks; preview's job is
        // strictly size estimation, so refusing to touch them at all
        // is the right invariant.
        if crate::util::is_reparse_point(&path) {
            continue;
        }
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
    // The public command enforces MAX_INPUT_PATHS, but the inner
    // helper has no caller-side check. Debug-mode assertion catches
    // any future internal caller that bypasses the public command.
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
    // Mirror the dedup `scan_files_inner` and `preflight_files_inner`
    // apply: a directory containing two siblings that resolve to the
    // same canonical path (e.g., `Foo.ttf` plus a same-directory
    // symlink `Bar.ttf` → `Foo.ttf`) would otherwise re-parse the
    // bytes twice and rely on SQLite's `UNIQUE(path, face_index)` to
    // surface them as `duplicated`. Wastes IO/parse cost.
    //
    // Bound on the dedup set: a directory of 1M empty / non-font
    // entries would otherwise inflate `seen` to ~100 MB before
    // `MAX_FONTS_PER_SCAN` (which counts faces) fires. Cap at
    // `MAX_PREFLIGHT_ENTRIES` so the directory scan can't outrun the
    // preflight ceiling on memory pressure.
    let mut seen: HashSet<String> = HashSet::new();

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
                reason: ScanStopReason::UserCancel,
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
        //
        // N-7 note: an in-directory junction whose target also lies under
        // the same picked directory passes `starts_with` and is parsed.
        // Acceptable — the target is still inside user-trusted space.
        // Adding an `is_reparse_point` early-skip here would also drop
        // legitimate symlink-organized font folders (some packagers
        // distribute fonts as symlinks to a shared store).
        let canonical = match path.canonicalize() {
            Ok(c) => c,
            Err(_) => continue,
        };
        if !canonical.starts_with(canonical_dir) {
            continue;
        }
        if seen.len() >= MAX_PREFLIGHT_ENTRIES {
            log::warn!(
                "font scan {} dedup set hit {MAX_PREFLIGHT_ENTRIES} entries in '{}'; \
                 stopping early to bound memory",
                scan_id,
                canonical_dir.display()
            );
            break;
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

    Ok(ScanOutcome {
        total,
        reason: ScanStopReason::Natural,
    })
}

/// Shared scan-command body: opens the SQLite transaction, drives the
/// inner scan loop with an emit closure that imports each batch into
/// the source index AND streams a count-only progress event, then sends
/// the Done sentinel on the Ok path.
///
/// Lifted out of `scan_font_directory` and `scan_font_files` once the
/// duplication crossed the "real" threshold (round-3 review N1-13).
/// The two commands now differ only in their pre-validation +
/// canonicalize stages and the inner scan they invoke through `scan_body`.
///
/// `log_label` is the human-readable scan target (directory path, or
/// "local font files" for the file-list command) — folded into the
/// completion log line.
fn run_streaming_scan_command<S>(
    scan_id: u64,
    source_id: &str,
    progress: tauri::ipc::Channel<ScanProgress>,
    log_label: &str,
    // When `Some`, every batch is cloned into the supplied Vec before
    // being passed downstream to the session-DB import. The directory
    // scan path uses this to feed `try_record_folder_in_gui_cache`
    // after commit; the file-list scan path passes `None` because it
    // has no folder anchor for the cache's drift model. Clone cost is
    // O(scan size) with peak memory at scan completion; for typical
    // libraries (sub-10k faces) it's negligible, and for the XL bucket
    // (~17k faces) it's <10 MB which the JS-heap streaming refactor
    // ruled out only on the JS side.
    mut collected_for_cache: Option<&mut Vec<LocalFontEntry>>,
    scan_body: S,
) -> Result<(), String>
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
    let source_order = create_user_font_source_tx(&tx, source_id)?;
    let mut import = ImportOutcome::default();
    let mut progress_total = 0usize;
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
        if let Some(c) = collected_for_cache.as_mut() {
            c.extend(batch.iter().cloned());
        }
        // Run the SQLite import FIRST so progress_total only advances on
        // committed work. If the import errors, the closure short-
        // circuits via `?` and the next outer `?` rolls the whole scan
        // back without leaving the user staring at a count that
        // overshoots the registered source.
        let batch_import = import_user_font_batch_tx(&tx, source_id, source_order, batch)?;
        progress_total += batch_size;
        import.added += batch_import.added;
        import.duplicated += batch_import.duplicated;
        let _ = progress.send(ScanProgress::Batch {
            total: progress_total,
        });
        Ok(())
    })?;
    remove_empty_user_font_source_tx(&tx, source_id, import.added)?;
    tx.commit()
        .map_err(|e| db_error("transaction commit failed", e))?;

    // End-of-stream sentinel; see ScanProgress::Done. MUST be the last
    // send on the Ok path so every progress event has drained before
    // the frontend reports the registered source count. NOT emitted on
    // the Err path — the IPC rejection handles that signal, and
    // runStreamingScan never reaches the donePromise await when invoke
    // rejects.
    let _ = progress.send(ScanProgress::Done {
        reason: outcome.reason,
        added: import.added,
        duplicated: import.duplicated,
    });

    log::info!(
        "{log_label} with scan {scan_id}: {} faces total, {} added, {} duplicate{}",
        outcome.total,
        import.added,
        import.duplicated,
        match outcome.reason {
            ScanStopReason::Natural => "",
            ScanStopReason::UserCancel => " (cancelled)",
            ScanStopReason::CeilingHit => " (ceiling hit)",
        }
    );
    Ok(())
}

fn run_blocking_scan_import<S>(
    source_id: &str,
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
    let source_order = create_user_font_source_tx(&tx, source_id)?;
    let mut import = ImportOutcome::default();
    let outcome = scan_body(NO_SCAN_ID, &mut |batch| {
        let batch_import = import_user_font_batch_tx(&tx, source_id, source_order, batch)?;
        import.added += batch_import.added;
        import.duplicated += batch_import.duplicated;
        Ok(())
    })?;
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
    let canonical = dir
        .canonicalize()
        .map_err(|e| format!("Cannot resolve directory '{}': {e}", dir.display()))?;
    if !canonical.is_dir() {
        return Err(format!("Not a directory: {}", canonical.display()));
    }
    let mut entries: Vec<LocalFontEntry> = Vec::new();
    scan_directory_inner(&canonical, NO_SCAN_ID, |batch| {
        entries.extend(batch);
        Ok(())
    })?;
    Ok(entries)
}

pub fn import_font_directory_for_cli(
    dir: &Path,
    source_id: &str,
) -> Result<FontSourceImportSummary, String> {
    // Canonicalize: the font scanner indexes by the resolved path so
    // sources reached via different symlinks aren't double-imported.
    // (CLI's --output-dir handling deliberately does NOT canonicalize
    // — see absolute_path() in bin/cli/main.rs — because output paths
    // round-trip through user-facing diagnostics where the user-typed
    // form should be preserved. Indexing has no such constraint.)
    let canonical_dir = dir.canonicalize().map_err(|e| {
        log::warn!("canonicalize directory failed: {e}");
        "Cannot resolve directory path".to_string()
    })?;
    if !canonical_dir.is_dir() {
        return Err("Not a directory".to_string());
    }
    run_blocking_scan_import(source_id, |scan_id, emit_batch| {
        scan_directory_inner(&canonical_dir, scan_id, emit_batch)
    })
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
    run_blocking_scan_import(source_id, |scan_id, emit_batch| {
        scan_files_inner(paths, scan_id, emit_batch)
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
    if scan_id == NO_SCAN_ID {
        return Err("Scan id must be non-zero".to_string());
    }
    validate_ipc_path(&dir, "Directory")?;
    validate_font_source_id(&source_id)?;

    let active_scan = begin_font_scan(scan_id)?;
    tauri::async_runtime::spawn_blocking(move || {
        // The guard's only job is to clear ACTIVE_SCAN_ID on Drop when
        // the worker thread exits — Ok or Err. Bind it into the
        // closure's scope (the move at the spawn_blocking boundary
        // already transferred ownership; this just keeps it alive
        // through every return path inside).
        let _active_scan = active_scan;
        let canonical_dir = Path::new(&dir).canonicalize().map_err(|e| {
            log::warn!("canonicalize directory failed: {e}");
            "Cannot resolve directory path".to_string()
        })?;
        if !canonical_dir.is_dir() {
            return Err("Not a directory".to_string());
        }
        let log_label = format!("Scanned font directory '{}'", canonical_dir.display());
        // Collect entries for the GUI persistent cache (#5 Step 8b).
        // Best-effort: if the cache populate later fails or the cache
        // handle isn't available, the user-visible scan still
        // succeeded. Empty Vec when the scan returns no faces is fine
        // — `try_record_folder_in_gui_cache` will write an empty
        // folder row, which `diff_against` later treats as a known
        // folder with no faces (consistent with the cache's data
        // model).
        let mut entries_for_cache: Vec<LocalFontEntry> = Vec::new();
        run_streaming_scan_command(
            scan_id,
            &source_id,
            progress,
            &log_label,
            Some(&mut entries_for_cache),
            |scan_id, emit_batch| scan_directory_inner(&canonical_dir, scan_id, emit_batch),
        )?;
        crate::font_cache_commands::try_record_folder_in_gui_cache(
            &canonical_dir,
            &entries_for_cache,
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
    // Public command enforces MAX_INPUT_PATHS; debug-assert catches any
    // future internal caller that bypasses that check.
    debug_assert!(
        paths.len() <= MAX_INPUT_PATHS,
        "scan_files_inner: paths.len()={} exceeds MAX_INPUT_PATHS={}",
        paths.len(),
        MAX_INPUT_PATHS
    );
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
                reason: ScanStopReason::UserCancel,
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

    Ok(ScanOutcome {
        total,
        reason: ScanStopReason::Natural,
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
            &source_id,
            progress,
            "Scanned local font files",
            // No GUI cache populate for file-list scans: the cache's
            // drift model is folder-anchored (folder mtime vs cached
            // mtime), and an arbitrary file list has no single folder
            // to anchor against. Files-mode sources stay session-only.
            None,
            |scan_id, emit_batch| scan_files_inner(paths, scan_id, emit_batch),
        )
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
    // operations. The fetch_max below uses Release so weakly-ordered
    // ISAs (ARM / Apple Silicon) make this write visible to the
    // worker's poll loop on its next iteration via the paired Acquire
    // load in `font_scan_cancelled`. The per-scan-id equality check
    // in `font_scan_cancelled` is the actual correctness gate, not
    // the ordering of CANCEL_SCAN_ID writes — the Release here is
    // for prompt cancel propagation, not correctness.
    //
    // `fetch_max` ensures a stale cancel for an OLDER (smaller-id) scan
    // arriving after a newer cancel cannot regress CANCEL_SCAN_ID. The
    // returned prior max is intentionally discarded — caller has no
    // useful action either way.
    CANCEL_SCAN_ID.fetch_max(scan_id, Ordering::Release);
}

/// Look up a font face in the user's local source index by family
/// + bold + italic. Returns `None` if no match — callers fall back
/// to system fonts.
///
/// Stale-path note: a path returned here is one that was on disk at
/// the time of the most-recent scan that produced this row. If the
/// file was deleted or moved between then and now, the caller's
/// downstream `subset_font` will fail at the actual fs::read step
/// and surface a normal IO error. Acceptable by design — the
/// alternative (stat-validating every row at lookup) would multiply
/// embed-pass IO without changing the outcome (subset_font would
/// fail the same way half a second later). The scan-then-resolve
/// model assumes the user doesn't shuffle font files mid-embed.
#[tauri::command]
pub fn resolve_user_font(
    family: String,
    bold: bool,
    italic: bool,
) -> Result<Option<FontLookupResult>, String> {
    // Codepoint-count gate, mirrors `find_system_font` and
    // `parse_local_font_file`. Byte-length gating would silently reject
    // valid CJK family names (3 bytes/char) that fit the 256-codepoint
    // intent.
    if family.is_empty() || family.chars().count() > 256 {
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
    let mut conn = open_user_font_db()?;
    let tx = conn
        .transaction()
        .map_err(|e| db_error("transaction start failed", e))?;
    // Guard inside the transaction: a check-before-open-transaction
    // race could let a scan start between the guard and the DELETE,
    // causing the DELETE to block on the scan's tx then immediately
    // remove rows the scan just inserted. Inside the transaction the
    // ACTIVE_SCAN_ID load happens-after BEGIN, so any concurrent
    // begin_font_scan either ran-before-us (guard catches it) or
    // ran-after-our-BEGIN (its inserts wait on us via WAL +
    // busy_timeout, then commit after our DELETE finishes).
    reject_during_active_scan("Cannot remove font source while a scan is running")?;
    // Step 8c: capture one font_face.path BEFORE the DELETE so we can
    // evict the matching folder from the GUI cache after commit. For
    // dir-mode sources every face shares the same parent (scan is
    // non-recursive), so any one face's parent identifies the folder
    // that was cached. Files-mode sources may yield a parent that
    // doesn't correspond to anything in the cache; cache.remove_folder
    // on an unknown folder is a harmless no-op. ON DELETE CASCADE on
    // font_sources sweeps font_faces, so we have to read this BEFORE
    // the DELETE.
    let evict_folder: Option<String> = tx
        .query_row(
            "SELECT path FROM font_faces WHERE source_id = ?1 LIMIT 1",
            params![source_id],
            |r| r.get::<_, String>(0),
        )
        .optional()
        .map_err(|e| db_error("source-faces lookup failed", e))?
        .and_then(|p| {
            Path::new(&p).parent().map(|pp| {
                // font_faces.path is normalized at insert
                // (`canonical_string = normalize_canonical_path(...)`),
                // so `parent()` already returns the prefix-stripped form
                // matching the cache write key. Calling
                // normalize_canonical_path again here is a no-op for
                // current data but makes the contract self-evident at
                // the call site — defends against a future code path
                // that bypasses insert-time normalization (e.g., a
                // file-mode source where pp could be a `\\?\…`-form
                // path that didn't get normalized through scan).
                normalize_canonical_path(&pp.to_string_lossy())
            })
        });
    tx.execute(
        "DELETE FROM font_sources WHERE source_id = ?1",
        params![source_id],
    )
    .map_err(|e| db_error("source delete failed", e))?;
    tx.commit()
        .map_err(|e| db_error("source delete commit failed", e))?;
    if let Some(folder) = evict_folder {
        crate::font_cache_commands::try_remove_folder_from_gui_cache(&folder);
    }
    Ok(())
}

#[tauri::command]
pub fn clear_font_sources() -> Result<(), String> {
    let mut conn = open_user_font_db()?;
    let tx = conn
        .transaction()
        .map_err(|e| db_error("transaction start failed", e))?;
    // Same in-transaction guard as remove_font_source — see WHY there.
    reject_during_active_scan("Cannot clear font sources while a scan is running")?;
    tx.execute("DELETE FROM font_sources", [])
        .map_err(|e| db_error("source clear failed", e))?;
    tx.commit()
        .map_err(|e| db_error("source clear commit failed", e))?;
    // Reset the per-process subset-fallback budget on user-visible
    // session boundaries. Without this, a long-running app that hit
    // the 50 MB ceiling once (e.g., on a single corrupt font) keeps
    // rejecting every fallback for the rest of the lifetime — even
    // after the user has cleared sources and switched to an entirely
    // different font set. The user just signaled "fresh slate"; honor
    // it.
    CUMULATIVE_FALLBACK_BYTES.store(0, Ordering::Release);
    Ok(())
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
fn reject_during_active_scan(message: &str) -> Result<(), String> {
    if ACTIVE_SCAN_ID.load(Ordering::Acquire) != NO_SCAN_ID {
        return Err(message.to_string());
    }
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
    // Single HashSet hit via `insert` (returns true if newly added).
    // Was previously `contains` + `insert` — two lookups for the
    // common case. `insert` returning true means the slot was free
    // before; cache.len() now reflects the post-insert count, so the
    // cap check uses `>` (strictly above the pre-insert size limit).
    let newly_added = cache.insert(canonical_string.clone());
    if newly_added && cache.len() > MAX_PROVENANCE_CACHE_SIZE {
        // Roll back the speculative insert so the cap is firm.
        cache.remove(&canonical_string);
        return Err(format!(
            "Too many registered font paths (> {MAX_PROVENANCE_CACHE_SIZE}). \
             Restart the app to clear the cache."
        ));
    }

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
///
/// Public IPC entry point + the CLI's standalone-embed callsite both
/// invoke this function as a regular `pub fn`; the `#[tauri::command]`
/// shim `subset_font_b64` below wraps it for the GUI's IPC path with
/// base64 encoding so the frontend doesn't pay the JSON `[byte, ...]`
/// expansion (~4–5× per byte → ~50 MB on the worst-case fallback
/// path). CLI's chain mode marshals subsets via base64 inline (see
/// `process_one_chain_input`); CLI's standalone embed bundles them
/// into `engine::FontSubsetPayload` and ships through the engine's
/// JSON-payload boundary (where the expansion is bounded by per-font
/// caps, not the cumulative ceiling).
pub fn subset_font(
    font_path: String,
    font_index: u32,
    codepoints: Vec<u32>,
) -> Result<Vec<u8>, String> {
    // IPC boundary validation: font_index and codepoints come from untrusted JS.
    // font_path also from JS — validate length / control-char / DOS-device
    // shape before any allocation, matching find_system_font's posture.
    crate::util::validate_ipc_path(&font_path, "Font")?;
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
    // Third tier: the persistent GUI cache. The session DB is empty on
    // a fresh launch, so a cross-launch `lookupFontFamily` cache hit
    // would have its returned path land here as "not registered" and
    // subset would reject it — the entire cross-launch use case the
    // persistent cache exists for. Cache membership IS legitimate
    // provenance: the user's `refresh-fonts` (CLI) or scan_font_directory
    // (GUI) is what populated it, both consent paths.
    let is_cache = if is_system || is_user {
        false
    } else {
        crate::font_cache_commands::path_in_gui_cache(&canonical_string)
    };
    if !is_system && !is_user && !is_cache {
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

    // Attempt subsetting; fall back to full font if it fails.
    //
    // catch_unwind around the fontcull call: the klippa engine is in
    // active development and has had documented panics on malformed
    // input (corrupted CFF, bad TTC face counts). User-picked path
    // means a malformed user font crashing fontcull would otherwise
    // panic the IPC command and surface to the frontend as a generic
    // "command failed" with no actionable text. AssertUnwindSafe is
    // sound here: `font_data` is owned (Vec<u8>) and not mutated by
    // the closure (fontcull takes a slice), so unwinding can't leave
    // it in a torn state. Convert any panic into a structured error
    // string the frontend's existing IPC error path can render.
    let subset_result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        if font_index == 0 {
            // Common path: single font or first face in TTC.
            // Display, not Debug — Debug repr leaks internal struct fields,
            // table tags, and byte offsets into a frontend-visible error.
            fontcull::subset_font_data_unicode(&font_data, &all_codepoints, &[])
                .map_err(|e| format!("{e}"))
        } else {
            // TTC with face index > 0: use internal crates with from_index
            subset_with_index(&font_data, font_index, &all_codepoints)
        }
    }))
    .unwrap_or_else(|panic_payload| {
        // Convert panic payload (Box<dyn Any>) into a string for the log
        // and IPC return. Try common payload shapes — &str, String, and
        // boxed error types (anyhow::Error, std::io::Error,
        // Box<dyn Error+Send+Sync>) — before falling back to a generic
        // message. The boxed-error path picks up panics from `expect`
        // chains in fontcull that wrap non-string Display impls.
        let panic_msg = if let Some(s) = panic_payload.downcast_ref::<&str>() {
            (*s).to_string()
        } else if let Some(s) = panic_payload.downcast_ref::<String>() {
            s.clone()
        } else if let Some(e) =
            panic_payload.downcast_ref::<Box<dyn std::error::Error + Send + Sync>>()
        {
            e.to_string()
        } else if let Some(e) = panic_payload.downcast_ref::<std::io::Error>() {
            e.to_string()
        } else {
            format!(
                "fontcull panicked with unknown payload type {:?}",
                panic_payload.type_id()
            )
        };
        log::warn!(
            "fontcull panicked while subsetting '{filename}' (face {font_index}): {panic_msg}"
        );
        Err(format!("Subset panic: {panic_msg}"))
    });

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
            // Cap per-file fallback size first — full font goes through IPC →
            // JS heap → ASS string, so a single 50 MB font would already pin
            // ~70 MB after UUEncode expansion.
            if font_data.len() > MAX_FONT_FALLBACK_SIZE {
                return Err(format!(
                    "Subsetting failed and full font too large ({:.1} MB, max {} MB for fallback)",
                    font_data.len() as f64 / (1024.0 * 1024.0),
                    MAX_FONT_FALLBACK_SIZE / 1024 / 1024
                ));
            }
            // Cumulative session cap — a subtitle referencing N corrupt fonts
            // could otherwise stack-feed the JS heap with N × per-file
            // fallbacks. CAS loop (not fetch_add + rollback) so concurrent
            // fallback emissions can't each observe a stale `prior` and
            // collectively overshoot the budget — fetch_update succeeds only
            // when the CAS sees the live value, so the cap is firm at every
            // moment, not just eventually. Counter only resets on app
            // restart; a session that accumulates 50 MB of fallback has
            // corrupt font sources and should restart anyway.
            let total_after = CUMULATIVE_FALLBACK_BYTES
                .fetch_update(Ordering::AcqRel, Ordering::Acquire, |cur| {
                    let next = cur.checked_add(font_data.len())?;
                    if next > MAX_CUMULATIVE_FALLBACK_BYTES {
                        None
                    } else {
                        Some(next)
                    }
                })
                .map_err(|_| {
                    format!(
                        "Cumulative font fallback exceeded {} MB this session — restart the app and check your font sources",
                        MAX_CUMULATIVE_FALLBACK_BYTES / 1024 / 1024
                    )
                })?
                + font_data.len();
            log::warn!(
                "Subsetting failed for '{}' (face {}): {}, falling back to full font ({:.1} MB cumulative)",
                filename,
                font_index,
                e,
                total_after as f64 / (1024.0 * 1024.0),
            );
            Ok(font_data)
        }
    }
}

/// IPC wrapper around `subset_font` that base64-encodes the result so
/// the GUI's frontend doesn't pay the JSON `[byte, byte, …]` expansion.
/// Pre-fix this returned `Vec<u8>` directly; serde-json would write each
/// byte as decimal+comma (~4–5× per byte), and a 10 MB fallback subset
/// would expand to ~50 MB IPC payload + a main-thread JSON parse pass.
/// Frontend `subsetFont()` decodes via `atob` (mirrors chain-runtime's
/// `decodeBase64`).
#[tauri::command]
pub fn subset_font_b64(
    font_path: String,
    font_index: u32,
    codepoints: Vec<u32>,
) -> Result<String, String> {
    use base64::Engine as _;
    let bytes = subset_font(font_path, font_index, codepoints)?;
    Ok(base64::engine::general_purpose::STANDARD.encode(&bytes))
}

/// Subset a specific face from a TTC/OTC collection file.
/// Uses fontcull's internal crates directly for `FontRef::from_index`.
fn subset_with_index(font_data: &[u8], index: u32, codepoints: &[u32]) -> Result<Vec<u8>, String> {
    use fontcull_klippa::{subset_font, Plan, SubsetFlags};
    use fontcull_read_fonts::collections::IntSet;
    use fontcull_skrifa::{FontRef, GlyphId, Tag};
    use fontcull_write_fonts::types::NameId;

    // Display, not Debug — same anti-pattern as the unicode-subset path
    // (Debug repr leaks internal struct fields, table tags, byte offsets
    // into a frontend-visible error). Round 2's pass missed this site.
    let font = FontRef::from_index(font_data, index)
        .map_err(|e| format!("Cannot parse font face {index}: {e}"))?;

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

    /// Serializes any unit test that reads or mutates DB state, the
    /// `ACTIVE_SCAN_ID` / `CANCEL_SCAN_ID` atomics, or both. cargo test
    /// runs in parallel by default — without serialization, two tests
    /// can race on `compare_exchange` / `fetch_max` and silently flake.
    /// Renamed from `DB_TEST_LOCK` once the cancel tests revealed it
    /// wasn't DB-only.
    static SCAN_TEST_LOCK: Mutex<()> = Mutex::new(());

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

    #[test]
    fn subset_with_index_display_error_carries_diagnostic_context() {
        // A truncated TTF header (4 magic bytes + 3 noise bytes) fails the
        // skrifa parser inside fontcull immediately. The IPC return path
        // formats fontcull errors via `Display` rather than `Debug`
        // because Debug leaks internal struct fields, table tags, and
        // byte offsets into the frontend; this test pins the contract
        // that Display still carries enough context for support
        // diagnostics — i.e., it isn't a bare "subset failed" string.
        //
        // Wrapper prefixes ("Cannot parse font face N:" / "Subset failed
        // for face N:") guarantee a baseline of context regardless of
        // fontcull's Display verbosity, so the assertion below also
        // accepts the prefix text. If a future fontcull upgrade renames
        // the parse error or strips its Display, this test still pins
        // that the wrapper-supplied context is intact.
        let truncated: Vec<u8> = vec![0x00, 0x01, 0x00, 0x00, b'A', b'B', b'C'];
        let err = subset_with_index(&truncated, 0, &[0x41])
            .expect_err("malformed TTF should fail subset_with_index at parse time");
        assert!(!err.is_empty(), "subset error must be non-empty");
        // Either the wrapper prefix OR a recognizable font-format term
        // surfaces — both are acceptable diagnostic signals.
        let lower = err.to_lowercase();
        let has_context = lower.contains("face")
            || lower.contains("font")
            || lower.contains("table")
            || lower.contains("parse")
            || lower.contains("read")
            || lower.contains("invalid")
            || lower.contains("magic")
            || lower.contains("collection");
        assert!(
            has_context,
            "subset error should carry diagnostic context — got: {err}"
        );
        // Same contract for the unicode-subset path that subset_font's
        // index==0 branch hits directly.
        let unicode_err = fontcull::subset_font_data_unicode(&truncated, &[0x41], &[])
            .expect_err("malformed TTF should fail unicode subset")
            .to_string();
        assert!(
            !unicode_err.is_empty(),
            "fontcull Display must be non-empty"
        );
    }

    #[test]
    fn user_font_key_lowercases_nfc_normalizes_and_separates_with_us() {
        // Case-insensitive — mirrors TS userFontKey case-fold contract.
        assert_eq!(
            user_font_key("Arial", false, false),
            user_font_key("ARIAL", false, false)
        );
        // NFC normalization — precomposed `é` and decomposed `e + ´` key
        // identically. Without NFC, a font name table storing `café` would
        // miss an ASS \fn `café` lookup (and vice versa).
        let precomposed = user_font_key("caf\u{00e9}", false, false);
        let decomposed = user_font_key("cafe\u{0301}", false, false);
        assert_eq!(precomposed, decomposed);
        // Bold and italic flags carry distinctly.
        let plain = user_font_key("Arial", false, false);
        let bold = user_font_key("Arial", true, false);
        let italic = user_font_key("Arial", false, true);
        let both = user_font_key("Arial", true, true);
        assert_eq!(
            [&plain, &bold, &italic, &both]
                .iter()
                .copied()
                .collect::<HashSet<_>>()
                .len(),
            4
        );
        // Separator is U+001F (Unit Separator); pin the exact byte shape so
        // future cross-layer audits land on the same encoding the TS layer
        // produces.
        assert_eq!(plain, "arial\u{001F}0\u{001F}0");
    }

    #[test]
    fn bounded_font_family_name_trims_and_rejects_overlong_values() {
        assert_eq!(
            bounded_font_family_name("  Demo Sans  ".chars()),
            Some("Demo Sans".to_string())
        );
        assert_eq!(
            bounded_font_family_name("x".repeat(256).chars()),
            Some("x".repeat(256))
        );
        assert!(bounded_font_family_name("x".repeat(257).chars()).is_none());
        assert!(bounded_font_family_name("   ".chars()).is_none());
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
        let _guard = SCAN_TEST_LOCK.lock().unwrap();
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
        let _guard = SCAN_TEST_LOCK.lock().unwrap();
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
        let _guard = SCAN_TEST_LOCK.lock().unwrap();
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
        let _guard = SCAN_TEST_LOCK.lock().unwrap();
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
        assert_eq!(outcome.reason, ScanStopReason::Natural);
        assert!(emitted.is_empty());
    }

    /// A targeted cancel before the first file causes an immediate return on
    /// the first iteration. Validates the cancel-poll path without depending
    /// on real font files. Buffer is empty so no batch is emitted.
    #[test]
    fn files_inner_honors_targeted_cancel_before_first_file() {
        let _guard = SCAN_TEST_LOCK.lock().unwrap();
        let scan_id = CANCEL_SCAN_ID.load(Ordering::Relaxed).saturating_add(1);
        CANCEL_SCAN_ID.fetch_max(scan_id, Ordering::Relaxed);
        let mut emitted: Vec<Vec<LocalFontEntry>> = Vec::new();
        let outcome = scan_files_inner(vec!["irrelevant.ttf".to_string()], scan_id, |batch| {
            emitted.push(batch);
            Ok(())
        })
        .expect("cancel returns Ok with partial results");
        assert_eq!(outcome.total, 0);
        assert_eq!(outcome.reason, ScanStopReason::UserCancel);
        assert!(emitted.is_empty());
    }

    /// `cancel_font_scan` records the requested id and stale lower ids do not
    /// overwrite a newer cancel request. Requires an active scan guard so
    /// the new range check (must target the currently-active id) lets the
    /// cancel through.
    #[test]
    fn cancel_command_records_scan_id_monotonically() {
        let _lock = SCAN_TEST_LOCK.lock().unwrap();
        let scan_id = CANCEL_SCAN_ID.load(Ordering::Relaxed).saturating_add(10);
        let _guard = begin_font_scan(scan_id).expect("begin scan");
        cancel_font_scan(scan_id);
        assert_eq!(CANCEL_SCAN_ID.load(Ordering::Relaxed), scan_id);
        // A stale lower id (scan_id - 1 < active) is NOT rejected by the
        // range check — it satisfies `scan_id <= active`, so it reaches
        // `fetch_max`. But `fetch_max` is a no-op because CANCEL_SCAN_ID
        // is already at the higher value (scan_id), so the lower id can't
        // regress the field. Net effect: stale lower-id cancels are
        // accepted-but-harmless.
        cancel_font_scan(scan_id - 1);
        assert_eq!(CANCEL_SCAN_ID.load(Ordering::Relaxed), scan_id);
    }

    /// `cancel_font_scan` rejects ids that don't belong to a current or
    /// past active scan — defense-in-depth against a misbehaving frontend
    /// that could otherwise permanently disable cancellation by calling
    /// with a u64::MAX id.
    #[test]
    fn cancel_command_rejects_future_id_when_no_active_scan() {
        let _guard = SCAN_TEST_LOCK.lock().unwrap();
        let baseline = CANCEL_SCAN_ID.load(Ordering::Relaxed);
        cancel_font_scan(u64::MAX);
        assert_eq!(CANCEL_SCAN_ID.load(Ordering::Relaxed), baseline);
    }
}
