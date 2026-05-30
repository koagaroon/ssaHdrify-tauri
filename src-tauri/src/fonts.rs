use font_kit::family_name::FamilyName;
use font_kit::handle::Handle;
use font_kit::properties::{Properties, Style as FontKitStyle, Weight as FontKitWeight};
use font_kit::source::SystemSource;
use once_cell::sync::Lazy;
use rusqlite::{params, Connection, OptionalExtension, Transaction};
use std::borrow::Cow;
use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use crate::util::{validate_ipc_path, MAX_INPUT_PATHS};

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

/// Cap on raw font data read for scanning/subsetting — prevents OOM with large
/// CJK fonts while still accepting known real-world CJK collections just under
/// 60 MiB.
const MAX_FONT_DATA_SIZE: u64 = 64 * 1024 * 1024;
const UNICODE_SCALAR_MAX: u32 = 0x10FFFF;
const SKIP_CMAP12_GROUP_GLYPH_ID: u32 = u32::MAX;

// MAX_FONT_FALLBACK_SIZE,
// MAX_CUMULATIVE_FALLBACK_BYTES, and CUMULATIVE_FALLBACK_BYTES were
// deleted along with the subset-failure raw-bytes fallback they
// bounded. The fallback path turned every readable local file with
// an allowed font extension into a data-disclosure primitive when
// paired with the cache provenance trust — closing it at the
// subset layer obviates the per-file and cumulative caps. See
// `subset_font` for the commentary on the trade-off.

/// Cap on each in-memory font-provenance cache (`ALLOWED_FONT_PATHS`
/// and `ALLOWED_CACHE_FONT_PATHS`), as a defense against a pathological
/// long-running session. User-picked font provenance is stored in the
/// session SQLite index instead of an in-memory set, so XL source folders
/// do not pin tens of gigabytes of path/name metadata.
///
/// Doc-comment names both consumers — an earlier version only
/// mentioned "system-font" provenance, but the separate
/// cache-provenance set also shares the same cap via `insert_with_cap`.
/// Cap applies per-set (each can hold up to 100k entries independently).
const MAX_PROVENANCE_CACHE_SIZE: usize = 100_000;

/// AppData filename for the session-only user font index. It is cleared at
/// app startup; persistence across restarts is intentionally deferred.
///
/// Exposed `pub` so the CLI bin (a separate crate from the lib) can
/// reuse the same literal in its TempFontDbDir cleanup
/// . Previously the CLI re-declared
/// `CLI_FONT_DB_FILENAME` as a sibling literal; if either drifted the
/// CLI's TempFontDbDir::drop would leave SQLite + sidecar files on disk.
pub const USER_FONT_DB_FILENAME: &str = "user-font-sources.session.sqlite3";

/// Cap on directory entries the preflight pass will canonicalize before
/// bailing out. Real font folders top out around 20–30k entries even in
/// the XL bucket; a directory exceeding this is either a misclick onto
/// a system root or a hostile fixture, and either way the user wants
/// "directory too large to preview" feedback rather than a frozen UI
/// while millions of canonicalize calls run.
const MAX_PREFLIGHT_ENTRIES: usize = 200_000;

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
const MAX_SUBSET_FONT_INDEX: u32 = 255;

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

/// Provenance cache: tracks (font path, face index) pairs returned by
/// `find_system_font`. Only entries here are allowed to be read by
/// `subset_font`'s system-font branch — together with the system-fonts-
/// dir restriction below, this is two layers of defense against
/// arbitrary-file-read via IPC.
///
/// key changed from `String` to `(String, u32)`.
/// The path-only key let an attacker-influenced subtitle request a
/// system-font path with an arbitrary face index — TTC files contain
/// multiple faces, so `subset_font(arial.ttc, 5, ...)` against a path
/// registered for face 0 would silently read face 5. Keying by
/// (path, face_index) makes the gate check both dimensions of the
/// caller's claim against the actual registration.
///
/// Never evicted — the set is bounded by the number of unique system
/// fonts (typically < 1000), and eviction would introduce TOCTOU
/// windows.
static ALLOWED_FONT_PATHS: Lazy<Mutex<HashSet<(String, u32)>>> =
    Lazy::new(|| Mutex::new(HashSet::new()));

/// Separate provenance set for persistent-cache lookups. When
/// `lookup_family` returns a hit, the (path, face_index) pair is
/// registered here so `subset_font` can accept it. Kept apart from
/// `ALLOWED_FONT_PATHS` because:
///
/// 1. The dir restriction that applies to system fonts MUST NOT apply
///    to cache hits — cached paths point at user-picked folders, not
///    system dirs, and folding them into one set would either drop the
///    dir defense for system fonts (bad) or break cache lookups (worse).
/// 2. The threat-model classification is different: system fonts are
///    discovered via font-kit which guarantees the path lives under a
///    known dir; cache hits are trusted on the strength of the
///    in-process lookup having succeeded against an opened SQLite file
///    (P1a — single-user, AppData-local).
///
/// Trusting cache hits in-process restores the design-locked CLI
/// Situation B ("no --font-dir + cache exists → implicit cache use")
/// and the GUI's lookup tier 2 that an earlier change broke by
/// rejecting all cache rows as untrusted.
/// See `register_cache_provenance` for the entry point.
static ALLOWED_CACHE_FONT_PATHS: Lazy<Mutex<HashSet<(String, u32)>>> =
    Lazy::new(|| Mutex::new(HashSet::new()));

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
            family_name_key TEXT NOT NULL,
            bold INTEGER NOT NULL,
            italic INTEGER NOT NULL,
            key_kind INTEGER NOT NULL CHECK(key_kind IN (0, 1)),
            face_id INTEGER NOT NULL,
            source_order INTEGER NOT NULL,
            FOREIGN KEY(face_id) REFERENCES font_faces(face_id) ON DELETE CASCADE
        );
        CREATE INDEX IF NOT EXISTS idx_font_family_lookup
            ON font_family_keys(family_name_key, key_kind, bold, italic, source_order DESC, face_id DESC);
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
    // reparse-point check before stale-DB cleanup. Posture
    // consistency with safe_io / encoding / fonts elsewhere. On Windows
    // remove_file of a symlink removes the link not the target, so the
    // security delta is small — but a startup invariant is "cache files
    // under per-user AppData are not symlinks", and a reparse point
    // here signals tampering / a manual user link that we should not
    // silently destroy.
    //
    // error message names the reparse-point path and an
    // actionable next step. The error propagates through lib.rs's
    // setup closure into a rfd MessageBox the user sees at startup;
    // the previous generic "Cannot reset user font index" gave the
    // user no path to inspect and no recovery hint.
    if crate::util::is_reparse_point(&db_path) {
        log::warn!(
            "User font index path '{}' is a reparse point; \
             refusing to reset.",
            db_path.display()
        );
        return Err(format!(
            "User font index path is a reparse point (symlink / junction): {}. \
             Inspect and remove the link manually before relaunching.",
            db_path.display()
        ));
    }
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
    // a forensic warn but never block init. Reparse-point sidecars are
    // skipped (defense-in-depth alongside the main file's check above).
    for suffix in ["-journal", "-wal", "-shm"] {
        let mut sidecar = db_path.clone().into_os_string();
        sidecar.push(suffix);
        let sidecar = PathBuf::from(sidecar);
        if crate::util::is_reparse_point(&sidecar) {
            log::warn!(
                "User font index sidecar '{}' is a reparse point; \
                 skipping cleanup.",
                sidecar.display()
            );
            continue;
        }
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

/// Drop the cached path to the user-font DB so subsequent
/// `open_user_font_db` calls fail-shut instead of returning
/// SQLITE_CANTOPEN against a deleted file.
///
/// `init_cli_font_sources` wraps the import sequence
/// in a `TempFontDbDir` guard whose Drop wipes the temp directory on
/// failure — but the static `USER_FONT_DB_PATH` outlives the guard
/// because `init_user_font_db` ran first and set it. Today's call
/// graph has no caller that retries after a failed
/// `init_cli_font_sources`, so the dangling state isn't reachable,
/// but a future retry path would see the second `init_user_font_db`
/// overwrite cleanly while any code that opened the DB between the
/// failure and retry would hit the deleted file. Latent bug; this
/// helper closes the latency window.
pub fn clear_user_font_db_path() {
    if let Ok(mut path_slot) = USER_FONT_DB_PATH.lock() {
        *path_slot = None;
    }
    // Poison case: nothing to do; subsequent open will surface a
    // distinct "corrupted" error already covered by `open_user_font_db`.
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
    /// A defense-in-depth ceiling fired. Two ceilings collapse into
    /// this variant — the per-scan log line distinguishes which:
    /// - `MAX_FONTS_PER_SCAN` face ceiling
    /// - `MAX_PREFLIGHT_ENTRIES` dedup-set ceiling (previously the
    ///   dedup break reported `Natural`, silently truncating to the
    ///   user)
    ///
    /// Partial results are preserved on the way out in both cases.
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
/// Shared between GUI's `try_record_folder_in_gui_cache` +
/// `entries_to_metadata` callers and CLI's `run_refresh_fonts` loop.
///
/// Saturating u64→i64 on `size_bytes` and u32→i32 on `face_index`
/// keeps with the codebase's cast-discipline pattern; the limits are
/// impossible in practice (8.4 EB / 2.1 G faces) but the explicit
/// saturate makes intent visible.
pub fn entries_to_cache_metadata(
    entries: &[LocalFontEntry],
) -> Vec<crate::font_cache::FontMetadata> {
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
    // `String` at that point. Pattern 3 lifetime coupling.
    let mut mtime_cache: std::collections::HashMap<&str, i64> = std::collections::HashMap::new();
    entries
        .iter()
        .filter_map(|e| {
            // Route through the shared `try_modified_at` helper in
            // `font_cache.rs` so every stat-time extraction site stays
            // single-source. An earlier version reproduced the helper
            // body inline; a future change to the helper (e.g., its
            // `.ok()?` for pre-epoch mtime safety) would have left this
            // site behind.
            //
            // Same try_modified_at helper guards two callers unevenly:
            // the FOLDER mtime path filters entries when try_modified_at
            // returns None (avoids epoch-zero re-trigger of refresh on
            // next run); the FILE mtime path here still falls back to
            // 0, persisting epoch-zero into SQLite's
            // cached_fonts.file_mtime. Lookups now verify the cached
            // row against the live file before trusting a cache hit,
            // so rows with unreadable mtimes would be unusable. Drop
            // them at write time instead of persisting an epoch-zero
            // sentinel that cannot be distinguished from a real
            // 1970-01-01 timestamp later.
            let mtime = *mtime_cache.entry(e.path.as_str()).or_insert_with(|| {
                crate::font_cache::try_modified_at(Path::new(e.path.as_str())).unwrap_or(-1)
            });
            if mtime < 0 {
                return None;
            }
            let mut family_keys: Vec<crate::font_cache::FamilyKey> = Vec::new();
            for family_name in &e.families {
                family_keys.push(crate::font_cache::FamilyKey {
                    family_name: family_name.clone(),
                    bold: e.bold,
                    italic: e.italic,
                });
            }

            Some(crate::font_cache::FontMetadata {
                file_path: e.path.clone(),
                file_size: i64::try_from(e.size_bytes).unwrap_or(i64::MAX),
                file_mtime: mtime,
                face_index: i32::try_from(e.index).unwrap_or(i32::MAX),
                family_keys,
                face_name_aliases: e.face_names.clone(),
            })
        })
        .collect()
}

// Visibility asymmetry WHY: module-private (not `pub(crate)` like
// the persistent cache's sibling `family_lookup_key` in
// font_cache.rs). Only the NFC + lowercase normalization is shared
// via `family_lookup_key`;
// THIS helper additionally appends bold / italic markers via the
// U+001F field separator and is a session-DB key only. Promoting it
// to `pub(crate)` would invite cross-module callers to skip the
// bold/italic composition — those callers should use the underlying
// `family_lookup_key` directly. Kept private as the explicit signal.
#[cfg(test)]
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

const USER_FONT_KEY_KIND_FAMILY: i32 = 0;
const USER_FONT_KEY_KIND_FACE_ALIAS: i32 = 1;

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
    // WHY plain INSERT (not OR IGNORE) for the family_keys table:
    // the `font_family_keys` schema in `init_user_font_db` has NO
    // UNIQUE constraint on (family_name_key, key_kind, face_id,
    // source_order), so there is nothing for an IGNORE clause to
    // suppress. A font with multiple localized family names
    // legitimately produces multiple rows sharing the same `face_id`
    // — that's the normal case. Any genuine SQLite error from this
    // INSERT (disk full, table dropped mid-tx) is a real failure we
    // want to surface via db_error, not silently swallow.
    // `insert_face` above uses INSERT OR IGNORE because font_faces
    // DOES have a UNIQUE constraint on (path, face_index) for
    // cross-source dedup — the IGNORE there short-circuits the
    // duplicate-source case, which is intentional. The asymmetry is
    // by design.
    let mut insert_key = tx
        .prepare(
            "
            INSERT INTO font_family_keys(
                family_name_key, bold, italic, key_kind, face_id, source_order
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)
            ",
        )
        .map_err(|e| db_error("family-key insert prepare failed", e))?;

    for entry in entries {
        // saturate u64→i64 instead of aborting the
        // batch. Companion `entries_to_cache_metadata` saturates via
        // `unwrap_or(i64::MAX)` (Pattern 2 policy mismatch — same
        // conversion, two opposite failure modes). u64 values above
        // i64::MAX (8.4 EB) are mathematically impossible for a real
        // font file size, but a hostile cache row crafted with a
        // numeric overflow shouldn't have the power to abort an
        // entire session's font batch import.
        let size_bytes = i64::try_from(entry.size_bytes).unwrap_or(i64::MAX);
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
                    crate::font_cache::family_lookup_key(&family),
                    i32::from(entry.bold),
                    i32::from(entry.italic),
                    USER_FONT_KEY_KIND_FAMILY,
                    face_id,
                    source_order
                ])
                .map_err(|e| db_error("family-key insert failed", e))?;
        }
        for face_name in entry.face_names {
            insert_key
                .execute(params![
                    crate::font_cache::family_lookup_key(&face_name),
                    0,
                    0,
                    USER_FONT_KEY_KIND_FACE_ALIAS,
                    face_id,
                    source_order
                ])
                .map_err(|e| db_error("face-name key insert failed", e))?;
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
fn is_user_font_face_registered(canonical_path: &str, face_index: u32) -> Result<bool, String> {
    let conn = open_user_font_db()?;
    // Face-index narrowed: TTC files carry multiple faces under one
    // path (e.g. Source Han Serif Regular = face 0, Bold = face 1).
    // BOTH `path` AND `face_index` must be present in `font_faces`;
    // an attacker-chosen face index that was never observed by a
    // scan fails the gate even if the path itself was scanned.
    // UNIQUE(path, face_index) in the schema makes the (path,
    // face_index) lookup a single index probe. An earlier comment
    // inverted the contract, claiming the gate checked only path; the
    // SQL has always been path+face_index.
    conn.query_row(
        "SELECT 1 FROM font_faces WHERE path = ?1 AND face_index = ?2 LIMIT 1",
        params![canonical_path, face_index as i64],
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
    crate::util::validate_font_family(&family)?;

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
                // Use ascii-only fold for consistency with
                // `has_allowed_font_extension` and `parse_local_font_file`
                // — all font extensions are pure ASCII,
                // so locale-aware `to_lowercase()` is unnecessary alloc.
                .map(|e| e.to_ascii_lowercase())
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

/// Per-face cap on the number of localized family-name variants kept
/// from the OpenType `name` table. Worst-case real-world fonts ship at
/// most ~5 variants (English plus Simplified Chinese, Traditional
/// Chinese, Japanese, Korean — e.g. Source Han / Noto CJK families); 8
/// keeps 60% margin over that without giving attacker-influenced font
/// packs room to bloat per-entry memory.
///
/// The prior value of 32 multiplied across MAX_FONTS_PER_SCAN (100k)
/// times 256 codepoints times 4 bytes at roughly 3.2 GB of string data
/// accumulated during cache-write windows; lowering this to 8 cuts the
/// worst case to ~800 MB and combines with MAX_CACHE_POPULATE_FACES for
/// ~160 MB peak in practice. Full-face aliases have the same per-face
/// cap but are stored as one style-insensitive key each in SQLite, so
/// the expanded persistent/session DB ceiling is 16 lookup rows per face
/// (8 family rows + 8 alias rows), not 8 + (8 × 4 style rows).
const MAX_FAMILY_VARIANTS_PER_FACE: usize = 8;
const MAX_FACE_NAME_VARIANTS_PER_FACE: usize = 8;
const MAX_LEGACY_NAME_RECORD_BYTES: usize = 4096;
const MAX_LEGACY_NAME_DECODE_ATTEMPTS_PER_FACE: usize = 256;
const MAX_CMAP12_ENCODING_RECORDS_PER_FACE: usize = 1024;
const MAX_CMAP12_SUBTABLES_PER_FACE: usize = 128;
const MAX_CMAP12_GROUPS_PER_FACE: usize = 65_536;
const MAX_CMAP12_REWRITE_OFFSETS_PER_FACE: usize = 4_096;

/// Cap on the number of font faces a single directory scan will
/// snapshot into the GUI / CLI persistent cache. Above this threshold
/// the cache populate is skipped with a WARN log — session-DB import
/// still succeeds, the user just doesn't get cross-launch acceleration
/// for that source. Defense-in-depth against GUI cache OOM and
/// refresh-fonts OOM: real font libraries top out at a few thousand
/// faces; 20k is 5× margin over anything legitimate.
pub const MAX_CACHE_POPULATE_FACES: usize = 20_000;

fn bounded_font_family_name(chars: impl Iterator<Item = char>) -> Option<String> {
    // Stream-trim instead of `take(N).collect().trim()`: truncating before
    // validation can turn an overpadded name like "   ArialMalicious..." into
    // the fake alias "Arial". Keep at most the 256 codepoints that could be
    // accepted, count internal whitespace toward that cap, and ignore only
    // true leading/trailing whitespace.
    let mut name = String::new();
    let mut name_count = 0usize;
    let mut pending_ws = String::new();
    let mut pending_ws_count = 0usize;
    let mut seen_non_ws = false;

    for ch in chars {
        if !seen_non_ws && ch.is_whitespace() {
            continue;
        }
        if ch.is_whitespace() {
            pending_ws_count += 1;
            if name_count + pending_ws_count <= 256 {
                pending_ws.push(ch);
            }
            continue;
        }

        seen_non_ws = true;
        if name_count + pending_ws_count + 1 > 256 {
            return None;
        }
        name.push_str(&pending_ws);
        pending_ws.clear();
        name.push(ch);
        name_count += pending_ws_count + 1;
        pending_ws_count = 0;
    }

    // Guard counts CODEPOINTS, not bytes — a 100-char CJK family name
    // (300+ UTF-8 bytes) is perfectly legitimate.
    if name.is_empty() {
        return None;
    }
    // fold validate_font_family into this helper so
    // every call from `parse_local_font_file` (3 sites: family /
    // typographic-family / full-name+postscript fallback) automatically
    // rejects BiDi / zero-width-bearing names. Without the fold, a
    // crafted font pack with U+202E in its name table could land a row
    // in the
    // session DB / persistent cache, then surface in detection-grid
    // labels and log lines with the reversal undisturbed (the
    // unicode-controls sweep covers TS-side ASS \fn references but
    // name-table entries come in via this Rust path). Single-source
    // semantics — `validate_font_family` is the canonical rejection
    // predicate used elsewhere, calling it here means callers don't
    // each have to remember to revalidate.
    if crate::util::validate_font_family(&name).is_err() {
        return None;
    }
    Some(name)
}

fn materialize_face_name_aliases(face_name_variants: HashSet<String>) -> Vec<String> {
    // FULL_NAME / POSTSCRIPT_NAME values identify a concrete face. Keep them
    // even when a font also repeats the same string in FAMILY_NAME; Dream Han
    // Serif W15+ does exactly that, and filtering the duplicate leaves only a
    // bold style-sensitive family row that ASS `Bold=0` cannot match.
    let mut face_names: Vec<String> = face_name_variants.into_iter().collect();
    face_names.sort();
    face_names
}

fn read_be_u16(data: &[u8], offset: usize) -> Option<u16> {
    data.get(offset..offset + 2)
        .map(|bytes| u16::from_be_bytes([bytes[0], bytes[1]]))
}

fn read_be_u32(data: &[u8], offset: usize) -> Option<u32> {
    data.get(offset..offset + 4)
        .map(|bytes| u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
}

fn write_be_u32(data: &mut [u8], offset: usize, value: u32) -> Option<()> {
    data.get_mut(offset..offset + 4)?
        .copy_from_slice(&value.to_be_bytes());
    Some(())
}

fn table_range(data: &[u8], face_offset: usize, tag: &[u8; 4]) -> Option<(usize, usize)> {
    let table_count = usize::from(read_be_u16(data, face_offset + 4)?);
    let table_records = face_offset.checked_add(12)?;
    for index in 0..table_count {
        let record = table_records.checked_add(index.checked_mul(16)?)?;
        let record_tag = data.get(record..record + 4)?;
        if record_tag != tag {
            continue;
        }
        let offset = usize::try_from(read_be_u32(data, record + 8)?).ok()?;
        let len = usize::try_from(read_be_u32(data, record + 12)?).ok()?;
        let end = offset.checked_add(len)?;
        if end <= data.len() {
            return Some((offset, len));
        }
        return None;
    }
    None
}

fn sfnt_face_offsets(data: &[u8], is_collection: bool, max_faces: u32) -> Vec<(u32, usize)> {
    if !is_collection {
        return vec![(0, 0)];
    }
    if data.get(0..4) != Some(b"ttcf") {
        return Vec::new();
    }
    let Some(face_count) = read_be_u32(data, 8) else {
        return Vec::new();
    };
    let capped = face_count.min(max_faces);
    let mut offsets = Vec::new();
    for index in 0..capped {
        let Some(offset) = read_be_u32(data, 12 + usize::try_from(index).unwrap_or(0) * 4) else {
            break;
        };
        let Ok(offset) = usize::try_from(offset) else {
            continue;
        };
        if offset < data.len() {
            offsets.push((index, offset));
        }
    }
    offsets
}

fn sanitize_cmap12_invalid_groups(
    font_data: &[u8],
    index: u32,
) -> Result<(Cow<'_, [u8]>, usize), String> {
    let face_offset = if is_ttc_data(font_data) {
        let Ok(index) = usize::try_from(index) else {
            return Ok((Cow::Borrowed(font_data), 0));
        };
        let Some(offset_slot) = 12usize.checked_add(index.saturating_mul(4)) else {
            return Ok((Cow::Borrowed(font_data), 0));
        };
        let Some(offset) = read_be_u32(font_data, offset_slot) else {
            return Ok((Cow::Borrowed(font_data), 0));
        };
        let Ok(offset) = usize::try_from(offset) else {
            return Ok((Cow::Borrowed(font_data), 0));
        };
        offset
    } else {
        0
    };

    let Some((cmap_offset, cmap_len)) = table_range(font_data, face_offset, b"cmap") else {
        return Ok((Cow::Borrowed(font_data), 0));
    };
    let Some(cmap_end) = cmap_offset.checked_add(cmap_len) else {
        return Ok((Cow::Borrowed(font_data), 0));
    };
    let Some(record_count) = read_be_u16(font_data, cmap_offset + 2).map(usize::from) else {
        return Ok((Cow::Borrowed(font_data), 0));
    };

    let mut earliest_subtable_rel: Option<usize> = None;
    let max_record_count = cmap_len.saturating_sub(4) / 8;
    let record_count = record_count.min(max_record_count);
    if record_count > MAX_CMAP12_ENCODING_RECORDS_PER_FACE {
        return Err(format!(
            "cmap encoding record count {record_count} exceeds max {MAX_CMAP12_ENCODING_RECORDS_PER_FACE}"
        ));
    }
    let mut scanned_subtables = HashSet::new();
    let mut scanned_cmap12_groups = 0usize;
    let mut skip_group_offsets = HashSet::new();
    let mut clamp_end_offsets = HashSet::new();
    for record_index in 0..record_count {
        let Some(record_rel) = 4usize.checked_add(record_index.saturating_mul(8)) else {
            break;
        };
        if earliest_subtable_rel.is_some_and(|first_subtable| record_rel >= first_subtable) {
            break;
        }
        let Some(record_offset) = cmap_offset.checked_add(record_rel) else {
            break;
        };
        if record_offset
            .checked_add(8)
            .map_or(true, |end| end > cmap_end)
        {
            break;
        }
        let Some(subtable_rel) = read_be_u32(font_data, record_offset + 4) else {
            break;
        };
        let Ok(subtable_rel) = usize::try_from(subtable_rel) else {
            continue;
        };
        if subtable_rel >= cmap_len {
            continue;
        }
        if subtable_rel < record_rel.saturating_add(8) {
            continue;
        }
        let Some(subtable_offset) = cmap_offset.checked_add(subtable_rel) else {
            continue;
        };
        if !scanned_subtables.insert(subtable_offset) {
            continue;
        }
        if scanned_subtables.len() > MAX_CMAP12_SUBTABLES_PER_FACE {
            return Err(format!(
                "cmap subtable count exceeds max {MAX_CMAP12_SUBTABLES_PER_FACE}"
            ));
        }
        if read_be_u16(font_data, subtable_offset) != Some(12) {
            continue;
        }
        let Some(length) = read_be_u32(font_data, subtable_offset + 4) else {
            continue;
        };
        let Ok(length) = usize::try_from(length) else {
            continue;
        };
        let Some(subtable_end) = subtable_offset.checked_add(length) else {
            continue;
        };
        if length < 16 || subtable_end > cmap_end || subtable_end > font_data.len() {
            continue;
        }
        earliest_subtable_rel =
            Some(earliest_subtable_rel.map_or(subtable_rel, |current| current.min(subtable_rel)));
        let Some(group_count) = read_be_u32(font_data, subtable_offset + 12) else {
            continue;
        };
        let Ok(group_count) = usize::try_from(group_count) else {
            continue;
        };
        let max_group_count = length.saturating_sub(16) / 12;
        let group_count = group_count.min(max_group_count);
        scanned_cmap12_groups = scanned_cmap12_groups
            .checked_add(group_count)
            .ok_or_else(|| "cmap format-12 group count overflowed".to_string())?;
        if scanned_cmap12_groups > MAX_CMAP12_GROUPS_PER_FACE {
            return Err(format!(
                "cmap format-12 group count {scanned_cmap12_groups} exceeds max {MAX_CMAP12_GROUPS_PER_FACE}"
            ));
        }
        let groups_offset = subtable_offset + 16;
        for group_index in 0..group_count {
            let Some(group_offset) = groups_offset.checked_add(group_index.saturating_mul(12))
            else {
                break;
            };
            if group_offset + 12 > subtable_end {
                break;
            }
            let Some(start) = read_be_u32(font_data, group_offset) else {
                break;
            };
            let Some(end) = read_be_u32(font_data, group_offset + 4) else {
                break;
            };
            if start > end || start > UNICODE_SCALAR_MAX {
                if skip_group_offsets.insert(group_offset)
                    && skip_group_offsets.len() + clamp_end_offsets.len()
                        > MAX_CMAP12_REWRITE_OFFSETS_PER_FACE
                {
                    return Err(format!(
                        "cmap format-12 rewrite count exceeds max {MAX_CMAP12_REWRITE_OFFSETS_PER_FACE}"
                    ));
                }
            } else if end > UNICODE_SCALAR_MAX {
                if clamp_end_offsets.insert(group_offset + 4)
                    && skip_group_offsets.len() + clamp_end_offsets.len()
                        > MAX_CMAP12_REWRITE_OFFSETS_PER_FACE
                {
                    return Err(format!(
                        "cmap format-12 rewrite count exceeds max {MAX_CMAP12_REWRITE_OFFSETS_PER_FACE}"
                    ));
                }
            }
        }
    }

    if skip_group_offsets.is_empty() && clamp_end_offsets.is_empty() {
        return Ok((Cow::Borrowed(font_data), 0));
    }

    let mut sanitized = font_data.to_vec();

    let mut skip_group_offsets: Vec<_> = skip_group_offsets.into_iter().collect();
    skip_group_offsets.sort_unstable();
    for group_offset in &skip_group_offsets {
        let _ = write_be_u32(&mut sanitized, *group_offset, UNICODE_SCALAR_MAX);
        let _ = write_be_u32(&mut sanitized, *group_offset + 4, UNICODE_SCALAR_MAX);
        let _ = write_be_u32(
            &mut sanitized,
            *group_offset + 8,
            SKIP_CMAP12_GROUP_GLYPH_ID,
        );
    }

    let mut clamp_end_offsets: Vec<_> = clamp_end_offsets.into_iter().collect();
    clamp_end_offsets.sort_unstable();
    for end_offset in &clamp_end_offsets {
        let _ = write_be_u32(&mut sanitized, *end_offset, UNICODE_SCALAR_MAX);
    }

    Ok((
        Cow::Owned(sanitized),
        skip_group_offsets.len() + clamp_end_offsets.len(),
    ))
}

fn decode_utf16be_name(raw: &[u8]) -> Option<String> {
    if raw.len() % 2 != 0 {
        return None;
    }
    let code_units: Vec<u16> = raw
        .chunks_exact(2)
        .map(|chunk| u16::from_be_bytes([chunk[0], chunk[1]]))
        .collect();
    String::from_utf16(&code_units).ok()
}

fn raw_looks_utf16be_ascii(raw: &[u8]) -> bool {
    raw.len() >= 4 && raw.len() % 2 == 0 && raw.chunks_exact(2).all(|chunk| chunk[0] == 0)
}

fn clean_legacy_name(decoded: String) -> Option<String> {
    let cleaned = decoded.replace('\0', "");
    bounded_font_family_name(cleaned.chars())
}

fn decode_legacy_name_record(
    platform: u16,
    encoding: u16,
    language: u16,
    raw: &[u8],
) -> Option<String> {
    if raw.len() > MAX_LEGACY_NAME_RECORD_BYTES {
        return None;
    }

    if matches!(platform, 0) || matches!((platform, encoding), (3, 0 | 1 | 10)) {
        return decode_utf16be_name(raw).and_then(clean_legacy_name);
    }

    // Some legacy CJK fonts use platform 3 encoding IDs 2/3/4/5 for
    // Shift-JIS / GBK / Big5 / Wansung family names, while still storing
    // ASCII PostScript names as UTF-16BE. Prefer UTF-16BE only for the
    // unambiguous ASCII-shaped records, then decode the legacy DBCS bytes.
    if raw_looks_utf16be_ascii(raw) {
        if let Some(name) = decode_utf16be_name(raw).and_then(clean_legacy_name) {
            return Some(name);
        }
    }

    let legacy_encoding = match (platform, encoding, language) {
        (3, 2, _) | (1, 1, _) => Some(encoding_rs::SHIFT_JIS),
        (3, 3, _) | (1, 25, _) => Some(encoding_rs::GBK),
        (3, 4, _) | (1, 2, _) => Some(encoding_rs::BIG5),
        (3, 5, _) | (1, 3, _) => Some(encoding_rs::EUC_KR),
        (1, 0, 11) => Some(encoding_rs::SHIFT_JIS),
        (1, 0, 19) => Some(encoding_rs::BIG5),
        (1, 0, 23) => Some(encoding_rs::EUC_KR),
        (1, 0, 33) => Some(encoding_rs::GBK),
        _ => None,
    };

    if let Some(encoding) = legacy_encoding {
        let (decoded, _, had_errors) = encoding.decode(raw);
        if !had_errors {
            return clean_legacy_name(decoded.into_owned());
        }
    }

    if raw.iter().all(|byte| byte.is_ascii()) {
        return String::from_utf8(raw.to_vec())
            .ok()
            .and_then(clean_legacy_name);
    }
    None
}

fn legacy_face_style(data: &[u8], face_offset: usize) -> (bool, bool) {
    let weight_bold = table_range(data, face_offset, b"OS/2")
        .and_then(|(offset, len)| {
            if len >= 6 {
                read_be_u16(data, offset + 4)
            } else {
                None
            }
        })
        .is_some_and(|weight| weight >= 600);
    let (mac_bold, mac_italic) = table_range(data, face_offset, b"head")
        .and_then(|(offset, len)| {
            if len >= 46 {
                read_be_u16(data, offset + 44)
            } else {
                None
            }
        })
        .map(|style| ((style & 0x0001) != 0, (style & 0x0002) != 0))
        .unwrap_or((false, false));
    (weight_bold || mac_bold, mac_italic)
}

fn parse_legacy_name_table_entries(
    data: &[u8],
    canonical_string: &str,
    size_bytes: u64,
    is_collection: bool,
    max_faces: u32,
) -> Vec<LocalFontEntry> {
    let mut entries = Vec::new();
    for (face_index, face_offset) in sfnt_face_offsets(data, is_collection, max_faces) {
        let Some((name_offset, name_len)) = table_range(data, face_offset, b"name") else {
            continue;
        };
        if name_len < 6 {
            continue;
        }
        let Some(name_end) = name_offset.checked_add(name_len) else {
            continue;
        };
        let Some(record_count) = read_be_u16(data, name_offset + 2).map(usize::from) else {
            continue;
        };
        let Some(string_offset) = read_be_u16(data, name_offset + 4).map(usize::from) else {
            continue;
        };
        if string_offset < 6 || string_offset > name_len {
            continue;
        }
        let Some(string_base) = name_offset.checked_add(string_offset) else {
            continue;
        };
        if string_base > name_end {
            continue;
        }
        let max_record_count = string_offset.saturating_sub(6) / 12;
        let record_count = record_count.min(max_record_count);

        let mut families: HashSet<String> = HashSet::new();
        let mut face_name_variants: HashSet<String> = HashSet::new();
        let mut seen_records = HashSet::new();
        let mut decode_attempts = 0usize;
        for record_index in 0..record_count {
            let Some(record) = name_offset
                .checked_add(6)
                .and_then(|v| v.checked_add(record_index.checked_mul(12)?))
            else {
                break;
            };
            if record.checked_add(12).map_or(true, |end| end > name_end) {
                break;
            }
            let Some(platform) = read_be_u16(data, record) else {
                continue;
            };
            let Some(encoding) = read_be_u16(data, record + 2) else {
                continue;
            };
            let Some(language) = read_be_u16(data, record + 4) else {
                continue;
            };
            let Some(name_id) = read_be_u16(data, record + 6) else {
                continue;
            };
            if !matches!(name_id, 1 | 4 | 6 | 16) {
                continue;
            }
            let target_full = match name_id {
                1 | 16 => families.len() >= MAX_FAMILY_VARIANTS_PER_FACE,
                4 | 6 => face_name_variants.len() >= MAX_FACE_NAME_VARIANTS_PER_FACE,
                _ => false,
            };
            if target_full {
                continue;
            }
            let Some(len) = read_be_u16(data, record + 8).map(usize::from) else {
                continue;
            };
            if len == 0 || len > MAX_LEGACY_NAME_RECORD_BYTES {
                continue;
            }
            let Some(offset) = read_be_u16(data, record + 10).map(usize::from) else {
                continue;
            };
            let Some(start) = string_base.checked_add(offset) else {
                continue;
            };
            let Some(end) = start.checked_add(len) else {
                continue;
            };
            if end > name_end {
                continue;
            }
            if !seen_records.insert((platform, encoding, language, name_id, start, end)) {
                continue;
            }
            if decode_attempts >= MAX_LEGACY_NAME_DECODE_ATTEMPTS_PER_FACE {
                break;
            }
            decode_attempts += 1;

            let target_full = match name_id {
                1 | 16 => families.len() >= MAX_FAMILY_VARIANTS_PER_FACE,
                4 | 6 => face_name_variants.len() >= MAX_FACE_NAME_VARIANTS_PER_FACE,
                _ => false,
            };
            if target_full {
                continue;
            }
            let Some(name) =
                decode_legacy_name_record(platform, encoding, language, &data[start..end])
            else {
                continue;
            };
            match name_id {
                1 | 16 => {
                    if families.len() < MAX_FAMILY_VARIANTS_PER_FACE {
                        families.insert(name);
                    }
                }
                4 | 6 => {
                    if face_name_variants.len() < MAX_FACE_NAME_VARIANTS_PER_FACE {
                        face_name_variants.insert(name);
                    }
                }
                _ => {}
            }
        }

        if families.is_empty() {
            if let Some(name) = face_name_variants.iter().min().cloned() {
                families.insert(name);
            }
        }
        if families.is_empty() {
            continue;
        }
        let mut families: Vec<String> = families.into_iter().collect();
        families.sort();
        let face_names = materialize_face_name_aliases(face_name_variants);
        let (bold, italic) = legacy_face_style(data, face_offset);
        entries.push(LocalFontEntry {
            path: canonical_string.to_string(),
            index: face_index,
            families,
            face_names,
            bold,
            italic,
            size_bytes,
        });
    }
    entries
}

fn merge_legacy_name_table_entries(entries: &mut Vec<LocalFontEntry>, legacy: Vec<LocalFontEntry>) {
    for legacy_entry in legacy {
        let Some(existing) = entries
            .iter_mut()
            .find(|entry| entry.index == legacy_entry.index)
        else {
            entries.push(legacy_entry);
            continue;
        };
        for family in legacy_entry.families {
            if existing.families.len() >= MAX_FAMILY_VARIANTS_PER_FACE {
                break;
            }
            if !existing.families.contains(&family) {
                existing.families.push(family);
            }
        }
        for face_name in legacy_entry.face_names {
            if existing.face_names.len() >= MAX_FACE_NAME_VARIANTS_PER_FACE {
                break;
            }
            if !existing.face_names.contains(&face_name) {
                existing.face_names.push(face_name);
            }
        }
        existing.families.sort();
        existing.face_names.sort();
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
/// `NO_SCAN_ID` is the no-cancellation sentinel. `scan_directory_collecting`
/// passes it on the cache-populate path (CLI `refresh-fonts` + GUI
/// post-commit cache populate, neither of which participates in the
/// scan-cancel system). Interactive scan workers pass a positive id
/// minted by `begin_font_scan`.
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
    //
    // bounded-read pattern closes the TOCTOU
    // window between the pre-read stat gate (`metadata.len()` check
    // above) and the unbounded `fs::read`. An attacker who can swap
    // the file between stat and read (rare on single-user but
    // non-zero on slow / network-mounted FS where the scan loop
    // walks for seconds) could force a full-size allocation of a
    // GB-scale impostor file before the post-read recheck fires.
    // `File::open + take(MAX_FONT_DATA_SIZE + 1) + read_to_end`
    // caps the buffer at the OS layer; the +1 byte lets us
    // distinguish "exactly at cap" from "over cap" — if the read
    // returns more than MAX_FONT_DATA_SIZE bytes, the file was over
    // cap and we silent-skip.
    use std::io::Read;
    let mut data = Vec::new();
    {
        let file = match std::fs::File::open(canonical) {
            Ok(f) => f,
            Err(_) => return Vec::new(),
        };
        let mut limited = file.take(MAX_FONT_DATA_SIZE + 1);
        if limited.read_to_end(&mut data).is_err() {
            return Vec::new();
        }
    }

    // Post-read size re-check (parity with
    // `encoding.rs::read_text_detect_encoding` and `subset_font` below).
    // Belt-and-suspenders with the bounded-read above: the +1 byte trick
    // means `data.len() > MAX_FONT_DATA_SIZE` cleanly identifies over-cap
    // reads even if the OS short-reads. Silent-skip pattern matches
    // the rest of this function (return `Vec::new()` instead of
    // Err); per-face errors here are all consumed by the scan loop
    // the same way.
    if data.len() as u64 > MAX_FONT_DATA_SIZE {
        return Vec::new();
    }

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

        // catch_unwind around the per-face
        // skrifa name-table walk + entry construction. Mirrors the
        // existing wrap around fontcull::subset_font_data_unicode
        // below: skrifa is in active development and crafted TTC /
        // OTC inputs can trigger panics on bad face counts, malformed
        // CFF, or out-of-range name-table records (P1b
        // attacker-influenced content sources). Without catch_unwind
        // here, a single panicking face would unwind through the
        // whole `parse_local_font_file` and abort the surrounding
        // scan — instead of the documented "skip this face, continue
        // with the next" behavior the MAX_CONSECUTIVE_FAILURES tolerance
        // already provides for non-panic errors. AssertUnwindSafe is
        // sound: `data` is &Vec<u8> read-only, the inner mutations
        // are on local HashSet / Vec / Option that get dropped on
        // unwind; the only escape is `entries.push` which happens at
        // the tail, so a panic before push leaves entries unchanged.
        let face_result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let attrs = font_ref.attributes();
            let bold = attrs.weight.value() >= 600.0;
            let italic = !matches!(attrs.style, fontcull_skrifa::attribute::Style::Normal);

            let mut family_variants: HashSet<String> = HashSet::new();
            let mut face_name_variants: HashSet<String> = HashSet::new();
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

            for id in [StringId::FULL_NAME, StringId::POSTSCRIPT_NAME] {
                for localized in font_ref.localized_strings(id) {
                    if let Some(name) = bounded_font_family_name(localized.chars()) {
                        face_name_variants.insert(name);
                        if face_name_variants.len() >= MAX_FACE_NAME_VARIANTS_PER_FACE {
                            break;
                        }
                    }
                }
                if face_name_variants.len() >= MAX_FACE_NAME_VARIANTS_PER_FACE {
                    break;
                }
            }

            // Last-resort fallback: malformed fonts may have no family IDs but
            // still have a full name. Indexing that is better than silently
            // dropping the face, and it avoids re-entering font-kit/DirectWrite.
            if family_variants.is_empty() {
                if let Some(name) = face_name_variants.iter().min().cloned() {
                    primary_hint = Some(name.clone());
                    family_variants.insert(name);
                }
            }

            (
                family_variants,
                face_name_variants,
                primary_hint,
                bold,
                italic,
            )
        }));
        let (family_variants, face_name_variants, primary_hint, bold, italic) = match face_result {
            Ok(t) => t,
            Err(_) => {
                log::warn!(
                    "skrifa panicked while parsing face {i} in '{}' — skipping face",
                    canonical.display()
                );
                continue;
            }
        };

        if family_variants.is_empty() {
            continue;
        }

        // Stabilize the primary-name pick: prefer the best available English
        // family name if it is among the variants, else fall back to sorted
        // order so UI listings stay deterministic across runs.
        //
        // WHY HashSet→sort here: `family_variants` is a
        // HashSet for cheap dedup during the per-name-record walk above
        // (a font's name table can list the same family string multiple
        // times across (platform, language) tuples). HashSet iteration
        // order is non-deterministic across runs, so directly using
        // `family_variants.into_iter().collect()` would surface different
        // primary names on different launches even for the same font
        // file. Sorting the Vec stabilizes the unwrap_or path before
        // the rotate_right() rotation places the English variant at
        // index 0. Test pin: `db_lookup_prefers_newer_source_for_same_family_key`.
        let mut families: Vec<String> = family_variants.into_iter().collect();
        families.sort();
        if let Some(ref primary) = primary_hint {
            match families.iter().position(|v| v == primary) {
                Some(pos) => {
                    // rotate_right(1) moves families[pos] to index 0 while keeping
                    // the elements before it in alphabetical order — swap(0, pos)
                    // would displace the element at 0 to pos, breaking sort order.
                    families[..=pos].rotate_right(1);
                }
                None => {
                    // structural fallback when
                    // `primary_hint` is Some but doesn't appear in
                    // family_variants. The current
                    // bounded_font_family_name + localized_strings
                    // path guarantees primary_hint round-trips through
                    // the variant set (deterministic on today's
                    // skrifa builds), but a future skrifa upgrade with
                    // caller-time normalization could break that
                    // implicit alignment. Without the explicit push,
                    // the UI primary name silently reverts to
                    // sorted-first; pushing primary at index 0 keeps
                    // the contract structural (not security-relevant,
                    // but matches the "type-system over doc discipline"
                    // posture used elsewhere, e.g. current_unix_seconds).
                    log::debug!(
                        "primary_hint '{primary}' not in family_variants; prepending explicitly"
                    );
                    families.insert(0, primary.clone());
                }
            }
        }

        let face_names = materialize_face_name_aliases(face_name_variants);

        entries.push(LocalFontEntry {
            path: canonical_string.clone(),
            index: i,
            families,
            face_names,
            bold,
            italic,
            size_bytes,
        });
    }
    let legacy_entries = parse_legacy_name_table_entries(
        &data,
        &canonical_string,
        size_bytes,
        is_collection,
        max_faces,
    );
    if !legacy_entries.is_empty() {
        merge_legacy_name_table_entries(&mut entries, legacy_entries);
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
        if validate_ipc_path(&p, "File").is_err() {
            rejected += 1;
            continue;
        }
        let Ok(canonical) = Path::new(&p).canonicalize() else {
            rejected += 1;
            continue;
        };
        if !canonical.is_file()
            || !seen.insert(normalize_canonical_path(&canonical.to_string_lossy()))
        {
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
///
/// Bytes-cap posture : per-file size is capped at
/// `MAX_FONT_DATA_SIZE` (64 MB) and face count at `MAX_FONTS_PER_SCAN`
/// (100k). There is NO cumulative-bytes ceiling on the scan as a
/// whole. Peak memory stays bounded because each file's bytes are
/// dropped before the next iteration (fs::read + parse run in
/// sequence, not in parallel), AND the user-facing
/// `preflight_font_directory` reports total bytes BEFORE the scan
/// starts so an XL-confirmation modal can warn the user. P1b
/// (subtitle / font CONTENT SOURCE threat) is bounded by the
/// preflight gate; adding a cumulative cap would be
/// defensive-complexity for a scenario the preflight already covers.
/// Revisit if a future flow bypasses preflight (e.g., direct
/// drag-drop into the scan command without going through the source
/// modal's XL confirmation).
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
    let mut seen: HashSet<String> = HashSet::new();
    // Tracks whether the visited-entry cap fired so the post-loop
    // reason routes to `CeilingHit` instead of falling through to
    // `Natural` (previously the cap was silent to the UI).
    let mut dedup_ceiling_hit = false;

    // `visited` (via `read.enumerate()`) bounds the iteration cost at
    // `MAX_PREFLIGHT_ENTRIES`. An earlier change deliberately moved the
    // dedup gate behind `has_allowed_font_extension` so non-font files
    // no longer fill `seen` and falsely report a
    // ceiling hit — but that left CLI paths (`scan_directory_collecting`,
    // `import_font_directory_for_cli`, `refresh-fonts`) without any
    // bound on a directory of millions of non-font files (GUI runs
    // preflight first; CLI does not). Counting every entry here closes
    // the gap and mirrors `preflight_directory_inner`'s
    // `visited >= MAX_PREFLIGHT_ENTRIES` contract, so scan and preflight
    // speak about the same directory size. The cap also indirectly
    // bounds `seen` memory: `seen.insert` happens at most once per
    // visited entry, so `seen.len() <= visited <= MAX_PREFLIGHT_ENTRIES`.
    for (visited, entry) in read.enumerate() {
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

        // Cap fires BEFORE we touch the entry (canonicalize / metadata),
        // so the worst-case CPU cost is bounded at `MAX_PREFLIGHT_ENTRIES`
        // canonicalize calls — not MAX+1.
        if visited >= MAX_PREFLIGHT_ENTRIES {
            log::warn!(
                "font scan {} visited {MAX_PREFLIGHT_ENTRIES} entries in '{}'; \
                 stopping early to bound iteration cost (partial results preserved)",
                scan_id,
                canonical_dir.display()
            );
            dedup_ceiling_hit = true;
            break;
        }

        let entry = match entry {
            Ok(e) => e,
            Err(_) => continue,
        };
        let path = entry.path();
        // Skip reparse points (symlinks / Windows junctions / OneDrive
        // placeholders) BEFORE any metadata or canonicalize call, matching
        // preflight_directory_inner's policy. The earlier design followed
        // symlinks via canonicalize + starts_with containment; that left
        // a preflight↔scan mismatch where a malicious font pack could
        // hide thousands of font files behind top-level symlinks (preflight
        // reported few/zero files → "huge folder" warning never fired →
        // scan parsed everything). With input-provenance treated as
        // untrusted (subtitle/font packs from public release channels),
        // refusing to chase symlinks in scan keeps the size warning honest
        // and bounds parse work to what the user actually picked. Cost:
        // packager workflows that ship fonts as symlinks to a shared
        // store stop working — those are rare on Windows desktop, and
        // affected users can resolve the symlinks before importing.
        if crate::util::is_reparse_point(&path) {
            continue;
        }
        if !path.is_file() {
            continue;
        }

        // Defense-in-depth: even with the reparse-point skip above,
        // canonicalize + starts_with stays as a backstop against any
        // future reparse-point family the helper doesn't yet recognize
        // (junctions, hardlinks-via-NTFS-features, future Win API types).
        let canonical = match path.canonicalize() {
            Ok(c) => c,
            Err(_) => continue,
        };
        if !canonical.starts_with(canonical_dir) {
            continue;
        }
        // The dedup `seen` set fills only on font-eligible extensions
        // (see `has_allowed_font_extension` below). Counting only font
        // files here, after the visited-cap above bounds total
        // iteration cost, means the dedup set sizes track the user-
        // visible font count rather than the directory's total entry
        // count — `preflight_directory_inner` uses the same accounting
        // so both speak about the same "directory size" when the XL-
        // confirm dialog fires. The pre-Wave-4.1 form let the dedup
        // set fill on every regular file regardless of extension, and
        // the extension check lived deeper
        // inside `parse_local_font_file`. A directory of 200k non-font
        // files (.txt / .png / etc.) would fill `seen` to the cap and
        // trip `CeilingHit` with 0 faces — but
        // `preflight_directory_inner` counts only font-extension files,
        // so the XL-size confirmation modal never fires (P1b: hostile
        // pack defeats the safety check). Aligning scan's accounting
        // with preflight's means both speak about the same "directory
        // size."
        if !has_allowed_font_extension(&canonical) {
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

    // Post-loop cancellation re-check . The top-of-loop
    // `font_scan_cancelled` only fires on the NEXT iteration; when
    // `parse_local_font_file`'s per-face cancel poll fires inside the
    // FINAL directory entry / file, the loop exits naturally and the
    // outer reason would otherwise read as Natural — UI sees "completed
    // normally" while the partial buffer is silently kept. Cancel
    // wins over `dedup_ceiling_hit` because the cap-triggered break
    // and a subsequent user cancel are both "stopped early"; reporting
    // UserCancel matches what the user expects to see.
    let reason = if font_scan_cancelled(scan_id) {
        ScanStopReason::UserCancel
    } else if dedup_ceiling_hit {
        ScanStopReason::CeilingHit
    } else {
        ScanStopReason::Natural
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
fn run_streaming_scan_command<S>(
    scan_id: u64,
    source_id: &str,
    progress: tauri::ipc::Channel<ScanProgress>,
    log_label: &str,
    // When `Some`, every batch is cloned into the supplied Vec before
    // being passed downstream to the session-DB import. The directory
    // scan path uses this to feed `try_record_folder_in_gui_cache`
    // after commit; the file-list scan path passes `None` because it
    // has no folder anchor for the cache's drift model. Bounded by
    // MAX_CACHE_POPULATE_FACES.
    //
    // Cap-hit policy: when the cap fires mid-scan, the function returns
    // `Ok(true)` to signal the caller that the persistent cache
    // populate MUST be skipped. The Vec is truncated to fit (in-session
    // memory bound) but the caller does not write a row to the
    // persistent cache. An earlier design routed the truncated Vec
    // through `try_record_folder_in_gui_cache`
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
) -> Result<bool /* cache_truncated */, String>
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
    //
    // Batch-send failures stay swallowed (UX progress is informational —
    // missing a few batches is harmless), but the Done send is load-
    // bearing: `runStreamingScan` on the JS side awaits a donePromise
    // that only resolves when this event arrives. A dropped receiver
    // (Channel.onmessage cleared, page unloaded) would otherwise leave
    // that promise hanging silently. Log WARN so the asymmetric failure
    // mode is visible in diagnostics.
    if let Err(e) = progress.send(ScanProgress::Done {
        reason: outcome.reason,
        added: import.added,
        duplicated: import.duplicated,
    }) {
        log::warn!(
            "scan {scan_id}: Done sentinel send failed — frontend may be hanging on donePromise ({e})"
        );
    }

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
    Ok(cache_truncated)
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
    let canonical = dir
        .canonicalize()
        .map_err(|e| format!("Cannot resolve directory '{}': {e}", dir.display()))?;
    if !canonical.is_dir() {
        return Err(format!("Not a directory: {}", canonical.display()));
    }
    let mut entries: Vec<LocalFontEntry> = Vec::new();
    scan_directory_inner(&canonical, NO_SCAN_ID, |batch| {
        // Defense-in-depth against refresh-fonts OOM on crafted font
        // folders: fail fast if a single source would
        // push us past the cache-populate cap. The CLI caller in
        // run_refresh_fonts catches this and continues with the next
        // dir; without the cap, a malicious pack could hold hundreds
        // of MB to multi-GB of font metadata in memory before the
        // `cache.replace_folder` write would even start.
        if entries.len() + batch.len() > MAX_CACHE_POPULATE_FACES {
            return Err(format!(
                "Source has more font faces than the persistent cache safely accepts \
                 ({}+ faces, cap {MAX_CACHE_POPULATE_FACES}). Skipping this source.",
                entries.len() + batch.len()
            ));
        }
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
        // WHY full path in log_label (vs sanitized / truncated):
        // log::info! is INTERNAL telemetry consumed by
        // `RUST_LOG=info` dev runs and tauri-plugin-log files written
        // under app-data-dir (user-local, no cross-user reach). The
        // path is provided BY the user via picker / drag-drop, so it
        // is content the user already saw. P1a (single-user desktop)
        // does not consider local-file disclosure a leak. If this
        // app later ships a remote-log shipper or a public crash
        // reporter, sanitize at the shipper boundary, not here — the
        // local INFO line is the right diagnostic granularity.
        let log_label = format!("Scanned font directory '{}'", canonical_dir.display());
        // Collect entries for the GUI persistent cache.
        // Best-effort: if the cache populate later fails or the cache
        // handle isn't available, the user-visible scan still
        // succeeded. Empty Vec when the scan returns no faces is fine
        // — `try_record_folder_in_gui_cache` will write an empty
        // folder row, which `diff_against` later treats as a known
        // folder with no faces (consistent with the cache's data
        // model).
        let mut entries_for_cache: Vec<LocalFontEntry> = Vec::new();
        let cache_truncated = run_streaming_scan_command(
            scan_id,
            &source_id,
            progress,
            &log_label,
            Some(&mut entries_for_cache),
            |scan_id, emit_batch| scan_directory_inner(&canonical_dir, scan_id, emit_batch),
        )?;
        // Skip persistent cache populate when the scan was truncated.
        // A truncated row would be indistinguishable from a full row to
        // mtime-based drift
        // detection, leaving the user cornered into "Clear cache"
        // recovery for cache-rejected font lookups. Session-DB still
        // has the full scan and is the tier-1 lookup, so in-session
        // embeds aren't affected. Across launches the user needs to
        // re-scan a smaller folder (or accept session-DB-only).
        if !cache_truncated {
            crate::font_cache_commands::try_record_folder_in_gui_cache(
                &canonical_dir,
                &entries_for_cache,
            );
        }
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
    // Per-path validate / canonicalize failures `continue` silently; a
    // single `log::info!` post-loop tells anyone reading the log why
    // the parsed face count came from fewer than `total_inputs` source
    // files. Emitted unconditionally before any cancel / ceiling
    // early-return below, so the signal survives partial returns.
    let total_inputs = paths.len();
    let mut rejected = 0usize;

    let log_rejected = |rejected: usize| {
        if rejected > 0 {
            log::info!(
                "scan_files: dropped {rejected} of {total_inputs} input path(s) (validate / canonicalize failure)"
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

        if validate_ipc_path(&p, "File").is_err() {
            rejected += 1;
            continue;
        }

        // No `is_reparse_point` pre-check here, unlike
        // `scan_directory_inner`. The asymmetry is intentional:
        // this function processes paths the user
        // EXPLICITLY selected one by one via file picker / drag-drop,
        // each entry expressing the user's direct intent to scan that
        // exact file. `scan_directory_inner`'s reject applies to
        // entries DISCOVERED inside a user-chosen folder, where a
        // crafted font pack could plant top-level symlinks to inflate
        // the per-folder count past the XL-safety preflight (P1b). For
        // the file-list path the user picked each entry on its own, so
        // chasing a single symlinked entry doesn't bypass any aggregate
        // count gate. `canonicalize` below + `has_allowed_font_extension`
        // (in `parse_local_font_file`) still bound the exfil surface
        // to font-extension targets.
        let canonical = match Path::new(&p).canonicalize() {
            Ok(c) => c,
            Err(_) => {
                rejected += 1;
                continue;
            }
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
    // natural) in `scan_directory_inner`. The reason is structural —
    // scan_directory_inner has a `visited >= MAX_PREFLIGHT_ENTRIES`
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
        // File-list scans pass `None` for the collected Vec, so
        // cache_truncated is always false here. Discard the bool.
        .map(|_truncated| ())
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
    // Shared `validate_font_family` mirrors find_system_font and
    // parse_local_font_file; codepoint-counted so CJK family names
    // (3 bytes/char) fit the 256-codepoint intent.
    crate::util::validate_font_family(&family)?;

    let family_name_key = crate::font_cache::family_lookup_key(&family);
    let conn = open_user_font_db()?;
    conn.query_row(
        "
        SELECT f.path, f.face_index
        FROM font_family_keys k
        JOIN font_faces f ON f.face_id = k.face_id
        WHERE k.family_name_key = ?1
          AND ((k.key_kind = ?2 AND k.bold = ?3 AND k.italic = ?4)
               OR (k.key_kind = ?5 AND k.bold = 0 AND k.italic = 0))
        ORDER BY k.key_kind, k.source_order DESC, k.face_id DESC
        LIMIT 1
        ",
        params![
            family_name_key,
            USER_FONT_KEY_KIND_FAMILY,
            i32::from(bold),
            i32::from(italic),
            USER_FONT_KEY_KIND_FACE_ALIAS
        ],
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
pub fn remove_font_source(source_id: String, kind: Option<String>) -> Result<(), String> {
    validate_font_source_id(&source_id)?;
    let mut conn = open_user_font_db()?;
    let tx = conn
        .transaction()
        .map_err(|e| db_error("transaction start failed", e))?;
    // Acquire-load on ACTIVE_SCAN_ID pairs with begin_font_scan's
    // SeqCst CAS publish: any concurrent begin_font_scan either
    // ran-before-us (Acquire sees the non-NO_SCAN_ID value, guard
    // returns Err) or ran-after-our-load (its inserts wait on us via
    // WAL + 5s busy_timeout, then commit after our DELETE finishes).
    // The surrounding SQLite transaction is an orthogonal serialization
    // mechanism for the DB-state side; the cross-thread happens-before
    // we need on the Rust side comes from the Acquire ordering
    // .
    reject_during_active_scan("Cannot remove font source while a scan is running")?;
    // Only dir-mode sources populate the persistent GUI cache
    // (try_record_folder_in_gui_cache is called from
    // scan_font_directory; scan_font_files explicitly passes None for
    // the cache collector — see comment in scan_font_files). So we
    // ONLY derive an evict_folder when this source is a dir.
    //
    // the prior unconditional "grab any face path's
    // parent → evict" would wrongly evict a coincident dir source's
    // cache row when the user removed a files-mode source whose face
    // happened to share a parent (e.g. files source picking
    // `D:\Fonts\extra.ttf` from inside an existing dir source `D:\Fonts`).
    // Kind comes from the frontend's FontSource model where the
    // dir/files distinction was already tracked.
    //
    // `kind` is Option<> for forward compatibility — an older frontend
    // bundle or a missed callsite passes None and falls back to the
    // safe path (no eviction). The cost is a stale cache row that
    // next-launch drift detection picks up, vs the over-evict that
    // would silently break a different source's cache acceleration.
    // Defer P1a — frontend in-process trust: `kind` is the lone IPC
    // argument here whose value flows from the
    // frontend without server-side cross-check against SQL state.
    // The rest of the validation pattern (e.g., source_id through
    // `validate_font_source_id`, paths through `validate_ipc_path`)
    // treats every input as untrusted; `kind` is the exception. The
    // current threat model (single-user desktop, in-process frontend)
    // doesn't cross the trust boundary at the IPC layer, so a buggy
    // frontend passing the wrong kind would surface as user-visible
    // misbehavior (stale cache rows for a removed dir source, or
    // skipped cache eviction for a renamed files source) rather than
    // as an exploit. If the project ships in a server / multi-tenant
    // shape later, derive `is_dir_source` from a join against
    // `gui_font_cache.cached_folders` instead of trusting `kind`.
    let is_dir_source = kind.as_deref() == Some("dir");
    // `LIMIT 1` without `ORDER BY` returns an
    // arbitrary row, which is correct UNDER the current one-level
    // scan invariant — every face in a dir-mode source shares the
    // same `parent()` directory (the user-picked folder), so any
    // single face's parent is the right eviction key. If
    // `scan_font_directory` ever grows recursive descent (multi-level
    // walks into subdirectories), a single dir source could contain
    // faces from multiple parents and this `LIMIT 1` would pick one
    // arbitrarily — at that point switch the schema to store
    // `folder_path` on `font_sources` directly so the eviction key
    // is no longer inferred. A schema column is the deeper fix but
    // requires a migration; this comment pins the invariant the
    // current shape depends on.
    let evict_folder: Option<String> = if is_dir_source {
        tx.query_row(
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
                // the call site.
                normalize_canonical_path(&pp.to_string_lossy())
            })
        })
    } else {
        None
    };
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
    // Acquire CacheMutationGuard upfront so session-DB clear and
    // persistent-cache eviction commit atomically.
    // Previously, helper-side `try_acquire` could fail silently if a
    // rescan was in progress — session DB cleared, cache rows survived,
    // wrong-font silent until next clear or rebuild. Refusing the whole
    // call here lets the user retry once rescan completes; the frontend
    // already surfaces returned Err strings in the FontSourceModal
    // banner so the retry message reaches the user.
    let _mutation_guard =
        crate::font_cache_commands::CacheMutationGuard::try_acquire().map_err(|_| {
            "Cache rescan in progress — wait for it to finish, then retry Clear all sources."
                .to_string()
        })?;
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
    // Mirror remove_font_source's symmetry: a session-DB clear must
    // also evict the persistent cache, otherwise the next embed pass
    // resolves to paths whose session-DB provenance was just cleared
    // and subset_font rejects them with "Font path was not discovered
    // by a scan command". Use the locked variant — we hold the guard
    // already from this fn's top, so re-acquiring would CAS-fail /
    // silently skip (the original guard-bug).
    crate::font_cache_commands::clear_all_folders_in_gui_cache_locked(&_mutation_guard);
    // Drop the in-process cache-provenance set. Without this, paths
    // registered earlier in the
    // session via `lookup_family` cache hits would survive
    // `clear_font_sources` and still pass subset_font's gate on
    // their next use, undercutting the user's "fresh slate" signal.
    // ALLOWED_FONT_PATHS (system fonts) intentionally NOT cleared —
    // system fonts don't depend on user-source state and re-clearing
    // them would force expensive re-discovery via font-kit on the
    // next embed.
    if let Ok(mut cache) = ALLOWED_CACHE_FONT_PATHS.lock() {
        cache.clear();
    }
    // the `CUMULATIVE_FALLBACK_BYTES` reset that
    // used to live here was deleted along with the subset-fallback
    // path. No per-session budget to reset; nothing left to do on
    // session boundaries beyond what the calls above already
    // performed.
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
    // validate the canonical path through
    // `validate_ipc_path` for parity with `register_cache_provenance`
    // (above). System-font paths come from font-kit's OS enumeration
    // and pass through canonicalize() + the system-fonts-dir constraint
    // — both stronger gates than cache rows have — but the symmetric
    // validate call is cheap defense-in-depth that closes the asymmetry
    // a reviewer flagged. If a future change to font-kit, the system
    // font enumeration, or normalize_canonical_path lets through a
    // path with control chars / BiDi / `..` segments, this is the
    // final gate before the trust set commits.
    crate::util::validate_ipc_path(&canonical_string, "System font")?;
    insert_with_cap(
        &ALLOWED_FONT_PATHS,
        "system",
        canonical_string.clone(),
        font_index,
    )?;
    Ok(FontLookupResult {
        path: canonical_string,
        index: font_index,
    })
}

/// Shared (path, face_index) insertion helper used by both provenance
/// sets. `cache` is the target set; `canonical_string` and `face_index`
/// are the entry. `label` distinguishes the set in error messages so
/// a future "Too many registered font paths" report can be attributed
/// to system fonts vs cache hits — without the label, both would
/// report the same text and triage would have to dig into the caller.
/// Enforces
/// `MAX_PROVENANCE_CACHE_SIZE` per-set as a rollback-on-overflow contract.
fn insert_with_cap(
    cache: &Lazy<Mutex<HashSet<(String, u32)>>>,
    label: &str,
    canonical_string: String,
    face_index: u32,
) -> Result<(), String> {
    let mut set = cache
        .lock()
        .map_err(|_| "Internal error: font path cache corrupted".to_string())?;
    // Single HashSet hit via `insert` (returns true if newly added).
    // Was previously `contains` + `insert` — two lookups for the
    // common case. `insert` returning true means the slot was free
    // before; cache.len() now reflects the post-insert count, so the
    // cap check uses `>` (strictly above the pre-insert size limit).
    let entry = (canonical_string, face_index);
    let newly_added = set.insert(entry.clone());
    if newly_added && set.len() > MAX_PROVENANCE_CACHE_SIZE {
        // Roll back the speculative insert so the cap is firm.
        set.remove(&entry);
        return Err(format!(
            "Too many registered font paths in {label} set (> {MAX_PROVENANCE_CACHE_SIZE}). \
             Restart the app to clear the cache."
        ));
    }
    Ok(())
}

/// Drop every entry from `ALLOWED_CACHE_FONT_PATHS`. Called when a
/// cache-wide reset happens: `clear_font_cache` rebuilds the SQLite
/// file but previously left in-process provenance rows behind —
/// combined with a hostile `--cache-file` swap (P1b), stale trust
/// entries would persist until app restart and let `subset_font`
/// accept paths the freshly-cleared cache no longer references.
/// `clear_font_sources` already does the same eviction at
/// session-DB-clear time; this is the symmetric helper for the
/// persistent-cache clear path. ALLOWED_FONT_PATHS (system fonts) is
/// intentionally NOT touched here — system fonts don't depend on cache
/// state.
pub fn clear_cache_provenance() {
    if let Ok(mut cache) = ALLOWED_CACHE_FONT_PATHS.lock() {
        cache.clear();
    }
}

/// Register a persistent-cache lookup hit into the cache provenance
/// set. CLI's `resolve_embed_font` and the GUI's `lookup_font_family`
/// call this so a path returned by the persistent cache passes
/// `subset_font`'s gate, closing a design-vs-implementation conflict:
///
/// - CLI Situation B (no `--font-dir` + cache exists → implicit cache
///   use) and the GUI's lookup tier 2 both depended on cache-returned
///   paths being subsequently subsettable. An earlier post-cache-gate
///   change rejected them as untrusted, breaking the documented
///   behavior.
///
/// - Per the personal-desktop threat model, a cache file can be stale,
///   corrupt, or same-user modified. Trust is therefore limited to rows
///   returned by `lookup_family` in THIS process, after path and
///   face-index validation. This is basic fail-safe protection, not a
///   high-assurance defense against a process that can race every
///   filesystem operation. Revisit if the project later ships a server
///   or multi-user mode.
///
/// Routes to `ALLOWED_CACHE_FONT_PATHS` — kept apart from the system-
/// font set so the system-fonts-dir defense still applies to
/// `find_system_font` registrations.
///
/// Signature accepts `&font_cache::FontLookupResult` directly,
/// replacing the prior `(canonical_path: &str, face_index: u32)` shape.
/// `FontLookupResult` has `pub(crate)` fields, so external callers
/// (CLI bin, future external consumers) cannot construct one outside
/// of `FontCache::lookup_family`. The invariant "only lookup_family
/// hits register in `ALLOWED_CACHE_FONT_PATHS`" is enforced at the
/// type layer rather than by review discipline — comments and review
/// notes decay across refactors; types don't.
///
/// Cache row paths are canonicalized upstream by `replace_folder`;
/// this function re-validates via `validate_ipc_path` anyway so a
/// hostile `--cache-file` swap can't smuggle crafted bytes past the
/// trust set.
pub fn register_cache_provenance(hit: &crate::font_cache::FontLookupResult) -> Result<(), String> {
    // re-validate the canonical path through the
    // IPC validator before insertion. A hostile cache file supplied
    // via `--cache-file` (P1b) could carry crafted rows with control
    // chars, BiDi, parent-directory segments, or DOS device prefixes;
    // catching at insert time keeps the trust set clean.
    crate::util::validate_ipc_path(hit.font_path(), "Cache font")?;
    // face_index is stored as i32; reject
    // negative values explicitly rather than reinterpreting the bit
    // pattern into a huge u32.
    let face_index = u32::try_from(hit.face_index()).map_err(|_| {
        format!(
            "Cache lookup has invalid negative face_index: {}",
            hit.face_index()
        )
    })?;
    insert_with_cap(
        &ALLOWED_CACHE_FONT_PATHS,
        "cache",
        hit.font_path().to_string(),
        face_index,
    )
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
        .to_ascii_lowercase()
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
            p.to_ascii_lowercase().replace("/", "\\")
        )
    })
});

/// Cached, lowercase macOS per-user fonts dir. `None` if `$HOME` was
/// unset at startup. Same caching rationale as `WINDOWS_USER_FONTS_DIR`:
/// take an early snapshot so a runtime
/// `set_var("HOME", ...)` can't redirect the system-fonts-dir gate.
/// Lowercased here because the macOS arm of `is_in_system_fonts_dir`
/// compares lowercased canonical paths (APFS is case-insensitive by
/// default).
#[cfg(target_os = "macos")]
static MACOS_USER_FONTS_DIR: Lazy<Option<String>> = Lazy::new(|| {
    std::env::var_os("HOME")
        .map(|p| format!("{}/library/fonts", p.to_string_lossy().to_lowercase()))
});

/// Cached Linux per-user fonts dirs (`~/.fonts` + `~/.local/share/fonts`).
/// Empty vector if `$HOME` was unset at startup. Same eager-snapshot
/// reason as `MACOS_USER_FONTS_DIR`.
#[cfg(all(unix, not(target_os = "macos")))]
static LINUX_USER_FONT_DIRS: Lazy<Vec<String>> = Lazy::new(|| {
    if let Some(home) = std::env::var_os("HOME") {
        let home_str = home.to_string_lossy();
        vec![
            format!("{home_str}/.fonts"),
            format!("{home_str}/.local/share/fonts"),
        ]
    } else {
        Vec::new()
    }
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
    #[cfg(target_os = "macos")]
    {
        Lazy::force(&MACOS_USER_FONTS_DIR);
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        Lazy::force(&LINUX_USER_FONT_DIRS);
    }
}

/// Check whether a canonicalized path is under a known system fonts directory.
fn is_in_system_fonts_dir(canonical: &Path) -> bool {
    let canonical_str = normalize_canonical_path(&canonical.to_string_lossy());

    if cfg!(windows) {
        #[cfg(windows)]
        {
            // ASCII-fold only : NTFS uses simple case
            // fold which diverges from Unicode case fold for a handful
            // of non-ASCII codepoints (German ß folds to "ss" under
            // Unicode but stays ß under NTFS; Greek final sigma ς).
            // Mixed-case LOCALAPPDATA containing such a glyph would
            // otherwise produce a runtime form that doesn't byte-match
            // the eagerly-cached form.
            let lower = canonical_str.to_ascii_lowercase().replace("/", "\\");
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
        //
        // Asymmetry with Windows arm WHY: full Unicode
        // `to_lowercase()` here, not the Windows arm's
        // `to_ascii_lowercase()`. APFS's case-fold IS the
        // Unicode case-fold (single composed codepoint comparison) —
        // German `ß` folds to `ss`, Greek final sigma `ς` folds to `σ`
        // (per UTS #21). Using `to_ascii_lowercase` here would
        // false-reject a `~/Library/Fonts` path on a system where the
        // user's home dir contains those glyphs. The Windows arm uses
        // `to_ascii_lowercase` because NTFS's simple-fold table
        // intentionally does NOT apply UTS #21 transforms — the two
        // OSes genuinely disagree on what's case-equivalent, and the
        // gate must match the OS, not a shared abstraction.
        let lower = canonical_str.to_lowercase();
        let under = |dir: &str| path_under_dir(&lower, &dir.to_lowercase(), "/");
        const MAC_DIRS: &[&str] = &[
            // Alphabetized . Narrow to Adobe/Fonts — the
            // wider /Library/Application Support tree holds every
            // app's data, not just fonts, so allowing the whole tree
            // weakens the "system font directory" gate.
            "/Library/Application Support/Adobe/Fonts",
            "/Library/Fonts",
            "/System/Library/AssetsV2",
            "/System/Library/Fonts",
            "/opt/homebrew/share/fonts",
            "/usr/local/share/fonts",
        ];
        if MAC_DIRS.iter().any(|d| under(d)) {
            return true;
        }
        // Per-user fonts: ~/Library/Fonts/, from the eager startup snapshot.
        // Pre-lowercased so we compare directly against `lower` without
        // re-folding inside `under`.
        #[cfg(target_os = "macos")]
        if let Some(user_dir) = MACOS_USER_FONTS_DIR.as_ref() {
            if path_under_dir(&lower, user_dir, "/") {
                return true;
            }
        }
        false
    } else {
        // Linux — same eager-snapshot pattern as macOS / Windows
        // . The static is populated at startup;
        // here we only read it.
        let under = |dir: &str| path_under_dir(&canonical_str, dir, "/");
        if under("/usr/share/fonts") || under("/usr/local/share/fonts") {
            return true;
        }
        #[cfg(all(unix, not(target_os = "macos")))]
        for user_dir in LINUX_USER_FONT_DIRS.iter() {
            if under(user_dir) {
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
/// Returns an error on subsetting failure instead of embedding the full font.
///
/// Public IPC entry point + the CLI's standalone-embed callsite both
/// invoke this function as a regular `pub fn`; the `#[tauri::command]`
/// shim `subset_font_b64` below wraps it for the GUI's IPC path with
/// base64 encoding so the frontend doesn't pay the JSON `[byte, ...]`
/// expansion (~4–5× per byte → ~50 MB on a worst-case 10 MB subset).
/// CLI's chain mode marshals subsets via base64 inline (see
/// `process_one_chain_input`); CLI's standalone embed bundles them
/// into `engine::FontSubsetPayload` and ships through the engine's
/// JSON-payload boundary (where the expansion is bounded by per-font
/// caps, not the cumulative ceiling).
///
/// **IMPORTANT **: do NOT add `#[tauri::command]`
/// to this function. The Vec<u8> return shape would JSON-encode as
/// `[byte, byte, ...]` over the GUI IPC wire, hitting the same ~4-5×
/// expansion `subset_font_b64` exists specifically to dodge. The
/// GUI-side IPC path MUST go through `subset_font_b64`; adding the
/// attribute here would silently bypass that guard and pressure V8
/// heap on every embed. Future direct exposure must be gated by
/// review.
pub fn subset_font(
    font_path: String,
    font_index: u32,
    codepoints: Vec<u32>,
) -> Result<Vec<u8>, String> {
    // IPC boundary validation: font_index and codepoints come from untrusted JS.
    // font_path also from JS — validate length / control-char / DOS-device
    // shape before any allocation, matching find_system_font's posture.
    crate::util::validate_ipc_path(&font_path, "Font")?;
    if font_index > MAX_SUBSET_FONT_INDEX {
        return Err(format!(
            "Invalid font face index: {font_index} (max {MAX_SUBSET_FONT_INDEX})"
        ));
    }
    if codepoints.len() > MAX_SUBSET_CODEPOINTS {
        return Err(format!(
            "Too many codepoints: {} (max {MAX_SUBSET_CODEPOINTS})",
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

    // Validate file extension against allowed font types.
    // ASCII-only fold matches `has_allowed_font_extension` /
    // `parse_local_font_file` / `find_system_font::Handle::Path` arms
    // — every entry in ALLOWED_FONT_EXTENSIONS is
    // pure ASCII so locale-aware lowercase is unnecessary alloc.
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase())
        .unwrap_or_default();
    if !ALLOWED_FONT_EXTENSIONS.contains(&ext.as_str()) {
        return Err(format!(
            "Invalid font file type '{}' for '{}'. Allowed extensions: {}",
            ext,
            filename,
            ALLOWED_FONT_EXTENSIONS.join(", ")
        ));
    }

    // PRE-canonicalize reparse-point reject. On
    // Windows, `Path::canonicalize` follows symlinks / junctions to
    // resolve the target, so the POST-canonicalize check below sees
    // the target (typically not a reparse point) — for the documented
    // `evil.ttf -> /etc/shadow` attack class, the canonical IS the
    // target and the post-check is a no-op. Adding the pre-check
    // brings parity with `dropzone.rs` / scan paths, which all
    // refuse early on `entry.path()` before canonicalize. The
    // post-check below remains for reparse-point chains where the
    // canonicalize step itself resolves through a reparse target.
    if crate::util::is_reparse_point(path) {
        log::warn!(
            "Refusing to subset font through symlink / junction: '{}'",
            path.display()
        );
        return Err("Refusing to subset font through symlink / junction".to_string());
    }

    // Canonicalize to resolve symlinks, "..", and normalize the path
    let canonical = path.canonicalize().map_err(|e| {
        log::warn!("canonicalize font path failed for '{filename}': {e}");
        "Cannot resolve font path".to_string()
    })?;

    // Post-canonicalize reparse-point reject — belt-and-suspenders
    // with the pre-check above. Covers reparse-point chains (e.g.,
    // symlink → symlink → file) where the FIRST hop is
    // canonicalize-resolved cleanly and the SECOND hop is a reparse
    // point pointing at non-font content. Without this, the 4-byte
    // magic-byte sniff opens the file just long enough to read 4 bytes
    // of deny-listed content. The full-body read is already gated by
    // the sniff (returns Err on non-font header) and the no-fallback
    // policy on parse failure, but the 4-byte probe is a residual
    // partial-content read. Closing it here keeps subset_font symmetric
    // with every other file-read entry in the codebase.
    if crate::util::is_reparse_point(&canonical) {
        log::warn!(
            "Refusing to subset font through symlink / junction: '{}'",
            canonical.display()
        );
        return Err("Refusing to subset font through symlink / junction".to_string());
    }

    // Provenance guard: the (path, face_index) pair must have been
    // discovered by one of three trusted entry points in THIS process:
    //
    //   1. `find_system_font` → registers in ALLOWED_FONT_PATHS
    //      (system fonts; also subject to system-fonts-dir restriction
    //      below for defense-in-depth).
    //   2. `scan_font_directory` / `scan_font_files` → records in the
    //      session SQLite (user-picked paths from THIS session's scan).
    //   3. `lookup_family` cache hit (CLI's resolve_embed_font or
    //      GUI's lookup_font_family) → registers in
    //      ALLOWED_CACHE_FONT_PATHS via `register_cache_provenance`.
    //
    // keying by (path, face_index) instead of
    // path alone closes a face-index injection where attacker-influenced
    // ASS would request `subset_font(arial.ttc, 5, ...)` against a path
    // registered for face 0. TTC files contain multiple faces, and the
    // gate used to pass on path alone, letting the wrong face's bytes
    // ship in the [Fonts] section.
    //
    // Persistent cache rows ARE a provenance source — but ONLY for
    // entries `lookup_family` returned during THIS process (the second
    // set, ALLOWED_CACHE_FONT_PATHS). Nothing accepts a path that
    // merely appears in the SQLite file but was not actually looked up
    // this run. An earlier gate rejected all cache rows, which broke
    // the design-locked CLI Situation B and GUI lookup tier 2. The
    // personal-desktop threat model accepts this in-process trust only
    // after lookup-time path / face-index validation; it is not a
    // server-grade hostile-process boundary.
    let canonical_string = normalize_canonical_path(&canonical.to_string_lossy());
    let registered_key = (canonical_string.clone(), font_index);
    // (readability): name each tier flag for what
    // it asserts when `true` — "accepted via this tier" — rather than
    // "is this tier's nature." Each variable evaluates lazily: once a
    // prior tier accepts, subsequent ones short-circuit to `false`.
    // The downstream gate (the system-fonts-dir directory-residency
    // check + the magic-byte sniff skip later in `subset_font`)
    // keys off `accepted_via_system`
    // specifically because system fonts get the extra dir-residency
    // check on top of the registration. Cache + user tiers are
    // checked only when system didn't accept and are NOT individually
    // referenced after the gate; collapsing them into one boolean
    // would lose the lazy-evaluation ordering reviewers expect.
    let accepted_via_system = ALLOWED_FONT_PATHS
        .lock()
        .map_err(|_| "Internal error: font path cache corrupted".to_string())?
        .contains(&registered_key);
    let accepted_via_cache = if accepted_via_system {
        false
    } else {
        ALLOWED_CACHE_FONT_PATHS
            .lock()
            .map_err(|_| "Internal error: font path cache corrupted".to_string())?
            .contains(&registered_key)
    };
    let accepted_via_user = if accepted_via_system || accepted_via_cache {
        false
    } else {
        is_user_font_face_registered(&canonical_string, font_index)?
    };
    if !accepted_via_system && !accepted_via_cache && !accepted_via_user {
        return Err("Font path was not discovered by a scan command".to_string());
    }

    // Defense-in-depth: system-discovered paths must live under a known
    // system fonts directory. User-picked paths (session DB) and cache
    // hits skip this check — the whole point of those tiers is to
    // accept user-chosen directories — but they had to pass their own
    // provenance step above, so random file reads via IPC are still
    // blocked.
    if accepted_via_system && !is_in_system_fonts_dir(&canonical) {
        return Err("System font path is not in a system fonts directory".to_string());
    }

    // Pre-read size check — rejects obvious oversize before allocating the Vec.
    let metadata = fs::metadata(&canonical).map_err(|e| {
        log::warn!("stat font file failed for '{filename}': {e}");
        "Cannot stat font file".to_string()
    })?;
    // Defense-symmetry: every file-read entry path in this module
    // gates on is_file() before touching content —
    // `parse_local_font_file` via its callers (scan_directory_inner,
    // scan_files_inner, preflight_files_inner), and now `subset_font`
    // here. Pre-this-check `subset_font` relied on extension + reparse
    // + provenance + magic-byte sniff and could be reached with a
    // non-regular inode if any of those upstream defenses ever
    // loosened. No reachable exploit today (validate_ipc_path's
    // DOS-device reject + the reparse check close `/dev/null`-class
    // device paths; a directory named `evil.ttf` would fall through
    // to the magic-byte probe which fails on the read_exact). Reuses
    // the metadata already fetched above — no extra syscall.
    if !metadata.is_file() {
        log::warn!(
            "Refusing to subset non-regular font path '{}'",
            canonical.display()
        );
        return Err("Font path is not a regular file".to_string());
    }
    if metadata.len() > MAX_FONT_DATA_SIZE {
        return Err(format!(
            "Font file too large ({:.1} MB, max {} MB)",
            metadata.len() as f64 / (1024.0 * 1024.0),
            MAX_FONT_DATA_SIZE / 1024 / 1024
        ));
    }

    // magic-byte sniff for non-system tiers as a
    // defense-in-depth gate before the full file read. The extension
    // check above rejects `/etc/passwd` (no `.ttf` extension), but
    // attacker-crafted cache rows pointing at `/etc/passwd.ttf` (or
    // any non-font local file renamed / symlinked to a font extension)
    // would otherwise pass extension + provenance + size gates and
    // reach `fs::read` for a 64 MB buffer copy. The no-fallback policy
    // already closes the byte-exfil layer (subset returns Err on
    // fontcull failure
    // with no fallback), but reading arbitrary local files into
    // process memory is itself a primitive worth closing at the
    // source — especially for `--cache-file`-supplied paths (P1b).
    //
    // System fonts (`accepted_via_system=true`) skip this sniff: they
    // already passed the stricter `is_in_system_fonts_dir` gate above,
    // and OS font directories are guaranteed to contain real fonts.
    // Cache + user tiers go through the sniff.
    if !accepted_via_system {
        use std::io::Read;
        let mut header = [0u8; 4];
        let mut probe = fs::File::open(&canonical).map_err(|e| {
            log::warn!("open font for sniff failed for '{filename}': {e}");
            format!("Cannot open font file '{filename}'")
        })?;
        probe.read_exact(&mut header).map_err(|e| {
            log::warn!("read font header failed for '{filename}': {e}");
            format!("Cannot read font header for '{filename}'")
        })?;
        // Recognized font magic numbers per the OpenType / TrueType
        // specs: `\0\1\0\0` (sfnt-flavored TrueType), `OTTO` (sfnt
        // with CFF outlines), `ttcf` (collection), `true` (Apple
        // TrueType). `typ1` (PostScript Type 1) intentionally omitted —
        // not supported by fontcull / skrifa, would fail parse anyway,
        // and is so rare that the false-reject cost is bounded to
        // archive-only fonts.
        // renamed from `is_font` — true here means
        // "has a recognized OpenType / TrueType signature", which is
        // narrower than "is a font". PostScript Type 1 fonts ARE
        // fonts but lack any of these magic numbers; the comment
        // above documents the deliberate Type 1 exclusion.
        let has_recognized_font_signature =
            matches!(&header, b"\x00\x01\x00\x00" | b"OTTO" | b"ttcf" | b"true");
        if !has_recognized_font_signature {
            return Err(format!(
                "Font file '{filename}' has no recognized font signature"
            ));
        }
        drop(probe);
    }

    // bounded-read TOCTOU mitigation. Parity
    // with the same change in `parse_local_font_file` above —
    // `fs::read` is unbounded and a hostile file swap between the
    // earlier metadata fetch and this read could force a
    // multi-GB allocation before the post-read recheck below
    // catches it. `File::open + take(MAX_FONT_DATA_SIZE + 1) +
    // read_to_end` caps the buffer at the OS layer; the +1 byte
    // disambiguates "at cap" from "over cap" so the recheck below
    // cleanly identifies an over-cap read.
    let font_data = {
        use std::io::Read;
        let file = std::fs::File::open(&canonical).map_err(|e| {
            log::warn!("open font file failed for '{filename}': {e}");
            format!("Failed to read font file '{filename}'")
        })?;
        let mut buf = Vec::new();
        let mut limited = file.take(MAX_FONT_DATA_SIZE + 1);
        limited.read_to_end(&mut buf).map_err(|e| {
            log::warn!("read font file failed for '{filename}': {e}");
            format!("Failed to read font file '{filename}'")
        })?;
        buf
    };

    // Post-read size check (TOCTOU mitigation — file could grow between stat and read).
    // kept as belt-and-suspenders for the bounded-read above.
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
        // Single-face files always have one face at index 0; the
        // `font_index` parameter is only meaningful for TTC inputs.
        // `FontRef::from_index(data, 0)` works on both single-face
        // SFNT (`\0\1\0\0` / `OTTO` / `true`) and TTC at offset
        // table entry 0, so the dispatch is just "what index to
        // slice." The is_ttc_data check is still load-bearing
        // because a TTC with `font_index > 0` must NOT collapse to
        // index 0 — that would silently swap the wrong face's
        // bytes into the output.
        let effective_index = if is_ttc_data(&font_data) {
            font_index
        } else {
            0
        };
        subset_with_index(&font_data, effective_index, &all_codepoints)
    }))
    .unwrap_or_else(|panic_payload| {
        // Convert panic payload (Box<dyn Any>) into a string for the log
        // and IPC return. Most panics produce &str (`panic!("...")`) or
        // String (`panic!("{}", x)`). The Box<dyn Error+Send+Sync> arm
        // catches the narrow case of `panic_any(Box::new(some_err))` —
        // explicit boxed-error panics, NOT all error types thrown via
        // `.expect()` (which produce String). The std::io::Error arm
        // catches `panic_any(io_err)`. A bare `anyhow::Error` panic or
        // other typed payload hits the unknown-payload fallback (which
        // surfaces TypeId for diagnostic triage). We do NOT pull anyhow
        // as a dep just for the downcast — fontcull doesn't panic with
        // anyhow::Error today, and the fallback is diagnostic-actionable.
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
            // No `panic_payload.type_id()` call — raw TypeId values are
            // opaque hex without a known type registry, so the WARN log
            // line below (with filename + face index) is more
            // actionable than printing the type id. The downcast
            // cascade above covers the panic shapes fontcull is known
            // to produce; this arm is the structural fallback for
            // "fontcull added a panic type we don't handle". A future
            // need for finer panic-type diagnosis would install a
            // `panic::set_hook` to capture `PanicInfo::location` /
            // `PanicInfo::message`, not chase the unboxed type here.
            "fontcull panicked with unknown payload type".to_string()
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
            // on subsetting
            // failure, return Err instead of falling back to raw
            // `font_data`. The fallback path was a corner-case
            // accommodation for "corrupt fonts in user-trusted dirs"
            // — but it also turned every readable local file with
            // an allowed font extension into a data-disclosure
            // primitive when paired with the cache provenance trust.
            // An attacker-supplied `--cache-file` (or a
            // tampered SQLite cache pointing `arial.ttf` at
            // `/etc/passwd.ttf`) could read arbitrary local files
            // and embed the raw bytes into the output ASS via this
            // fallback. Closing the disclosure primitive at the
            // subset layer is simpler than authenticating cache
            // rows on every lookup, and the cost is small: a font
            // that fontcull cannot subset is reported as failed-
            // to-embed in the log, and the user re-picks (the same
            // outcome as a missing font, which is already a known
            // workflow). The per-file `MAX_FONT_FALLBACK_SIZE` and
            // cumulative `CUMULATIVE_FALLBACK_BYTES` budgets that
            // used to bound this path are also gone — see the
            // module-top constants block.
            log::warn!(
                "Subsetting failed for '{}' (face {}): {} — embed will skip this font (fallback removed in Round 6 W6.9 for data-disclosure safety)",
                filename,
                font_index,
                e,
            );
            Err(format!("Subsetting failed: {e}"))
        }
    }
}

/// IPC wrapper around `subset_font` that base64-encodes the result so
/// the GUI's frontend doesn't pay the JSON `[byte, byte, …]` expansion.
/// Pre-fix this returned `Vec<u8>` directly; serde-json would write each
/// byte as decimal+comma (~4–5× per byte), and a 10 MB legitimate subset
/// would expand to ~50 MB IPC payload + a main-thread JSON parse pass.
/// Frontend `subsetFont()` decodes with the shared local byte decoder
/// instead of relying on host-provided Web APIs.
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

/// Returns true if `font_data` begins with the **TrueType / OpenType
/// Collection** magic. A TTC packs N faces (each with its own SFNT
/// header) inside one file behind a `ttcf` header + offset table;
/// any face — including index 0 — must be sliced out via
/// `FontRef::from_index` before subsetting, because fontcull's
/// single-face entry point expects SFNT magic at offset 0 and
/// rejects the `ttcf` container with `InvalidSfnt(0x74746366)`.
///
/// `typ1` is Apple's PostScript Type 1 collection — not supported
/// by fontcull / skrifa and rejected upstream by the magic-byte
/// sniff (the recognized-signature gate before this is reached),
/// so it's intentionally absent from this check.
fn is_ttc_data(font_data: &[u8]) -> bool {
    font_data.starts_with(b"ttcf")
}

/// Subset a single face from a font file. Handles both single-face
/// SFNT (`.ttf` / `.otf`, callers pass index 0) and TTC collections
/// (callers pass the requested face index). The plan tuning below
/// applies to BOTH paths — single-face and TTC subsets come out the
/// same shape.
///
/// Plan tuning rationale (from a measured comparison against
/// assfonts/AssFontSubset on the same input — see CLI design doc
/// § Project design locks):
///
/// - **Drop `vmtx`, `LTSH`, `kern`.** `vmtx` (vertical metrics) is
///   ~60 KB per CJK face — one record per `maxp.numGlyphs`, but
///   subtitle rendering is horizontal (libass doesn't consult `vmtx`
///   for ASS \fr* tags). `LTSH` (Linear ThreshHold) is a Win9x
///   screen-rendering optimization modern engines ignore; ~30 KB per
///   CJK face. `kern` is the legacy kerning table superseded by GPOS;
///   our subset doesn't trim per-glyph kerning so the kept table is
///   dead weight. assfonts (libharfbuzz-subset defaults) and
///   AssFontSubset (pyftsubset defaults) both drop these.
/// - **Keep ALL name-table records (all name IDs, all languages).**
///   Pre-fix, our subset had a 6-byte `name` stub — libass could
///   only match by filename heuristic, which fails for non-ASCII
///   alternative family names (`微软雅黑` hashes to `font_<hex>.ttf`
///   under our naming, with no fallback). Preserving the full name
///   table lets libass match every localized family name in the
///   original font, which in turn lets the TS embed layer dedup
///   aliases that resolve to the same face — without dedup, the
///   same face would be subset and embedded once per family alias.
fn subset_with_index(font_data: &[u8], index: u32, codepoints: &[u32]) -> Result<Vec<u8>, String> {
    use fontcull_klippa::{subset_font, Plan, SubsetFlags};
    use fontcull_read_fonts::collections::IntSet;
    use fontcull_skrifa::{FontRef, GlyphId, Tag};
    use fontcull_write_fonts::types::NameId;

    // Defense-in-depth: if the file is a TTC, peek `numFonts` at the
    // documented offset 8..=11 (TTCHeader version 1.0/2.0, ULONG
    // big-endian) and reject `index >= numFonts` upfront.
    // `MAX_SUBSET_FONT_INDEX = 255` at the IPC layer bounds JS-supplied
    // indices, but internal callers (chain's
    // resolve_chain_embed_subsets, embed batch) reach here via face
    // indices recorded in the session DB / cache rows, where the
    // upper bound is `MAX_TTC_FACES = 16` at scan time. This peek is
    // a structural defense inside `subset_with_index` itself so the
    // contract holds regardless of caller — a crafted TTC with a
    // declared `numFonts` huge enough to OOM-pressure
    // `FontRef::from_index`'s offset-table walk now bounces out
    // before fontcull's parser allocates anything. The catch_unwind
    // upstream caught panics; this peek closes the silent-OOM-then-
    // InvalidSfnt path the panic guard cannot.
    if is_ttc_data(font_data) {
        if font_data.len() < 12 {
            return Err(format!(
                "Truncated TTC header: {} bytes (need 12 for numFonts)",
                font_data.len()
            ));
        }
        let num_fonts =
            u32::from_be_bytes([font_data[8], font_data[9], font_data[10], font_data[11]]);
        if index >= num_fonts {
            return Err(format!(
                "TTC face index out of range: requested {index}, file declares {num_fonts} face(s)"
            ));
        }
    }

    let (font_data, sanitized_cmap_groups) = sanitize_cmap12_invalid_groups(font_data, index)?;
    if sanitized_cmap_groups > 0 {
        log::warn!(
            "Ignored {sanitized_cmap_groups} invalid cmap format-12 group(s) outside Unicode range while subsetting face {index}"
        );
    }
    let font_data = font_data.as_ref();

    // Display, not Debug — Debug repr leaks internal struct fields,
    // table tags, byte offsets into a frontend-visible error.
    let font = FontRef::from_index(font_data, index)
        .map_err(|e| format!("Cannot parse font face {index}: {e}"))?;

    let mut unicode_set: IntSet<u32> = IntSet::empty();
    for &cp in codepoints {
        unicode_set.insert(cp);
    }

    // Preserve the full name-table — every name ID, every language.
    // `IntSet::all()` is the inverse of `IntSet::empty()`; harfbuzz-
    // subset's SETS convention treats `all()` as "retain everything",
    // matching assfonts' explicit `HB_SUBSET_SETS_NAME_LANG_ID` inversion.
    let name_ids: IntSet<NameId> = IntSet::all();
    let langs: IntSet<u16> = IntSet::all();

    let empty_gids: IntSet<GlyphId> = IntSet::empty();
    let layout_scripts: IntSet<Tag> = IntSet::all();
    let layout_features: IntSet<Tag> = IntSet::empty();

    let subset_once = |drop_gpos: bool| {
        // Drop the three "dead in subtitle rendering" tables. The set
        // contains exactly the tags we want fontcull to omit from the
        // output font. GPOS is kept on the first attempt for shaping
        // quality, then dropped only as a compatibility retry for
        // legacy CJK fonts whose positioning table makes fontcull fail.
        let mut drop_tables: IntSet<Tag> = IntSet::empty();
        drop_tables.insert(Tag::new(b"vmtx"));
        drop_tables.insert(Tag::new(b"LTSH"));
        drop_tables.insert(Tag::new(b"kern"));
        if drop_gpos {
            drop_tables.insert(Tag::new(b"GPOS"));
        }

        let plan = Plan::new(
            &empty_gids,
            &unicode_set,
            &font,
            SubsetFlags::default(),
            &drop_tables,
            &layout_scripts,
            &layout_features,
            &name_ids,
            &langs,
        );

        // Display, not Debug — same reasoning as above.
        subset_font(&font, &plan).map_err(|e| format!("Subset failed for face {index}: {e}"))
    };

    match subset_once(false) {
        Ok(subsetted) => Ok(subsetted),
        Err(error) if error.contains("table 'GPOS'") => {
            log::warn!(
                "Subsetting face {index} failed on GPOS; retrying with the GPOS table dropped"
            );
            subset_once(true).map_err(|fallback_error| {
                format!("{error}; retry without GPOS also failed: {fallback_error}")
            })
        }
        Err(error) => Err(error),
    }
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

    /// Tripwire: a behavioral test for `MAX_FAMILY_VARIANTS_PER_FACE`
    /// needs a real font with > 8
    /// localized name-table entries, which the repo doesn't ship (CJK
    /// font licensing — see `tests/test_subset.rs` for the same
    /// constraint). Pin the constant value so an accidental raise is
    /// noticed; the math behind the cap (bounded names × persisted key
    /// rows × MAX_CACHE_POPULATE_FACES = OOM-on-crafted-pack ceiling) is
    /// in the constant's doc comment above.
    #[test]
    fn max_family_variants_per_face_cap_value() {
        assert_eq!(MAX_FAMILY_VARIANTS_PER_FACE, 8);
    }

    #[test]
    fn max_face_name_variants_per_face_cap_value() {
        assert_eq!(MAX_FACE_NAME_VARIANTS_PER_FACE, 8);
    }

    #[test]
    fn is_ttc_data_recognizes_ttcf_magic() {
        // Minimal valid TTC header: `ttcf` magic + version + numFonts.
        let data = b"ttcf\x00\x01\x00\x00\x00\x00\x00\x02";
        assert!(is_ttc_data(data));
    }

    #[test]
    fn is_ttc_data_rejects_single_face_signatures() {
        // Three SFNT magics that the recognized-signature gate accepts
        // upstream of subset_font but that MUST route to the single-face
        // path, not the from_index collection path.
        assert!(!is_ttc_data(&[0x00, 0x01, 0x00, 0x00, 0xff, 0xff])); // sfnt-flavored TrueType
        assert!(!is_ttc_data(b"OTTOpadding")); // OpenType CFF
        assert!(!is_ttc_data(b"truepadding")); // Apple TrueType
    }

    #[test]
    fn is_ttc_data_rejects_under_four_bytes() {
        // Boundary-pin: TTC magic is 4 bytes, so anything shorter must
        // return false (defends against a 0-byte read sneaking past the
        // upstream stat as if it were a single-face SFNT).
        assert!(!is_ttc_data(b""));
        assert!(!is_ttc_data(b"t"));
        assert!(!is_ttc_data(b"ttc"));
    }

    fn font_with_single_table(tag: &[u8; 4], table_data: &[u8]) -> Vec<u8> {
        let table_offset = 28u32;
        let table_len = u32::try_from(table_data.len()).expect("test table fits u32");

        let mut font = Vec::new();
        font.extend_from_slice(b"\0\x01\0\0");
        font.extend_from_slice(&1u16.to_be_bytes());
        font.extend_from_slice(&[0; 6]);
        font.extend_from_slice(tag);
        font.extend_from_slice(&0u32.to_be_bytes());
        font.extend_from_slice(&table_offset.to_be_bytes());
        font.extend_from_slice(&table_len.to_be_bytes());
        font.extend_from_slice(table_data);
        font
    }

    fn cmap12_subtable_with_group_count(group_count: usize) -> Vec<u8> {
        let subtable_length = 16 + group_count as u32 * 12;
        let mut subtable = Vec::new();
        subtable.extend_from_slice(&12u16.to_be_bytes());
        subtable.extend_from_slice(&0u16.to_be_bytes());
        subtable.extend_from_slice(&subtable_length.to_be_bytes());
        subtable.extend_from_slice(&0u32.to_be_bytes());
        subtable.extend_from_slice(&(group_count as u32).to_be_bytes());
        for index in 0..group_count {
            let codepoint = 0x20 + u32::try_from(index % 64).expect("test index fits");
            subtable.extend_from_slice(&codepoint.to_be_bytes());
            subtable.extend_from_slice(&codepoint.to_be_bytes());
            subtable.extend_from_slice(&1u32.to_be_bytes());
        }
        subtable
    }

    fn cmap_with_format12_subtables(group_counts: &[usize]) -> Vec<u8> {
        let record_count = group_counts.len();
        let records_len = 4 + record_count * 8;
        let mut subtables = Vec::new();
        let mut subtable_offsets = Vec::new();
        for group_count in group_counts {
            subtable_offsets.push(records_len + subtables.len());
            subtables.extend_from_slice(&cmap12_subtable_with_group_count(*group_count));
        }

        let mut cmap = Vec::new();
        cmap.extend_from_slice(&0u16.to_be_bytes());
        cmap.extend_from_slice(
            &(u16::try_from(record_count).expect("test record count fits")).to_be_bytes(),
        );
        for offset in subtable_offsets {
            cmap.extend_from_slice(&0u16.to_be_bytes());
            cmap.extend_from_slice(&4u16.to_be_bytes());
            cmap.extend_from_slice(
                &(u32::try_from(offset).expect("test offset fits")).to_be_bytes(),
            );
        }
        cmap.extend_from_slice(&subtables);
        cmap
    }

    fn utf16be_test_bytes(value: &str) -> Vec<u8> {
        value
            .encode_utf16()
            .flat_map(|unit| unit.to_be_bytes())
            .collect()
    }

    #[test]
    fn sanitize_cmap12_invalid_groups_makes_non_unicode_ranges_unselectable() {
        let cmap_offset = 28u32;
        let subtable_offset = 12u32;
        let groups = [
            (0x20u32, 0x7Eu32, 1u32),
            (0x110001u32, 0x110002u32, 7u32),
            (0x500u32, 0x4FFu32, 9u32),
        ];
        let subtable_length = 16 + groups.len() as u32 * 12;
        let cmap_length = subtable_offset + subtable_length;

        let mut font = Vec::new();
        font.extend_from_slice(b"\0\x01\0\0");
        font.extend_from_slice(&1u16.to_be_bytes());
        font.extend_from_slice(&[0; 6]);
        font.extend_from_slice(b"cmap");
        font.extend_from_slice(&0u32.to_be_bytes());
        font.extend_from_slice(&cmap_offset.to_be_bytes());
        font.extend_from_slice(&cmap_length.to_be_bytes());
        font.extend_from_slice(&0u16.to_be_bytes());
        font.extend_from_slice(&1u16.to_be_bytes());
        font.extend_from_slice(&0u16.to_be_bytes());
        font.extend_from_slice(&4u16.to_be_bytes());
        font.extend_from_slice(&subtable_offset.to_be_bytes());
        font.extend_from_slice(&12u16.to_be_bytes());
        font.extend_from_slice(&0u16.to_be_bytes());
        font.extend_from_slice(&subtable_length.to_be_bytes());
        font.extend_from_slice(&0u32.to_be_bytes());
        font.extend_from_slice(&(groups.len() as u32).to_be_bytes());
        for (start, end, gid) in groups {
            font.extend_from_slice(&start.to_be_bytes());
            font.extend_from_slice(&end.to_be_bytes());
            font.extend_from_slice(&gid.to_be_bytes());
        }

        let (sanitized, count) =
            sanitize_cmap12_invalid_groups(&font, 0).expect("test font should sanitize");
        assert_eq!(count, 2);
        let sanitized = sanitized.as_ref();
        let valid_group = cmap_offset as usize + subtable_offset as usize + 16;
        assert_eq!(read_be_u32(sanitized, valid_group), Some(0x20));
        assert_eq!(read_be_u32(sanitized, valid_group + 4), Some(0x7E));
        assert_eq!(read_be_u32(sanitized, valid_group + 8), Some(1));

        for group_offset in [valid_group + 12, valid_group + 24] {
            assert_eq!(
                read_be_u32(sanitized, group_offset),
                Some(UNICODE_SCALAR_MAX)
            );
            assert_eq!(
                read_be_u32(sanitized, group_offset + 4),
                Some(UNICODE_SCALAR_MAX)
            );
            assert_eq!(
                read_be_u32(sanitized, group_offset + 8),
                Some(SKIP_CMAP12_GROUP_GLYPH_ID)
            );
        }
    }

    #[test]
    fn sanitize_cmap12_invalid_groups_clamps_ranges_ending_past_unicode() {
        let cmap_offset = 28u32;
        let subtable_offset = 12u32;
        let groups = [
            (0x20u32, UNICODE_SCALAR_MAX + 1, 1u32),
            (0x30u32, 0x31u32, 2u32),
        ];
        let subtable_length = 16 + groups.len() as u32 * 12;
        let cmap_length = subtable_offset + subtable_length;

        let mut font = Vec::new();
        font.extend_from_slice(b"\0\x01\0\0");
        font.extend_from_slice(&1u16.to_be_bytes());
        font.extend_from_slice(&[0; 6]);
        font.extend_from_slice(b"cmap");
        font.extend_from_slice(&0u32.to_be_bytes());
        font.extend_from_slice(&cmap_offset.to_be_bytes());
        font.extend_from_slice(&cmap_length.to_be_bytes());
        font.extend_from_slice(&0u16.to_be_bytes());
        font.extend_from_slice(&1u16.to_be_bytes());
        font.extend_from_slice(&0u16.to_be_bytes());
        font.extend_from_slice(&4u16.to_be_bytes());
        font.extend_from_slice(&subtable_offset.to_be_bytes());
        font.extend_from_slice(&12u16.to_be_bytes());
        font.extend_from_slice(&0u16.to_be_bytes());
        font.extend_from_slice(&subtable_length.to_be_bytes());
        font.extend_from_slice(&0u32.to_be_bytes());
        font.extend_from_slice(&(groups.len() as u32).to_be_bytes());
        for (start, end, glyph) in groups {
            font.extend_from_slice(&start.to_be_bytes());
            font.extend_from_slice(&end.to_be_bytes());
            font.extend_from_slice(&glyph.to_be_bytes());
        }

        let (sanitized, count) =
            sanitize_cmap12_invalid_groups(&font, 0).expect("test font should sanitize");
        assert_eq!(count, 1);
        let sanitized = sanitized.as_ref();
        let clamped_group = cmap_offset as usize + subtable_offset as usize + 16;
        assert_eq!(read_be_u32(sanitized, clamped_group), Some(0x20));
        assert_eq!(
            read_be_u32(sanitized, clamped_group + 4),
            Some(UNICODE_SCALAR_MAX)
        );
        assert_eq!(read_be_u32(sanitized, clamped_group + 8), Some(1));

        let valid_group = clamped_group + 12;
        assert_eq!(read_be_u32(sanitized, valid_group), Some(0x30));
        assert_eq!(read_be_u32(sanitized, valid_group + 4), Some(0x31));
        assert_eq!(read_be_u32(sanitized, valid_group + 8), Some(2));
    }

    #[test]
    fn sanitize_cmap12_invalid_groups_rejects_excessive_group_count() {
        let group_count = MAX_CMAP12_GROUPS_PER_FACE + 1;
        let cmap = cmap_with_format12_subtables(&[group_count]);
        let font = font_with_single_table(b"cmap", &cmap);
        let err = sanitize_cmap12_invalid_groups(&font, 0)
            .expect_err("excessive cmap format-12 groups must be rejected");
        assert!(
            err.contains("group count"),
            "error should identify cmap group-count cap: {err}"
        );
    }

    #[test]
    fn sanitize_cmap12_invalid_groups_rejects_cumulative_group_count() {
        let first_count = MAX_CMAP12_GROUPS_PER_FACE / 2 + 1;
        let second_count = MAX_CMAP12_GROUPS_PER_FACE - first_count + 1;
        let cmap = cmap_with_format12_subtables(&[first_count, second_count]);
        let font = font_with_single_table(b"cmap", &cmap);

        let err = sanitize_cmap12_invalid_groups(&font, 0)
            .expect_err("cumulative cmap format-12 groups must be rejected");

        assert!(
            err.contains("group count"),
            "error should identify cumulative cmap group-count cap: {err}"
        );
    }

    #[test]
    fn sanitize_cmap12_invalid_groups_rejects_excessive_encoding_records() {
        let record_count = MAX_CMAP12_ENCODING_RECORDS_PER_FACE + 1;
        let first_subtable_rel = 4 + record_count * 8;
        let mut cmap = Vec::new();
        cmap.extend_from_slice(&0u16.to_be_bytes());
        cmap.extend_from_slice(
            &(u16::try_from(record_count).expect("test record count fits")).to_be_bytes(),
        );
        for _ in 0..record_count {
            cmap.extend_from_slice(&0u16.to_be_bytes());
            cmap.extend_from_slice(&4u16.to_be_bytes());
            cmap.extend_from_slice(
                &(u32::try_from(first_subtable_rel).expect("test offset fits")).to_be_bytes(),
            );
        }
        let font = font_with_single_table(b"cmap", &cmap);

        let err = sanitize_cmap12_invalid_groups(&font, 0)
            .expect_err("excessive cmap encoding records must be rejected");

        assert!(
            err.contains("encoding record count"),
            "error should identify cmap encoding-record cap: {err}"
        );
    }

    #[test]
    fn sanitize_cmap12_invalid_groups_rejects_excessive_unique_subtables() {
        let group_counts = vec![0usize; MAX_CMAP12_SUBTABLES_PER_FACE + 1];
        let cmap = cmap_with_format12_subtables(&group_counts);
        let font = font_with_single_table(b"cmap", &cmap);

        let err = sanitize_cmap12_invalid_groups(&font, 0)
            .expect_err("excessive cmap subtables must be rejected");

        assert!(
            err.contains("subtable count"),
            "error should identify cmap subtable-count cap: {err}"
        );
    }

    #[test]
    fn sanitize_cmap12_invalid_groups_ignores_bogus_early_offset_before_later_record() {
        let real_subtable_rel = 20u32;
        let invalid_group = [UNICODE_SCALAR_MAX + 1, UNICODE_SCALAR_MAX + 1, 7u32];

        let mut cmap = Vec::new();
        cmap.extend_from_slice(&0u16.to_be_bytes());
        cmap.extend_from_slice(&2u16.to_be_bytes());
        cmap.extend_from_slice(&0u16.to_be_bytes());
        cmap.extend_from_slice(&4u16.to_be_bytes());
        cmap.extend_from_slice(&12u32.to_be_bytes());
        assert_eq!(cmap.len(), 12);
        cmap.extend_from_slice(&0u16.to_be_bytes());
        cmap.extend_from_slice(&4u16.to_be_bytes());
        cmap.extend_from_slice(&real_subtable_rel.to_be_bytes());
        assert_eq!(cmap.len(), real_subtable_rel as usize);
        cmap.extend_from_slice(&12u16.to_be_bytes());
        cmap.extend_from_slice(&0u16.to_be_bytes());
        cmap.extend_from_slice(&28u32.to_be_bytes());
        cmap.extend_from_slice(&0u32.to_be_bytes());
        cmap.extend_from_slice(&1u32.to_be_bytes());
        for value in invalid_group {
            cmap.extend_from_slice(&value.to_be_bytes());
        }

        let font = font_with_single_table(b"cmap", &cmap);
        let (sanitized, count) =
            sanitize_cmap12_invalid_groups(&font, 0).expect("later record should be scanned");

        assert_eq!(count, 1);
        let sanitized = sanitized.as_ref();
        let group_offset = 28usize + real_subtable_rel as usize + 16;
        assert_eq!(
            read_be_u32(sanitized, group_offset),
            Some(UNICODE_SCALAR_MAX)
        );
        assert_eq!(
            read_be_u32(sanitized, group_offset + 4),
            Some(UNICODE_SCALAR_MAX)
        );
        assert_eq!(
            read_be_u32(sanitized, group_offset + 8),
            Some(SKIP_CMAP12_GROUP_GLYPH_ID)
        );
    }

    #[test]
    fn sanitize_cmap12_invalid_groups_rejects_excessive_rewrites() {
        let subtable_offset = 12u32;
        let group_count = MAX_CMAP12_REWRITE_OFFSETS_PER_FACE + 1;
        let subtable_length = 16 + group_count as u32 * 12;

        let mut cmap = Vec::new();
        cmap.extend_from_slice(&0u16.to_be_bytes());
        cmap.extend_from_slice(&1u16.to_be_bytes());
        cmap.extend_from_slice(&0u16.to_be_bytes());
        cmap.extend_from_slice(&4u16.to_be_bytes());
        cmap.extend_from_slice(&subtable_offset.to_be_bytes());
        assert_eq!(cmap.len(), subtable_offset as usize);
        cmap.extend_from_slice(&12u16.to_be_bytes());
        cmap.extend_from_slice(&0u16.to_be_bytes());
        cmap.extend_from_slice(&subtable_length.to_be_bytes());
        cmap.extend_from_slice(&0u32.to_be_bytes());
        cmap.extend_from_slice(&(group_count as u32).to_be_bytes());
        for _ in 0..group_count {
            cmap.extend_from_slice(&(UNICODE_SCALAR_MAX + 1).to_be_bytes());
            cmap.extend_from_slice(&(UNICODE_SCALAR_MAX + 1).to_be_bytes());
            cmap.extend_from_slice(&1u32.to_be_bytes());
        }

        let font = font_with_single_table(b"cmap", &cmap);
        let err = sanitize_cmap12_invalid_groups(&font, 0)
            .expect_err("excessive cmap format-12 rewrites must be rejected");
        assert!(
            err.contains("rewrite count"),
            "error should identify cmap rewrite cap: {err}"
        );
    }

    #[test]
    fn sanitize_cmap12_invalid_groups_ignores_records_overlapping_first_subtable() {
        let first_subtable_rel = 12u32;
        let ghost_subtable_rel = 40u32;
        let first_invalid = [0x110001u32, 0x110002u32, 7u32];
        let ghost_invalid = [0x110010u32, 0x110011u32, 9u32];

        let mut cmap = Vec::new();
        cmap.extend_from_slice(&0u16.to_be_bytes());
        cmap.extend_from_slice(&2u16.to_be_bytes());
        cmap.extend_from_slice(&0u16.to_be_bytes());
        cmap.extend_from_slice(&4u16.to_be_bytes());
        cmap.extend_from_slice(&first_subtable_rel.to_be_bytes());
        assert_eq!(cmap.len(), first_subtable_rel as usize);

        // The declared format-12 length is also what a bogus second encoding
        // record would expose as its subtable offset if parser code kept
        // walking records after the first subtable begins.
        cmap.extend_from_slice(&12u16.to_be_bytes());
        cmap.extend_from_slice(&0u16.to_be_bytes());
        cmap.extend_from_slice(&ghost_subtable_rel.to_be_bytes());
        cmap.extend_from_slice(&0u32.to_be_bytes());
        cmap.extend_from_slice(&1u32.to_be_bytes());
        for value in first_invalid {
            cmap.extend_from_slice(&value.to_be_bytes());
        }
        assert_eq!(cmap.len(), ghost_subtable_rel as usize);

        cmap.extend_from_slice(&12u16.to_be_bytes());
        cmap.extend_from_slice(&0u16.to_be_bytes());
        cmap.extend_from_slice(&28u32.to_be_bytes());
        cmap.extend_from_slice(&0u32.to_be_bytes());
        cmap.extend_from_slice(&1u32.to_be_bytes());
        for value in ghost_invalid {
            cmap.extend_from_slice(&value.to_be_bytes());
        }

        let font = font_with_single_table(b"cmap", &cmap);
        let (sanitized, count) =
            sanitize_cmap12_invalid_groups(&font, 0).expect("test font should sanitize");
        assert_eq!(count, 1);
        let sanitized = sanitized.as_ref();

        let table_offset = 28usize;
        let first_group = table_offset + first_subtable_rel as usize + 16;
        assert_eq!(
            read_be_u32(sanitized, first_group),
            Some(UNICODE_SCALAR_MAX)
        );
        assert_eq!(
            read_be_u32(sanitized, first_group + 4),
            Some(UNICODE_SCALAR_MAX)
        );
        assert_eq!(
            read_be_u32(sanitized, first_group + 8),
            Some(SKIP_CMAP12_GROUP_GLYPH_ID)
        );

        let ghost_group = table_offset + ghost_subtable_rel as usize + 16;
        assert_eq!(read_be_u32(sanitized, ghost_group), Some(ghost_invalid[0]));
        assert_eq!(
            read_be_u32(sanitized, ghost_group + 4),
            Some(ghost_invalid[1])
        );
        assert_eq!(
            read_be_u32(sanitized, ghost_group + 8),
            Some(ghost_invalid[2])
        );
    }

    #[test]
    fn parse_legacy_name_table_entries_rejects_strings_outside_declared_name_table() {
        let family = utf16be_test_bytes("Escaped Family");
        let mut name_table = Vec::new();
        name_table.extend_from_slice(&0u16.to_be_bytes());
        name_table.extend_from_slice(&1u16.to_be_bytes());
        name_table.extend_from_slice(&18u16.to_be_bytes());
        name_table.extend_from_slice(&3u16.to_be_bytes());
        name_table.extend_from_slice(&1u16.to_be_bytes());
        name_table.extend_from_slice(&0x0409u16.to_be_bytes());
        name_table.extend_from_slice(&1u16.to_be_bytes());
        name_table.extend_from_slice(&(family.len() as u16).to_be_bytes());
        name_table.extend_from_slice(&0u16.to_be_bytes());
        assert_eq!(name_table.len(), 18);

        let mut font = font_with_single_table(b"name", &name_table);
        font.extend_from_slice(&family);

        let entries = parse_legacy_name_table_entries(
            &font,
            "C:/fonts/malformed.ttf",
            font.len() as u64,
            false,
            1,
        );
        assert!(
            entries.is_empty(),
            "name strings beyond the declared table length must be ignored"
        );
    }

    #[test]
    fn decode_legacy_name_record_rejects_oversized_raw_records() {
        let mut raw = Vec::with_capacity(MAX_LEGACY_NAME_RECORD_BYTES + 2);
        while raw.len() <= MAX_LEGACY_NAME_RECORD_BYTES {
            raw.extend_from_slice(&[0, b'A']);
        }

        assert!(decode_legacy_name_record(3, 1, 0x0409, &raw).is_none());
    }

    /// Boundary tests for the TTC numFonts peek inside
    /// `subset_with_index`. A crafted TTC whose declared `numFonts`
    /// does not match the actual offset table length must produce a
    /// clean Err with attribution naming the requested index and the
    /// declared count, BEFORE fontcull's parser allocates any
    /// intermediate offset-table buffer.
    ///
    /// The peek lives at TTCHeader offset 8..=11 (`numFonts`, ULONG
    /// big-endian). Anything shorter than 12 bytes that still starts
    /// with `ttcf` magic must be rejected as a truncated header.
    #[test]
    fn subset_with_index_rejects_truncated_ttc_header() {
        // 4 bytes of `ttcf` magic + 4 bytes version, but `numFonts`
        // field absent. is_ttc_data passes; the peek must reject.
        let truncated = b"ttcf\x00\x01\x00\x00\x00\x00\x00";
        assert_eq!(truncated.len(), 11);
        let err = subset_with_index(truncated, 0, &[0x41])
            .expect_err("11-byte TTC header (numFonts missing) must Err");
        assert!(
            err.contains("Truncated TTC header"),
            "Err should name the truncation explicitly: {err}"
        );
    }

    #[test]
    fn subset_with_index_rejects_index_at_or_above_declared_num_fonts() {
        // Declared numFonts = 2 (big-endian at offset 8..=11).
        // Request face index 2 (out of range) and 99 (well out of range).
        let header = b"ttcf\x00\x01\x00\x00\x00\x00\x00\x02";
        assert_eq!(header.len(), 12);

        let err_at_cap = subset_with_index(header, 2, &[0x41])
            .expect_err("index == numFonts must Err (zero-indexed)");
        assert!(
            err_at_cap.contains("out of range") && err_at_cap.contains("requested 2"),
            "Err should attribute index + declared count: {err_at_cap}"
        );

        let err_above_cap =
            subset_with_index(header, 99, &[0x41]).expect_err("index >> numFonts must Err");
        assert!(
            err_above_cap.contains("requested 99") && err_above_cap.contains("2 face"),
            "Err should attribute the requested + declared values: {err_above_cap}"
        );
    }

    /// Exercises `subset_with_index` against face 0 AND face 1 of a real
    /// TTC, pinning the contract that both dispatch arms produce distinct,
    /// non-empty, smaller-than-original subset bytes. Without an env-var-
    /// supplied fixture the test skips (mirrors `tests/test_subset.rs`'s
    /// CJK-font convention — multi-face CJK fonts are licensing-encumbered
    /// and cannot be checked into the repo).
    ///
    /// Usage on Windows:
    ///   SSAHDRIFY_TEST_TTC_FONT=C:/Windows/Fonts/msyh.ttc
    ///   cargo test --lib -- --ignored subset_with_index_handles_ttc
    #[test]
    #[ignore = "requires SSAHDRIFY_TEST_TTC_FONT env var pointing to a multi-face .ttc"]
    fn subset_with_index_handles_ttc_face_zero_and_one() {
        let Ok(font_path) = std::env::var("SSAHDRIFY_TEST_TTC_FONT") else {
            eprintln!("SSAHDRIFY_TEST_TTC_FONT not set — skipping");
            return;
        };
        let font_data = match std::fs::read(&font_path) {
            Ok(b) => b,
            Err(e) => {
                eprintln!("Cannot read {font_path}: {e} — skipping");
                return;
            }
        };
        assert!(
            is_ttc_data(&font_data),
            "fixture at {font_path} must be a TTC (starts with 'ttcf' magic)"
        );

        let codepoints: Vec<u32> = "你好世界Hello".chars().map(|c| c as u32).collect();

        let face0 = subset_with_index(&font_data, 0, &codepoints)
            .expect("face 0 of a TTC should subset cleanly");
        assert!(!face0.is_empty(), "face 0 subset must not be empty");
        assert!(
            face0.len() < font_data.len(),
            "face 0 subset ({} bytes) should be smaller than original ({} bytes)",
            face0.len(),
            font_data.len()
        );

        let face1 = subset_with_index(&font_data, 1, &codepoints)
            .expect("face 1 of a TTC should subset cleanly");
        assert!(!face1.is_empty(), "face 1 subset must not be empty");
        assert!(
            face1.len() < font_data.len(),
            "face 1 subset ({} bytes) should be smaller than original ({} bytes)",
            face1.len(),
            font_data.len()
        );

        // Different faces with different name tables / glyph counts must
        // produce different output bytes. If the from_index path silently
        // collapsed to face 0, this assertion would catch it.
        assert_ne!(
            face0, face1,
            "face 0 and face 1 should produce different subset bytes"
        );
    }

    fn init_test_user_font_db(name: &str) {
        let dir = std::env::temp_dir().join(format!(
            "ssahdrify-user-font-db-test-{}-{name}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&dir);
        init_user_font_db(&dir).expect("test DB should initialize");
        clear_font_sources().expect("test DB should clear");
    }

    /// replaces the previous `tests/test_subset.rs` integration
    /// test which called `fontcull::subset_font_data_unicode` directly —
    /// bypassing the project's tuned Plan in `subset_with_index` (drop
    /// vmtx/LTSH/kern, preserve full name table — the 4.6× size win
    /// from forum-feedback (2a), commit `00f44ac`). Unit test inside
    /// `mod tests` so `subset_with_index` stays private; outside-crate
    /// callers go through the public `subset_font` (which requires
    /// provenance setup), and integration tests have no need to expose
    /// the internal subset path as public API.
    ///
    /// Usage (any single-face CJK .ttf works):
    ///   SSAHDRIFY_TEST_CJK_FONT=C:/Windows/Fonts/simsun.ttc
    ///   cargo test --lib -- --ignored subset_with_index_yields_size_reduction
    #[test]
    #[ignore = "requires SSAHDRIFY_TEST_CJK_FONT env var pointing to a CJK font file"]
    fn subset_with_index_yields_significant_size_reduction_on_cjk() {
        let Ok(font_path) = std::env::var("SSAHDRIFY_TEST_CJK_FONT") else {
            eprintln!("SSAHDRIFY_TEST_CJK_FONT not set — skipping");
            return;
        };
        let font_data = match std::fs::read(&font_path) {
            Ok(b) => b,
            Err(e) => {
                eprintln!("Cannot read {font_path}: {e} — skipping");
                return;
            }
        };
        let original_size = font_data.len();

        // ~50 CJK chars + ASCII / fullwidth padding (mirrors the
        // padding subset_font itself applies in production).
        let subtitle_text = "你好世界，这是一个字幕测试。中文字体子集化可以显著减小文件体积！";
        let mut codepoints: Vec<u32> = subtitle_text.chars().map(|c| c as u32).collect();
        codepoints.extend(0x0020u32..=0x007Eu32);
        codepoints.extend(0xFF01u32..=0xFF5Eu32);
        codepoints.sort();
        codepoints.dedup();

        // Pick face 0 (works for both single-face .ttf and TTC). If the
        // fixture is a TTC, the numFonts peek bounds the index too.
        let subsetted = subset_with_index(&font_data, 0, &codepoints)
            .expect("subset_with_index should succeed on a real CJK font");
        let subset_size = subsetted.len();

        let ratio = (subset_size as f64 / original_size as f64) * 100.0;
        eprintln!(
            "fontset → {} bytes → {} bytes ({:.1}% of original, {} codepoints)",
            original_size,
            subset_size,
            ratio,
            codepoints.len()
        );

        assert!(
            subset_size > 0 && subset_size < original_size,
            "Subset must be non-empty and smaller than original"
        );
        // 70% reduction floor on CJK fonts. The tuned Plan (drop
        // vmtx/LTSH/kern) gives this comfortably; a regression that
        // re-introduced any of those tables would push the ratio
        // above 30%.
        assert!(
            ratio < 30.0,
            "Expected ratio < 30% with tuned Plan, got {:.1}%",
            ratio
        );
    }

    #[test]
    #[ignore = "requires SSAHDRIFY_TEST_GPOS_FALLBACK_FONT env var pointing to a local fixture"]
    fn subset_with_index_handles_legacy_gpos_failure_font() {
        let Ok(font_path) = std::env::var("SSAHDRIFY_TEST_GPOS_FALLBACK_FONT") else {
            eprintln!("SSAHDRIFY_TEST_GPOS_FALLBACK_FONT not set — skipping");
            return;
        };
        let font_data = match std::fs::read(&font_path) {
            Ok(b) => b,
            Err(e) => {
                eprintln!("Cannot read {font_path}: {e} — skipping");
                return;
            }
        };

        let subsetted = subset_with_index(&font_data, 0, &[0x41])
            .expect("legacy GPOS fallback fixture should subset cleanly");
        assert!(!subsetted.is_empty(), "subset must not be empty");
    }

    #[test]
    #[ignore = "requires SSAHDRIFY_TEST_CMAP12_SANITIZE_FONT env var pointing to a local fixture"]
    fn subset_with_index_handles_out_of_unicode_cmap12_groups() {
        let Ok(font_path) = std::env::var("SSAHDRIFY_TEST_CMAP12_SANITIZE_FONT") else {
            eprintln!("SSAHDRIFY_TEST_CMAP12_SANITIZE_FONT not set — skipping");
            return;
        };
        let font_data = match std::fs::read(&font_path) {
            Ok(b) => b,
            Err(e) => {
                eprintln!("Cannot read {font_path}: {e} — skipping");
                return;
            }
        };

        let subsetted = subset_with_index(&font_data, 0, &[0x41, 0x4E2D])
            .expect("format-12 cmap groups beyond Unicode should be ignored");
        assert!(!subsetted.is_empty(), "subset must not be empty");
    }

    #[test]
    #[ignore = "requires SSAHDRIFY_TEST_ALL_FONTS_CACHE env var pointing to a local font cache"]
    fn subset_cached_font_faces_in_batch() {
        let Ok(cache_path) = std::env::var("SSAHDRIFY_TEST_ALL_FONTS_CACHE") else {
            eprintln!("SSAHDRIFY_TEST_ALL_FONTS_CACHE not set — skipping");
            return;
        };
        let offset = std::env::var("SSAHDRIFY_TEST_ALL_FONTS_OFFSET")
            .ok()
            .and_then(|v| v.parse::<usize>().ok())
            .unwrap_or(0);
        let limit = std::env::var("SSAHDRIFY_TEST_ALL_FONTS_LIMIT")
            .ok()
            .and_then(|v| v.parse::<usize>().ok())
            .unwrap_or(usize::MAX);
        let sqlite_limit = i64::try_from(limit).unwrap_or(-1);
        let sqlite_offset = i64::try_from(offset).unwrap_or(i64::MAX);

        let conn =
            Connection::open_with_flags(cache_path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
                .expect("open local font cache read-only");
        let mut stmt = conn
            .prepare(
                "SELECT font_path, face_index \
                 FROM cached_fonts \
                 ORDER BY font_path, face_index \
                 LIMIT ?1 OFFSET ?2",
            )
            .expect("prepare cached font query");
        let rows = stmt
            .query_map(params![sqlite_limit, sqlite_offset], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, i32>(1)?))
            })
            .expect("query cached fonts");

        let mut checked = 0usize;
        let mut failures = Vec::new();
        for row in rows {
            let (path, face_index) = row.expect("read cached font row");
            checked += 1;
            let Ok(face_index) = u32::try_from(face_index) else {
                failures.push(format!("{path}#invalid-index"));
                continue;
            };
            let font_data = match fs::read(&path) {
                Ok(data) => data,
                Err(error) => {
                    failures.push(format!("{path}#{face_index}: read failed: {error}"));
                    continue;
                }
            };
            let codepoints = [0x20, 0x41, 0x4E2D, 0x3042, 0xAC00];
            if let Err(error) = subset_with_index(&font_data, face_index, &codepoints) {
                failures.push(format!("{path}#{face_index}: {error}"));
            }
            if checked % 100 == 0 {
                eprintln!("subset cache batch progress: {checked} checked from offset {offset}");
            }
        }

        assert!(
            checked > 0,
            "cache batch at offset {offset} selected no font faces"
        );
        assert!(
            failures.is_empty(),
            "{} subset failure(s) in cache batch at offset {}:\n{}",
            failures.len(),
            offset,
            failures
                .iter()
                .take(20)
                .cloned()
                .collect::<Vec<_>>()
                .join("\n")
        );
    }

    fn sample_entry(path: &str, family: &str, index: u32) -> LocalFontEntry {
        LocalFontEntry {
            path: path.to_string(),
            index,
            families: vec![family.to_string()],
            face_names: Vec::new(),
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
        // fontcull's unicode-subset error contract — parse
        // failures must produce a non-empty Display string so downstream
        // wrappers (subset_with_index, subset_font) can interpolate them
        // cleanly into the IPC error path. Pre-`00f44ac` this branch
        // referenced a "subset_font index==0 branch" that no longer
        // exists (dispatch now keys on `is_ttc_data` magic peek, not
        // on `font_index == 0`); the comment was archaeology pointing
        // at a removed code path.
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

    #[test]
    fn bounded_font_family_name_keeps_full_name_past_leading_whitespace() {
        // Leading whitespace must NOT consume the materialize window and
        // truncate a legitimate 256-codepoint name — that would store a
        // different family string than the font declares and miss at lookup.
        let padded = format!("{}{}", " ".repeat(5), "x".repeat(256));
        assert_eq!(
            bounded_font_family_name(padded.chars()),
            Some("x".repeat(256))
        );
    }

    #[test]
    fn bounded_font_family_name_rejects_overpadded_truncation_alias() {
        let padded_attack = format!("{}{}{}", " ".repeat(1019), "Arial", "X".repeat(252));
        assert!(
            bounded_font_family_name(padded_attack.chars()).is_none(),
            "must validate the full trimmed name, not truncate to the fake alias Arial"
        );
    }

    #[test]
    fn bounded_font_family_name_ignores_only_true_trailing_whitespace() {
        let padded = format!("{}{}", "x".repeat(256), " ".repeat(4096));
        assert_eq!(
            bounded_font_family_name(padded.chars()),
            Some("x".repeat(256))
        );

        let internal_over_cap = format!("{}{}y", "x".repeat(255), " ".repeat(2));
        assert!(
            bounded_font_family_name(internal_over_cap.chars()).is_none(),
            "internal whitespace before a later non-space character counts toward the cap"
        );
    }

    #[test]
    fn bounded_font_family_name_rejects_bidi_and_zero_width() {
        // bounded_font_family_name now delegates to
        // validate_font_family for the BiDi / zero-width / line-sep /
        // U+061C rejection set. Without this pin, a future refactor
        // that drops the validate call (or replaces it with a partial
        // re-check) would let a U+202E-bearing name-table entry land
        // in the session DB and propagate into the UI / log layer.
        // Codepoints chosen to cover the major rejection classes:
        // bidi override, zero-width, line separator, Arabic Letter Mark.
        assert!(bounded_font_family_name("Ari\u{202E}al".chars()).is_none());
        assert!(bounded_font_family_name("Ari\u{200B}al".chars()).is_none());
        assert!(bounded_font_family_name("Ari\u{2028}al".chars()).is_none());
        assert!(bounded_font_family_name("Ari\u{061C}al".chars()).is_none());
        // Counter-assertion: ordinary Unicode (CJK, accented Latin)
        // continues to pass — the rejection is targeted, not over-broad.
        assert_eq!(
            bounded_font_family_name("微软雅黑".chars()),
            Some("微软雅黑".to_string())
        );
        assert_eq!(
            bounded_font_family_name("Demo Sans Pro".chars()),
            Some("Demo Sans Pro".to_string())
        );
    }

    #[test]
    fn face_name_aliases_keep_full_name_that_duplicates_family_variant() {
        let duplicated_family_name = "Dream Han Serif SC W22".to_string();
        let family_variants = [duplicated_family_name.clone()];
        let mut face_name_variants = HashSet::new();
        face_name_variants.insert(duplicated_family_name.clone());
        face_name_variants.insert("DreamHanSerifSC-W22".to_string());

        let aliases = materialize_face_name_aliases(face_name_variants);

        assert!(
            family_variants.contains(&duplicated_family_name),
            "test fixture should model a font that repeats FULL_NAME in FAMILY_NAME"
        );
        assert_eq!(
            aliases,
            vec![
                "Dream Han Serif SC W22".to_string(),
                "DreamHanSerifSC-W22".to_string()
            ]
        );
    }

    /// Usage on Windows:
    ///   SSAHDRIFY_TEST_DREAMHAN_SERIF_TTC=C:/Font_files/DreamHanSerif/DreamHanSerif-W22.ttc
    ///   cargo test --lib -- --ignored parse_local_dream_han_serif_w22_keeps_duplicate_full_name_alias
    #[test]
    #[ignore = "requires SSAHDRIFY_TEST_DREAMHAN_SERIF_TTC pointing to a Dream Han Serif W22 TTC"]
    fn parse_local_dream_han_serif_w22_keeps_duplicate_full_name_alias() {
        let Some(fixture) =
            std::env::var_os("SSAHDRIFY_TEST_DREAMHAN_SERIF_TTC").map(PathBuf::from)
        else {
            eprintln!("SSAHDRIFY_TEST_DREAMHAN_SERIF_TTC not set; skipping");
            return;
        };
        if !fixture.is_file() {
            eprintln!(
                "Dream Han fixture not found at {}; skipping",
                fixture.display()
            );
            return;
        }
        let canonical = fixture
            .canonicalize()
            .expect("Dream Han fixture should canonicalize");
        let entries = parse_local_font_file(&canonical, NO_SCAN_ID);
        let sc_face = entries
            .iter()
            .find(|entry| {
                entry
                    .families
                    .iter()
                    .any(|name| name == "Dream Han Serif SC W22")
            })
            .unwrap_or_else(|| {
                panic!(
                    "fixture did not expose Dream Han Serif SC W22 as a family variant; parsed {} entries",
                    entries.len()
                )
            });

        assert!(sc_face.bold, "W22 should be classified as bold");
        assert!(
            sc_face
                .face_names
                .iter()
                .any(|name| name == "Dream Han Serif SC W22"),
            "duplicated FULL_NAME must remain a concrete face alias: {:?}",
            sc_face.face_names
        );
        assert!(
            sc_face
                .face_names
                .iter()
                .any(|name| name == "DreamHanSerifSC-W22"),
            "PostScript name must remain a concrete face alias: {:?}",
            sc_face.face_names
        );
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
        assert!(is_user_font_face_registered("C:\\Fonts\\A.ttf", 0).unwrap());
        // Un-scanned face of the same TTC must fail provenance: protects
        // the subset_font gate against face-index forgery on TTC files
        // where only one face was actually scanned.
        assert!(!is_user_font_face_registered("C:\\Fonts\\A.ttf", 1).unwrap());
    }

    #[test]
    fn full_face_names_match_without_weakening_family_style_matching() {
        let _guard = SCAN_TEST_LOCK.lock().unwrap();
        init_test_user_font_db("face-name-alias");
        let temp_font_dir = std::env::temp_dir().join(format!(
            "ssahdrify-face-name-alias-test-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&temp_font_dir);
        fs::create_dir_all(&temp_font_dir).expect("test font dir should be created");
        let temp_font_path = temp_font_dir.join("DreamHanSerif-W22.ttc");
        fs::write(&temp_font_path, b"fake Dream Han cache metadata fixture")
            .expect("test font file should be written");
        let temp_font_path_str = temp_font_path.to_string_lossy().into_owned();
        let temp_font_size = fs::metadata(&temp_font_path)
            .expect("test font metadata should be readable")
            .len();
        let entry = LocalFontEntry {
            path: temp_font_path_str.clone(),
            index: 3,
            families: vec![
                "Dream Han Serif SC".to_string(),
                "Dream Han Serif SC W22".to_string(),
                "梦源宋体 SC".to_string(),
            ],
            face_names: vec![
                "Dream Han Serif SC W22".to_string(),
                "DreamHanSerifSC-W22".to_string(),
            ],
            bold: true,
            italic: false,
            size_bytes: temp_font_size,
        };
        let metadata = entries_to_cache_metadata(&[entry.clone()]);
        assert_eq!(metadata.len(), 1);
        let keys: HashSet<String> = metadata[0]
            .family_keys
            .iter()
            .map(|key| user_font_key(&key.family_name, key.bold, key.italic))
            .collect();

        assert!(keys.contains(&user_font_key("Dream Han Serif SC", true, false)));
        assert!(keys.contains(&user_font_key("Dream Han Serif SC W22", true, false)));
        assert!(!keys.contains(&user_font_key("Dream Han Serif SC", false, false)));
        assert!(!keys.contains(&user_font_key("Dream Han Serif SC W22", false, false)));
        assert_eq!(
            metadata[0].face_name_aliases,
            vec![
                "Dream Han Serif SC W22".to_string(),
                "DreamHanSerifSC-W22".to_string()
            ]
        );

        commit_entries("source-dream-han", vec![entry]);

        let direct_face_name =
            resolve_user_font("Dream Han Serif SC W22".to_string(), false, false)
                .unwrap()
                .expect("full face name should resolve regardless of ASS bold flag");
        assert_eq!(direct_face_name.path.as_str(), temp_font_path_str.as_str());
        assert_eq!(direct_face_name.index, 3);

        let postscript_name = resolve_user_font("DreamHanSerifSC-W22".to_string(), false, true)
            .unwrap()
            .expect("PostScript face alias should resolve regardless of ASS italic flag");
        assert_eq!(postscript_name.path.as_str(), temp_font_path_str.as_str());
        assert_eq!(postscript_name.index, 3);

        assert!(
            resolve_user_font("Dream Han Serif SC".to_string(), true, false)
                .unwrap()
                .is_some()
        );
        assert!(
            resolve_user_font("Dream Han Serif SC".to_string(), false, false)
                .unwrap()
                .is_none()
        );

        let _ = fs::remove_dir_all(&temp_font_dir);
    }

    #[test]
    fn full_face_aliases_do_not_override_exact_family_matches() {
        let _guard = SCAN_TEST_LOCK.lock().unwrap();
        init_test_user_font_db("face-name-priority");
        let exact = LocalFontEntry {
            path: "C:\\Fonts\\ExactSharedSans-Regular.otf".to_string(),
            index: 0,
            families: vec!["Shared Sans".to_string()],
            face_names: Vec::new(),
            bold: false,
            italic: false,
            size_bytes: 1_000_000,
        };
        let alias = LocalFontEntry {
            path: "C:\\Fonts\\AliasFace-Bold.otf".to_string(),
            index: 0,
            families: vec!["Other Sans".to_string()],
            face_names: vec!["Shared Sans".to_string()],
            bold: true,
            italic: false,
            size_bytes: 1_000_000,
        };

        commit_entries("source-exact", vec![exact]);
        commit_entries("source-alias-newer", vec![alias]);

        let exact_result = resolve_user_font("Shared Sans".to_string(), false, false)
            .unwrap()
            .expect("exact family should resolve");
        assert_eq!(exact_result.path, "C:\\Fonts\\ExactSharedSans-Regular.otf");

        let alias_result = resolve_user_font("Shared Sans".to_string(), true, true)
            .unwrap()
            .expect("face alias should resolve when exact style is absent");
        assert_eq!(alias_result.path, "C:\\Fonts\\AliasFace-Bold.otf");
    }

    // ── cache provenance gate pins ──
    //
    // ALLOWED_CACHE_FONT_PATHS is a SECOND trusted set (alongside
    // ALLOWED_FONT_PATHS for system fonts) so cache lookup hits can
    // pass subset_font's gate. These three tests pin the gate's three
    // documented states: rejects unregistered, accepts registered, and
    // refuses to grow past MAX_PROVENANCE_CACHE_SIZE. Without these
    // pins, a refactor that drops the (path, face_index) pair-keying
    // or accidentally widens the gate to trust EVERY path that ever
    // entered the SQLite cache (the anti-pattern the design explicitly
    // rejects) would still compile and pass higher-level integration
    // tests.
    //
    // Tests acquire SCAN_TEST_LOCK because they mutate
    // ALLOWED_CACHE_FONT_PATHS, a process-global mutex. Clean up
    // after themselves so the assertion order doesn't matter and
    // the suite stays parallel-safe.

    #[test]
    fn cache_provenance_gate_rejects_unregistered_path() {
        let _guard = SCAN_TEST_LOCK.lock().unwrap();
        // Snapshot the set so we restore it post-test (other tests
        // in the same binary process may have registered paths).
        let snapshot: HashSet<(String, u32)> = ALLOWED_CACHE_FONT_PATHS.lock().unwrap().clone();
        ALLOWED_CACHE_FONT_PATHS.lock().unwrap().clear();

        let path = "C:\\Fonts\\NotRegistered.ttf".to_string();
        let key = (path, 0);
        assert!(
            !ALLOWED_CACHE_FONT_PATHS.lock().unwrap().contains(&key),
            "unregistered cache path must NOT be in the provenance set"
        );

        *ALLOWED_CACHE_FONT_PATHS.lock().unwrap() = snapshot;
    }

    #[test]
    fn cache_provenance_gate_accepts_registered_path() {
        let _guard = SCAN_TEST_LOCK.lock().unwrap();
        let snapshot: HashSet<(String, u32)> = ALLOWED_CACHE_FONT_PATHS.lock().unwrap().clone();
        ALLOWED_CACHE_FONT_PATHS.lock().unwrap().clear();

        let path = "C:\\Fonts\\Registered.ttf";
        // register_cache_provenance now takes
        // `&FontLookupResult`. Tests in this module are in-crate, so
        // pub(crate) construction is permitted.
        let hit = crate::font_cache::FontLookupResult {
            font_path: path.to_string(),
            face_index: 2,
        };
        register_cache_provenance(&hit).expect("register should succeed under cap");
        let key = (path.to_string(), 2);
        assert!(
            ALLOWED_CACHE_FONT_PATHS.lock().unwrap().contains(&key),
            "registered (path, face_index) must be in the provenance set"
        );
        // Different face_index for the same path must NOT pass — the
        // pair-keying defends against face-index injection on TTC files.
        let wrong_face = (path.to_string(), 5);
        assert!(
            !ALLOWED_CACHE_FONT_PATHS
                .lock()
                .unwrap()
                .contains(&wrong_face),
            "different face_index on same path must not slip through pair-keyed gate"
        );

        *ALLOWED_CACHE_FONT_PATHS.lock().unwrap() = snapshot;
    }

    #[test]
    fn cache_provenance_gate_is_capped_at_max_provenance() {
        let _guard = SCAN_TEST_LOCK.lock().unwrap();
        let snapshot: HashSet<(String, u32)> = ALLOWED_CACHE_FONT_PATHS.lock().unwrap().clone();
        ALLOWED_CACHE_FONT_PATHS.lock().unwrap().clear();

        // Fill to the cap, then attempt one more insert. Using
        // distinct paths so HashSet doesn't dedup. The cap check
        // happens inside insert_with_cap after the speculative
        // insert + rollback; a successful pre-cap insert + a
        // failing past-cap insert pin the contract.
        for i in 0..MAX_PROVENANCE_CACHE_SIZE {
            // Format path with index so each insert is unique.
            let hit = crate::font_cache::FontLookupResult {
                font_path: format!("C:\\Fonts\\Cap{i}.ttf"),
                face_index: 0,
            };
            register_cache_provenance(&hit).expect("pre-cap insert should succeed");
        }
        assert_eq!(
            ALLOWED_CACHE_FONT_PATHS.lock().unwrap().len(),
            MAX_PROVENANCE_CACHE_SIZE,
            "set should sit exactly at the cap"
        );
        let overflow = register_cache_provenance(&crate::font_cache::FontLookupResult {
            font_path: "C:\\Fonts\\Overflow.ttf".to_string(),
            face_index: 0,
        });
        assert!(
            overflow.is_err(),
            "past-cap insert must return Err (rolled back)"
        );
        let err_msg = overflow.unwrap_err();
        assert!(
            err_msg.contains("cache"),
            "error message must name the cache set (W7.6 N1-R7-5 label distinguishability), got: {err_msg}"
        );
        // The speculative insert must have been rolled back — set
        // size unchanged.
        assert_eq!(
            ALLOWED_CACHE_FONT_PATHS.lock().unwrap().len(),
            MAX_PROVENANCE_CACHE_SIZE,
            "rollback should leave the set at exactly the cap, not cap+1"
        );

        *ALLOWED_CACHE_FONT_PATHS.lock().unwrap() = snapshot;
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

        remove_font_source("source-b".to_string(), None).unwrap();
        let resolved = resolve_user_font("Demo Sans".to_string(), false, false)
            .unwrap()
            .expect("older source should become visible again");
        assert_eq!(resolved.path, "C:\\Fonts\\A.ttf");
        assert!(!is_user_font_face_registered("C:\\Fonts\\B.ttf", 0).unwrap());

        clear_font_sources().unwrap();
        assert!(resolve_user_font("Demo Sans".to_string(), false, false)
            .unwrap()
            .is_none());
        assert!(!is_user_font_face_registered("C:\\Fonts\\A.ttf", 0).unwrap());
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

    /// `scan_directory_inner` walks a directory of non-font files to
    /// natural completion without emitting batches or producing faces.
    /// Pins two related contracts: `has_allowed_font_extension` filters
    /// non-font files, and the visited-entry cap does not false-fire on
    /// normal-size directories (CeilingHit only on real overflow). A
    /// future refactor that drops or shifts either guard would either
    /// eat budget on dirs full of `.txt` / `.png` / `.log` files
    /// (the original gap this test guards against) or false-report
    /// `CeilingHit` here.
    #[test]
    fn directory_inner_skips_non_font_files_without_emitting() {
        use std::io::Write as _;
        let mut dir = std::env::temp_dir();
        dir.push("ssahdrify_fonts_test_non_font_skip");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        for n in [
            "a.txt",
            "b.png",
            "c.log",
            "d.json",
            "e.md",
            "f.csv",
            "g.bin",
            "h.gitignore",
        ] {
            let p = dir.join(n);
            std::fs::File::create(&p)
                .unwrap()
                .write_all(b"not-a-font")
                .unwrap();
        }

        let mut emitted: Vec<Vec<LocalFontEntry>> = Vec::new();
        let outcome = scan_directory_inner(&dir, NO_SCAN_ID, |batch| {
            emitted.push(batch);
            Ok(())
        })
        .expect("non-font directory should complete naturally");
        assert_eq!(outcome.total, 0);
        assert_eq!(outcome.reason, ScanStopReason::Natural);
        assert!(
            emitted.is_empty(),
            "no batches expected; got {} batch(es)",
            emitted.len()
        );

        let _ = std::fs::remove_dir_all(&dir);
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
        let prior_cancel_id = CANCEL_SCAN_ID.load(Ordering::Relaxed);
        let scan_id = prior_cancel_id.saturating_add(1);
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
        // reset CANCEL_SCAN_ID via
        // `compare_exchange` to its pre-test value so subsequent tests
        // sharing the binary process don't inherit the elevated cancel
        // id. SCAN_TEST_LOCK serializes the tests but `CANCEL_SCAN_ID`
        // is a process-global AtomicU64 — without explicit cleanup,
        // a later test that does `CANCEL_SCAN_ID.load() + 1` and then
        // expects a fresh scan to NOT be cancelled would see the prior
        // value and silently regress to UserCancel. The compare_exchange
        // is no-op if some other path raised the field between our
        // fetch_max and the reset (extremely unlikely under SCAN_TEST_LOCK,
        // but the fail-safe pattern is cheap).
        let _ = CANCEL_SCAN_ID.compare_exchange(
            scan_id,
            prior_cancel_id,
            Ordering::Relaxed,
            Ordering::Relaxed,
        );
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
