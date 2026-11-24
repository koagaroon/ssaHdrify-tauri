use std::collections::HashMap;
use std::path::Path;

use rusqlite::{params, Connection, OpenFlags};

use super::*;

/// Persistent font cache backed by SQLite. One instance per binary
/// (gui vs cli) — the caller chooses the file path.
pub struct FontCache {
    pub(super) conn: Connection,
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
        // path under a not-yet-created folder (e.g., %APPDATA%/ssahdrify
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

        reject_cache_reparse_paths(cache_path)?;

        // Residual TOCTOU window between the `is_reparse_point`
        // check above and the `Connection::open` below — an attacker
        // who can swap `cache_path` to a symlink in this window
        // bypasses the guard. Accepted local-user risk: the per-user AppData
        // location requires an attacker who already has write
        // access to %APPDATA%, which is well outside
        // the single-user-desktop threat model. Sibling acknowledgment
        // lives in `migrate_legacy_gui_cache` (also a single-syscall
        // narrow). Tighter forms would need OS-level
        // `FILE_FLAG_OPEN_REPARSE_POINT` which
        // rusqlite doesn't expose.
        let already_existed = cache_path.exists();
        let conn = Connection::open(cache_path)
            .map_err(|e| cache_sqlite_error(format!("opening {}", cache_path.display()), e))?;

        // Keep the cross-run persistent cache in rollback-journal mode.
        // `diagnose-fonts` has a read-only contract, and a read-only WAL
        // open either needs SQLite sidecar access or an `immutable=1`
        // lie that can ignore committed WAL content. DELETE mode keeps
        // lookup-only diagnostics side-effect-free without sacrificing
        // cache correctness. The session DB in fonts.rs still uses WAL
        // because it is temp/run-local and write-heavy.
        conn.pragma_update(None, "journal_mode", "DELETE")
            .map_err(|e| cache_sqlite_error("setting DELETE journal mode", e))?;
        conn.busy_timeout(std::time::Duration::from_secs(5))
            .map_err(|e| cache_sqlite_error("setting busy_timeout", e))?;
        // Per-connection: SQLite ships with foreign_keys=OFF by default,
        // so the FOREIGN KEY clauses in SCHEMA_SQL would be decorative
        // unless turned on here. The session DB `open_user_font_db`
        // mirrors this PRAGMA for the same reason.
        conn.pragma_update(None, "foreign_keys", "ON")
            .map_err(|e| cache_sqlite_error("enabling foreign_keys", e))?;

        let cache = Self { conn };
        if already_existed {
            cache.verify_schema_version()?;
        } else {
            cache.init_schema()?;
        }
        Ok(cache)
    }

    /// Open an existing cache for lookup-only diagnostics.
    ///
    /// Unlike `open_or_create`, this does not create parent
    /// directories, initialize schema, or change journal mode. Callers
    /// use it when the command contract is read-only (`diagnose-fonts`).
    pub fn open_existing_read_only(cache_path: &Path) -> Result<Self, CacheError> {
        reject_cache_reparse_paths(cache_path)?;
        let conn = Connection::open_with_flags(cache_path, OpenFlags::SQLITE_OPEN_READ_ONLY)
            .map_err(|e| {
                cache_sqlite_error(format!("opening {} read-only", cache_path.display()), e)
            })?;
        conn.busy_timeout(std::time::Duration::from_secs(5))
            .map_err(|e| cache_sqlite_error("setting busy_timeout", e))?;
        conn.pragma_update(None, "foreign_keys", "ON")
            .map_err(|e| cache_sqlite_error("enabling foreign_keys", e))?;

        let cache = Self { conn };
        cache.verify_schema_version()?;
        Ok(cache)
    }

    /// Initialize an empty cache: create the four tables and write the
    /// current `SCHEMA_VERSION` to `cache_meta`. Called once on fresh
    /// create; idempotent if called on an empty DB but never invoked
    /// after open.
    pub(super) fn init_schema(&self) -> Result<(), CacheError> {
        self.conn
            .execute_batch(SCHEMA_SQL)
            .map_err(|e| cache_sqlite_error("initializing schema", e))?;
        self.conn
            .execute(
                "INSERT INTO cache_meta(key, value) VALUES('schema_version', ?1)",
                params![SCHEMA_VERSION.to_string()],
            )
            .map_err(|e| cache_sqlite_error("writing schema_version", e))?;
        Ok(())
    }

    /// Read the schema_version row and compare against `SCHEMA_VERSION`.
    /// A missing or unparseable row counts as mismatch (cache predates
    /// version tracking, or corrupt).
    pub(super) fn verify_schema_version(&self) -> Result<(), CacheError> {
        let has_cache_meta = self
            .conn
            .table_exists(None::<&str>, "cache_meta")
            .map_err(|e| cache_sqlite_error("checking cache_meta table", e))?;
        if !has_cache_meta {
            return Err(CacheError::SchemaVersionMismatch {
                found: -1,
                expected: SCHEMA_VERSION,
            });
        }

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
            Err(e) => Err(cache_sqlite_error("reading schema_version", e)),
        }
    }

    /// Atomically replace one source and every row it owns. Source identity is
    /// `(source_root, scope)`, so overlapping roots and shallow/recursive views
    /// of the same root cannot steal or delete each other's faces.
    pub fn replace_source(
        &mut self,
        snapshot: &CacheSourceSnapshot,
        fonts: &[FontMetadata],
    ) -> Result<(), CacheError> {
        validate_source_snapshot_shape(snapshot, fonts)?;
        let scope = snapshot.scope.db_value();
        let other_source_count: i64 = self
            .conn
            .query_row(
                "SELECT COUNT(*) FROM cached_sources \
                 WHERE NOT (source_root = ?1 AND scope = ?2)",
                params![snapshot.source_root, scope],
                |row| row.get(0),
            )
            .map_err(|e| CacheError::Io(format!("count cached_sources: {e}")))?;
        if usize::try_from(other_source_count).unwrap_or(usize::MAX) >= MAX_CACHED_SOURCES {
            return Err(CacheError::Io(format!(
                "cached_sources is at the {MAX_CACHED_SOURCES}-source sanity cap"
            )));
        }
        let other_directory_count: i64 = self
            .conn
            .query_row(
                "SELECT COUNT(*) FROM cached_directories \
                 WHERE NOT (source_root = ?1 AND scope = ?2)",
                params![snapshot.source_root, scope],
                |row| row.get(0),
            )
            .map_err(|e| CacheError::Io(format!("count cached_directories: {e}")))?;
        checked_projected_count(
            other_directory_count,
            snapshot.directories.len(),
            MAX_CACHED_DIRECTORIES,
            "cached_directories",
        )?;
        let other_file_count: i64 = self
            .conn
            .query_row(
                "SELECT COUNT(*) FROM cached_source_files \
                 WHERE NOT (source_root = ?1 AND scope = ?2)",
                params![snapshot.source_root, scope],
                |row| row.get(0),
            )
            .map_err(|e| CacheError::Io(format!("count cached_source_files: {e}")))?;
        checked_projected_count(
            other_file_count,
            snapshot.files.len(),
            crate::fonts::MAX_PREFLIGHT_ENTRIES,
            "cached_source_files",
        )?;
        let other_path_bytes: i64 = self
            .conn
            .query_row(
                "SELECT \
                   COALESCE((SELECT SUM(length(CAST(source_root AS BLOB))) \
                             FROM cached_sources \
                             WHERE NOT (source_root = ?1 AND scope = ?2)), 0) + \
                   COALESCE((SELECT SUM(length(CAST(directory_path AS BLOB))) \
                             FROM cached_directories \
                             WHERE NOT (source_root = ?1 AND scope = ?2)), 0) + \
                   COALESCE((SELECT SUM(length(CAST(file_path AS BLOB))) \
                             FROM cached_source_files \
                             WHERE NOT (source_root = ?1 AND scope = ?2)), 0)",
                params![snapshot.source_root, scope],
                |row| row.get(0),
            )
            .map_err(|e| CacheError::Io(format!("count cached snapshot path bytes: {e}")))?;
        checked_projected_path_bytes(other_path_bytes, snapshot_retained_path_bytes(snapshot)?)?;

        let now = current_unix_seconds().ok_or_else(|| {
            CacheError::Io("system clock is before Unix epoch (1970-01-01)".to_string())
        })?;
        let source_order: i64 = self
            .conn
            .query_row(
                "SELECT COALESCE(\
                    (SELECT source_order FROM cached_sources \
                     WHERE source_root = ?1 AND scope = ?2), \
                    (SELECT COALESCE(MAX(source_order), 0) + 1 FROM cached_sources)\
                 )",
                params![snapshot.source_root, scope],
                |row| row.get(0),
            )
            .map_err(|e| CacheError::Io(format!("compute source order: {e}")))?;

        let tx = self
            .conn
            .transaction()
            .map_err(|e| CacheError::Io(format!("begin transaction: {e}")))?;
        tx.execute(
            "DELETE FROM cached_sources WHERE source_root = ?1 AND scope = ?2",
            params![snapshot.source_root, scope],
        )
        .map_err(|e| CacheError::Io(format!("delete previous source: {e}")))?;
        tx.execute(
            "INSERT INTO cached_sources(source_root, scope, source_order, last_scanned_at) \
             VALUES(?1, ?2, ?3, ?4)",
            params![snapshot.source_root, scope, source_order, now],
        )
        .map_err(|e| CacheError::Io(format!("insert source: {e}")))?;

        for directory in &snapshot.directories {
            tx.execute(
                "INSERT INTO cached_directories(\
                    source_root, scope, directory_path, directory_mtime\
                 ) VALUES(?1, ?2, ?3, ?4)",
                params![
                    snapshot.source_root,
                    scope,
                    directory.folder_path,
                    directory.folder_mtime
                ],
            )
            .map_err(|e| CacheError::Io(format!("insert tracked directory: {e}")))?;
        }
        for file in &snapshot.files {
            tx.execute(
                "INSERT INTO cached_source_files(\
                    source_root, scope, file_path, file_size, file_mtime\
                 ) VALUES(?1, ?2, ?3, ?4, ?5)",
                params![
                    snapshot.source_root,
                    scope,
                    file.file_path,
                    file.file_size,
                    file.file_mtime
                ],
            )
            .map_err(|e| CacheError::Io(format!("insert candidate file: {e}")))?;
        }
        for font in fonts {
            tx.execute(
                "INSERT INTO cached_fonts(\
                    source_root, scope, font_path, face_index, file_size, file_mtime\
                 ) VALUES(?1, ?2, ?3, ?4, ?5, ?6)",
                params![
                    snapshot.source_root,
                    scope,
                    font.file_path,
                    font.face_index,
                    font.file_size,
                    font.file_mtime,
                ],
            )
            .map_err(|e| CacheError::Io(format!("insert font {}: {e}", font.file_path)))?;

            for key in &font.family_keys {
                tx.execute(
                    "INSERT OR IGNORE INTO cached_family_keys(\
                        source_root, scope, font_path, face_index, family_name, \
                        family_name_key, key_kind, bold, italic\
                     ) VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                    params![
                        snapshot.source_root,
                        scope,
                        font.file_path,
                        font.face_index,
                        key.family_name,
                        family_lookup_key(&key.family_name),
                        KEY_KIND_FAMILY,
                        i32::from(key.bold),
                        i32::from(key.italic),
                    ],
                )
                .map_err(|e| CacheError::Io(format!("insert family key: {e}")))?;
            }
            for alias in &font.face_name_aliases {
                tx.execute(
                    "INSERT OR IGNORE INTO cached_family_keys(\
                        source_root, scope, font_path, face_index, family_name, \
                        family_name_key, key_kind, bold, italic\
                     ) VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, 0, 0)",
                    params![
                        snapshot.source_root,
                        scope,
                        font.file_path,
                        font.face_index,
                        alias,
                        family_lookup_key(alias),
                        KEY_KIND_FACE_ALIAS,
                    ],
                )
                .map_err(|e| CacheError::Io(format!("insert face-name alias: {e}")))?;
            }
        }
        tx.commit()
            .map_err(|e| CacheError::Io(format!("commit source replacement: {e}")))
    }

    /// Delete exactly one `(root, scope)` source and all owned rows. Foreign-key
    /// cascades make this one transaction and preserve overlapping owners.
    pub fn remove_source(
        &mut self,
        source_root: &str,
        scope: FontDirectoryScope,
    ) -> Result<(), CacheError> {
        reject_unsupported_cached_folder_path(source_root)?;
        let tx = self
            .conn
            .transaction()
            .map_err(|e| CacheError::Io(format!("begin transaction: {e}")))?;
        tx.execute(
            "DELETE FROM cached_sources WHERE source_root = ?1 AND scope = ?2",
            params![source_root, scope.db_value()],
        )
        .map_err(|e| CacheError::Io(format!("delete source: {e}")))?;
        tx.commit()
            .map_err(|e| CacheError::Io(format!("commit source removal: {e}")))
    }

    /// Remove every source atomically without dropping/recreating the cache.
    pub fn clear_sources(&mut self) -> Result<usize, CacheError> {
        let tx = self
            .conn
            .transaction()
            .map_err(|e| CacheError::Io(format!("begin transaction: {e}")))?;
        let removed = tx
            .execute("DELETE FROM cached_sources", [])
            .map_err(|e| CacheError::Io(format!("clear cached sources: {e}")))?;
        tx.commit()
            .map_err(|e| CacheError::Io(format!("commit source clear: {e}")))?;
        Ok(removed)
    }

    /// Test-only adapter for legacy shallow fixtures. It deliberately cannot
    /// represent recursive sources; production code must call
    /// [`Self::replace_source`] with a complete snapshot.
    #[cfg(test)]
    pub fn replace_folder(
        &mut self,
        folder_path: &str,
        folder_mtime: i64,
        fonts: &[FontMetadata],
    ) -> Result<(), CacheError> {
        let mut files: Vec<FileSnapshot> = fonts
            .iter()
            .map(|font| FileSnapshot {
                file_path: font.file_path.clone(),
                file_size: font.file_size,
                file_mtime: font.file_mtime,
            })
            .collect();
        files.sort_by(|a, b| a.file_path.cmp(&b.file_path));
        files.dedup_by(|a, b| a.file_path == b.file_path);
        self.replace_source(
            &CacheSourceSnapshot {
                source_root: folder_path.to_string(),
                scope: FontDirectoryScope::Shallow,
                directories: vec![FolderSnapshot {
                    folder_path: folder_path.to_string(),
                    folder_mtime,
                }],
                files,
            },
            fonts,
        )
    }

    /// Test-only adapter for legacy shallow fixtures. Production code must use
    /// [`Self::remove_source`] and preserve the source scope.
    #[cfg(test)]
    pub fn remove_folder(&mut self, folder_path: &str) -> Result<(), CacheError> {
        self.remove_source(folder_path, FontDirectoryScope::Shallow)
    }

    /// Test-only comparison for legacy shallow fixtures. It intentionally drops
    /// recursive scope and nested snapshots; production code must use
    /// [`FontCache::diff_sources`].
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
    #[cfg(test)]
    pub fn diff_against(
        &self,
        current_folders: &[(String, i64)],
    ) -> Result<DriftReport, CacheError> {
        // Pre-build a map of the shallow compatibility view keyed by root path;
        // subsequent membership checks are O(1).
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
                None => report.added.push(CacheSourceKey {
                    source_root: (*path).to_string(),
                    scope: FontDirectoryScope::Shallow,
                }),
                Some(cached_mtime) if cached_mtime != current_mtime => {
                    report.modified.push(CacheSourceKey {
                        source_root: (*path).to_string(),
                        scope: FontDirectoryScope::Shallow,
                    });
                }
                Some(_) => {
                    // mtime matches — unchanged, no report entry
                }
            }
        }

        for cached_path in cached.keys() {
            if !current.contains_key(cached_path.as_str()) {
                report.removed.push(CacheSourceKey {
                    source_root: cached_path.clone(),
                    scope: FontDirectoryScope::Shallow,
                });
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
    /// preferred match, or `None` if no font in the cache advertises the
    /// requested family + style combination.
    ///
    /// Match semantics: NFC-normalize + full Unicode lowercase via
    /// `family_lookup_key` on BOTH the query (here) and the storage
    /// path (`replace_source`). Exact family-name rows must match
    /// bold/italic exactly; full-face/PostScript alias rows are
    /// style-insensitive because the alias already names a concrete
    /// face. Exact family rows sort before alias rows so an alias can
    /// never override a true family/style hit. Mirrors the session
    /// DB lookup contract so a font's
    /// name-table form (often NFC) and an ASS file's `\fn` reference
    /// (which may use decomposed Unicode or arbitrary case) match consistently.
    /// (NOT ASCII-only lowercase — `to_ascii_lowercase` would miss
    /// `É` / `Ñ` / `Ü` and break Latin-extended / CJK lookups.)
    ///
    /// Resolution order matches the in-session database: exact family/style
    /// keys beat face-name aliases; shallow sources beat recursive sources;
    /// the newest source wins within a tier; path and face index break any
    /// remaining tie deterministically. The same source set therefore resolves
    /// the same way before and after an app restart.
    ///
    /// Note on shared helper: persistent cache and session DB use
    /// different schemas (`cached_family_keys` vs `font_family_keys`)
    /// and different column names — extracting a single
    /// `family_lookup(conn, ...)` would require parameterizing the
    /// table/column shape, dragging both consumers' invariants into
    /// one helper without a real win. Both queries are short and
    /// already tested in isolation; intentional duplication.
    pub fn lookup_family(
        &self,
        family_name: &str,
        bold: bool,
        italic: bool,
    ) -> Result<Option<FontLookupResult>, CacheError> {
        let lookup_key = family_lookup_key(family_name);
        let mut statement = self
            .conn
            .prepare(FAMILY_LOOKUP_SQL)
            .map_err(|error| CacheError::Io(format!("prepare lookup_family: {error}")))?;
        let candidate_limit = i64::try_from(MAX_CACHE_LOOKUP_CANDIDATES + 1)
            .expect("cache lookup candidate limit fits i64");
        let mut rows = statement
            .query(params![
                lookup_key,
                KEY_KIND_FAMILY,
                i32::from(bold),
                i32::from(italic),
                KEY_KIND_FACE_ALIAS,
                candidate_limit,
            ])
            .map_err(|error| CacheError::Io(format!("query lookup_family: {error}")))?;

        let mut skipped = 0usize;
        let mut first_validation_error = None;
        while let Some(row) = rows
            .next()
            .map_err(|error| CacheError::Io(format!("read lookup_family row: {error}")))?
        {
            if skipped >= MAX_CACHE_LOOKUP_CANDIDATES {
                warn_skipped_cache_candidates(skipped);
                return Err(first_validation_error.unwrap_or_else(|| {
                    CacheError::Io(format!(
                        "font cache lookup exceeds the {MAX_CACHE_LOOKUP_CANDIDATES}-candidate filesystem-probe budget; rebuild required"
                    ))
                }));
            }
            let candidate = (
                row.get::<_, String>(0),
                row.get::<_, i32>(1),
                row.get::<_, String>(2),
                row.get::<_, i64>(3),
                row.get::<_, i64>(4),
            );
            let (font_path, face_index, folder_path, file_size, file_mtime) = match candidate {
                (Ok(font_path), Ok(face_index), Ok(folder_path), Ok(file_size), Ok(file_mtime)) => {
                    (font_path, face_index, folder_path, file_size, file_mtime)
                }
                _ => {
                    skipped += 1;
                    if first_validation_error.is_none() {
                        first_validation_error = Some(CacheError::Io(
                            "lookup_family row has invalid column types; rebuild required"
                                .to_string(),
                        ));
                    }
                    continue;
                }
            };
            match validate_cached_font_candidate(
                &font_path,
                face_index,
                &folder_path,
                file_size,
                file_mtime,
            ) {
                Ok(()) => {
                    warn_skipped_cache_candidates(skipped);
                    return Ok(Some(FontLookupResult {
                        font_path,
                        face_index,
                    }));
                }
                Err(error) => {
                    skipped += 1;
                    if first_validation_error.is_none() {
                        first_validation_error = Some(error);
                    }
                }
            }
        }

        warn_skipped_cache_candidates(skipped);
        match first_validation_error {
            Some(error) => Err(error),
            None => Ok(None),
        }
    }

    /// List every source with the full directory and candidate-file snapshot.
    pub fn list_sources(&self) -> Result<Vec<CacheSourceRecord>, CacheError> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT source_root, scope, source_order, last_scanned_at \
                 FROM cached_sources ORDER BY source_root, scope",
            )
            .map_err(|e| CacheError::Io(format!("prepare list_sources: {e}")))?;
        let rows = stmt
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, i32>(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, i64>(3)?,
                ))
            })
            .map_err(|e| CacheError::Io(format!("execute list_sources: {e}")))?;
        let mut out = Vec::new();
        let mut total_directory_rows = 0usize;
        let mut total_file_rows = 0usize;
        let mut total_path_bytes = 0usize;
        for row in rows {
            let (source_root, scope_value, source_order, last_scanned_at) =
                row.map_err(|e| CacheError::Io(format!("read source row: {e}")))?;
            reject_unsupported_cached_folder_path(&source_root)?;
            let mut source_path_bytes = source_root.len();
            total_path_bytes = total_path_bytes
                .checked_add(source_root.len())
                .ok_or_else(|| CacheError::Io("cached path-byte count overflowed".to_string()))?;
            enforce_cached_path_byte_budgets(source_path_bytes, total_path_bytes)?;
            let scope = FontDirectoryScope::from_db(scope_value)?;
            let mut directory_stmt = self
                .conn
                .prepare(
                    "SELECT directory_path, directory_mtime FROM cached_directories \
                     WHERE source_root = ?1 AND scope = ?2 ORDER BY directory_path",
                )
                .map_err(|e| CacheError::Io(format!("prepare source directories: {e}")))?;
            let directory_rows = directory_stmt
                .query_map(params![source_root, scope_value], |row| {
                    Ok(FolderSnapshot {
                        folder_path: row.get(0)?,
                        folder_mtime: row.get(1)?,
                    })
                })
                .map_err(|e| CacheError::Io(format!("read source directories: {e}")))?;
            let mut directories = Vec::new();
            for directory in directory_rows {
                let directory =
                    directory.map_err(|e| CacheError::Io(format!("read directory row: {e}")))?;
                reject_unsupported_cached_directory_path(&directory.folder_path)?;
                source_path_bytes = source_path_bytes
                    .checked_add(directory.folder_path.len())
                    .ok_or_else(|| {
                        CacheError::Io("cached source path-byte count overflowed".to_string())
                    })?;
                total_path_bytes = total_path_bytes
                    .checked_add(directory.folder_path.len())
                    .ok_or_else(|| {
                        CacheError::Io("cached path-byte count overflowed".to_string())
                    })?;
                enforce_cached_path_byte_budgets(source_path_bytes, total_path_bytes)?;
                directories.push(directory);
                total_directory_rows = total_directory_rows.saturating_add(1);
                if directories.len() > MAX_CACHED_DIRECTORIES {
                    return Err(CacheError::Io(format!(
                        "cached_directories exceeds the {MAX_CACHED_DIRECTORIES}-row sanity cap"
                    )));
                }
                if total_directory_rows > MAX_CACHED_DIRECTORIES {
                    return Err(CacheError::Io(format!(
                        "cached_directories exceeds the {MAX_CACHED_DIRECTORIES}-row global \
                         sanity cap; cache file appears corrupted or hostile — rebuild required"
                    )));
                }
            }
            let mut file_stmt = self
                .conn
                .prepare(
                    "SELECT file_path, file_size, file_mtime FROM cached_source_files \
                     WHERE source_root = ?1 AND scope = ?2 ORDER BY file_path",
                )
                .map_err(|e| CacheError::Io(format!("prepare source files: {e}")))?;
            let file_rows = file_stmt
                .query_map(params![source_root, scope_value], |row| {
                    Ok(FileSnapshot {
                        file_path: row.get(0)?,
                        file_size: row.get(1)?,
                        file_mtime: row.get(2)?,
                    })
                })
                .map_err(|e| CacheError::Io(format!("read source files: {e}")))?;
            let mut files = Vec::new();
            for file in file_rows {
                let file = file.map_err(|e| CacheError::Io(format!("read file row: {e}")))?;
                reject_unsupported_cached_font_path(&file.file_path)?;
                source_path_bytes = source_path_bytes
                    .checked_add(file.file_path.len())
                    .ok_or_else(|| {
                        CacheError::Io("cached source path-byte count overflowed".to_string())
                    })?;
                total_path_bytes = total_path_bytes
                    .checked_add(file.file_path.len())
                    .ok_or_else(|| {
                        CacheError::Io("cached path-byte count overflowed".to_string())
                    })?;
                enforce_cached_path_byte_budgets(source_path_bytes, total_path_bytes)?;
                files.push(file);
                total_file_rows = total_file_rows.saturating_add(1);
                if files.len() > crate::fonts::MAX_SCAN_FONT_FILES {
                    return Err(CacheError::Io(format!(
                        "one cached source exceeds the {}-candidate-file limit; rebuild required",
                        crate::fonts::MAX_SCAN_FONT_FILES
                    )));
                }
                if total_file_rows > crate::fonts::MAX_PREFLIGHT_ENTRIES {
                    return Err(CacheError::Io(format!(
                        "cached_source_files exceeds the {}-row global sanity cap; \
                         cache file appears corrupted or hostile — rebuild required",
                        crate::fonts::MAX_PREFLIGHT_ENTRIES
                    )));
                }
            }
            out.push(CacheSourceRecord {
                source_root,
                scope,
                source_order,
                last_scanned_at,
                directories,
                files,
            });
            if out.len() > MAX_CACHED_SOURCES {
                return Err(CacheError::Io(format!(
                    "cached_sources table exceeds {MAX_CACHED_SOURCES}-row sanity cap; \
                     cache file appears corrupted or hostile — rebuild required"
                )));
            }
        }
        Ok(out)
    }

    /// Compare complete source snapshots. Any nested directory or candidate-
    /// file delta collapses to the owning source key in `modified`.
    pub fn diff_sources(
        &self,
        current_sources: &[CacheSourceSnapshot],
    ) -> Result<DriftReport, CacheError> {
        let cached: HashMap<CacheSourceKey, (Vec<FolderSnapshot>, Vec<FileSnapshot>)> = self
            .list_sources()?
            .into_iter()
            .map(|source| (source.key(), (source.directories, source.files)))
            .collect();
        let current: HashMap<CacheSourceKey, (&[FolderSnapshot], &[FileSnapshot])> =
            current_sources
                .iter()
                .map(|source| {
                    (
                        source.key(),
                        (source.directories.as_slice(), source.files.as_slice()),
                    )
                })
                .collect();
        let mut report = DriftReport::default();
        for (key, (directories, files)) in &current {
            match cached.get(key) {
                None => report.added.push(key.clone()),
                Some((cached_directories, cached_files))
                    if cached_directories.as_slice() != *directories
                        || cached_files.as_slice() != *files =>
                {
                    report.modified.push(key.clone());
                }
                Some(_) => {}
            }
        }
        for key in cached.keys() {
            if !current.contains_key(key) {
                report.removed.push(key.clone());
            }
        }
        report.added.sort();
        report.modified.sort();
        report.removed.sort();
        Ok(report)
    }

    /// Test-only shallow-root view for legacy fixtures. Production code must
    /// call [`Self::list_sources`] so scope and nested snapshots are retained.
    #[cfg(test)]
    pub fn list_folders(&self) -> Result<Vec<FolderRecord>, CacheError> {
        self.list_sources()?
            .into_iter()
            .map(|source| {
                let root_mtime = source
                    .directories
                    .iter()
                    .find(|directory| directory.folder_path == source.source_root)
                    .map(|directory| directory.folder_mtime)
                    .ok_or_else(|| CacheError::Io("source root snapshot missing".to_string()))?;
                Ok(FolderRecord {
                    folder_path: source.source_root,
                    folder_mtime: root_mtime,
                    last_scanned_at: source.last_scanned_at,
                })
            })
            .collect()
    }
}

/// Current Unix timestamp in seconds. Used for `last_scanned_at` on
/// inserts. Returns `None` if the system clock is somehow before the
/// Unix epoch — impossible in practice, but the symmetric posture
/// matches `try_modified_at` (that helper was tightened from
/// `.unwrap_or(0)` to `.ok()?` for the same sentinel-collision
/// concern). Callers surface `None` as `CacheError::Io` with a
/// "system clock before Unix epoch" message rather than persisting
/// epoch-zero into SQLite.
pub(super) fn current_unix_seconds() -> Option<i64> {
    // `i64::try_from` (not `as i64`) for posture symmetry
    // with `try_modified_at`. The cast can't overflow until year ~292
    // billion, but the typed conversion makes the "this could fail"
    // contract type-level explicit rather than relying on doc
    // discipline.
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .and_then(|d| i64::try_from(d.as_secs()).ok())
}

/// Schema SQL — one statement per table. Tables match the current
/// persistent font-cache layout.
///
/// `cached_sources.last_scanned_at` is a diagnostic Unix-seconds timestamp.
/// Freshness instead compares nanosecond mtimes and sizes stored in
/// `cached_directories` and `cached_source_files`.
///
/// Every owned table includes `(source_root, scope)` in its key. A single
/// physical face may therefore belong to overlapping sources without either
/// source stealing the other's row. `cached_fonts` additionally keys by
/// `(font_path, face_index)`: a single
/// TTC file (TrueType Collection) holds multiple faces, each with its
/// own family names and addressable independently for subsetting. The
/// composite key lets one font_path appear N times — once per face.
/// `face_index` is 0 for non-TTC files; >=0 for TTC.
///
/// `cached_family_keys` PK includes normalized family/style identity plus
/// source + face identity, so the same face_index of the same file
/// can appear for multiple family aliases — CJK fonts especially
/// advertise family names in several language IDs (Latin + Simplified
/// Chinese + Traditional + Japanese + Korean) on one face. Embed-time
/// lookup must hit whichever locale the subtitle author wrote.
pub(super) const SCHEMA_SQL: &str = r#"
CREATE TABLE cached_sources (
    source_root     TEXT NOT NULL,
    scope           INTEGER NOT NULL CHECK(scope IN (0, 1)),
    source_order    INTEGER NOT NULL UNIQUE,
    last_scanned_at INTEGER NOT NULL,
    PRIMARY KEY (source_root, scope)
);
CREATE TABLE cached_directories (
    source_root     TEXT NOT NULL,
    scope           INTEGER NOT NULL,
    directory_path TEXT NOT NULL,
    directory_mtime INTEGER NOT NULL,
    PRIMARY KEY (source_root, scope, directory_path),
    FOREIGN KEY (source_root, scope) REFERENCES cached_sources(source_root, scope)
        ON DELETE CASCADE
);
CREATE TABLE cached_source_files (
    source_root     TEXT NOT NULL,
    scope           INTEGER NOT NULL,
    file_path       TEXT NOT NULL,
    file_size       INTEGER NOT NULL,
    file_mtime      INTEGER NOT NULL,
    PRIMARY KEY (source_root, scope, file_path),
    FOREIGN KEY (source_root, scope) REFERENCES cached_sources(source_root, scope)
        ON DELETE CASCADE
);
CREATE TABLE cached_fonts (
    source_root     TEXT NOT NULL,
    scope           INTEGER NOT NULL,
    font_path       TEXT NOT NULL,
    face_index      INTEGER NOT NULL CHECK(face_index >= 0),
    file_size       INTEGER NOT NULL CHECK(file_size >= 0),
    file_mtime      INTEGER NOT NULL CHECK(file_mtime >= 0),
    PRIMARY KEY (source_root, scope, font_path, face_index),
    FOREIGN KEY (source_root, scope) REFERENCES cached_sources(source_root, scope)
        ON DELETE CASCADE,
    FOREIGN KEY (source_root, scope, font_path)
        REFERENCES cached_source_files(source_root, scope, file_path)
        ON DELETE CASCADE
);
CREATE TABLE cached_family_keys (
    source_root      TEXT NOT NULL,
    scope             INTEGER NOT NULL,
    font_path         TEXT NOT NULL,
    face_index        INTEGER NOT NULL,
    family_name       TEXT NOT NULL,
    family_name_key   TEXT NOT NULL,
    key_kind          INTEGER NOT NULL CHECK(key_kind IN (0, 1)),
    bold              INTEGER NOT NULL,
    italic            INTEGER NOT NULL,
    PRIMARY KEY (
        source_root, scope, family_name_key, key_kind, bold, italic,
        font_path, face_index
    ),
    FOREIGN KEY (source_root, scope, font_path, face_index)
        REFERENCES cached_fonts(source_root, scope, font_path, face_index)
        ON DELETE CASCADE
);
CREATE INDEX idx_cached_family_lookup ON cached_family_keys(
    family_name_key, key_kind, bold, italic,
    source_root, scope, font_path, face_index
);
CREATE TABLE cache_meta (
    key             TEXT PRIMARY KEY,
    value           TEXT NOT NULL
);
"#;
