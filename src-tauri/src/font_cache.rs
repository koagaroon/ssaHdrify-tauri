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

use std::path::Path;

use rusqlite::{params, Connection};

/// Schema version. Bumped when any table layout changes; mismatch on
/// open returns `CacheError::SchemaVersionMismatch` so the caller
/// can rebuild (CLI: drift-equivalent fallback to no-cache; GUI:
/// prompt). Per the locked "no auto-migrate" decision, the cache is
/// never silently migrated — release notes call out version bumps so
/// users intentionally rebuild via `refresh-fonts` or the GUI modal.
pub const SCHEMA_VERSION: i32 = 1;

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
        let conn = Connection::open(cache_path).map_err(|e| {
            CacheError::Io(format!("opening {}: {e}", cache_path.display()))
        })?;

        // WAL journal mode + 5s busy_timeout matches the existing GUI
        // session DB convention. WAL keeps reader/writer concurrency
        // workable should we ever lift the per-binary-cache locked
        // decision; for now it costs nothing extra and keeps schema
        // patterns consistent across the project.
        conn.pragma_update(None, "journal_mode", "WAL")
            .map_err(|e| CacheError::Io(format!("setting WAL mode: {e}")))?;
        conn.busy_timeout(std::time::Duration::from_secs(5))
            .map_err(|e| CacheError::Io(format!("setting busy_timeout: {e}")))?;

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
            Err(rusqlite::Error::QueryReturnedNoRows) => {
                Err(CacheError::SchemaVersionMismatch {
                    found: -1,
                    expected: SCHEMA_VERSION,
                })
            }
            Err(e) => Err(CacheError::Io(format!(
                "reading schema_version: {e}"
            ))),
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
        // Foreign keys aren't enforced (no PRAGMA foreign_keys=ON in
        // open_or_create) but the deletion order keeps the row
        // graph consistent for any future enforcement.
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
                tx.execute(
                    "INSERT INTO cached_family_keys(font_path, family_name, bold, italic) \
                     VALUES(?1, ?2, ?3, ?4)",
                    params![
                        font.file_path,
                        key.family_name,
                        i32::from(key.bold),
                        i32::from(key.italic),
                    ],
                )
                .map_err(|e| CacheError::Io(format!(
                    "insert family_key for {}: {e}",
                    font.file_path
                )))?;
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
/// `cached_fonts.face_index`: 0 for non-TTC fonts; >=0 for TrueType
/// Collections. Identifies which face inside a TTC file the row
/// describes.
///
/// `cached_family_keys`: composite primary key on (family_name, bold,
/// italic, font_path). One font face produces multiple rows here —
/// CJK fonts especially advertise family names in several language
/// IDs (Latin transliteration + Simplified Chinese + Traditional +
/// Japanese + Korean), and embed-time lookup must hit the family name
/// the subtitle author wrote regardless of which locale that was.
const SCHEMA_SQL: &str = r#"
CREATE TABLE cached_folders (
    folder_path     TEXT PRIMARY KEY,
    folder_mtime    INTEGER NOT NULL,
    last_scanned_at INTEGER NOT NULL
);
CREATE TABLE cached_fonts (
    font_path       TEXT PRIMARY KEY,
    folder_path     TEXT NOT NULL,
    file_size       INTEGER NOT NULL,
    file_mtime      INTEGER NOT NULL,
    face_index      INTEGER NOT NULL,
    FOREIGN KEY (folder_path) REFERENCES cached_folders(folder_path)
);
CREATE TABLE cached_family_keys (
    font_path       TEXT NOT NULL,
    family_name     TEXT NOT NULL,
    bold            INTEGER NOT NULL,
    italic          INTEGER NOT NULL,
    PRIMARY KEY (family_name, bold, italic, font_path),
    FOREIGN KEY (font_path) REFERENCES cached_fonts(font_path)
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

    /// Generate a unique cache file path under the OS temp dir for one
    /// test. Caller is responsible for removing the parent directory
    /// after the test (best-effort; OS cleanup catches anything left).
    fn temp_cache_path() -> std::path::PathBuf {
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
        dir.join("cache.sqlite3")
    }

    #[test]
    fn fresh_open_creates_schema_and_writes_version() {
        let path = temp_cache_path();
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
        let path = temp_cache_path();
        // Create.
        FontCache::open_or_create(&path).expect("first open creates");
        // Reopen.
        FontCache::open_or_create(&path).expect("second open reuses existing");
        let _ = fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn schema_version_mismatch_detected_on_old_cache() {
        let path = temp_cache_path();
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
        let path = temp_cache_path();
        FontCache::open_or_create(&path).expect("first open");
        // Delete the schema_version row to simulate a pre-versioning
        // cache or a corrupt write.
        {
            let conn = Connection::open(&path).unwrap();
            conn.execute(
                "DELETE FROM cache_meta WHERE key = 'schema_version'",
                [],
            )
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
        let path = temp_cache_path();
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
        let path = temp_cache_path();
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
        let path = temp_cache_path();
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
        let path = temp_cache_path();
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
            .query_row(
                "SELECT family_name FROM cached_family_keys",
                [],
                |r| r.get(0),
            )
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
        let path = temp_cache_path();
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
        let path = temp_cache_path();
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
        let path = temp_cache_path();
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
        let path = temp_cache_path();
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
        let path = temp_cache_path();
        let cache = FontCache::open_or_create(&path).expect("open");
        let snapshot = vec![
            ("/test/a".to_string(), 100),
            ("/test/b".to_string(), 200),
        ];
        let report = cache.diff_against(&snapshot).expect("diff");
        assert_eq!(report.added, vec!["/test/a", "/test/b"]);
        assert!(report.modified.is_empty());
        assert!(report.removed.is_empty());
        assert!(!report.is_empty());
        let _ = fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn diff_against_perfect_match_is_empty() {
        let path = temp_cache_path();
        let mut cache = FontCache::open_or_create(&path).expect("open");
        cache.replace_folder("/test/a", 100, &[]).unwrap();
        cache.replace_folder("/test/b", 200, &[]).unwrap();
        let snapshot = vec![
            ("/test/a".to_string(), 100),
            ("/test/b".to_string(), 200),
        ];
        let report = cache.diff_against(&snapshot).expect("diff");
        assert!(report.is_empty(), "expected no drift, got {report:?}");
        let _ = fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn diff_against_detects_modified_folders_via_mtime() {
        let path = temp_cache_path();
        let mut cache = FontCache::open_or_create(&path).expect("open");
        cache.replace_folder("/test/a", 100, &[]).unwrap();
        cache.replace_folder("/test/b", 200, &[]).unwrap();
        // /test/a's mtime drifted; /test/b unchanged.
        let snapshot = vec![
            ("/test/a".to_string(), 150),
            ("/test/b".to_string(), 200),
        ];
        let report = cache.diff_against(&snapshot).expect("diff");
        assert_eq!(report.modified, vec!["/test/a"]);
        assert!(report.added.is_empty());
        assert!(report.removed.is_empty());
        let _ = fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn diff_against_detects_removed_folders() {
        let path = temp_cache_path();
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
        let path = temp_cache_path();
        let mut cache = FontCache::open_or_create(&path).expect("open");
        cache.replace_folder("/test/a", 100, &[]).unwrap();
        // Snapshot has /test/a + a new /test/c.
        let snapshot = vec![
            ("/test/a".to_string(), 100),
            ("/test/c".to_string(), 300),
        ];
        let report = cache.diff_against(&snapshot).expect("diff");
        assert_eq!(report.added, vec!["/test/c"]);
        assert!(report.modified.is_empty());
        assert!(report.removed.is_empty());
        let _ = fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn diff_against_handles_all_three_categories_at_once() {
        let path = temp_cache_path();
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
        let path = temp_cache_path();
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
