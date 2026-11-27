use super::*;
use rusqlite::{params, Connection};
use std::{fs, path::Path};

/// RAII guard for one test's temporary cache directory. Drop
/// removes the entire dir + WAL sidecars, even on test panic —
/// the previous bare-PathBuf helper relied on OS temp cleanup
/// for panic paths and accumulated stale dirs across runs.
///
/// Cross-reference: a sibling `TempCacheDir` lives at
/// `font_cache_commands.rs::tests::TempCacheDir` for the GUI-side
/// IPC tests. Same posture, NOT identical: the commands-side
/// version takes a `name: &str` constructor arg, whereas this one
/// takes no args. Both combine `PID + UNIX nanos + AtomicU64` and
/// `fs::create_dir` so parallel test threads on coarse-tick clocks
/// (e.g. 100ns Windows ticks) can never share a directory. Don't
/// conflate the two when reading either side's WHY block.
///
/// Other test modules (`dropzone.rs::tests`,
/// `safe_io.rs::tests::temp_dir`, `fonts.rs::tests`) use inline
/// `std::env::temp_dir().join(...)` without RAII — those test
/// bodies rely on per-test `fs::remove_dir_all` calls. A refactor
/// extracting a single shared `#[cfg(test)] mod test_helpers`
/// would need to harmonize the suffix shapes (each module's
/// per-suite prefix carries diagnostic value when stale dirs need
/// attribution); the cost-benefit hasn't tipped toward
/// consolidation.
struct TempCacheDir(std::path::PathBuf);

impl TempCacheDir {
    fn new() -> Self {
        static NEXT_ID: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let seq = NEXT_ID.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let base = std::env::temp_dir();
        fs::create_dir_all(&base).expect("ensure OS temp dir");
        let dir = base.join(format!(
            "ssahdrify-font-cache-test-{}-{stamp}-{seq}",
            std::process::id(),
        ));
        fs::create_dir(&dir).expect("create unique test temp dir");
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

/// Like `temp_cache_path`, but the cache file path lives one
/// directory deeper than the guard — used to verify
/// `open_or_create` creates missing parent directories. The guard
/// owns the OUTER directory and Drop-cleans the entire tree.
fn temp_nested_cache_path() -> (TempCacheDir, std::path::PathBuf) {
    let guard = TempCacheDir::new();
    let nested_path = guard.0.join("nested").join("cache.sqlite3");
    (guard, nested_path)
}

fn sqlite_sidecar_path(path: &Path, suffix: &str) -> std::path::PathBuf {
    let mut sidecar = path.as_os_str().to_os_string();
    sidecar.push(suffix);
    std::path::PathBuf::from(sidecar)
}

fn sqlite_failure(code: i32) -> rusqlite::Error {
    rusqlite::Error::SqliteFailure(rusqlite::ffi::Error::new(code), None)
}

#[test]
fn bundled_sqlite_includes_required_recovery_hardening() {
    let bundle: serde_json::Value = serde_json::from_str(include_str!("../../sqlite-bundle.json"))
        .expect("reviewed SQLite bundle metadata must parse");
    assert!(
        rusqlite::version_number() >= 3_053_004,
        "SQLite {} predates the required recovery hardening",
        rusqlite::version()
    );
    assert_eq!(
        Some(rusqlite::version()),
        bundle["sqlite"]["version"].as_str(),
        "the linked SQLite version must match the reviewed bundle"
    );
    let conn = Connection::open_in_memory().expect("open SQLite probe");
    let source_id: String = conn
        .query_row("SELECT sqlite_source_id()", [], |row| row.get(0))
        .expect("read the linked SQLite source ID");
    assert_eq!(
        source_id,
        rusqlite::ffi::SQLITE_SOURCE_ID.to_str().unwrap(),
        "the linked SQLite library must match the reviewed bundled source"
    );
    assert_eq!(
        Some(source_id.as_str()),
        bundle["sqlite"]["sourceId"].as_str(),
        "the linked source ID must match the reviewed provenance"
    );
}

enum SuperJournalFixture {
    None,
    UnrelatedFilename,
    MissingBacklink,
    Valid,
}

fn assert_hot_journal_recovery(fixture: SuperJournalFixture) {
    const JOURNAL_MAGIC: [u8; 8] = [0xd9, 0xd5, 0x05, 0xf9, 0x20, 0xa1, 0x63, 0xd7];
    let (guard, path) = temp_cache_path();
    let cache = FontCache::open_or_create(&path).expect("create recovery fixture");
    cache
        .conn
        .execute(
            "INSERT INTO cache_meta(key, value) VALUES('recovery_probe', 'before')",
            [],
        )
        .unwrap();
    let journal_name = format!("{}-journal", cache.conn.path().unwrap());
    let journal_path = std::path::PathBuf::from(&journal_name);
    let page_size: u32 = cache
        .conn
        .pragma_query_value(None, "page_size", |row| row.get(0))
        .unwrap();
    let committed = fs::read(&path).unwrap();
    cache
        .conn
        .execute_batch(
            "PRAGMA synchronous=OFF; BEGIN IMMEDIATE; \
             UPDATE cache_meta SET value='after' WHERE key='recovery_probe';",
        )
        .unwrap();
    cache.conn.cache_flush().expect("flush uncommitted pages");
    let dirty = fs::read(&path).unwrap();
    let mut journal = fs::read(&journal_path).unwrap();
    assert_ne!(dirty, committed, "recovery must undo an on-disk change");
    assert!(journal.starts_with(&JOURNAL_MAGIC));
    // Save/close/restore follows SQLite's mjournal.test without crashing a process.
    drop(cache);

    let referenced = match fixture {
        SuperJournalFixture::None => None,
        _ => {
            let name = if matches!(fixture, SuperJournalFixture::UnrelatedFilename) {
                "keep-me.txt"
            } else {
                "cache.sqlite3-mj1234569AA"
            };
            let target = guard.0.join(name);
            let child = if matches!(fixture, SuperJournalFixture::MissingBacklink) {
                guard.0.join("unrelated.sqlite3-journal")
            } else {
                journal_path.clone()
            };
            let mut contents = child.to_str().unwrap().as_bytes().to_vec();
            contents.push(0);
            fs::write(&target, &contents).unwrap();
            let target_bytes = target.to_str().unwrap().as_bytes();
            let checksum = target_bytes.iter().fold(0u32, |sum, byte| {
                sum.wrapping_add(i32::from(*byte as i8) as u32)
            });
            journal.extend_from_slice(&(0x4000_0000 / page_size + 1).to_be_bytes());
            journal.extend_from_slice(target_bytes);
            journal.extend_from_slice(&u32::try_from(target_bytes.len()).unwrap().to_be_bytes());
            journal.extend_from_slice(&checksum.to_be_bytes());
            journal.extend_from_slice(&JOURNAL_MAGIC);
            Some((target, contents))
        }
    };
    fs::write(&path, dirty).unwrap();
    fs::write(&journal_path, journal).unwrap();

    let recovered = FontCache::open_or_create(&path).expect("recover hot journal");
    let value: String = recovered
        .conn
        .query_row(
            "SELECT value FROM cache_meta WHERE key='recovery_probe'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(value, "before");
    let integrity: String = recovered
        .conn
        .pragma_query_value(None, "integrity_check", |row| row.get(0))
        .unwrap();
    assert_eq!(integrity, "ok");
    assert!(!journal_path.exists(), "the recovered journal is removed");
    if let Some((target, contents)) = referenced {
        if matches!(fixture, SuperJournalFixture::Valid) {
            assert!(
                !target.exists(),
                "a completed valid super-journal is removed"
            );
        } else {
            assert_eq!(
                fs::read(&target).expect("recovery must preserve unrelated files"),
                contents
            );
        }
    }
}

#[test]
fn hot_journal_recovery_preserves_unrelated_filenames() {
    assert_hot_journal_recovery(SuperJournalFixture::UnrelatedFilename);
}

#[test]
fn hot_journal_recovery_preserves_files_without_a_backlink() {
    assert_hot_journal_recovery(SuperJournalFixture::MissingBacklink);
}

#[test]
fn hot_journal_recovery_cleans_up_a_valid_super_journal() {
    assert_hot_journal_recovery(SuperJournalFixture::Valid);
}

#[test]
fn hot_journal_recovery_rolls_back_an_ordinary_transaction() {
    assert_hot_journal_recovery(SuperJournalFixture::None);
}

#[test]
fn sqlite_error_classifier_only_marks_invalid_database_codes_for_rebuild() {
    for code in [rusqlite::ffi::SQLITE_CORRUPT, rusqlite::ffi::SQLITE_NOTADB] {
        let error = cache_sqlite_error("opening cache", sqlite_failure(code));
        assert!(
            matches!(&error, CacheError::InvalidDatabase(_)),
            "SQLite code {code} must permit invalid-database recovery advice, got {error}"
        );
    }

    for code in [rusqlite::ffi::SQLITE_PERM, rusqlite::ffi::SQLITE_CANTOPEN] {
        let error = cache_sqlite_error("opening cache", sqlite_failure(code));
        assert!(
            matches!(&error, CacheError::Io(_)),
            "SQLite code {code} must remain an ordinary I/O error, got {error}"
        );
    }
}

#[test]
fn non_sqlite_cache_is_invalid_database_in_both_open_modes() {
    let guard = TempCacheDir::new();
    let read_write_path = guard.0.join("invalid-read-write.sqlite3");
    let read_only_path = guard.0.join("invalid-read-only.sqlite3");
    let invalid_bytes = b"this is deliberately not a SQLite database";
    fs::write(&read_write_path, invalid_bytes).expect("write read-write fixture");
    fs::write(&read_only_path, invalid_bytes).expect("write read-only fixture");

    for (mode, result) in [
        ("read-write", FontCache::open_or_create(&read_write_path)),
        (
            "read-only",
            FontCache::open_existing_read_only(&read_only_path),
        ),
    ] {
        assert!(
            matches!(&result, Err(CacheError::InvalidDatabase(_))),
            "non-SQLite cache must be classified as InvalidDatabase in {mode} mode, got {result:?}"
        );
    }
}

#[test]
fn fresh_open_creates_schema_and_writes_version() {
    let (_guard, path) = temp_cache_path();
    let cache = FontCache::open_or_create(&path).expect("fresh cache opens");

    // All six schema-v6 tables present.
    let table_count: i32 = cache
        .conn
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name IN \
             ('cached_sources', 'cached_directories', 'cached_source_files', \
              'cached_fonts', 'cached_family_keys', 'cache_meta')",
            [],
            |r| r.get(0),
        )
        .expect("query schema tables");
    assert_eq!(table_count, 6, "expected all six tables created");

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
}

#[test]
fn family_lookup_uses_the_family_first_index() {
    let (_guard, path) = temp_cache_path();
    let cache = FontCache::open_or_create(&path).expect("fresh cache opens");
    let explain_sql = format!("EXPLAIN QUERY PLAN {FAMILY_LOOKUP_SQL}");
    let mut statement = cache.conn.prepare(&explain_sql).unwrap();
    let details: Vec<String> = statement
        .query_map(
            params![
                "demo",
                0,
                0,
                0,
                1,
                i64::try_from(MAX_CACHE_LOOKUP_CANDIDATES + 1).unwrap()
            ],
            |row| row.get(3),
        )
        .unwrap()
        .map(Result::unwrap)
        .collect();
    assert!(
        details
            .iter()
            .any(|detail| detail.contains("idx_cached_family_lookup")),
        "lookup plan must use the family-first index: {details:?}"
    );
}

#[test]
fn projected_global_cache_budgets_accept_boundary_and_reject_one_over() {
    assert_eq!(
        checked_projected_count(199_999, 1, 200_000, "candidate rows").unwrap(),
        200_000
    );
    assert!(checked_projected_count(200_000, 1, 200_000, "candidate rows").is_err());

    assert_eq!(
        checked_projected_path_bytes(
            i64::try_from(MAX_CACHED_SNAPSHOT_PATH_BYTES - 1).unwrap(),
            1,
        )
        .unwrap(),
        MAX_CACHED_SNAPSHOT_PATH_BYTES
    );
    assert!(checked_projected_path_bytes(
        i64::try_from(MAX_CACHED_SNAPSHOT_PATH_BYTES).unwrap(),
        1,
    )
    .is_err());
    assert!(enforce_cached_path_byte_budgets(
        crate::fonts::MAX_SCAN_PATH_BYTES,
        MAX_CACHED_SNAPSHOT_PATH_BYTES,
    )
    .is_ok());
    assert!(enforce_cached_path_byte_budgets(
        crate::fonts::MAX_SCAN_PATH_BYTES + 1,
        MAX_CACHED_SNAPSHOT_PATH_BYTES,
    )
    .is_err());
    assert!(enforce_cached_path_byte_budgets(
        crate::fonts::MAX_SCAN_PATH_BYTES,
        MAX_CACHED_SNAPSHOT_PATH_BYTES + 1,
    )
    .is_err());
}

#[test]
fn read_only_open_does_not_create_sqlite_sidecars() {
    let (_guard, path) = temp_cache_path();
    FontCache::open_or_create(&path).expect("fresh cache opens");

    for suffix in ["-wal", "-shm"] {
        let sidecar = sqlite_sidecar_path(&path, suffix);
        match fs::remove_file(&sidecar) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => panic!(
                "failed to remove setup sidecar {}: {error}",
                sidecar.display()
            ),
        }
        assert!(
            !sidecar.exists(),
            "setup should start without {suffix} sidecar"
        );
    }

    let cache = FontCache::open_existing_read_only(&path).expect("read-only cache opens");
    assert!(cache
        .list_folders()
        .expect("read-only query works")
        .is_empty());
    drop(cache);

    for suffix in ["-wal", "-shm"] {
        let sidecar = sqlite_sidecar_path(&path, suffix);
        assert!(
            !sidecar.exists(),
            "read-only diagnostics must not create {}",
            sidecar.display()
        );
    }
}

#[test]
fn read_only_open_reads_committed_wal_content_from_older_cache() {
    let (_guard, path) = temp_cache_path();
    let writer = Connection::open(&path).expect("open writer");
    writer
        .pragma_update(None, "journal_mode", "WAL")
        .expect("enable WAL for older-cache fixture");
    writer
        .pragma_update(None, "wal_autocheckpoint", 0)
        .expect("disable auto-checkpoint");
    writer
        .execute_batch(SCHEMA_SQL)
        .expect("create schema in WAL");
    writer
        .execute(
            "INSERT INTO cache_meta(key, value) VALUES('schema_version', ?1)",
            params![SCHEMA_VERSION.to_string()],
        )
        .expect("write schema version in WAL");

    assert!(
        sqlite_sidecar_path(&path, "-wal").exists(),
        "fixture should keep committed content in a WAL sidecar"
    );

    let reader = FontCache::open_existing_read_only(&path).expect("read-only cache opens");
    assert!(reader
        .list_folders()
        .expect("read-only query sees WAL schema")
        .is_empty());
    drop(reader);
    drop(writer);
}

#[test]
fn reopen_of_valid_cache_succeeds() {
    let (_guard, path) = temp_cache_path();
    // Create.
    FontCache::open_or_create(&path).expect("first open creates");
    // Reopen.
    FontCache::open_or_create(&path).expect("second open reuses existing");
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
}

#[test]
fn version_six_cache_requires_explicit_recovery_in_both_open_modes() {
    let (_guard, path) = temp_cache_path();
    FontCache::open_or_create(&path).expect("create cache");
    {
        let conn = Connection::open(&path).unwrap();
        conn.execute(
            "UPDATE cache_meta SET value = '6' WHERE key = 'schema_version'",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO cache_meta(key, value) VALUES('preserve-until-rebuild', 'old-data')",
            [],
        )
        .unwrap();
    }

    for result in [
        FontCache::open_or_create(&path),
        FontCache::open_existing_read_only(&path),
    ] {
        match result {
            Err(CacheError::SchemaVersionMismatch { found, expected }) => {
                assert_eq!(found, 6);
                assert_eq!(expected, 7);
            }
            other => panic!("old incomplete scans require explicit rebuild: {other:?}"),
        }
    }

    let conn = Connection::open(&path).expect("old cache must still exist");
    let version: String = conn
        .query_row(
            "SELECT value FROM cache_meta WHERE key = 'schema_version'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    let old_data: String = conn
        .query_row(
            "SELECT value FROM cache_meta WHERE key = 'preserve-until-rebuild'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(version, "6", "opening must not silently migrate the cache");
    assert_eq!(old_data, "old-data", "opening must not silently clear rows");
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
}

#[test]
fn missing_cache_meta_table_treated_as_mismatch_for_both_open_modes() {
    let (_guard, path) = temp_cache_path();
    {
        let conn = Connection::open(&path).unwrap();
        conn.execute("CREATE TABLE unrelated(value TEXT)", [])
            .unwrap();
    }

    let assert_mismatch = |mode: &str, result: Result<FontCache, CacheError>| match result {
        Err(CacheError::SchemaVersionMismatch { found, expected }) => {
            assert_eq!(found, -1, "missing-table sentinel in {mode} mode");
            assert_eq!(expected, SCHEMA_VERSION);
        }
        other => panic!(
            "expected SchemaVersionMismatch for missing cache_meta in {mode} mode, got {other:?}"
        ),
    };

    assert_mismatch("read-write", FontCache::open_or_create(&path));
    assert_mismatch("read-only", FontCache::open_existing_read_only(&path));
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
        face_name_aliases: Vec::new(),
    }
}

fn family_key(family_name: &str, bold: bool, italic: bool) -> FamilyKey {
    FamilyKey {
        family_name: family_name.to_string(),
        bold,
        italic,
    }
}

fn canonical_path_string(path: &Path) -> String {
    let canonical = path
        .canonicalize()
        .expect("canonicalize test path")
        .to_string_lossy()
        .into_owned();
    crate::fonts::normalize_canonical_path(&canonical)
}

fn real_folder_path(folder: &Path) -> String {
    fs::create_dir_all(folder).expect("create test font folder");
    canonical_path_string(folder)
}

fn real_font_metadata(
    folder: &Path,
    file_name: &str,
    face_index: i32,
    family_keys: Vec<FamilyKey>,
    face_name_aliases: Vec<String>,
) -> FontMetadata {
    fs::create_dir_all(folder).expect("create test font folder");
    let file_path = folder.join(file_name);
    fs::write(
        &file_path,
        format!("fake font cache fixture: {file_name}:{face_index}"),
    )
    .expect("write test font file");
    let metadata = fs::metadata(&file_path).expect("stat test font file");
    let file_mtime = try_modified_at(&file_path).expect("test font mtime");
    FontMetadata {
        file_path: canonical_path_string(&file_path),
        file_size: i64::try_from(metadata.len()).unwrap_or(i64::MAX),
        file_mtime,
        face_index,
        family_keys,
        face_name_aliases,
    }
}

fn source_snapshot_for_fonts(
    source_root: &str,
    scope: FontDirectoryScope,
    fonts: &[FontMetadata],
) -> CacheSourceSnapshot {
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
    CacheSourceSnapshot {
        source_root: source_root.to_string(),
        scope,
        directories: vec![FolderSnapshot {
            folder_path: source_root.to_string(),
            folder_mtime: try_modified_at(Path::new(source_root)).unwrap_or(1),
        }],
        files,
    }
}

fn insert_raw_lookup_fixture(
    cache: &FontCache,
    source_root: &str,
    font_path: &str,
    file_size: i64,
    file_mtime: i64,
    family: &str,
) {
    cache
        .conn
        .pragma_update(None, "foreign_keys", "OFF")
        .unwrap();
    cache
        .conn
        .execute(
            "INSERT INTO cached_sources(source_root, scope, source_order, last_scanned_at) \
             VALUES(?1, 0, 1, 1)",
            params![source_root],
        )
        .unwrap();
    cache
        .conn
        .execute(
            "INSERT INTO cached_fonts(\
                source_root, scope, font_path, face_index, file_size, file_mtime\
             ) VALUES(?1, 0, ?2, 0, ?3, ?4)",
            params![source_root, font_path, file_size, file_mtime],
        )
        .unwrap();
    cache
        .conn
        .execute(
            "INSERT INTO cached_family_keys(\
                source_root, scope, font_path, face_index, family_name, \
                family_name_key, key_kind, bold, italic\
             ) VALUES(?1, 0, ?2, 0, ?3, ?4, ?5, 0, 0)",
            params![
                source_root,
                font_path,
                family,
                family_lookup_key(family),
                KEY_KIND_FAMILY
            ],
        )
        .unwrap();
    cache
        .conn
        .pragma_update(None, "foreign_keys", "ON")
        .unwrap();
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
        face_name_aliases: Vec::new(),
    };
    cache
        .replace_folder("/test/cjk", 1_700_000_000, &[cjk_font])
        .expect("replace");

    let key_count: i32 = cache
        .conn
        .query_row("SELECT COUNT(*) FROM cached_family_keys", [], |r| r.get(0))
        .expect("count keys");
    assert_eq!(key_count, 3, "all three family aliases should be indexed");
}

/// `INSERT OR IGNORE` on `cached_family_keys` ensures duplicate
/// raw family names that NFC-normalize + lowercase to the same
/// `family_name_key` (e.g. the NFC and NFD forms of `Café`, or
/// two case variants of an English name) don't violate the PK
/// `(family_name_key, key_kind, bold, italic, font_path, face_index)`.
/// Without OR IGNORE this would throw, aborting the whole
/// `replace_folder` transaction.
#[test]
fn replace_folder_dedupes_normalize_equal_family_keys() {
    let (_guard, path) = temp_cache_path();
    let mut cache = FontCache::open_or_create(&path).expect("open");
    let font = FontMetadata {
        file_path: "/test/dedupe/normalize.otf".to_string(),
        file_size: 1_000,
        file_mtime: 1_700_000_000,
        face_index: 0,
        family_keys: vec![
            FamilyKey {
                family_name: "Café".to_string(), // NFC: U+00E9
                bold: false,
                italic: false,
            },
            FamilyKey {
                family_name: "Cafe\u{0301}".to_string(), // NFD: e + U+0301
                bold: false,
                italic: false,
            },
            FamilyKey {
                family_name: "CAFÉ".to_string(), // case variant
                bold: false,
                italic: false,
            },
            FamilyKey {
                family_name: "Other Font".to_string(), // distinct
                bold: false,
                italic: false,
            },
        ],
        face_name_aliases: Vec::new(),
    };
    cache
        .replace_folder("/test/dedupe", 1_700_000_000, &[font])
        .expect("replace");

    let key_count: i32 = cache
        .conn
        .query_row("SELECT COUNT(*) FROM cached_family_keys", [], |r| r.get(0))
        .expect("count keys");
    assert_eq!(
        key_count, 2,
        "three Café variants should collapse to one row + one Other Font row"
    );
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
}

#[test]
fn cache_rejects_un_normalized_extended_windows_paths() {
    let (_guard, path) = temp_cache_path();
    let mut cache = FontCache::open_or_create(&path).expect("open");
    let err = cache
        .replace_folder(r"\\?\C:\Fonts\Anime", 1_700_000_000, &[])
        .unwrap_err();
    assert!(err.to_string().contains("reserved device namespace"));
}

#[test]
fn list_folders_rejects_network_namespace_rows_before_stat_callers() {
    let (_guard, path) = temp_cache_path();
    let mut cache = FontCache::open_or_create(&path).expect("open");
    let err = cache
        .replace_folder(r"\\?\UNC\dead-host\Fonts", 1_700_000_000, &[])
        .unwrap_err();
    assert!(
        err.to_string().contains("reserved device namespace"),
        "expected cached folder namespace rejection, got: {err}"
    );
}

#[test]
fn list_folders_rejects_over_sanity_cap_before_stat_callers() {
    let (_guard, path) = temp_cache_path();
    let cache = FontCache::open_or_create(&path).expect("open");
    for i in 0..=MAX_CACHED_FOLDERS {
        cache
            .conn
            .execute(
                "INSERT INTO cached_sources(source_root, scope, source_order, last_scanned_at) \
                 VALUES(?1, 0, ?2, ?3)",
                params![
                    format!("/hostile/{i:04}"),
                    i64::try_from(i).unwrap(),
                    1_700_000_001_i64
                ],
            )
            .unwrap();
    }

    let err = cache.list_folders().unwrap_err();
    assert!(
        err.to_string().contains(&format!(
            "cached_sources table exceeds {MAX_CACHED_FOLDERS}-row sanity cap"
        )),
        "expected cached_folders sanity-cap error, got: {err}"
    );
}

#[test]
fn replace_folder_refuses_to_create_unreadable_over_cap_cache() {
    let (_guard, path) = temp_cache_path();
    let mut cache = FontCache::open_or_create(&path).expect("open");
    for i in 0..MAX_CACHED_FOLDERS {
        cache
            .replace_folder(&format!("/cache/{i:04}"), 1_700_000_000, &[])
            .unwrap();
    }

    cache
        .replace_folder("/cache/0000", 1_700_000_123, &[])
        .expect("replacing an existing folder at the cap should remain legal");

    let err = cache
        .replace_folder("/cache/overflow", 1_700_000_000, &[])
        .unwrap_err();
    assert!(
        err.to_string().contains(&format!(
            "cached_sources is at the {MAX_CACHED_FOLDERS}-source sanity cap"
        )),
        "expected replace_folder cap error, got: {err}"
    );
}

#[test]
fn last_scanned_at_set_to_current_time() {
    let (_guard, path) = temp_cache_path();
    let mut cache = FontCache::open_or_create(&path).expect("open");
    // Test-only `.expect` — running on a sane system clock is a
    // baseline test-environment assumption; if SystemTime is
    // broken, every other test in the suite would also fail.
    let before = current_unix_seconds().expect("sane system clock");
    cache
        .replace_folder("/test/timing", 1_700_000_000, &[])
        .unwrap();
    let after = current_unix_seconds().expect("sane system clock");
    let folders = cache.list_folders().expect("list");
    assert!(folders[0].last_scanned_at >= before);
    assert!(folders[0].last_scanned_at <= after);
}

// ── Drift detection ─────────────────────────────────────

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
}

#[test]
fn drift_report_is_empty_method() {
    let empty = DriftReport::default();
    assert!(empty.is_empty());
    let with_added = DriftReport {
        added: vec![CacheSourceKey {
            source_root: "x".to_string(),
            scope: FontDirectoryScope::Shallow,
        }],
        ..Default::default()
    };
    assert!(!with_added.is_empty());
    let with_modified = DriftReport {
        modified: vec![CacheSourceKey {
            source_root: "x".to_string(),
            scope: FontDirectoryScope::Shallow,
        }],
        ..Default::default()
    };
    assert!(!with_modified.is_empty());
    let with_removed = DriftReport {
        removed: vec![CacheSourceKey {
            source_root: "x".to_string(),
            scope: FontDirectoryScope::Shallow,
        }],
        ..Default::default()
    };
    assert!(!with_removed.is_empty());
}

// ── Family-name lookup ──────────────────────────────────

#[test]
fn lookup_family_returns_match() {
    let (guard, path) = temp_cache_path();
    let mut cache = FontCache::open_or_create(&path).expect("open");
    let font_dir = guard.0.join("lookup-match");
    let arial = real_font_metadata(
        &font_dir,
        "arial.ttf",
        0,
        vec![family_key("Arial", false, false)],
        Vec::new(),
    );
    cache
        .replace_folder(
            &real_folder_path(&font_dir),
            100,
            std::slice::from_ref(&arial),
        )
        .unwrap();
    let result = cache
        .lookup_family("Arial", false, false)
        .expect("lookup")
        .expect("hit expected");
    assert_eq!(result.font_path, arial.file_path);
    assert_eq!(result.face_index, 0);
}

#[test]
fn lookup_family_returns_none_for_missing_family() {
    let (guard, path) = temp_cache_path();
    let mut cache = FontCache::open_or_create(&path).expect("open");
    let font_dir = guard.0.join("lookup-miss");
    let arial = real_font_metadata(
        &font_dir,
        "arial.ttf",
        0,
        vec![family_key("Arial", false, false)],
        Vec::new(),
    );
    cache
        .replace_folder(&real_folder_path(&font_dir), 100, &[arial])
        .unwrap();
    let result = cache
        .lookup_family("Helvetica", false, false)
        .expect("lookup ok");
    assert!(result.is_none(), "expected None, got {result:?}");
}

#[test]
fn lookup_family_ignores_orphan_family_key_rows() {
    let (_guard, path) = temp_cache_path();
    let cache = FontCache::open_or_create(&path).expect("open");
    cache
        .conn
        .pragma_update(None, "foreign_keys", "OFF")
        .unwrap();
    cache
        .conn
        .execute(
            "INSERT INTO cached_family_keys(\
                source_root, scope, font_path, face_index, family_name, \
                family_name_key, key_kind, bold, italic\
             ) VALUES('/orphan', 0, ?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                "/orphan/arial.ttf",
                0,
                "Arial",
                family_lookup_key("Arial"),
                KEY_KIND_FAMILY,
                0,
                0,
            ],
        )
        .unwrap();
    cache
        .conn
        .pragma_update(None, "foreign_keys", "ON")
        .unwrap();

    let result = cache
        .lookup_family("Arial", false, false)
        .expect("lookup ok");
    assert!(
        result.is_none(),
        "orphan family key row must not bypass cached_fonts/cached_folders anchoring"
    );
}

#[test]
fn lookup_family_rejects_network_namespace_font_path_rows() {
    let (_guard, path) = temp_cache_path();
    let cache = FontCache::open_or_create(&path).expect("open");
    let bad_font_path = r"\\?\UNC\dead-host\Fonts\arial.ttf";
    insert_raw_lookup_fixture(
        &cache,
        "/test/dir",
        bad_font_path,
        100_000,
        1_700_000_000,
        "Arial",
    );

    let err = cache.lookup_family("Arial", false, false).unwrap_err();
    assert!(
        err.to_string().contains("reserved device namespace"),
        "expected cached font namespace rejection, got: {err}"
    );
}

#[test]
fn lookup_family_rejects_network_namespace_folder_path_rows() {
    let (_guard, path) = temp_cache_path();
    let cache = FontCache::open_or_create(&path).expect("open");
    let bad_folder_path = r"\\?\UNC\dead-host\Fonts";
    let local_font_path = r"C:\Fonts\arial.ttf";
    insert_raw_lookup_fixture(
        &cache,
        bad_folder_path,
        local_font_path,
        100_000,
        1_700_000_000,
        "Arial",
    );

    let err = cache.lookup_family("Arial", false, false).unwrap_err();
    assert!(
        err.to_string().contains("reserved device namespace"),
        "expected cached folder namespace rejection, got: {err}"
    );
}

#[test]
fn lookup_family_rejects_font_path_outside_cached_folder() {
    let (guard, path) = temp_cache_path();
    let cache = FontCache::open_or_create(&path).expect("open");
    let trusted_dir = guard.0.join("trusted-fonts");
    let outside_dir = guard.0.join("outside-fonts");
    let trusted_folder_path = real_folder_path(&trusted_dir);
    let outside_font = real_font_metadata(
        &outside_dir,
        "private.ttf",
        0,
        vec![family_key("Private Sans", false, false)],
        Vec::new(),
    );
    let outside_font_path = outside_font.file_path.clone();

    insert_raw_lookup_fixture(
        &cache,
        &trusted_folder_path,
        &outside_font_path,
        outside_font.file_size,
        outside_font.file_mtime,
        "Private Sans",
    );

    let err = cache
        .lookup_family("Private Sans", false, false)
        .unwrap_err();
    assert!(
        err.to_string()
            .contains("cached_fonts.font_path is outside cached_sources.source_root"),
        "expected outside-folder cache rejection, got: {err}"
    );
}

#[test]
fn lookup_family_rejects_stale_cached_font_metadata() {
    let (guard, path) = temp_cache_path();
    let mut cache = FontCache::open_or_create(&path).expect("open");
    let font_dir = guard.0.join("stale-fonts");
    let font = real_font_metadata(
        &font_dir,
        "arial.ttf",
        0,
        vec![family_key("Arial", false, false)],
        Vec::new(),
    );
    cache
        .replace_folder(
            &real_folder_path(&font_dir),
            100,
            std::slice::from_ref(&font),
        )
        .unwrap();

    fs::write(Path::new(&font.file_path), b"changed font fixture bytes").expect("mutate font file");

    let err = cache.lookup_family("Arial", false, false).unwrap_err();
    assert!(
        err.to_string()
            .contains("cached_fonts metadata no longer matches the live font file"),
        "expected stale cached font metadata rejection, got: {err}"
    );
}

#[test]
fn lookup_family_skips_stale_newest_candidate_and_returns_valid_older_source() {
    let (guard, path) = temp_cache_path();
    let mut cache = FontCache::open_or_create(&path).expect("open");
    let older_dir = guard.0.join("fallback-older");
    let newer_dir = guard.0.join("fallback-newer");
    let older = real_font_metadata(
        &older_dir,
        "older.ttf",
        0,
        vec![family_key("Fallback Sans", false, false)],
        Vec::new(),
    );
    let newer = real_font_metadata(
        &newer_dir,
        "newer.ttf",
        0,
        vec![family_key("Fallback Sans", false, false)],
        Vec::new(),
    );
    cache
        .replace_folder(
            &real_folder_path(&older_dir),
            100,
            std::slice::from_ref(&older),
        )
        .unwrap();
    cache
        .replace_folder(
            &real_folder_path(&newer_dir),
            200,
            std::slice::from_ref(&newer),
        )
        .unwrap();
    fs::remove_file(&newer.file_path).expect("make highest-priority cache row stale");

    let result = cache
        .lookup_family("Fallback Sans", false, false)
        .expect("a valid lower-ranked cache candidate should recover the lookup")
        .expect("older source should match");
    assert_eq!(result.font_path, older.file_path);
    assert_eq!(result.face_index, 0);
}

#[test]
fn lookup_family_skips_malformed_top_row_and_returns_valid_lower_source() {
    let (guard, path) = temp_cache_path();
    let mut cache = FontCache::open_or_create(&path).expect("open");
    let valid_dir = guard.0.join("malformed-row-valid");
    let malformed_dir = guard.0.join("malformed-row-top");
    let valid = real_font_metadata(
        &valid_dir,
        "valid.ttf",
        0,
        vec![family_key("Malformed Fallback", false, false)],
        Vec::new(),
    );
    cache
        .replace_folder(
            &real_folder_path(&valid_dir),
            100,
            std::slice::from_ref(&valid),
        )
        .unwrap();
    let malformed = real_font_metadata(&malformed_dir, "malformed.ttf", 0, Vec::new(), Vec::new());
    let malformed_root = real_folder_path(&malformed_dir);
    cache
        .conn
        .pragma_update(None, "foreign_keys", "OFF")
        .unwrap();
    cache
        .conn
        .execute(
            "INSERT INTO cached_sources(source_root, scope, source_order, last_scanned_at) \
             VALUES(?1, 0, 999, 1)",
            params![&malformed_root],
        )
        .unwrap();
    cache
        .conn
        .execute(
            "INSERT INTO cached_fonts(\
                source_root, scope, font_path, face_index, file_size, file_mtime\
             ) VALUES(?1, 0, ?2, 'not-an-int', ?3, ?4)",
            params![
                &malformed_root,
                &malformed.file_path,
                malformed.file_size,
                malformed.file_mtime
            ],
        )
        .unwrap();
    cache
        .conn
        .execute(
            "INSERT INTO cached_family_keys(\
                source_root, scope, font_path, face_index, family_name, \
                family_name_key, key_kind, bold, italic\
             ) VALUES(?1, 0, ?2, 'not-an-int', ?3, ?4, ?5, 0, 0)",
            params![
                &malformed_root,
                &malformed.file_path,
                "Malformed Fallback",
                family_lookup_key("Malformed Fallback"),
                KEY_KIND_FAMILY
            ],
        )
        .unwrap();
    cache
        .conn
        .pragma_update(None, "foreign_keys", "ON")
        .unwrap();

    let result = cache
        .lookup_family("Malformed Fallback", false, false)
        .expect("malformed top row should not suppress a valid candidate")
        .expect("valid lower source should match");
    assert_eq!(result.font_path, valid.file_path);
    assert_eq!(result.face_index, 0);
}

#[test]
fn lookup_family_skips_over_limit_face_index_and_returns_valid_lower_source() {
    let (guard, path) = temp_cache_path();
    let mut cache = FontCache::open_or_create(&path).expect("open");
    let valid_dir = guard.0.join("index-limit-valid");
    let invalid_dir = guard.0.join("index-limit-top");
    let valid = real_font_metadata(
        &valid_dir,
        "valid.ttf",
        0,
        vec![family_key("Index Fallback", false, false)],
        Vec::new(),
    );
    cache
        .replace_folder(
            &real_folder_path(&valid_dir),
            100,
            std::slice::from_ref(&valid),
        )
        .unwrap();
    let invalid = real_font_metadata(
        &invalid_dir,
        "invalid.ttf",
        i32::try_from(crate::fonts::MAX_SUBSET_FONT_INDEX + 1).unwrap(),
        vec![family_key("Index Fallback", false, false)],
        Vec::new(),
    );
    cache
        .replace_folder(
            &real_folder_path(&invalid_dir),
            200,
            std::slice::from_ref(&invalid),
        )
        .unwrap();

    let result = cache
        .lookup_family("Index Fallback", false, false)
        .expect("over-limit top row should not suppress a valid candidate")
        .expect("valid lower source should match");
    assert_eq!(result.font_path, valid.file_path);
    assert_eq!(result.face_index, 0);
}

#[test]
fn lookup_family_distinguishes_bold_and_italic() {
    let (guard, path) = temp_cache_path();
    let mut cache = FontCache::open_or_create(&path).expect("open");
    let font_dir = guard.0.join("lookup-style");
    // Two synthetic faces of "Source Han Sans": regular and bold.
    let regular = real_font_metadata(
        &font_dir,
        "SHS-Regular.otf",
        0,
        vec![family_key("Source Han Sans", false, false)],
        Vec::new(),
    );
    let bold = real_font_metadata(
        &font_dir,
        "SHS-Bold.otf",
        0,
        vec![family_key("Source Han Sans", true, false)],
        Vec::new(),
    );
    let regular_path = regular.file_path.clone();
    let bold_path = bold.file_path.clone();
    cache
        .replace_folder(&real_folder_path(&font_dir), 100, &[regular, bold])
        .unwrap();

    // Regular query hits regular file.
    let r = cache
        .lookup_family("Source Han Sans", false, false)
        .unwrap()
        .unwrap();
    assert_eq!(r.font_path, regular_path);
    // Bold query hits bold file.
    let b = cache
        .lookup_family("Source Han Sans", true, false)
        .unwrap()
        .unwrap();
    assert_eq!(b.font_path, bold_path);
    // Italic-not-present query misses.
    let i = cache.lookup_family("Source Han Sans", false, true).unwrap();
    assert!(i.is_none());
}

#[test]
fn lookup_family_prefers_exact_family_before_face_alias() {
    let (guard, path) = temp_cache_path();
    let mut cache = FontCache::open_or_create(&path).expect("open");
    let font_dir = guard.0.join("lookup-alias");
    let exact = real_font_metadata(
        &font_dir,
        "zzz-exact-shared-sans.otf",
        0,
        vec![family_key("Shared Sans", false, false)],
        Vec::new(),
    );
    let alias = real_font_metadata(
        &font_dir,
        "aaa-alias-face.otf",
        0,
        vec![family_key("Other Sans", true, false)],
        vec!["Shared Sans".into()],
    );
    let exact_path = exact.file_path.clone();
    let alias_path = alias.file_path.clone();
    cache
        .replace_folder(&real_folder_path(&font_dir), 100, &[exact, alias])
        .unwrap();

    let exact_result = cache
        .lookup_family("Shared Sans", false, false)
        .unwrap()
        .unwrap();
    assert_eq!(exact_result.font_path, exact_path);

    let alias_result = cache
        .lookup_family("Shared Sans", true, true)
        .unwrap()
        .unwrap();
    assert_eq!(alias_result.font_path, alias_path);

    let alias_row_count: i32 = cache
        .conn
        .query_row(
            "SELECT COUNT(*) FROM cached_family_keys \
             WHERE key_kind = ?1 AND family_name_key = ?2",
            params![KEY_KIND_FACE_ALIAS, family_lookup_key("Shared Sans")],
            |r| r.get(0),
        )
        .expect("count alias rows");
    assert_eq!(
        alias_row_count, 1,
        "style-insensitive face aliases should be stored once"
    );
}

#[test]
fn lookup_family_finds_cjk_alias() {
    // CJK font advertises multiple family aliases on the same face.
    // Lookup must hit any of them.
    let (guard, path) = temp_cache_path();
    let mut cache = FontCache::open_or_create(&path).expect("open");
    let font_dir = guard.0.join("lookup-cjk");
    let cjk = real_font_metadata(
        &font_dir,
        "SourceHanSans.otf",
        0,
        vec![
            family_key("Source Han Sans CN", false, false),
            family_key("思源黑体 CN", false, false),
            family_key("Noto Sans CJK SC", false, false),
        ],
        Vec::new(),
    );
    let cjk_path = cjk.file_path.clone();
    cache
        .replace_folder(&real_folder_path(&font_dir), 100, &[cjk])
        .unwrap();

    for name in &["Source Han Sans CN", "思源黑体 CN", "Noto Sans CJK SC"] {
        let result = cache
            .lookup_family(name, false, false)
            .unwrap()
            .unwrap_or_else(|| panic!("expected hit for {name}"));
        assert_eq!(result.font_path, cjk_path);
    }
}

#[test]
fn ttc_file_with_multiple_faces_is_supported() {
    // TrueType Collection: one file, multiple faces, each its
    // own family. Schema's composite PK on (font_path,
    // face_index) lets all faces coexist.
    let (guard, path) = temp_cache_path();
    let mut cache = FontCache::open_or_create(&path).expect("open");
    let font_dir = guard.0.join("lookup-ttc");
    let mingliu_face0 = real_font_metadata(
        &font_dir,
        "MingLiU.ttc",
        0,
        vec![family_key("MingLiU", false, false)],
        Vec::new(),
    );
    let mut mingliu_face1 = mingliu_face0.clone();
    mingliu_face1.face_index = 1;
    mingliu_face1.family_keys = vec![family_key("PMingLiU", false, false)];
    let mingliu_path = mingliu_face0.file_path.clone();
    cache
        .replace_folder(
            &real_folder_path(&font_dir),
            100,
            &[mingliu_face0, mingliu_face1],
        )
        .expect("TTC with 2 faces inserts cleanly");

    // Both family names resolve, each to the right face.
    let m0 = cache
        .lookup_family("MingLiU", false, false)
        .unwrap()
        .unwrap();
    assert_eq!(m0.font_path, mingliu_path);
    assert_eq!(m0.face_index, 0);
    let m1 = cache
        .lookup_family("PMingLiU", false, false)
        .unwrap()
        .unwrap();
    assert_eq!(m1.font_path, mingliu_path);
    assert_eq!(m1.face_index, 1);
}

#[test]
fn lookup_family_is_deterministic_across_collisions() {
    // Two different files claim the same family name (rare in
    // practice — alternate vendor's "Arial" — but the API must
    // produce the same answer across runs).
    let (guard, path) = temp_cache_path();
    let mut cache = FontCache::open_or_create(&path).expect("open");
    let font_dir = guard.0.join("lookup-collision");
    let zzz = real_font_metadata(
        &font_dir,
        "zzz_arial.ttf",
        0,
        vec![family_key("Arial", false, false)],
        Vec::new(),
    );
    let aaa = real_font_metadata(
        &font_dir,
        "aaa_arial.ttf",
        0,
        vec![family_key("Arial", false, false)],
        Vec::new(),
    );
    let aaa_path = aaa.file_path.clone();
    cache
        .replace_folder(&real_folder_path(&font_dir), 100, &[zzz, aaa])
        .unwrap();
    // ORDER BY font_path → "aaa..." comes first.
    let result = cache.lookup_family("Arial", false, false).unwrap().unwrap();
    assert_eq!(result.font_path, aaa_path);
}

#[test]
fn lookup_family_matches_across_case_and_nfc_form() {
    // Cache lookup must NFC-normalize + lowercase BOTH the stored
    // key and the query so a font's name-table form (often NFC)
    // matches an ASS file's `\fn` reference regardless of NFD/NFC
    // or case.
    let (guard, path) = temp_cache_path();
    let mut cache = FontCache::open_or_create(&path).expect("open");
    let font_dir = guard.0.join("lookup-normalized");
    // Store an NFC-form precomposed family name.
    let cafe = real_font_metadata(
        &font_dir,
        "cafe.ttf",
        0,
        vec![family_key("Café", false, false)],
        Vec::new(),
    );
    cache
        .replace_folder(&real_folder_path(&font_dir), 100, &[cafe])
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
}

#[test]
fn open_creates_parent_directory_if_missing() {
    // Don't pre-create the inner `nested/` — let open_or_create
    // do it. TempCacheDir owns the outer dir and cleans the tree
    // on Drop, including whatever open_or_create added under it.
    let (_guard, path) = temp_nested_cache_path();
    FontCache::open_or_create(&path).expect("creates nested parents");
    assert!(path.exists());
}

#[test]
fn try_modified_at_preserves_nanosecond_precision() {
    let guard = TempCacheDir::new();
    let file = guard.0.join("mtime.ttf");
    fs::write(&file, b"mtime fixture").unwrap();
    let modified = fs::metadata(&file)
        .unwrap()
        .modified()
        .unwrap()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap();
    let expected = i64::try_from(modified.as_secs()).unwrap() * 1_000_000_000
        + i64::from(modified.subsec_nanos());
    assert_eq!(try_modified_at(&file), Some(expected));
}

#[test]
fn candidate_file_change_marks_recursive_source_modified() {
    let (guard, path) = temp_cache_path();
    let mut cache = FontCache::open_or_create(&path).unwrap();
    let root = guard.0.join("library");
    let nested = root.join("nested");
    fs::create_dir_all(&nested).unwrap();
    let malformed = nested.join("broken.ttf");
    fs::write(&malformed, b"bad").unwrap();

    let original = snapshot_source_directories(&root, FontDirectoryScope::Recursive).unwrap();
    cache.replace_source(&original, &[]).unwrap();
    // Size is a second independent freshness signal, so this remains
    // deterministic even on filesystems with coarse mtime resolution.
    fs::write(&malformed, b"now a different candidate").unwrap();
    let changed = snapshot_source_directories(&root, FontDirectoryScope::Recursive).unwrap();
    let report = cache.diff_sources(&[changed]).unwrap();
    assert_eq!(report.modified, vec![original.key()]);
}

#[test]
fn same_root_scopes_own_faces_independently() {
    let (guard, path) = temp_cache_path();
    let mut cache = FontCache::open_or_create(&path).unwrap();
    let root = guard.0.join("shared-root");
    let font = real_font_metadata(
        &root,
        "shared.ttf",
        0,
        vec![family_key("Shared Sans", false, false)],
        Vec::new(),
    );
    let source_root = real_folder_path(&root);
    let shallow = source_snapshot_for_fonts(
        &source_root,
        FontDirectoryScope::Shallow,
        std::slice::from_ref(&font),
    );
    let recursive = source_snapshot_for_fonts(
        &source_root,
        FontDirectoryScope::Recursive,
        std::slice::from_ref(&font),
    );
    cache
        .replace_source(&shallow, std::slice::from_ref(&font))
        .unwrap();
    cache
        .replace_source(&recursive, std::slice::from_ref(&font))
        .unwrap();
    let owned_faces: i64 = cache
        .conn
        .query_row("SELECT COUNT(*) FROM cached_fonts", [], |row| row.get(0))
        .unwrap();
    assert_eq!(owned_faces, 2);

    cache
        .remove_source(&source_root, FontDirectoryScope::Recursive)
        .unwrap();
    let remaining_faces: i64 = cache
        .conn
        .query_row("SELECT COUNT(*) FROM cached_fonts", [], |row| row.get(0))
        .unwrap();
    assert_eq!(remaining_faces, 1);
    assert!(cache
        .lookup_family("Shared Sans", false, false)
        .unwrap()
        .is_some());
}

#[test]
fn transactional_clear_success_leaves_no_cached_sources() {
    let (guard, path) = temp_cache_path();
    let mut cache = FontCache::open_or_create(&path).unwrap();
    let root = guard.0.join("clear-source");
    let font = real_font_metadata(
        &root,
        "clear.ttf",
        0,
        vec![family_key("Clear Sans", false, false)],
        Vec::new(),
    );
    let source_root = real_folder_path(&root);
    let snapshot = source_snapshot_for_fonts(
        &source_root,
        FontDirectoryScope::Shallow,
        std::slice::from_ref(&font),
    );
    cache
        .replace_source(&snapshot, std::slice::from_ref(&font))
        .unwrap();

    assert_eq!(cache.clear_sources().unwrap(), 1);
    assert!(cache.list_sources().unwrap().is_empty());
    assert!(cache
        .lookup_family("Clear Sans", false, false)
        .unwrap()
        .is_none());
}

#[test]
fn lookup_prefers_shallow_then_newest_source() {
    let (guard, path) = temp_cache_path();
    let mut cache = FontCache::open_or_create(&path).unwrap();
    let shallow_old_dir = guard.0.join("shallow-old");
    let recursive_dir = guard.0.join("recursive-newer");
    let shallow_new_dir = guard.0.join("shallow-newest");
    let shallow_old = real_font_metadata(
        &shallow_old_dir,
        "font.ttf",
        0,
        vec![family_key("Priority Sans", false, false)],
        Vec::new(),
    );
    let recursive = real_font_metadata(
        &recursive_dir,
        "font.ttf",
        0,
        vec![family_key("Priority Sans", false, false)],
        Vec::new(),
    );
    let shallow_new = real_font_metadata(
        &shallow_new_dir,
        "font.ttf",
        0,
        vec![family_key("Priority Sans", false, false)],
        Vec::new(),
    );
    let shallow_old_root = real_folder_path(&shallow_old_dir);
    let recursive_root = real_folder_path(&recursive_dir);
    let shallow_new_root = real_folder_path(&shallow_new_dir);
    cache
        .replace_source(
            &source_snapshot_for_fonts(
                &shallow_old_root,
                FontDirectoryScope::Shallow,
                std::slice::from_ref(&shallow_old),
            ),
            std::slice::from_ref(&shallow_old),
        )
        .unwrap();
    cache
        .replace_source(
            &source_snapshot_for_fonts(
                &recursive_root,
                FontDirectoryScope::Recursive,
                std::slice::from_ref(&recursive),
            ),
            std::slice::from_ref(&recursive),
        )
        .unwrap();
    let hit = cache
        .lookup_family("Priority Sans", false, false)
        .unwrap()
        .unwrap();
    assert_eq!(hit.font_path, shallow_old.file_path);

    cache
        .replace_source(
            &source_snapshot_for_fonts(
                &shallow_new_root,
                FontDirectoryScope::Shallow,
                std::slice::from_ref(&shallow_new),
            ),
            std::slice::from_ref(&shallow_new),
        )
        .unwrap();
    // Refreshing an older source updates its content but not its addition
    // order; the newer shallow source must remain the winner.
    cache
        .replace_source(
            &source_snapshot_for_fonts(
                &shallow_old_root,
                FontDirectoryScope::Shallow,
                std::slice::from_ref(&shallow_old),
            ),
            std::slice::from_ref(&shallow_old),
        )
        .unwrap();
    let hit = cache
        .lookup_family("Priority Sans", false, false)
        .unwrap()
        .unwrap();
    assert_eq!(hit.font_path, shallow_new.file_path);
}

#[test]
fn lookup_prefers_exact_family_across_scopes_before_shallow_alias() {
    let (guard, path) = temp_cache_path();
    let mut cache = FontCache::open_or_create(&path).unwrap();
    let shallow_dir = guard.0.join("shallow-alias");
    let recursive_dir = guard.0.join("recursive-exact");
    let shallow_alias = real_font_metadata(
        &shallow_dir,
        "alias.ttf",
        0,
        vec![family_key("Unrelated Family", false, false)],
        vec!["Cross Scope Sans".to_string()],
    );
    let recursive_exact = real_font_metadata(
        &recursive_dir,
        "exact.ttf",
        0,
        vec![family_key("Cross Scope Sans", false, false)],
        Vec::new(),
    );
    let shallow_root = real_folder_path(&shallow_dir);
    let recursive_root = real_folder_path(&recursive_dir);
    cache
        .replace_source(
            &source_snapshot_for_fonts(
                &shallow_root,
                FontDirectoryScope::Shallow,
                std::slice::from_ref(&shallow_alias),
            ),
            std::slice::from_ref(&shallow_alias),
        )
        .unwrap();
    cache
        .replace_source(
            &source_snapshot_for_fonts(
                &recursive_root,
                FontDirectoryScope::Recursive,
                std::slice::from_ref(&recursive_exact),
            ),
            std::slice::from_ref(&recursive_exact),
        )
        .unwrap();

    let hit = cache
        .lookup_family("Cross Scope Sans", false, false)
        .unwrap()
        .unwrap();
    assert_eq!(hit.font_path, recursive_exact.file_path);
}

#[test]
fn cache_accepts_representative_20130_face_source() {
    let (_guard, path) = temp_cache_path();
    let mut cache = FontCache::open_or_create(&path).unwrap();
    let mut fonts = Vec::with_capacity(20_130);
    let mut files = Vec::with_capacity(20_130);
    for index in 0..20_130 {
        let file_path = format!("/large/font-{index:05}.ttf");
        fonts.push(synthetic_font(&file_path, "Large Library Sans"));
        files.push(FileSnapshot {
            file_path,
            file_size: 100_000,
            file_mtime: 1_700_000_000_000_000_000,
        });
    }
    let snapshot = CacheSourceSnapshot {
        source_root: "/large".to_string(),
        scope: FontDirectoryScope::Recursive,
        directories: vec![FolderSnapshot {
            folder_path: "/large".to_string(),
            folder_mtime: 1_700_000_000_000_000_000,
        }],
        files,
    };
    // Align synthetic helper's legacy seconds fixture with schema-v6 ns.
    for font in &mut fonts {
        font.file_mtime = 1_700_000_000_000_000_000;
    }
    cache.replace_source(&snapshot, &fonts).unwrap();
    let face_count: i64 = cache
        .conn
        .query_row("SELECT COUNT(*) FROM cached_fonts", [], |row| row.get(0))
        .unwrap();
    assert_eq!(face_count, 20_130);
}

#[test]
fn snapshot_children_preserve_sorting_at_the_entry_limit() {
    let mut visited = crate::fonts::MAX_PREFLIGHT_ENTRIES - 2;
    let children = collect_snapshot_children(
        [Ok(PathBuf::from("z.ttf")), Ok(PathBuf::from("a.ttf"))],
        Path::new("library"),
        &mut visited,
        0,
    )
    .unwrap();
    assert_eq!(children, [PathBuf::from("a.ttf"), PathBuf::from("z.ttf")]);
    assert_eq!(visited, crate::fonts::MAX_PREFLIGHT_ENTRIES);
}

#[test]
fn snapshot_children_stop_consuming_an_over_limit_iterator() {
    let read = std::cell::Cell::new(0);
    let entries = std::iter::repeat_with(|| {
        read.set(read.get() + 1);
        assert!(
            read.get() <= 3,
            "must stop before consuming the rest of a wide directory"
        );
        Ok(PathBuf::from("candidate.ttf"))
    });
    let mut visited = crate::fonts::MAX_PREFLIGHT_ENTRIES - 2;
    let error =
        collect_snapshot_children(entries, Path::new("library"), &mut visited, 0).unwrap_err();
    assert!(error.contains("traversal limit"));
    assert_eq!(read.get(), 3);
    assert_eq!(visited, crate::fonts::MAX_PREFLIGHT_ENTRIES);
}

#[test]
fn snapshot_children_include_temporary_paths_in_the_byte_budget() {
    let path = PathBuf::from("candidate.ttf");
    let remaining = path.as_os_str().len();
    let mut visited = 0;
    assert!(collect_snapshot_children(
        [Ok(path.clone())],
        Path::new("library"),
        &mut visited,
        crate::fonts::MAX_SCAN_PATH_BYTES - remaining
    )
    .is_ok());
    let error = collect_snapshot_children(
        [Ok(path)],
        Path::new("library"),
        &mut visited,
        crate::fonts::MAX_SCAN_PATH_BYTES - remaining + 1,
    )
    .unwrap_err();
    assert!(error.contains("retained-path limit"));
}

#[test]
fn cache_accepts_4096_directory_snapshot() {
    let (_guard, path) = temp_cache_path();
    let mut cache = FontCache::open_or_create(&path).unwrap();
    let mut directories = vec![FolderSnapshot {
        folder_path: "/tree".to_string(),
        folder_mtime: 1,
    }];
    directories.extend((1..MAX_CACHED_DIRECTORIES).map(|index| FolderSnapshot {
        folder_path: format!("/tree/d-{index:04}"),
        folder_mtime: i64::try_from(index).unwrap(),
    }));
    cache
        .replace_source(
            &CacheSourceSnapshot {
                source_root: "/tree".to_string(),
                scope: FontDirectoryScope::Recursive,
                directories,
                files: Vec::new(),
            },
            &[],
        )
        .unwrap();
    let directory_count: i64 = cache
        .conn
        .query_row("SELECT COUNT(*) FROM cached_directories", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(directory_count, MAX_CACHED_DIRECTORIES as i64);
}

#[test]
fn snapshot_rejects_bidi_candidate_path() {
    let guard = TempCacheDir::new();
    let root = guard.0.join("bidi-library");
    fs::create_dir_all(&root).unwrap();
    fs::write(root.join("evil\u{202e}ttf.ttf"), b"candidate").unwrap();
    let err = snapshot_source_directories(&root, FontDirectoryScope::Shallow).unwrap_err();
    assert!(err.contains("contains invalid characters"), "got: {err}");
}
