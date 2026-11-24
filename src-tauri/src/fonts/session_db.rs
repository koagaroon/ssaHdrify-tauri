//! Session-scoped SQLite index for user-added font directories and files.
//!
//! Owns `USER_FONT_DB_PATH`, schema setup, batch import transactions, and
//! `USER_FONT_LOOKUP_SQL` (whose resolution order must stay aligned with
//! `crate::font_cache`).

use super::*;
use once_cell::sync::Lazy;
use rusqlite::{params, Connection, OptionalExtension, Transaction};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// Session SQLite path for user-picked font sources. Commands open short-lived
/// connections to this path instead of sharing a global Connection, which keeps
/// the static state simple and avoids holding SQLite page caches for longer
/// than one operation.
pub(super) static USER_FONT_DB_PATH: Lazy<Mutex<Option<PathBuf>>> = Lazy::new(|| Mutex::new(None));

#[derive(Debug, Clone, Copy, Default)]
pub(super) struct ImportOutcome {
    pub(super) added: usize,
    pub(super) duplicated: usize,
}

pub(super) fn db_error(context: &str, error: rusqlite::Error) -> String {
    log::warn!("user font index {context}: {error}");
    format!("Internal error: user font index {context}")
}

pub(super) fn user_font_db_path() -> Result<PathBuf, String> {
    USER_FONT_DB_PATH
        .lock()
        .map_err(|_| "Internal error: user font index path corrupted".to_string())?
        .clone()
        .ok_or_else(|| "User font index is not initialized".to_string())
}

pub(super) fn open_user_font_db() -> Result<Connection, String> {
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
pub(super) fn set_user_font_db_journal_mode_once(conn: &Connection) -> Result<(), String> {
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

pub(super) fn init_user_font_schema(conn: &Connection) -> Result<(), String> {
    // foreign_keys is set per-connection in `open_user_font_db`; no need
    // to repeat it here. Schema-only batch.
    conn.execute_batch(
        "
        CREATE TABLE IF NOT EXISTS font_sources (
            source_id TEXT PRIMARY KEY,
            source_order INTEGER NOT NULL UNIQUE,
            source_kind INTEGER NOT NULL CHECK(source_kind IN (0, 1, 2)),
            root_path TEXT,
            scan_reason INTEGER NOT NULL CHECK(scan_reason IN (-1, 0, 1, 2, 3)),
            CHECK((source_kind = 2 AND root_path IS NULL)
               OR (source_kind IN (0, 1) AND root_path IS NOT NULL))
        );
        CREATE TABLE IF NOT EXISTS font_faces (
            face_id INTEGER PRIMARY KEY,
            path TEXT NOT NULL,
            face_index INTEGER NOT NULL,
            bold INTEGER NOT NULL,
            italic INTEGER NOT NULL,
            size_bytes INTEGER NOT NULL,
            UNIQUE(path, face_index)
        );
        CREATE TABLE IF NOT EXISTS font_source_faces (
            source_id TEXT NOT NULL,
            face_id INTEGER NOT NULL,
            PRIMARY KEY(source_id, face_id),
            FOREIGN KEY(source_id) REFERENCES font_sources(source_id) ON DELETE CASCADE,
            FOREIGN KEY(face_id) REFERENCES font_faces(face_id) ON DELETE CASCADE
        );
        CREATE TABLE IF NOT EXISTS font_family_keys (
            family_name_key TEXT NOT NULL,
            bold INTEGER NOT NULL,
            italic INTEGER NOT NULL,
            key_kind INTEGER NOT NULL CHECK(key_kind IN (0, 1)),
            face_id INTEGER NOT NULL,
            FOREIGN KEY(face_id) REFERENCES font_faces(face_id) ON DELETE CASCADE
        );
        CREATE INDEX IF NOT EXISTS idx_font_family_lookup
            ON font_family_keys(family_name_key, key_kind, bold, italic, face_id);
        CREATE INDEX IF NOT EXISTS idx_font_family_face
            ON font_family_keys(face_id);
        CREATE INDEX IF NOT EXISTS idx_font_source_faces_face
            ON font_source_faces(face_id);
        CREATE INDEX IF NOT EXISTS idx_font_faces_path_index
            ON font_faces(path, face_index);
        CREATE UNIQUE INDEX IF NOT EXISTS idx_font_sources_root_kind
            ON font_sources(root_path, source_kind)
            WHERE root_path IS NOT NULL;
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
pub(super) fn user_font_key(family: &str, bold: bool, italic: bool) -> String {
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

pub(super) const USER_FONT_KEY_KIND_FAMILY: i32 = 0;
pub(super) const USER_FONT_KEY_KIND_FACE_ALIAS: i32 = 1;
pub(super) const USER_FONT_LOOKUP_SQL: &str = "
        SELECT f.path, f.face_index
        FROM font_family_keys k
        JOIN font_faces f ON f.face_id = k.face_id
        JOIN font_source_faces sf ON sf.face_id = f.face_id
        JOIN font_sources s ON s.source_id = sf.source_id
        WHERE k.family_name_key = ?1
          AND ((k.key_kind = ?2 AND k.bold = ?3 AND k.italic = ?4)
               OR (k.key_kind = ?5 AND k.bold = 0 AND k.italic = 0))
        ORDER BY k.key_kind, s.source_kind DESC, s.source_order DESC,
                 f.path, f.face_index
        LIMIT 1
        ";

pub(super) fn validate_font_source_id(source_id: &str) -> Result<(), String> {
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

pub(super) fn create_user_font_source_tx(
    tx: &Transaction<'_>,
    source_id: &str,
    source_kind: FontSourceKind,
    root_path: Option<&str>,
) -> Result<i64, String> {
    validate_font_source_id(source_id)?;
    match (source_kind, root_path) {
        (FontSourceKind::Files, None) => {}
        (FontSourceKind::ShallowDirectory | FontSourceKind::RecursiveDirectory, Some(_)) => {}
        _ => return Err("Font source kind and root path do not match".to_string()),
    }
    let source_order: i64 = tx
        .query_row(
            "SELECT COALESCE(MAX(source_order), 0) + 1 FROM font_sources",
            [],
            |row| row.get(0),
        )
        .map_err(|e| db_error("source order query failed", e))?;
    tx.execute(
        "INSERT INTO font_sources(
            source_id, source_order, source_kind, root_path, scan_reason
         ) VALUES (?1, ?2, ?3, ?4, -1)",
        params![source_id, source_order, source_kind.as_i32(), root_path],
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
            "That font source is already loaded".to_string()
        } else {
            db_error("source insert failed", e)
        }
    })?;
    Ok(source_order)
}

pub(super) fn import_user_font_batch_tx(
    tx: &Transaction<'_>,
    source_id: &str,
    entries: Vec<LocalFontEntry>,
) -> Result<ImportOutcome, String> {
    let mut added = 0;
    let mut duplicated = 0;
    let mut insert_face = tx
        .prepare(
            "
            INSERT OR IGNORE INTO font_faces(
                path, face_index, bold, italic, size_bytes
            ) VALUES (?1, ?2, ?3, ?4, ?5)
            ",
        )
        .map_err(|e| db_error("face insert prepare failed", e))?;
    let mut insert_membership = tx
        .prepare("INSERT OR IGNORE INTO font_source_faces(source_id, face_id) VALUES (?1, ?2)")
        .map_err(|e| db_error("source-face insert prepare failed", e))?;
    let mut membership_exists = tx
        .prepare(
            "SELECT EXISTS(
                 SELECT 1 FROM font_source_faces
                 WHERE source_id = ?1 AND face_id = ?2
             )",
        )
        .map_err(|e| db_error("source-face lookup prepare failed", e))?;
    let mut update_face = tx
        .prepare(
            "UPDATE font_faces
             SET bold = ?2, italic = ?3, size_bytes = ?4
             WHERE face_id = ?1",
        )
        .map_err(|e| db_error("face refresh prepare failed", e))?;
    let mut delete_keys = tx
        .prepare("DELETE FROM font_family_keys WHERE face_id = ?1")
        .map_err(|e| db_error("family-key refresh prepare failed", e))?;
    let mut insert_key = tx
        .prepare(
            "
            INSERT INTO font_family_keys(
                family_name_key, bold, italic, key_kind, face_id
            ) VALUES (?1, ?2, ?3, ?4, ?5)
            ",
        )
        .map_err(|e| db_error("family-key insert prepare failed", e))?;

    for entry in entries {
        // saturate u64→i64 instead of aborting the
        // batch. Companion `entries_to_cache_metadata` saturates via
        // `unwrap_or(i64::MAX)`: same conversion, two opposite failure
        // modes. u64 values above
        // i64::MAX (8.4 EB) are mathematically impossible for a real
        // font file size, but a hostile cache row crafted with a
        // numeric overflow shouldn't have the power to abort an
        // entire session's font batch import.
        let size_bytes = i64::try_from(entry.size_bytes).unwrap_or(i64::MAX);
        let changed = insert_face
            .execute(params![
                &entry.path,
                entry.index,
                if entry.bold { 1 } else { 0 },
                if entry.italic { 1 } else { 0 },
                size_bytes
            ])
            .map_err(|e| db_error("face insert failed", e))?;
        let face_id = if changed > 0 {
            tx.last_insert_rowid()
        } else {
            let face_id = tx
                .query_row(
                    "SELECT face_id FROM font_faces WHERE path = ?1 AND face_index = ?2",
                    params![&entry.path, entry.index],
                    |row| row.get(0),
                )
                .map_err(|e| db_error("existing face lookup failed", e))?;
            let already_owned: bool = membership_exists
                .query_row(params![source_id, face_id], |row| row.get(0))
                .map_err(|e| db_error("source-face lookup failed", e))?;
            if already_owned {
                // Two parsed entries for the same path/index in one source are
                // duplicates, not a rescan. Keep the first entry's metadata and
                // keys; otherwise the last duplicate silently rewrites which
                // family resolves from that source.
                duplicated += 1;
                continue;
            }
            // A later overlapping source may be scanning a replacement at the
            // same path/index. The face is shared globally, so refresh its
            // parsed attributes and lookup keys before adding the new owner;
            // otherwise stale family names survive even after the original
            // source is removed.
            update_face
                .execute(params![
                    face_id,
                    i32::from(entry.bold),
                    i32::from(entry.italic),
                    size_bytes
                ])
                .map_err(|e| db_error("face refresh failed", e))?;
            delete_keys
                .execute(params![face_id])
                .map_err(|e| db_error("family-key refresh failed", e))?;
            face_id
        };

        for family in &entry.families {
            insert_key
                .execute(params![
                    crate::font_cache::family_lookup_key(family),
                    i32::from(entry.bold),
                    i32::from(entry.italic),
                    USER_FONT_KEY_KIND_FAMILY,
                    face_id,
                ])
                .map_err(|e| db_error("family-key insert failed", e))?;
        }
        for face_name in &entry.face_names {
            insert_key
                .execute(params![
                    crate::font_cache::family_lookup_key(face_name),
                    0,
                    0,
                    USER_FONT_KEY_KIND_FACE_ALIAS,
                    face_id,
                ])
                .map_err(|e| db_error("face-name key insert failed", e))?;
        }

        if insert_membership
            .execute(params![source_id, face_id])
            .map_err(|e| db_error("source-face insert failed", e))?
            > 0
        {
            added += 1;
        } else {
            duplicated += 1;
        }
    }

    Ok(ImportOutcome { added, duplicated })
}

pub(super) fn finish_user_font_source_tx(
    tx: &Transaction<'_>,
    source_id: &str,
    reason: ScanStopReason,
) -> Result<(), String> {
    tx.execute(
        "UPDATE font_sources SET scan_reason = ?2 WHERE source_id = ?1",
        params![source_id, scan_reason_code(reason)],
    )
    .map_err(|e| db_error("source completion update failed", e))?;
    Ok(())
}

pub(super) fn remove_empty_user_font_source_tx(
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
    delete_orphan_user_font_faces_tx(tx)?;
    Ok(())
}

pub(super) fn delete_orphan_user_font_faces_tx(tx: &Transaction<'_>) -> Result<(), String> {
    tx.execute(
        "DELETE FROM font_faces
         WHERE NOT EXISTS (
             SELECT 1 FROM font_source_faces sf WHERE sf.face_id = font_faces.face_id
         )",
        [],
    )
    .map_err(|e| db_error("orphan face cleanup failed", e))?;
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
pub(super) fn is_user_font_face_registered(
    canonical_path: &str,
    face_index: u32,
) -> Result<bool, String> {
    let conn = open_user_font_db()?;
    // Face-index narrowed: TTC files carry multiple faces under one
    // path (e.g. Source Han Serif Regular = face 0, Bold = face 1).
    // BOTH `path` AND `face_index` must be present in `font_faces`;
    // an attacker-chosen face index that was never observed by a
    // scan fails the gate even if the path itself was scanned.
    // UNIQUE(path, face_index) makes this lookup a single index probe.
    conn.query_row(
        "SELECT 1 FROM font_faces WHERE path = ?1 AND face_index = ?2 LIMIT 1",
        params![canonical_path, face_index as i64],
        |_| Ok(()),
    )
    .optional()
    .map(|v| v.is_some())
    .map_err(|e| db_error("path lookup failed", e))
}

/// Look up a font face in the user's local source index by family plus the
/// bold and italic flags. Returns `None` if no match — callers fall back to
/// system fonts.
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
        USER_FONT_LOOKUP_SQL,
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

pub fn remove_font_source(source_id: String, kind: Option<String>) -> Result<(), String> {
    validate_font_source_id(&source_id)?;
    let _mutation_guard =
        crate::font_cache_commands::CacheMutationGuard::try_acquire().map_err(|_| {
            "Cache rescan in progress — wait for it to finish, then retry removing the source."
                .to_string()
        })?;
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
    let _ = kind; // Compatibility argument from older frontend bundles; server state is authoritative.
    let source_record: Option<(i32, Option<String>)> = tx
        .query_row(
            "SELECT source_kind, root_path FROM font_sources WHERE source_id = ?1",
            params![source_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()
        .map_err(|e| db_error("source lookup failed", e))?;
    let Some((source_kind, root_path)) = source_record else {
        return Err("Font source no longer exists".to_string());
    };
    if let Some(root) = root_path.as_deref() {
        let scope = match source_kind {
            0 => FontDirectoryScope::Recursive,
            1 => FontDirectoryScope::Shallow,
            _ => return Err("Stored font source kind is invalid".to_string()),
        };
        // Cache-first failure ordering: losing acceleration is recoverable;
        // deleting session ownership while stale persistent rows survive is not.
        crate::font_cache_commands::remove_source_from_gui_cache_locked(
            &_mutation_guard,
            root,
            scope,
        )?;
    }
    tx.execute(
        "DELETE FROM font_sources WHERE source_id = ?1",
        params![source_id],
    )
    .map_err(|e| db_error("source delete failed", e))?;
    delete_orphan_user_font_faces_tx(&tx)?;
    tx.commit()
        .map_err(|e| db_error("source delete commit failed", e))?;
    clear_cache_provenance();
    Ok(())
}

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
    // Cache-first failure ordering matches individual removal: if the
    // persistent transaction fails, leave the session sources untouched so
    // the user can retry without losing ownership state.
    crate::font_cache_commands::clear_all_sources_in_gui_cache_locked(&_mutation_guard)?;
    tx.execute("DELETE FROM font_sources", [])
        .map_err(|e| db_error("source clear failed", e))?;
    delete_orphan_user_font_faces_tx(&tx)?;
    tx.commit()
        .map_err(|e| db_error("source clear commit failed", e))?;
    // Drop the in-process cache-provenance set. Without this, paths
    // registered earlier in the
    // session via `lookup_family` cache hits would survive
    // `clear_font_sources` and still pass subset_font's gate on
    // their next use, undercutting the user's "fresh slate" signal.
    // ALLOWED_FONT_PATHS (system fonts) intentionally NOT cleared —
    // system fonts don't depend on user-source state and re-clearing
    // them would force expensive re-discovery via font-kit on the
    // next embed.
    clear_cache_provenance();
    Ok(())
}
