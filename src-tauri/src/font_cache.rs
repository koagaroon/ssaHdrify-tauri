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
/// open returns `CacheOpenError::SchemaVersionMismatch` so the caller
/// can rebuild (CLI: drift-equivalent fallback to no-cache; GUI:
/// prompt). Per the locked "no auto-migrate" decision, the cache is
/// never silently migrated — release notes call out version bumps so
/// users intentionally rebuild via `refresh-fonts` or the GUI modal.
pub const SCHEMA_VERSION: i32 = 1;

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

/// Recoverable errors when opening or creating the cache. The caller
/// chooses how to react: CLI falls back to no-cache and warns; GUI
/// prompts the user.
#[derive(Debug)]
pub enum CacheOpenError {
    /// Filesystem or SQLite-level failure. Includes a human-readable
    /// message embedding the underlying error.
    Io(String),
    /// Existing cache file was opened, but its schema_version row
    /// either doesn't match `SCHEMA_VERSION` (different release) or is
    /// missing entirely (corrupt or pre-versioned cache). Both cases
    /// route to the same recovery path: rebuild the cache. `found =
    /// -1` is the sentinel for "row missing"; any other negative or
    /// positive value comes from the cache file itself.
    SchemaVersionMismatch { found: i32, expected: i32 },
}

impl std::fmt::Display for CacheOpenError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(msg) => write!(f, "cache I/O error: {msg}"),
            Self::SchemaVersionMismatch { found, expected } if *found == -1 => write!(
                f,
                "cache schema_version row missing (cache predates version tracking \
                 or is corrupt); expected version {expected}, must rebuild"
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
    pub fn open_or_create(cache_path: &Path) -> Result<Self, CacheOpenError> {
        // Ensure the parent directory exists. If the caller passed a
        // path under a not-yet-created folder (e.g., %APPDATA%/ssaHdrify
        // on a fresh user profile), this avoids a confusing
        // SQLITE_CANTOPEN error in favor of a clear filesystem error.
        if let Some(parent) = cache_path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent).map_err(|e| {
                    CacheOpenError::Io(format!(
                        "creating parent directory {}: {e}",
                        parent.display()
                    ))
                })?;
            }
        }

        let already_existed = cache_path.exists();
        let conn = Connection::open(cache_path).map_err(|e| {
            CacheOpenError::Io(format!("opening {}: {e}", cache_path.display()))
        })?;

        // WAL journal mode + 5s busy_timeout matches the existing GUI
        // session DB convention. WAL keeps reader/writer concurrency
        // workable should we ever lift the per-binary-cache locked
        // decision; for now it costs nothing extra and keeps schema
        // patterns consistent across the project.
        conn.pragma_update(None, "journal_mode", "WAL")
            .map_err(|e| CacheOpenError::Io(format!("setting WAL mode: {e}")))?;
        conn.busy_timeout(std::time::Duration::from_secs(5))
            .map_err(|e| CacheOpenError::Io(format!("setting busy_timeout: {e}")))?;

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
    fn init_schema(&self) -> Result<(), CacheOpenError> {
        self.conn
            .execute_batch(SCHEMA_SQL)
            .map_err(|e| CacheOpenError::Io(format!("initializing schema: {e}")))?;
        self.conn
            .execute(
                "INSERT INTO cache_meta(key, value) VALUES('schema_version', ?1)",
                params![SCHEMA_VERSION.to_string()],
            )
            .map_err(|e| CacheOpenError::Io(format!("writing schema_version: {e}")))?;
        Ok(())
    }

    /// Read the schema_version row and compare against `SCHEMA_VERSION`.
    /// A missing or unparseable row counts as mismatch (cache predates
    /// version tracking, or corrupt).
    fn verify_schema_version(&self) -> Result<(), CacheOpenError> {
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
                    Err(CacheOpenError::SchemaVersionMismatch {
                        found,
                        expected: SCHEMA_VERSION,
                    })
                } else {
                    Ok(())
                }
            }
            Err(rusqlite::Error::QueryReturnedNoRows) => {
                Err(CacheOpenError::SchemaVersionMismatch {
                    found: -1,
                    expected: SCHEMA_VERSION,
                })
            }
            Err(e) => Err(CacheOpenError::Io(format!(
                "reading schema_version: {e}"
            ))),
        }
    }
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
            Err(CacheOpenError::SchemaVersionMismatch { found, expected }) => {
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
            Err(CacheOpenError::SchemaVersionMismatch { found, expected }) => {
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
            Err(CacheOpenError::SchemaVersionMismatch { found, expected }) => {
                assert_eq!(found, -2, "unparseable sentinel");
                assert_eq!(expected, SCHEMA_VERSION);
            }
            other => panic!("expected SchemaVersionMismatch, got {other:?}"),
        }
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
