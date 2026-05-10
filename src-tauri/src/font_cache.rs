//! Persistent font cache — metadata index across app lifetimes.
//!
//! Memoizes the expensive scan-and-name-table-read step of font-source
//! resolution. The cache stores per-folder mtime, per-file mtime/size,
//! and the family-name lookup keys; it does NOT cache subset bytes
//! (subsetting is per-subtitle and depends on glyph sets that vary).
//!
//! Decoupled from the existing GUI session DB (`init_user_font_db` in
//! `fonts.rs`): different lifetime (cross-run vs single-app-run),
//! different access pattern (read-mostly vs write-heavy), different
//! invalidation needs (mtime/size based vs always-fresh). Per-binary
//! storage at the caller-supplied path so GUI and CLI run independently
//! without lock contention.
//!
//! See `docs/architecture/ssahdrify_cli_design.md` § "v1.4.1 stable
//! 后续用户反馈" #5 for the full design lock.
//!
//! Step 1 of the implementation plan (this file): schema + open/create + version check.
//! Subsequent steps add scan/populate, drift detection, family-name lookup, CLI integration, and GUI integration.

use std::path::{Path, PathBuf};

use rusqlite::{params, Connection};

/// Default cache file path for the CLI binary, OS-specific:
/// - Windows: `%APPDATA%/ssaHdrify/cli_font_cache.sqlite3`
/// - macOS:   `$HOME/Library/Application Support/ssaHdrify/cli_font_cache.sqlite3`
/// - Linux:   `${XDG_DATA_HOME:-$HOME/.local/share}/ssaHdrify/cli_font_cache.sqlite3`
///
/// Returns an `Err` when the platform's environment for the canonical
/// per-user data directory isn't set (broken environment). Caller can
/// override via `--cache-file <PATH>`.
pub fn default_cli_cache_path() -> Result<PathBuf, String> {
    let base = platform_data_dir()?;
    Ok(base.join("ssaHdrify").join("cli_font_cache.sqlite3"))
}

/// Per-user data directory, resolved per-OS without pulling in the
/// `dirs` crate (one usage didn't justify the dep). Mirrors the
/// well-known XDG and platform conventions:
/// - Windows: `%APPDATA%` (Roaming)
/// - macOS:   `$HOME/Library/Application Support`
/// - Linux:   `$XDG_DATA_HOME` if set, else `$HOME/.local/share`
fn platform_data_dir() -> Result<PathBuf, String> {
    #[cfg(target_os = "windows")]
    {
        std::env::var("APPDATA").map(PathBuf::from).map_err(|_| {
            "APPDATA environment variable not set; cannot determine \
                 default cache location. Pass --cache-file <PATH> to override."
                .to_string()
        })
    }
    #[cfg(target_os = "macos")]
    {
        std::env::var("HOME")
            .map(|h| PathBuf::from(h).join("Library").join("Application Support"))
            .map_err(|_| {
                "HOME environment variable not set; cannot determine \
                 default cache location. Pass --cache-file <PATH> to override."
                    .to_string()
            })
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        // XDG Base Dir spec § "Environment variables": "If
        // $XDG_DATA_HOME is either not set or empty, a default equal
        // to $HOME/.local/share should be used." The empty-check is
        // spec-required, not defensive paranoia — don't simplify away
        // the !is_empty() check. The is_absolute() guard rejects
        // exotic values like "." or relative paths from a misconfigured
        // shell, falling through to the HOME default.
        if let Ok(xdg) = std::env::var("XDG_DATA_HOME") {
            if !xdg.is_empty() {
                let xdg_path = PathBuf::from(&xdg);
                if xdg_path.is_absolute() {
                    return Ok(xdg_path);
                }
            }
        }
        std::env::var("HOME")
            .map(|h| PathBuf::from(h).join(".local").join("share"))
            .map_err(|_| {
                "Neither XDG_DATA_HOME nor HOME is set; cannot determine \
                 default cache location. Pass --cache-file <PATH> to override."
                    .to_string()
            })
    }
}

/// Schema version. Bumped when any table layout changes; mismatch on
/// open returns `CacheError::SchemaVersionMismatch` so the caller
/// can rebuild (CLI: drift-equivalent fallback to no-cache; GUI:
/// prompt). Per the locked "no auto-migrate" decision, the cache is
/// never silently migrated — release notes call out version bumps so
/// users intentionally rebuild via `refresh-fonts` or the GUI modal.
///
/// v1 → v2 (Round 2 review): added `family_name_key` column to
/// `cached_family_keys` storing NFC-normalized lowercase form so
/// lookup hit rate matches the session DB's user_font_key contract.
/// Without it, CJK fonts whose name-table form differs from the
/// ASS \fn / Style Fontname spelling missed every cache lookup.
pub const SCHEMA_VERSION: i32 = 2;

/// Normalize a family-name string into the lookup key used by
/// `cached_family_keys.family_name_key`: NFC-normalize then full
/// Unicode lowercase (so `É`→`é`, not just ASCII-only `A`→`a`).
/// Mirrors `userFontKey`'s normalization in font-embedder.ts (which
/// uses JS `toLowerCase()`, also full Unicode) so a font's name-table
/// entry and an ASS file's `\fn` reference match regardless of NFC/NFD
/// form (macOS HFS+ NFD vs Windows NFC) or case (`Café` vs `CAFÉ`,
/// `Source Han Sans CN` vs `source han sans cn`). Plain ASCII
/// `to_ascii_lowercase` would miss `É`/`Ñ`/`Ü` etc., breaking the
/// CJK/Latin-extended fonts the cache exists to accelerate.
pub(crate) fn family_lookup_key(family_name: &str) -> String {
    use unicode_normalization::UnicodeNormalization;
    family_name.nfc().collect::<String>().to_lowercase()
}

/// One font face's metadata, ready to be written into the cache by
/// `FontCache::replace_folder`. The cache module deliberately does NOT
/// parse fonts — the caller (existing scan path in `app_lib::fonts`,
/// or a test fixture, or future scan code) produces these records and
/// hands them to the cache for persistence. This keeps font-parsing
/// concerns out of the cache module entirely.
#[derive(Debug, Clone)]
pub struct FontMetadata {
    /// Absolute path to the font file.
    pub file_path: String,
    /// File size in bytes from the OS at scan time.
    pub file_size: i64,
    /// File mtime as Unix seconds.
    pub file_mtime: i64,
    /// 0 for non-TTC; >=0 for TrueType Collection (face index inside).
    pub face_index: i32,
    /// Each (family_name, bold, italic) tuple this face advertises.
    /// CJK fonts typically produce multiple entries (Latin + Simplified
    /// Chinese + Traditional + Japanese, etc.) — embed-time lookup must
    /// hit whichever locale's name the subtitle author wrote.
    pub family_keys: Vec<FamilyKey>,
}

/// One (family_name, bold, italic) tuple advertised by a font face.
/// Stored 1:N relative to a `FontMetadata` (one face → multiple keys).
#[derive(Debug, Clone)]
pub struct FamilyKey {
    pub family_name: String,
    pub bold: bool,
    pub italic: bool,
}

/// One row from `cached_folders`, returned by `FontCache::list_folders`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FolderRecord {
    pub folder_path: String,
    pub folder_mtime: i64,
    pub last_scanned_at: i64,
}

/// Drift detection result. Each variant lists folder paths grouped by
/// what change is needed; the caller iterates these to decide actions
/// (rescan modified ones, evict removed ones, scan added ones).
///
/// Empty `added` / `modified` / `removed` collectively mean "cache is
/// in sync with current filesystem state" — caller can use the cache
/// as-is.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DriftReport {
    /// Folders present on the filesystem but not in the cache.
    /// Need a fresh scan to populate cache rows.
    pub added: Vec<String>,
    /// Folders in both cache and filesystem, but `folder_mtime`
    /// differs. Need a rescan to update files added/removed/renamed
    /// inside the folder.
    pub modified: Vec<String>,
    /// Folders in the cache but not on the filesystem (deleted, moved
    /// outside the current source roots, etc.). Need eviction.
    pub removed: Vec<String>,
}

impl DriftReport {
    /// True when the cache is fully in sync with the filesystem
    /// snapshot — no folders need scanning, rescanning, or eviction.
    /// CLI uses this to decide whether to print the drift warning at
    /// startup; GUI uses it to decide whether to show the modal.
    pub fn is_empty(&self) -> bool {
        self.added.is_empty() && self.modified.is_empty() && self.removed.is_empty()
    }
}

/// Lookup result from `FontCache::lookup_family`. Identifies a single
/// font face (`font_path` + `face_index`) — both pieces are needed
/// for subsetting since TTC files require the face index alongside
/// the file path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FontLookupResult {
    pub font_path: String,
    pub face_index: i32,
}

/// Persistent font cache backed by SQLite. One instance per binary
/// (gui vs cli) — the caller chooses the file path.
pub struct FontCache {
    conn: Connection,
}

// Manual Debug impl: rusqlite::Connection doesn't derive Debug, so a
// `#[derive(Debug)]` on FontCache fails to compile. The cache's
// internal state isn't useful in panic messages anyway — knowing
// "FontCache existed when the test panicked" is enough.
impl std::fmt::Debug for FontCache {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FontCache").finish_non_exhaustive()
    }
}

/// Recoverable errors during cache operations. The caller chooses how
/// to react: CLI falls back to no-cache and warns; GUI prompts the user.
///
/// Unified across open/read/write to keep the public API simple — the
/// caller mostly cares about "did it work" + a message; specific
/// variant only matters for `SchemaVersionMismatch` which has its own
/// recovery path.
#[derive(Debug)]
pub enum CacheError {
    /// Filesystem or SQLite-level failure. Includes a human-readable
    /// message embedding the underlying error.
    Io(String),
    /// Existing cache file was opened, but its schema_version row
    /// either doesn't match `SCHEMA_VERSION` (different release) or is
    /// missing entirely (corrupt or pre-versioned cache). Both cases
    /// route to the same recovery path: rebuild the cache.
    /// Sentinels: `found = -1` for "row missing", `-2` for
    /// "row present but unparseable", any other value for "actual
    /// version found in the file".
    SchemaVersionMismatch { found: i32, expected: i32 },
}

impl std::fmt::Display for CacheError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(msg) => write!(f, "cache I/O error: {msg}"),
            Self::SchemaVersionMismatch { found, expected } if *found == -1 => write!(
                f,
                "cache schema_version row missing (cache predates version tracking \
                 or is corrupt); expected version {expected}, must rebuild"
            ),
            Self::SchemaVersionMismatch { found, expected } if *found == -2 => write!(
                f,
                "cache schema_version value unparseable (corrupt cache); \
                 expected version {expected}, must rebuild"
            ),
            Self::SchemaVersionMismatch { found, expected } => write!(
                f,
                "cache schema version {found} does not match expected {expected}; \
                 cache is from a different release and must be rebuilt"
            ),
        }
    }
}

impl FontCache {
    /// Open an existing cache file or create a fresh one. The caller
    /// passes the full file path; choosing AppData / temp / a custom
    /// location is the caller's concern (CLI vs GUI vs tests).
    ///
    /// On a fresh create, the schema is initialized and the current
    /// `SCHEMA_VERSION` is written to `cache_meta`.
    ///
    /// On open of an existing file, the schema_version row is verified
    /// against `SCHEMA_VERSION`. Any mismatch (including missing row)
    /// returns `SchemaVersionMismatch`; the caller decides recovery.
    pub fn open_or_create(cache_path: &Path) -> Result<Self, CacheError> {
        // Ensure the parent directory exists. If the caller passed a
        // path under a not-yet-created folder (e.g., %APPDATA%/ssaHdrify
        // on a fresh user profile), this avoids a confusing
        // SQLITE_CANTOPEN error in favor of a clear filesystem error.
        if let Some(parent) = cache_path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent).map_err(|e| {
                    CacheError::Io(format!(
                        "creating parent directory {}: {e}",
                        parent.display()
                    ))
                })?;
            }
        }

        let already_existed = cache_path.exists();
        let conn = Connection::open(cache_path)
            .map_err(|e| CacheError::Io(format!("opening {}: {e}", cache_path.display())))?;

        // WAL journal mode + 5s busy_timeout matches the existing GUI
        // session DB convention. WAL keeps reader/writer concurrency
        // workable should we ever lift the per-binary-cache locked
        // decision; for now it costs nothing extra and keeps schema
        // patterns consistent across the project.
        conn.pragma_update(None, "journal_mode", "WAL")
            .map_err(|e| CacheError::Io(format!("setting WAL mode: {e}")))?;
        conn.busy_timeout(std::time::Duration::from_secs(5))
            .map_err(|e| CacheError::Io(format!("setting busy_timeout: {e}")))?;
        // Per-connection: SQLite ships with foreign_keys=OFF by default,
        // so the FOREIGN KEY clauses in SCHEMA_SQL would be decorative
        // unless turned on here. The session DB `open_user_font_db`
        // mirrors this PRAGMA for the same reason.
        conn.pragma_update(None, "foreign_keys", "ON")
            .map_err(|e| CacheError::Io(format!("enabling foreign_keys: {e}")))?;

        let cache = Self { conn };
        if already_existed {
            cache.verify_schema_version()?;
        } else {
            cache.init_schema()?;
        }
        Ok(cache)
    }

    /// Initialize an empty cache: create the four tables and write the
    /// current `SCHEMA_VERSION` to `cache_meta`. Called once on fresh
    /// create; idempotent if called on an empty DB but never invoked
    /// after open.
    fn init_schema(&self) -> Result<(), CacheError> {
        self.conn
            .execute_batch(SCHEMA_SQL)
            .map_err(|e| CacheError::Io(format!("initializing schema: {e}")))?;
        self.conn
            .execute(
                "INSERT INTO cache_meta(key, value) VALUES('schema_version', ?1)",
                params![SCHEMA_VERSION.to_string()],
            )
            .map_err(|e| CacheError::Io(format!("writing schema_version: {e}")))?;
        Ok(())
    }

    /// Read the schema_version row and compare against `SCHEMA_VERSION`.
    /// A missing or unparseable row counts as mismatch (cache predates
    /// version tracking, or corrupt).
    fn verify_schema_version(&self) -> Result<(), CacheError> {
        let row: Result<String, _> = self.conn.query_row(
            "SELECT value FROM cache_meta WHERE key = 'schema_version'",
            [],
            |r| r.get(0),
        );
        match row {
            Ok(value) => {
                // Parse failure → use -2 sentinel to distinguish from
                // missing-row's -1, in case future diagnostics want to
                // know which way the data was wrong. Both still route
                // to "rebuild the cache."
                let found: i32 = value.parse().unwrap_or(-2);
                if found != SCHEMA_VERSION {
                    Err(CacheError::SchemaVersionMismatch {
                        found,
                        expected: SCHEMA_VERSION,
                    })
                } else {
                    Ok(())
                }
            }
            Err(rusqlite::Error::QueryReturnedNoRows) => Err(CacheError::SchemaVersionMismatch {
                found: -1,
                expected: SCHEMA_VERSION,
            }),
            Err(e) => Err(CacheError::Io(format!("reading schema_version: {e}"))),
        }
    }

    /// Insert or replace all rows for one folder. Atomic — wraps the
    /// delete-and-rewrite in a single transaction so a partial
    /// failure leaves the previous state intact rather than partial
    /// rows.
    ///
    /// Use cases:
    /// - First-time scan of a folder: cache has no rows for it, this
    ///   inserts them.
    /// - Refresh after drift: cache has stale rows for this folder,
    ///   this replaces them with the current scan output.
    ///
    /// `last_scanned_at` is set to current Unix seconds. The
    /// `folder_mtime` value comes from the caller's `stat()` of the
    /// folder at scan time — it's the value drift detection compares
    /// against on next startup.
    pub fn replace_folder(
        &mut self,
        folder_path: &str,
        folder_mtime: i64,
        fonts: &[FontMetadata],
    ) -> Result<(), CacheError> {
        let tx = self
            .conn
            .transaction()
            .map_err(|e| CacheError::Io(format!("begin transaction: {e}")))?;

        // Delete in dependency order: family_keys → fonts → folder.
        // FK enforcement is on (PRAGMA foreign_keys=ON in
        // open_or_create); reverse order would violate the constraints.
        tx.execute(
            "DELETE FROM cached_family_keys WHERE font_path IN \
             (SELECT font_path FROM cached_fonts WHERE folder_path = ?1)",
            params![folder_path],
        )
        .map_err(|e| CacheError::Io(format!("delete family_keys: {e}")))?;
        tx.execute(
            "DELETE FROM cached_fonts WHERE folder_path = ?1",
            params![folder_path],
        )
        .map_err(|e| CacheError::Io(format!("delete fonts: {e}")))?;
        tx.execute(
            "DELETE FROM cached_folders WHERE folder_path = ?1",
            params![folder_path],
        )
        .map_err(|e| CacheError::Io(format!("delete folder: {e}")))?;

        let now = current_unix_seconds();
        tx.execute(
            "INSERT INTO cached_folders(folder_path, folder_mtime, last_scanned_at) \
             VALUES(?1, ?2, ?3)",
            params![folder_path, folder_mtime, now],
        )
        .map_err(|e| CacheError::Io(format!("insert folder: {e}")))?;

        for font in fonts {
            tx.execute(
                "INSERT INTO cached_fonts(font_path, folder_path, file_size, file_mtime, face_index) \
                 VALUES(?1, ?2, ?3, ?4, ?5)",
                params![
                    font.file_path,
                    folder_path,
                    font.file_size,
                    font.file_mtime,
                    font.face_index,
                ],
            )
            .map_err(|e| CacheError::Io(format!("insert font {}: {e}", font.file_path)))?;

            for key in &font.family_keys {
                let lookup_key = family_lookup_key(&key.family_name);
                tx.execute(
                    "INSERT INTO cached_family_keys(\
                        font_path, face_index, family_name, family_name_key, bold, italic\
                     ) VALUES(?1, ?2, ?3, ?4, ?5, ?6)",
                    params![
                        font.file_path,
                        font.face_index,
                        key.family_name,
                        lookup_key,
                        i32::from(key.bold),
                        i32::from(key.italic),
                    ],
                )
                .map_err(|e| {
                    CacheError::Io(format!("insert family_key for {}: {e}", font.file_path))
                })?;
            }
        }

        tx.commit()
            .map_err(|e| CacheError::Io(format!("commit transaction: {e}")))?;
        Ok(())
    }

    /// Remove all rows for one folder (folder + its fonts + their
    /// family_keys). Atomic via transaction. Use case: drift
    /// detection found this folder is gone from the filesystem.
    pub fn remove_folder(&mut self, folder_path: &str) -> Result<(), CacheError> {
        let tx = self
            .conn
            .transaction()
            .map_err(|e| CacheError::Io(format!("begin transaction: {e}")))?;
        tx.execute(
            "DELETE FROM cached_family_keys WHERE font_path IN \
             (SELECT font_path FROM cached_fonts WHERE folder_path = ?1)",
            params![folder_path],
        )
        .map_err(|e| CacheError::Io(format!("delete family_keys: {e}")))?;
        tx.execute(
            "DELETE FROM cached_fonts WHERE folder_path = ?1",
            params![folder_path],
        )
        .map_err(|e| CacheError::Io(format!("delete fonts: {e}")))?;
        tx.execute(
            "DELETE FROM cached_folders WHERE folder_path = ?1",
            params![folder_path],
        )
        .map_err(|e| CacheError::Io(format!("delete folder: {e}")))?;
        tx.commit()
            .map_err(|e| CacheError::Io(format!("commit transaction: {e}")))?;
        Ok(())
    }

    /// Compare cached folders against a snapshot of currently-existing
    /// folders. Caller is responsible for producing the snapshot
    /// (walking source roots and `stat()`-ing each font-bearing
    /// folder) — keeps filesystem-walking code out of the cache
    /// module so this function is pure / unit-testable.
    ///
    /// The drift categories follow the locked design:
    /// - **added**: in the filesystem snapshot but not in the cache.
    ///   Caller scans these and calls `replace_folder` for each.
    /// - **modified**: in both, but `folder_mtime` differs. Caller
    ///   rescans (catches files added/removed/renamed inside the
    ///   folder per the locked stat-based invalidation strategy).
    /// - **removed**: in the cache but not in the filesystem
    ///   snapshot. Caller calls `remove_folder` for each.
    ///
    /// Folders unchanged (in both with matching mtime) are silently
    /// OK and don't appear in any report list.
    pub fn diff_against(
        &self,
        current_folders: &[(String, i64)],
    ) -> Result<DriftReport, CacheError> {
        // Pre-build a map of cached folders keyed by path. Single
        // O(N) read of cached_folders; subsequent membership checks
        // are O(1).
        let cached: std::collections::HashMap<String, i64> = self
            .list_folders()?
            .into_iter()
            .map(|r| (r.folder_path, r.folder_mtime))
            .collect();

        // Map current folders for O(1) "is this in the snapshot?"
        // lookup when checking the cache side. Last-write-wins on
        // duplicates (caller bug); we don't validate.
        let current: std::collections::HashMap<&str, i64> = current_folders
            .iter()
            .map(|(p, m)| (p.as_str(), *m))
            .collect();

        let mut report = DriftReport::default();

        for (path, current_mtime) in &current {
            match cached.get(*path) {
                None => report.added.push((*path).to_string()),
                Some(cached_mtime) if cached_mtime != current_mtime => {
                    report.modified.push((*path).to_string());
                }
                Some(_) => {
                    // mtime matches — unchanged, no report entry
                }
            }
        }

        for cached_path in cached.keys() {
            if !current.contains_key(cached_path.as_str()) {
                report.removed.push(cached_path.clone());
            }
        }

        // Sort each list for deterministic output (test assertions,
        // reproducible stderr diff reports). Cheap; lists are small.
        report.added.sort();
        report.modified.sort();
        report.removed.sort();

        Ok(report)
    }

    /// Look up a font face by family name + bold/italic flags. Returns
    /// `Some(FontLookupResult { font_path, face_index })` for the
    /// first match, or `None` if no font in the cache advertises the
    /// requested family + style combination.
    ///
    /// Match semantics: NFC-normalize + ASCII-lowercase via
    /// `family_lookup_key` on BOTH the query (here) and the storage
    /// path (`replace_folder`). Bold/italic must match exactly.
    /// Mirrors the session DB's `userFontKey` contract so a font's
    /// name-table form (often NFC) and an ASS file's `\fn` reference
    /// (often macOS-pasted NFD or arbitrary case) match consistently.
    ///
    /// Determinism: when multiple fonts advertise the same family
    /// alias (rare; typically alternate weights or different
    /// foundries' versions of a famous name), the result is sorted
    /// by `(font_path, face_index)` and the first row returned. Same
    /// query gives the same answer across runs.
    ///
    /// Future cleanup item from the design doc: extract a shared
    /// `family_lookup(db_conn, ...)` helper that this method and the
    /// GUI session DB's equivalent can both use. For now the queries
    /// live in their own modules; consolidation is a Step-1-of-real-
    /// implementation task whenever both consumers exist.
    pub fn lookup_family(
        &self,
        family_name: &str,
        bold: bool,
        italic: bool,
    ) -> Result<Option<FontLookupResult>, CacheError> {
        let lookup_key = family_lookup_key(family_name);
        let row: Result<(String, i32), _> = self.conn.query_row(
            "SELECT k.font_path, k.face_index \
             FROM cached_family_keys k \
             WHERE k.family_name_key = ?1 AND k.bold = ?2 AND k.italic = ?3 \
             ORDER BY k.font_path, k.face_index \
             LIMIT 1",
            params![lookup_key, i32::from(bold), i32::from(italic)],
            |r| Ok((r.get(0)?, r.get(1)?)),
        );
        match row {
            Ok((font_path, face_index)) => Ok(Some(FontLookupResult {
                font_path,
                face_index,
            })),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(CacheError::Io(format!("lookup_family: {e}"))),
        }
    }

    /// Cheap existence check for a font path. Used by `subset_font`'s
    /// provenance gate: a path lookup_family returned must also be
    /// recognized as a known scan output, otherwise subset rejects it
    /// as "not discovered by a scan command" — and on a fresh launch
    /// the session DB is empty, so this cache check is the only way
    /// for cross-launch lookup hits to clear provenance without the
    /// user re-adding the source.
    pub fn path_known(&self, font_path: &str) -> Result<bool, CacheError> {
        let row: Result<i64, _> = self.conn.query_row(
            "SELECT 1 FROM cached_fonts WHERE font_path = ?1 LIMIT 1",
            params![font_path],
            |r| r.get(0),
        );
        match row {
            Ok(_) => Ok(true),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(false),
            Err(e) => Err(CacheError::Io(format!("path_known: {e}"))),
        }
    }

    /// List every folder currently tracked in the cache. Used by
    /// drift detection (Step 3) to iterate cached folders and check
    /// each against the filesystem.
    pub fn list_folders(&self) -> Result<Vec<FolderRecord>, CacheError> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT folder_path, folder_mtime, last_scanned_at \
                 FROM cached_folders ORDER BY folder_path",
            )
            .map_err(|e| CacheError::Io(format!("prepare list_folders: {e}")))?;
        let rows = stmt
            .query_map([], |row| {
                Ok(FolderRecord {
                    folder_path: row.get(0)?,
                    folder_mtime: row.get(1)?,
                    last_scanned_at: row.get(2)?,
                })
            })
            .map_err(|e| CacheError::Io(format!("execute list_folders: {e}")))?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row.map_err(|e| CacheError::Io(format!("read row: {e}")))?);
        }
        Ok(out)
    }
}

/// Current Unix timestamp in seconds. Used for `last_scanned_at` on
/// inserts. Returns 0 if the system clock is somehow before the Unix
/// epoch (impossible in practice; defensive default).
///
/// Sentinel collision note: a future caller MUST NOT reinterpret a 0
/// in `last_scanned_at` as "missing" or "uninitialized" — every real
/// insert calls this and gets the current epoch second. The 0 sentinel
/// only fires for SystemTimeError, which can't happen on a sane clock.
fn current_unix_seconds() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// Schema SQL — one statement per table. Tables match the design
/// locked in the design doc § #5.
///
/// `cached_folders.last_scanned_at`: Unix timestamp (seconds since
/// epoch) of when this folder was last walked by `refresh-fonts`. Used
/// for diagnostics ("last refresh: 2 days ago") and for "is this row
/// older than the user's font collection?" sanity checks. NOT used as
/// the primary drift signal — that's `folder_mtime` compared against
/// the live `stat()`.
///
/// `cached_fonts` PK is composite `(font_path, face_index)`: a single
/// TTC file (TrueType Collection) holds multiple faces, each with its
/// own family names and addressable independently for subsetting. The
/// composite key lets one font_path appear N times — once per face.
/// `face_index` is 0 for non-TTC files; >=0 for TTC.
///
/// `cached_family_keys` PK includes `(family_name, bold, italic,
/// font_path, face_index)` so the same face_index of the same file
/// can appear for multiple family aliases — CJK fonts especially
/// advertise family names in several language IDs (Latin + Simplified
/// Chinese + Traditional + Japanese + Korean) on one face. Embed-time
/// lookup must hit whichever locale the subtitle author wrote.
const SCHEMA_SQL: &str = r#"
CREATE TABLE cached_folders (
    folder_path     TEXT PRIMARY KEY,
    folder_mtime    INTEGER NOT NULL,
    last_scanned_at INTEGER NOT NULL
);
CREATE TABLE cached_fonts (
    font_path       TEXT NOT NULL,
    face_index      INTEGER NOT NULL,
    folder_path     TEXT NOT NULL,
    file_size       INTEGER NOT NULL,
    file_mtime      INTEGER NOT NULL,
    PRIMARY KEY (font_path, face_index),
    FOREIGN KEY (folder_path) REFERENCES cached_folders(folder_path)
);
CREATE TABLE cached_family_keys (
    font_path        TEXT NOT NULL,
    face_index       INTEGER NOT NULL,
    family_name      TEXT NOT NULL,
    -- v2: NFC-normalized + ASCII-lowercase form of family_name,
    -- the actual lookup key. family_name kept verbatim for
    -- diagnostics + future case-preserving display.
    family_name_key  TEXT NOT NULL,
    bold             INTEGER NOT NULL,
    italic           INTEGER NOT NULL,
    PRIMARY KEY (family_name_key, bold, italic, font_path, face_index),
    FOREIGN KEY (font_path, face_index) REFERENCES cached_fonts(font_path, face_index)
);
CREATE TABLE cache_meta (
    key             TEXT PRIMARY KEY,
    value           TEXT NOT NULL
);
"#;

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    /// RAII guard for one test's temporary cache directory. Drop
    /// removes the entire dir + WAL sidecars, even on test panic —
    /// the previous bare-PathBuf helper relied on OS temp cleanup
    /// for panic paths and accumulated stale dirs across runs.
    struct TempCacheDir(std::path::PathBuf);

    impl TempCacheDir {
        fn new() -> Self {
            let stamp = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0);
            let dir = std::env::temp_dir().join(format!(
                "ssahdrify-font-cache-test-{}-{}",
                std::process::id(),
                stamp
            ));
            fs::create_dir_all(&dir).expect("create test temp dir");
            Self(dir)
        }

        fn cache_path(&self) -> std::path::PathBuf {
            self.0.join("cache.sqlite3")
        }
    }

    impl Drop for TempCacheDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    /// Convenience for tests that want the path directly. Returns
    /// (guard, path) — guard MUST stay in scope for the test's
    /// duration; binding it as `_` would drop it immediately.
    fn temp_cache_path() -> (TempCacheDir, std::path::PathBuf) {
        let guard = TempCacheDir::new();
        let path = guard.cache_path();
        (guard, path)
    }

    #[test]
    fn fresh_open_creates_schema_and_writes_version() {
        let (_guard, path) = temp_cache_path();
        let cache = FontCache::open_or_create(&path).expect("fresh cache opens");

        // All four tables present.
        let table_count: i32 = cache
            .conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name IN \
                 ('cached_folders', 'cached_fonts', 'cached_family_keys', 'cache_meta')",
                [],
                |r| r.get(0),
            )
            .expect("query schema tables");
        assert_eq!(table_count, 4, "expected all four tables created");

        // schema_version row written.
        let version: String = cache
            .conn
            .query_row(
                "SELECT value FROM cache_meta WHERE key = 'schema_version'",
                [],
                |r| r.get(0),
            )
            .expect("query schema_version");
        assert_eq!(version, SCHEMA_VERSION.to_string());

        let _ = fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn reopen_of_valid_cache_succeeds() {
        let (_guard, path) = temp_cache_path();
        // Create.
        FontCache::open_or_create(&path).expect("first open creates");
        // Reopen.
        FontCache::open_or_create(&path).expect("second open reuses existing");
        let _ = fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn schema_version_mismatch_detected_on_old_cache() {
        let (_guard, path) = temp_cache_path();
        // Create with current version.
        FontCache::open_or_create(&path).expect("first open");
        // Simulate an older release writing version 0.
        {
            let conn = Connection::open(&path).unwrap();
            conn.execute(
                "UPDATE cache_meta SET value = '0' WHERE key = 'schema_version'",
                [],
            )
            .unwrap();
        }
        // Reopen detects mismatch.
        match FontCache::open_or_create(&path) {
            Err(CacheError::SchemaVersionMismatch { found, expected }) => {
                assert_eq!(found, 0);
                assert_eq!(expected, SCHEMA_VERSION);
            }
            other => panic!("expected SchemaVersionMismatch, got {other:?}"),
        }
        let _ = fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn missing_schema_version_row_treated_as_mismatch() {
        let (_guard, path) = temp_cache_path();
        FontCache::open_or_create(&path).expect("first open");
        // Delete the schema_version row to simulate a pre-versioning
        // cache or a corrupt write.
        {
            let conn = Connection::open(&path).unwrap();
            conn.execute("DELETE FROM cache_meta WHERE key = 'schema_version'", [])
                .unwrap();
        }
        match FontCache::open_or_create(&path) {
            Err(CacheError::SchemaVersionMismatch { found, expected }) => {
                assert_eq!(found, -1, "missing row sentinel");
                assert_eq!(expected, SCHEMA_VERSION);
            }
            other => panic!("expected SchemaVersionMismatch, got {other:?}"),
        }
        let _ = fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn unparseable_schema_version_treated_as_mismatch() {
        let (_guard, path) = temp_cache_path();
        FontCache::open_or_create(&path).expect("first open");
        // Write garbage to the version row.
        {
            let conn = Connection::open(&path).unwrap();
            conn.execute(
                "UPDATE cache_meta SET value = 'not-a-number' WHERE key = 'schema_version'",
                [],
            )
            .unwrap();
        }
        match FontCache::open_or_create(&path) {
            Err(CacheError::SchemaVersionMismatch { found, expected }) => {
                assert_eq!(found, -2, "unparseable sentinel");
                assert_eq!(expected, SCHEMA_VERSION);
            }
            other => panic!("expected SchemaVersionMismatch, got {other:?}"),
        }
        let _ = fs::remove_dir_all(path.parent().unwrap());
    }

    /// Synthetic font metadata for tests — no real font file required.
    fn synthetic_font(file_path: &str, family: &str) -> FontMetadata {
        FontMetadata {
            file_path: file_path.to_string(),
            file_size: 100_000,
            file_mtime: 1_700_000_000,
            face_index: 0,
            family_keys: vec![FamilyKey {
                family_name: family.to_string(),
                bold: false,
                italic: false,
            }],
        }
    }

    #[test]
    fn replace_folder_with_no_fonts_inserts_empty_folder_row() {
        let (_guard, path) = temp_cache_path();
        let mut cache = FontCache::open_or_create(&path).expect("open");
        cache
            .replace_folder("/test/empty", 1_700_000_000, &[])
            .expect("replace empty");
        let folders = cache.list_folders().expect("list");
        assert_eq!(folders.len(), 1);
        assert_eq!(folders[0].folder_path, "/test/empty");
        assert_eq!(folders[0].folder_mtime, 1_700_000_000);
        let _ = fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn replace_folder_inserts_fonts_and_family_keys() {
        let (_guard, path) = temp_cache_path();
        let mut cache = FontCache::open_or_create(&path).expect("open");
        let fonts = vec![
            synthetic_font("/test/dir/font_a.otf", "Source Han Sans CN"),
            synthetic_font("/test/dir/font_b.ttf", "Arial"),
        ];
        cache
            .replace_folder("/test/dir", 1_700_000_000, &fonts)
            .expect("replace");

        // Verify font rows.
        let count: i32 = cache
            .conn
            .query_row("SELECT COUNT(*) FROM cached_fonts", [], |r| r.get(0))
            .expect("count fonts");
        assert_eq!(count, 2);

        // Verify family_key rows.
        let count: i32 = cache
            .conn
            .query_row("SELECT COUNT(*) FROM cached_family_keys", [], |r| r.get(0))
            .expect("count keys");
        assert_eq!(count, 2);

        let _ = fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn replace_folder_replaces_previous_rows() {
        let (_guard, path) = temp_cache_path();
        let mut cache = FontCache::open_or_create(&path).expect("open");
        // First scan.
        cache
            .replace_folder(
                "/test/dir",
                1_700_000_000,
                &[
                    synthetic_font("/test/dir/old1.otf", "Old1"),
                    synthetic_font("/test/dir/old2.otf", "Old2"),
                ],
            )
            .expect("first replace");
        // Second scan with different fonts.
        cache
            .replace_folder(
                "/test/dir",
                1_800_000_000,
                &[synthetic_font("/test/dir/new.otf", "New")],
            )
            .expect("second replace");

        // Should have only the new font + key.
        let font_count: i32 = cache
            .conn
            .query_row("SELECT COUNT(*) FROM cached_fonts", [], |r| r.get(0))
            .expect("count fonts");
        assert_eq!(font_count, 1);
        let family: String = cache
            .conn
            .query_row("SELECT family_name FROM cached_family_keys", [], |r| {
                r.get(0)
            })
            .expect("read family");
        assert_eq!(family, "New");
        // Folder mtime should be updated.
        let folders = cache.list_folders().expect("list");
        assert_eq!(folders[0].folder_mtime, 1_800_000_000);

        let _ = fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn replace_folder_with_multiple_family_keys_per_font() {
        // CJK fonts: one face advertises Latin + CJK names.
        let (_guard, path) = temp_cache_path();
        let mut cache = FontCache::open_or_create(&path).expect("open");
        let cjk_font = FontMetadata {
            file_path: "/test/cjk/SourceHanSans.otf".to_string(),
            file_size: 10_000_000,
            file_mtime: 1_700_000_000,
            face_index: 0,
            family_keys: vec![
                FamilyKey {
                    family_name: "Source Han Sans CN".to_string(),
                    bold: false,
                    italic: false,
                },
                FamilyKey {
                    family_name: "思源黑体 CN".to_string(),
                    bold: false,
                    italic: false,
                },
                FamilyKey {
                    family_name: "Noto Sans CJK SC".to_string(),
                    bold: false,
                    italic: false,
                },
            ],
        };
        cache
            .replace_folder("/test/cjk", 1_700_000_000, &[cjk_font])
            .expect("replace");

        let key_count: i32 = cache
            .conn
            .query_row("SELECT COUNT(*) FROM cached_family_keys", [], |r| r.get(0))
            .expect("count keys");
        assert_eq!(key_count, 3, "all three family aliases should be indexed");

        let _ = fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn remove_folder_clears_all_related_rows() {
        let (_guard, path) = temp_cache_path();
        let mut cache = FontCache::open_or_create(&path).expect("open");
        cache
            .replace_folder(
                "/test/a",
                1_700_000_000,
                &[synthetic_font("/test/a/f1.otf", "F1")],
            )
            .expect("replace a");
        cache
            .replace_folder(
                "/test/b",
                1_700_000_000,
                &[synthetic_font("/test/b/f2.otf", "F2")],
            )
            .expect("replace b");

        cache.remove_folder("/test/a").expect("remove a");

        // /test/a's rows gone, /test/b's intact.
        let folders = cache.list_folders().expect("list");
        assert_eq!(folders.len(), 1);
        assert_eq!(folders[0].folder_path, "/test/b");
        let font_count: i32 = cache
            .conn
            .query_row("SELECT COUNT(*) FROM cached_fonts", [], |r| r.get(0))
            .expect("count fonts");
        assert_eq!(font_count, 1);
        let key_count: i32 = cache
            .conn
            .query_row("SELECT COUNT(*) FROM cached_family_keys", [], |r| r.get(0))
            .expect("count keys");
        assert_eq!(key_count, 1);

        let _ = fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn list_folders_returns_in_path_order() {
        let (_guard, path) = temp_cache_path();
        let mut cache = FontCache::open_or_create(&path).expect("open");
        // Insert in non-alphabetical order.
        cache
            .replace_folder("/test/zzz", 1_700_000_000, &[])
            .unwrap();
        cache
            .replace_folder("/test/aaa", 1_700_000_000, &[])
            .unwrap();
        cache
            .replace_folder("/test/mmm", 1_700_000_000, &[])
            .unwrap();
        let folders = cache.list_folders().expect("list");
        let paths: Vec<&str> = folders.iter().map(|f| f.folder_path.as_str()).collect();
        assert_eq!(paths, vec!["/test/aaa", "/test/mmm", "/test/zzz"]);
        let _ = fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn last_scanned_at_set_to_current_time() {
        let (_guard, path) = temp_cache_path();
        let mut cache = FontCache::open_or_create(&path).expect("open");
        let before = current_unix_seconds();
        cache
            .replace_folder("/test/timing", 1_700_000_000, &[])
            .unwrap();
        let after = current_unix_seconds();
        let folders = cache.list_folders().expect("list");
        assert!(folders[0].last_scanned_at >= before);
        assert!(folders[0].last_scanned_at <= after);
        let _ = fs::remove_dir_all(path.parent().unwrap());
    }

    // ── Step 3: drift detection ─────────────────────────────

    #[test]
    fn diff_against_empty_cache_reports_all_as_added() {
        let (_guard, path) = temp_cache_path();
        let cache = FontCache::open_or_create(&path).expect("open");
        let snapshot = vec![("/test/a".to_string(), 100), ("/test/b".to_string(), 200)];
        let report = cache.diff_against(&snapshot).expect("diff");
        assert_eq!(report.added, vec!["/test/a", "/test/b"]);
        assert!(report.modified.is_empty());
        assert!(report.removed.is_empty());
        assert!(!report.is_empty());
        let _ = fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn diff_against_perfect_match_is_empty() {
        let (_guard, path) = temp_cache_path();
        let mut cache = FontCache::open_or_create(&path).expect("open");
        cache.replace_folder("/test/a", 100, &[]).unwrap();
        cache.replace_folder("/test/b", 200, &[]).unwrap();
        let snapshot = vec![("/test/a".to_string(), 100), ("/test/b".to_string(), 200)];
        let report = cache.diff_against(&snapshot).expect("diff");
        assert!(report.is_empty(), "expected no drift, got {report:?}");
        let _ = fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn diff_against_detects_modified_folders_via_mtime() {
        let (_guard, path) = temp_cache_path();
        let mut cache = FontCache::open_or_create(&path).expect("open");
        cache.replace_folder("/test/a", 100, &[]).unwrap();
        cache.replace_folder("/test/b", 200, &[]).unwrap();
        // /test/a's mtime drifted; /test/b unchanged.
        let snapshot = vec![("/test/a".to_string(), 150), ("/test/b".to_string(), 200)];
        let report = cache.diff_against(&snapshot).expect("diff");
        assert_eq!(report.modified, vec!["/test/a"]);
        assert!(report.added.is_empty());
        assert!(report.removed.is_empty());
        let _ = fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn diff_against_detects_removed_folders() {
        let (_guard, path) = temp_cache_path();
        let mut cache = FontCache::open_or_create(&path).expect("open");
        cache.replace_folder("/test/a", 100, &[]).unwrap();
        cache.replace_folder("/test/b", 200, &[]).unwrap();
        // Snapshot only has /test/a; /test/b vanished from FS.
        let snapshot = vec![("/test/a".to_string(), 100)];
        let report = cache.diff_against(&snapshot).expect("diff");
        assert_eq!(report.removed, vec!["/test/b"]);
        assert!(report.added.is_empty());
        assert!(report.modified.is_empty());
        let _ = fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn diff_against_detects_added_folders() {
        let (_guard, path) = temp_cache_path();
        let mut cache = FontCache::open_or_create(&path).expect("open");
        cache.replace_folder("/test/a", 100, &[]).unwrap();
        // Snapshot has /test/a + a new /test/c.
        let snapshot = vec![("/test/a".to_string(), 100), ("/test/c".to_string(), 300)];
        let report = cache.diff_against(&snapshot).expect("diff");
        assert_eq!(report.added, vec!["/test/c"]);
        assert!(report.modified.is_empty());
        assert!(report.removed.is_empty());
        let _ = fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn diff_against_handles_all_three_categories_at_once() {
        let (_guard, path) = temp_cache_path();
        let mut cache = FontCache::open_or_create(&path).expect("open");
        // Cache has a/b/c.
        cache.replace_folder("/test/a", 100, &[]).unwrap();
        cache.replace_folder("/test/b", 200, &[]).unwrap();
        cache.replace_folder("/test/c", 300, &[]).unwrap();
        // Snapshot: a unchanged, b modified, c removed, d added.
        let snapshot = vec![
            ("/test/a".to_string(), 100),
            ("/test/b".to_string(), 250), // mtime drifted
            ("/test/d".to_string(), 400), // new
        ];
        let report = cache.diff_against(&snapshot).expect("diff");
        assert_eq!(report.added, vec!["/test/d"]);
        assert_eq!(report.modified, vec!["/test/b"]);
        assert_eq!(report.removed, vec!["/test/c"]);
        assert!(!report.is_empty());
        let _ = fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn diff_against_lists_are_sorted_for_deterministic_output() {
        let (_guard, path) = temp_cache_path();
        let mut cache = FontCache::open_or_create(&path).expect("open");
        // Cache has folders in non-alpha order.
        cache.replace_folder("/test/zzz", 100, &[]).unwrap();
        cache.replace_folder("/test/aaa", 100, &[]).unwrap();
        // Snapshot adds in non-alpha order; doesn't include the
        // cached ones (so they all become removed).
        let snapshot = vec![
            ("/test/yyy".to_string(), 100),
            ("/test/bbb".to_string(), 100),
            ("/test/mmm".to_string(), 100),
        ];
        let report = cache.diff_against(&snapshot).expect("diff");
        assert_eq!(report.added, vec!["/test/bbb", "/test/mmm", "/test/yyy"]);
        assert_eq!(report.removed, vec!["/test/aaa", "/test/zzz"]);
        let _ = fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn drift_report_is_empty_method() {
        let empty = DriftReport::default();
        assert!(empty.is_empty());
        let with_added = DriftReport {
            added: vec!["x".to_string()],
            ..Default::default()
        };
        assert!(!with_added.is_empty());
        let with_modified = DriftReport {
            modified: vec!["x".to_string()],
            ..Default::default()
        };
        assert!(!with_modified.is_empty());
        let with_removed = DriftReport {
            removed: vec!["x".to_string()],
            ..Default::default()
        };
        assert!(!with_removed.is_empty());
    }

    // ── Step 4: family-name lookup ──────────────────────────

    #[test]
    fn lookup_family_returns_match() {
        let (_guard, path) = temp_cache_path();
        let mut cache = FontCache::open_or_create(&path).expect("open");
        cache
            .replace_folder(
                "/test/dir",
                100,
                &[synthetic_font("/test/dir/arial.ttf", "Arial")],
            )
            .unwrap();
        let result = cache
            .lookup_family("Arial", false, false)
            .expect("lookup")
            .expect("hit expected");
        assert_eq!(result.font_path, "/test/dir/arial.ttf");
        assert_eq!(result.face_index, 0);
        let _ = fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn lookup_family_returns_none_for_missing_family() {
        let (_guard, path) = temp_cache_path();
        let mut cache = FontCache::open_or_create(&path).expect("open");
        cache
            .replace_folder(
                "/test/dir",
                100,
                &[synthetic_font("/test/dir/arial.ttf", "Arial")],
            )
            .unwrap();
        let result = cache
            .lookup_family("Helvetica", false, false)
            .expect("lookup ok");
        assert!(result.is_none(), "expected None, got {result:?}");
        let _ = fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn lookup_family_distinguishes_bold_and_italic() {
        let (_guard, path) = temp_cache_path();
        let mut cache = FontCache::open_or_create(&path).expect("open");
        // Two synthetic faces of "Source Han Sans": regular and bold.
        let regular = FontMetadata {
            file_path: "/test/dir/SHS-Regular.otf".into(),
            file_size: 1_000_000,
            file_mtime: 100,
            face_index: 0,
            family_keys: vec![FamilyKey {
                family_name: "Source Han Sans".into(),
                bold: false,
                italic: false,
            }],
        };
        let bold = FontMetadata {
            file_path: "/test/dir/SHS-Bold.otf".into(),
            file_size: 1_000_000,
            file_mtime: 100,
            face_index: 0,
            family_keys: vec![FamilyKey {
                family_name: "Source Han Sans".into(),
                bold: true,
                italic: false,
            }],
        };
        cache
            .replace_folder("/test/dir", 100, &[regular, bold])
            .unwrap();

        // Regular query hits regular file.
        let r = cache
            .lookup_family("Source Han Sans", false, false)
            .unwrap()
            .unwrap();
        assert_eq!(r.font_path, "/test/dir/SHS-Regular.otf");
        // Bold query hits bold file.
        let b = cache
            .lookup_family("Source Han Sans", true, false)
            .unwrap()
            .unwrap();
        assert_eq!(b.font_path, "/test/dir/SHS-Bold.otf");
        // Italic-not-present query misses.
        let i = cache.lookup_family("Source Han Sans", false, true).unwrap();
        assert!(i.is_none());

        let _ = fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn lookup_family_finds_cjk_alias() {
        // CJK font advertises multiple family aliases on the same face.
        // Lookup must hit any of them.
        let (_guard, path) = temp_cache_path();
        let mut cache = FontCache::open_or_create(&path).expect("open");
        let cjk = FontMetadata {
            file_path: "/test/dir/SourceHanSans.otf".into(),
            file_size: 10_000_000,
            file_mtime: 100,
            face_index: 0,
            family_keys: vec![
                FamilyKey {
                    family_name: "Source Han Sans CN".into(),
                    bold: false,
                    italic: false,
                },
                FamilyKey {
                    family_name: "思源黑体 CN".into(),
                    bold: false,
                    italic: false,
                },
                FamilyKey {
                    family_name: "Noto Sans CJK SC".into(),
                    bold: false,
                    italic: false,
                },
            ],
        };
        cache.replace_folder("/test/dir", 100, &[cjk]).unwrap();

        for name in &["Source Han Sans CN", "思源黑体 CN", "Noto Sans CJK SC"] {
            let result = cache
                .lookup_family(name, false, false)
                .unwrap()
                .unwrap_or_else(|| panic!("expected hit for {name}"));
            assert_eq!(result.font_path, "/test/dir/SourceHanSans.otf");
        }

        let _ = fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn ttc_file_with_multiple_faces_is_supported() {
        // TrueType Collection: one file, multiple faces, each its
        // own family. Schema's composite PK on (font_path,
        // face_index) lets all faces coexist.
        let (_guard, path) = temp_cache_path();
        let mut cache = FontCache::open_or_create(&path).expect("open");
        let mingliu_face0 = FontMetadata {
            file_path: "/test/dir/MingLiU.ttc".into(),
            file_size: 5_000_000,
            file_mtime: 100,
            face_index: 0,
            family_keys: vec![FamilyKey {
                family_name: "MingLiU".into(),
                bold: false,
                italic: false,
            }],
        };
        let mingliu_face1 = FontMetadata {
            file_path: "/test/dir/MingLiU.ttc".into(), // same path
            file_size: 5_000_000,
            file_mtime: 100,
            face_index: 1,
            family_keys: vec![FamilyKey {
                family_name: "PMingLiU".into(),
                bold: false,
                italic: false,
            }],
        };
        cache
            .replace_folder("/test/dir", 100, &[mingliu_face0, mingliu_face1])
            .expect("TTC with 2 faces inserts cleanly");

        // Both family names resolve, each to the right face.
        let m0 = cache
            .lookup_family("MingLiU", false, false)
            .unwrap()
            .unwrap();
        assert_eq!(m0.font_path, "/test/dir/MingLiU.ttc");
        assert_eq!(m0.face_index, 0);
        let m1 = cache
            .lookup_family("PMingLiU", false, false)
            .unwrap()
            .unwrap();
        assert_eq!(m1.font_path, "/test/dir/MingLiU.ttc");
        assert_eq!(m1.face_index, 1);

        let _ = fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn lookup_family_is_deterministic_across_collisions() {
        // Two different files claim the same family name (rare in
        // practice — alternate vendor's "Arial" — but the API must
        // produce the same answer across runs).
        let (_guard, path) = temp_cache_path();
        let mut cache = FontCache::open_or_create(&path).expect("open");
        cache
            .replace_folder(
                "/test/dir",
                100,
                &[
                    synthetic_font("/test/dir/zzz_arial.ttf", "Arial"),
                    synthetic_font("/test/dir/aaa_arial.ttf", "Arial"),
                ],
            )
            .unwrap();
        // ORDER BY font_path → "aaa..." comes first.
        let result = cache.lookup_family("Arial", false, false).unwrap().unwrap();
        assert_eq!(result.font_path, "/test/dir/aaa_arial.ttf");
        let _ = fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn lookup_family_matches_across_case_and_nfc_form() {
        // Round 2 review N-R2-14 / N-R2-28: cache lookup must
        // NFC-normalize + lowercase BOTH the stored key and the
        // query so a font's name-table form (often NFC) matches an
        // ASS file's `\fn` reference regardless of NFD/NFC or case.
        let (_guard, path) = temp_cache_path();
        let mut cache = FontCache::open_or_create(&path).expect("open");
        // Store an NFC-form precomposed family name.
        cache
            .replace_folder(
                "/test/dir",
                100,
                &[synthetic_font("/test/dir/cafe.ttf", "Café")],
            )
            .unwrap();
        // Query in different case → hits.
        let by_case = cache.lookup_family("CAFÉ", false, false).unwrap();
        assert!(
            by_case.is_some(),
            "case-insensitive lookup should match Café"
        );
        // Query in NFD form (decomposed e + combining acute) → hits.
        let nfd = "Cafe\u{0301}";
        let by_nfd = cache.lookup_family(nfd, false, false).unwrap();
        assert!(by_nfd.is_some(), "NFD-form lookup should match NFC store");
        let _ = fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn open_creates_parent_directory_if_missing() {
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let parent = std::env::temp_dir().join(format!(
            "ssahdrify-font-cache-test-mkparent-{}-{}",
            std::process::id(),
            stamp
        ));
        // Don't create the parent — let open_or_create do it.
        let path = parent.join("nested").join("cache.sqlite3");
        FontCache::open_or_create(&path).expect("creates nested parents");
        assert!(path.exists());
        let _ = fs::remove_dir_all(&parent);
    }
}
