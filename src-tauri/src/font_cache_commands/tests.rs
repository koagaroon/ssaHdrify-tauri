use super::*;
use crate::font_cache::{
    snapshot_source_directories, try_modified_at, CacheError, CacheSourceSnapshot, FontMetadata,
};
use std::fs;

static GUI_CACHE_STATE_TEST_LOCK: Mutex<()> = Mutex::new(());

fn clear_gui_cache_state_for_test() {
    *GUI_FONT_CACHE
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner()) = None;
    *GUI_FONT_CACHE_PATH
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner()) = None;
    *GUI_FONT_CACHE_INIT_FAILURE
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner()) = None;
    CACHE_MUTATION_IN_PROGRESS.store(false, Ordering::Release);
    crate::fonts::clear_cache_provenance();
}

struct GuiCacheStateTestGuard {
    _font_state: std::sync::MutexGuard<'static, ()>,
    _serial: std::sync::MutexGuard<'static, ()>,
}

impl GuiCacheStateTestGuard {
    fn acquire() -> Self {
        let font_state = crate::fonts::lock_font_state_for_test();
        let serial = GUI_CACHE_STATE_TEST_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        clear_gui_cache_state_for_test();
        Self {
            _font_state: font_state,
            _serial: serial,
        }
    }
}

impl Drop for GuiCacheStateTestGuard {
    fn drop(&mut self) {
        clear_gui_cache_state_for_test();
    }
}

/// RAII guard mirroring `font_cache.rs::tests::TempCacheDir` —
/// the canonical-shape comment on that sibling enumerates every
/// other temp-dir construction in the workspace (dropzone.rs /
/// safe_io.rs / fonts.rs test modules) and explains why
/// consolidation hasn't landed.
///
/// Same posture, NOT identical: this version takes a `name:
/// &str` argument (the lib-side one is no-args). Both combine
/// `PID + UNIX nanos + AtomicU64` and `fs::create_dir` so parallel
/// test threads never share a directory even when spawned on the
/// same OS clock tick.
struct TempCacheDir(std::path::PathBuf);

fn next_cmds_test_id() -> u64 {
    static NEXT_ID: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    NEXT_ID.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
}

impl TempCacheDir {
    fn new(name: &str) -> Self {
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let seq = next_cmds_test_id();
        let base = std::env::temp_dir();
        fs::create_dir_all(&base).unwrap();
        let dir = base.join(format!(
            "ssahdrify_font_cache_cmds_test_{name}_{}_{stamp}_{seq}",
            std::process::id(),
        ));
        fs::create_dir(&dir).unwrap();
        Self(dir)
    }
}

impl Drop for TempCacheDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn temp_cache(name: &str) -> (TempCacheDir, FontCache) {
    let guard = TempCacheDir::new(name);
    let cache_path = guard.0.join("cache.sqlite3");
    let cache = FontCache::open_or_create(&cache_path).expect("open cache");
    (guard, cache)
}

#[test]
fn init_failure_is_reported_until_a_later_success_clears_it() {
    let guard = TempCacheDir::new("init_failure_lifecycle");
    let _state_guard = GuiCacheStateTestGuard::acquire();
    let blocked_dir = guard.0.join("not-a-directory");
    fs::write(&blocked_dir, b"file blocks create_dir_all").unwrap();

    let init_error = init_gui_font_cache(&blocked_dir).unwrap_err();
    assert!(init_error.contains("Cannot create app data dir"));
    let reported = open_font_cache().unwrap_err();
    assert!(
        reported.contains("a non-directory entry already exists there"),
        "stored error should retain the real failure class: {reported}"
    );
    assert!(!reported.contains("setup did not run"));
    assert!(
        !reported.contains(&blocked_dir.display().to_string()),
        "IPC-safe failure must not disclose the operational path: {reported}"
    );

    let working_dir = guard.0.join("working");
    init_gui_font_cache(&working_dir).expect("later initialization should succeed");
    let status = open_font_cache().expect("stale failure should be cleared");
    assert!(status.available);
    assert!(!status.schema_mismatch);
}

#[test]
fn schema_mismatch_clears_stale_init_failure_and_keeps_structured_status() {
    let guard = TempCacheDir::new("schema_mismatch_state");
    let _state_guard = GuiCacheStateTestGuard::acquire();
    let cache_path = guard.0.join(GUI_CACHE_FILE_NAME);
    let conn = rusqlite::Connection::open(&cache_path).unwrap();
    conn.execute_batch(
        "CREATE TABLE cache_meta(key TEXT PRIMARY KEY, value TEXT NOT NULL);\
         INSERT INTO cache_meta(key, value) VALUES('schema_version', '0');",
    )
    .unwrap();
    drop(conn);
    *GUI_FONT_CACHE_INIT_FAILURE.lock().unwrap() = Some("stale failure".to_string());

    init_gui_font_cache(&guard.0).expect("schema mismatch is recoverable at startup");
    let status = open_font_cache().expect("schema mismatch should be a status, not stale error");
    assert!(!status.available);
    assert!(status.schema_mismatch);
    assert!(GUI_FONT_CACHE_INIT_FAILURE.lock().unwrap().is_none());
}

#[test]
fn init_failure_sanitizer_strips_all_display_controls_and_caps_length() {
    let unsafe_message = format!(
        "prefix\u{061C}\u{202E}\u{2067}\u{200B}\n{}tail",
        "x".repeat(MAX_GUI_CACHE_INIT_FAILURE_CHARS + 20)
    );
    let sanitized = sanitize_gui_cache_init_failure(&unsafe_message);
    assert!(!sanitized.contains('\u{061C}'));
    assert!(!sanitized.contains('\u{202E}'));
    assert!(!sanitized.contains('\u{2067}'));
    assert!(!sanitized.contains('\u{200B}'));
    assert!(!sanitized.contains('\n'));
    assert_eq!(
        sanitized.chars().count(),
        MAX_GUI_CACHE_INIT_FAILURE_CHARS + 1,
        "capped messages include one trailing ellipsis"
    );
    assert!(sanitized.ends_with('…'));
}

#[derive(Default)]
struct FakeRescanCache {
    fail_replace_for: Vec<String>,
    fail_remove_for: Vec<String>,
    replace_attempts: Vec<String>,
    remove_attempts: Vec<String>,
}

impl RescanCacheWriter for FakeRescanCache {
    fn replace_source(
        &mut self,
        snapshot: &CacheSourceSnapshot,
        _fonts: &[FontMetadata],
    ) -> Result<(), CacheError> {
        let folder_path = &snapshot.source_root;
        self.replace_attempts.push(folder_path.to_string());
        if self
            .fail_replace_for
            .iter()
            .any(|folder| folder == folder_path)
        {
            return Err(CacheError::Io("injected replace failure".to_string()));
        }
        Ok(())
    }

    fn remove_source(
        &mut self,
        folder_path: &str,
        _scope: FontDirectoryScope,
    ) -> Result<(), CacheError> {
        self.remove_attempts.push(folder_path.to_string());
        if self
            .fail_remove_for
            .iter()
            .any(|folder| folder == folder_path)
        {
            return Err(CacheError::Io("injected remove failure".to_string()));
        }
        Ok(())
    }
}

fn missing_child_path(guard: &TempCacheDir, name: &str) -> String {
    guard.0.join(name).display().to_string()
}

fn shallow_snapshot(root: &str, mtime: i64) -> CacheSourceSnapshot {
    CacheSourceSnapshot {
        source_root: root.to_string(),
        scope: FontDirectoryScope::Shallow,
        directories: vec![crate::font_cache::FolderSnapshot {
            folder_path: root.to_string(),
            folder_mtime: mtime,
        }],
        files: Vec::new(),
    }
}

fn shallow_key(root: &str) -> CacheSourceKey {
    CacheSourceKey {
        source_root: root.to_string(),
        scope: FontDirectoryScope::Shallow,
    }
}

fn assert_single_apply_failed(skipped: &[SkippedFolder], folder: &str, reason_part: &str) {
    let matches: Vec<&SkippedFolder> = skipped
        .iter()
        .filter(|entry| entry.folder == folder && entry.kind == SkipKind::ApplyFailed)
        .collect();
    assert_eq!(
        matches.len(),
        1,
        "expected one ApplyFailed entry for {folder}, got {skipped:?}"
    );
    assert!(
        matches[0].reason.contains(reason_part),
        "ApplyFailed reason should mention {reason_part:?}, got {:?}",
        matches[0].reason
    );
}

#[test]
fn apply_rescan_evicts_skipped_folder_rows() {
    // Regression pin: a Phase-2 scan failure must drop
    // the stale rows so later lookup_font_family can't short-circuit
    // through them. Without this fix, `skipped` was silent and the
    // command returned Ok; the rows lingered for the rest of the
    // session.
    let (_guard, mut cache) = temp_cache("skipped_evict");
    cache
        .replace_folder("/bogus/skipped/folder", 12345, &[])
        .unwrap();
    assert!(
        cache
            .list_folders()
            .unwrap()
            .iter()
            .any(|f| f.folder_path == "/bogus/skipped/folder"),
        "seed row missing"
    );

    let mut skipped = vec![SkippedFolder {
        folder: "/bogus/skipped/folder".to_string(),
        scope: FontDirectoryScope::Shallow,
        reason: "Not a directory".to_string(),
        kind: SkipKind::ScanFailed,
    }];
    let (modified, evicted) = apply_rescan_to_cache(&mut cache, &[], &[], &mut skipped);
    assert_eq!(modified, 0);
    assert_eq!(evicted, 1);
    assert!(
        cache
            .list_folders()
            .unwrap()
            .iter()
            .all(|f| f.folder_path != "/bogus/skipped/folder"),
        "stale row still present after skip eviction"
    );
    // No new ApplyFailed entries when the eviction succeeded.
    assert!(skipped.iter().all(|s| s.kind == SkipKind::ScanFailed));
}

#[test]
fn apply_rescan_replaces_modified_and_leaves_others() {
    let mut cache = FakeRescanCache::default();
    let scanned = vec![(shallow_snapshot("/folder/a", 999), vec![])];
    let mut skipped: Vec<SkippedFolder> = Vec::new();
    let (modified, evicted) = apply_rescan_to_cache(&mut cache, &scanned, &[], &mut skipped);
    assert_eq!(modified, 1);
    assert_eq!(evicted, 0);
    assert!(skipped.is_empty(), "no errors expected");

    assert_eq!(cache.replace_attempts, vec!["/folder/a"]);
    assert!(cache.remove_attempts.is_empty());
}

#[test]
fn apply_rescan_does_not_evict_removed_that_reappeared() {
    // Existing re-walk dance: a source reported as removed in
    // Phase 1 may have been re-populated by a concurrent command
    // by the time Phase 3 runs. Eviction must skip when the
    // source is back on disk.
    let (guard, mut cache) = temp_cache("removed_reappeared");
    let real_path = guard.0.to_string_lossy().to_string();
    cache.replace_folder(&real_path, 100, &[]).unwrap();

    let removed = confirm_removed_sources_before_apply(&[shallow_key(&real_path)]);
    assert!(
        removed.is_empty(),
        "reappeared source must be filtered before DB apply"
    );
    let mut skipped: Vec<SkippedFolder> = Vec::new();
    let (_, evicted) = apply_rescan_to_cache(&mut cache, &[], &removed, &mut skipped);
    assert_eq!(evicted, 0, "reappeared source should be left alone");
    assert!(skipped.is_empty());
    assert!(
        cache
            .list_folders()
            .unwrap()
            .iter()
            .any(|f| f.folder_path == real_path),
        "row dropped despite re-stat"
    );
}

#[test]
fn try_modified_at_returns_none_for_missing_path() {
    // Symmetry contract: Phase 1 / Phase 3 / detect_drift all
    // gate on this helper, so a missing path must consistently
    // produce "not statable" (None) across every site.
    let mut dir = std::env::temp_dir();
    dir.push(format!(
        "ssahdrify_try_modified_missing_{}_{}",
        std::process::id(),
        next_cmds_test_id()
    ));
    // Don't create dir — we want a definitely-absent path.
    assert!(try_modified_at(&dir).is_none());
}

#[test]
fn try_modified_at_returns_some_for_existing_folder() {
    let (_guard, _) = temp_cache("try_modified_exists");
    let present = TempCacheDir::new("try_modified_present");
    let mtime = try_modified_at(&present.0);
    assert!(mtime.is_some(), "existing folder should yield a Some mtime");
}

#[test]
fn unreadable_existing_source_is_modified_not_removed() {
    let guard = TempCacheDir::new("unreadable_existing");
    let root = guard.0.join("library");
    fs::create_dir_all(&root).unwrap();
    fs::write(root.join("evil\u{202e}ttf.ttf"), b"candidate").unwrap();
    let root =
        crate::fonts::normalize_canonical_path(root.canonicalize().unwrap().to_str().unwrap());
    let source = crate::font_cache::CacheSourceRecord {
        source_root: root.clone(),
        scope: FontDirectoryScope::Recursive,
        source_order: 1,
        last_scanned_at: 1,
        directories: Vec::new(),
        files: Vec::new(),
    };
    let (snapshots, unreadable_existing) = collect_live_source_snapshots(&[source]);
    assert!(snapshots.is_empty());
    let key = shallow_key(&root);
    let recursive_key = CacheSourceKey {
        scope: FontDirectoryScope::Recursive,
        ..key
    };
    assert_eq!(unreadable_existing, vec![recursive_key.clone()]);

    let mut report = DriftReport {
        removed: vec![recursive_key.clone()],
        ..Default::default()
    };
    classify_unreadable_existing_as_modified(&mut report, &unreadable_existing);
    assert!(report.removed.is_empty());
    assert_eq!(report.modified, vec![recursive_key]);
}

#[test]
fn apply_rescan_evicts_removed_that_no_longer_resolves() {
    // A source that no longer passes the complete metadata walk
    // must still be evicted — Phase 3 must
    // NOT short-circuit to "reappeared".
    let (_guard, mut cache) = temp_cache("removed_actually_gone");
    let bogus = "/bogus/definitely-not-a-real-folder/round-2";
    cache.replace_folder(bogus, 100, &[]).unwrap();
    let removed = confirm_removed_sources_before_apply(&[shallow_key(bogus)]);
    assert_eq!(removed, vec![shallow_key(bogus)]);
    let mut skipped: Vec<SkippedFolder> = Vec::new();
    let (_, evicted) = apply_rescan_to_cache(&mut cache, &[], &removed, &mut skipped);
    assert_eq!(evicted, 1);
    assert!(cache
        .list_folders()
        .unwrap()
        .iter()
        .all(|f| f.folder_path != bogus));
    assert!(skipped.is_empty());
}

#[test]
fn apply_rescan_preserves_pre_existing_failed_entry_alongside_success() {
    // Pins the input-preservation side of ApplyFailed handling:
    // a pre-existing `SkippedFolder { kind: ApplyFailed }`
    // survives alongside successful operations instead of being
    // wiped or rewritten.
    let mut cache = FakeRescanCache::default();

    let scanned = vec![(shallow_snapshot("/folder/x", 999), vec![])];
    let mut skipped = vec![SkippedFolder {
        folder: "/already/failed".to_string(),
        scope: FontDirectoryScope::Shallow,
        reason: "previously failed".to_string(),
        kind: SkipKind::ApplyFailed,
    }];
    let (modified, _evicted) = apply_rescan_to_cache(&mut cache, &scanned, &[], &mut skipped);
    assert_eq!(modified, 1, "successful folder still counted");
    assert!(
        skipped
            .iter()
            .any(|s| s.folder == "/already/failed" && s.kind == SkipKind::ApplyFailed),
        "pre-existing ApplyFailed entry preserved"
    );
}

#[test]
fn apply_rescan_continues_after_replace_apply_failed() {
    let guard = TempCacheDir::new("replace_apply_failed");
    let first = missing_child_path(&guard, "first");
    let failing = missing_child_path(&guard, "failing");
    let third = missing_child_path(&guard, "third");
    let scanned = vec![
        (shallow_snapshot(&first, 100), Vec::new()),
        (shallow_snapshot(&failing, 200), Vec::new()),
        (shallow_snapshot(&third, 300), Vec::new()),
    ];
    let mut fake = FakeRescanCache {
        fail_replace_for: vec![failing.clone()],
        ..Default::default()
    };
    let mut skipped = Vec::new();

    let (modified, evicted) = apply_rescan_to_cache(&mut fake, &scanned, &[], &mut skipped);

    assert_eq!(modified, 2, "both non-failing replaces should count");
    assert_eq!(evicted, 1, "failed replacement must evict its stale row");
    assert_eq!(
        fake.replace_attempts,
        vec![first, failing.clone(), third],
        "replace failures must not short-circuit later folders"
    );
    assert_eq!(fake.remove_attempts, vec![failing.clone()]);
    assert_single_apply_failed(&skipped, &failing, "replace_source failed");
}

#[test]
fn changed_between_scan_and_apply_is_evicted_fail_closed() {
    let (guard, mut cache) = temp_cache("changed_before_apply");
    let root = guard.0.join("library");
    fs::create_dir_all(&root).unwrap();
    let candidate = root.join("changing.ttf");
    fs::write(&candidate, b"first candidate state").unwrap();
    let snapshot = snapshot_source_directories(&root, FontDirectoryScope::Recursive).unwrap();
    cache.replace_source(&snapshot, &[]).unwrap();

    fs::write(&candidate, b"second, visibly longer candidate state").unwrap();
    let mut skipped = Vec::new();
    let validated =
        validate_scanned_sources_before_apply(vec![(snapshot.clone(), vec![])], &mut skipped);
    assert!(validated.is_empty());
    assert_eq!(skipped.len(), 1);
    assert_eq!(skipped[0].kind, SkipKind::ScanFailed);

    let (_, evicted) = apply_rescan_to_cache(&mut cache, &validated, &[], &mut skipped);
    assert_eq!(evicted, 1);
    assert!(cache.list_sources().unwrap().is_empty());
}

#[test]
fn apply_rescan_continues_after_removed_apply_failed() {
    let guard = TempCacheDir::new("removed_apply_failed");
    let first = missing_child_path(&guard, "first");
    let failing = missing_child_path(&guard, "failing");
    let third = missing_child_path(&guard, "third");
    let removed = vec![
        shallow_key(&first),
        shallow_key(&failing),
        shallow_key(&third),
    ];
    let mut fake = FakeRescanCache {
        fail_remove_for: vec![failing.clone()],
        ..Default::default()
    };
    let mut skipped = Vec::new();

    let (modified, evicted) = apply_rescan_to_cache(&mut fake, &[], &removed, &mut skipped);

    assert_eq!(modified, 0);
    assert_eq!(evicted, 2, "both non-failing evictions should count");
    assert_eq!(
        fake.remove_attempts,
        vec![first, failing.clone(), third],
        "remove failures must not short-circuit later removed sources"
    );
    assert_single_apply_failed(&skipped, &failing, "remove_source failed");
}

#[test]
fn apply_rescan_continues_after_scan_failed_eviction_apply_failed() {
    let guard = TempCacheDir::new("scan_failed_eviction_apply_failed");
    let first = missing_child_path(&guard, "first");
    let failing = missing_child_path(&guard, "failing");
    let third = missing_child_path(&guard, "third");
    let mut fake = FakeRescanCache {
        fail_remove_for: vec![failing.clone()],
        ..Default::default()
    };
    let mut skipped = vec![
        SkippedFolder {
            folder: first.clone(),
            scope: FontDirectoryScope::Shallow,
            reason: "scan failed first".to_string(),
            kind: SkipKind::ScanFailed,
        },
        SkippedFolder {
            folder: failing.clone(),
            scope: FontDirectoryScope::Shallow,
            reason: "scan failed failing".to_string(),
            kind: SkipKind::ScanFailed,
        },
        SkippedFolder {
            folder: third.clone(),
            scope: FontDirectoryScope::Shallow,
            reason: "scan failed third".to_string(),
            kind: SkipKind::ScanFailed,
        },
    ];

    let (modified, evicted) = apply_rescan_to_cache(&mut fake, &[], &[], &mut skipped);

    assert_eq!(modified, 0);
    assert_eq!(
        evicted, 2,
        "both non-failing stale-row evictions should count"
    );
    assert_eq!(
        fake.remove_attempts,
        vec![first.clone(), failing.clone(), third.clone()],
        "scan-failed eviction failures must not short-circuit later folders"
    );
    assert_eq!(
        skipped
            .iter()
            .filter(|entry| entry.kind == SkipKind::ScanFailed)
            .count(),
        3,
        "original ScanFailed entries should stay visible to the UI"
    );
    assert_single_apply_failed(&skipped, &failing, "remove_source");
}

// ── migrate_legacy_gui_cache ──

fn make_legacy_pair(name: &str) -> (TempCacheDir, TempCacheDir) {
    // Two disjoint tempdirs simulate the legacy (Tauri-given) and
    // new (unified) data dirs. Each has its own cleanup guard so
    // a panic mid-test still removes both.
    (
        TempCacheDir::new(&format!("{name}_legacy")),
        TempCacheDir::new(&format!("{name}_new")),
    )
}

#[test]
fn migrate_legacy_gui_cache_moves_main_file_and_sidecars() {
    let (legacy, new) = make_legacy_pair("happy");
    let legacy_main = legacy.0.join(GUI_CACHE_FILE_NAME);
    fs::write(&legacy_main, b"sqlite-bytes").unwrap();
    let legacy_wal = {
        let mut p = legacy_main.clone().into_os_string();
        p.push("-wal");
        PathBuf::from(p)
    };
    fs::write(&legacy_wal, b"wal-bytes").unwrap();

    migrate_legacy_gui_cache(&legacy.0, &new.0);

    let new_main = new.0.join(GUI_CACHE_FILE_NAME);
    let new_wal = {
        let mut p = new_main.clone().into_os_string();
        p.push("-wal");
        PathBuf::from(p)
    };
    assert!(new_main.exists(), "main file should move to new location");
    assert!(new_wal.exists(), "sidecar should follow main");
    assert!(!legacy_main.exists(), "main file should leave legacy");
    assert!(!legacy_wal.exists(), "sidecar should leave legacy");
    assert_eq!(fs::read(&new_main).unwrap(), b"sqlite-bytes");
}

#[test]
fn migrate_legacy_gui_cache_skips_when_new_already_exists() {
    // Don't clobber: if the user already has data at the new path,
    // leave it alone and let the legacy file stay as orphan.
    let (legacy, new) = make_legacy_pair("no_clobber");
    let legacy_main = legacy.0.join(GUI_CACHE_FILE_NAME);
    let new_main = new.0.join(GUI_CACHE_FILE_NAME);
    fs::write(&legacy_main, b"legacy-bytes").unwrap();
    fs::write(&new_main, b"new-bytes").unwrap();

    migrate_legacy_gui_cache(&legacy.0, &new.0);

    assert!(legacy_main.exists(), "legacy left in place");
    assert_eq!(
        fs::read(&new_main).unwrap(),
        b"new-bytes",
        "new file must NOT be overwritten"
    );
}

#[test]
fn migrate_legacy_gui_cache_skips_when_legacy_missing() {
    // No-op when nothing to migrate (fresh-install user case).
    let (legacy, new) = make_legacy_pair("nothing_to_do");
    let new_main = new.0.join(GUI_CACHE_FILE_NAME);
    migrate_legacy_gui_cache(&legacy.0, &new.0);
    assert!(!new_main.exists(), "no new file synthesized");
}

#[test]
fn migrate_legacy_gui_cache_skips_when_paths_equal() {
    // Safety: callers shouldn't pass the same path on both sides,
    // but if they do, the helper must not attempt a self-rename.
    let dir = TempCacheDir::new("same_path");
    let main = dir.0.join(GUI_CACHE_FILE_NAME);
    fs::write(&main, b"x").unwrap();
    migrate_legacy_gui_cache(&dir.0, &dir.0);
    assert!(main.exists(), "self-rename must not destroy the file");
}

#[test]
fn rebuild_refuses_to_report_success_when_main_database_cannot_be_deleted() {
    let guard = TempCacheDir::new("main_delete_failure");
    let main_path = guard.0.join("cache-as-directory.sqlite3");
    fs::create_dir_all(&main_path).unwrap();

    let error = remove_cache_files_for_rebuild(std::slice::from_ref(&main_path))
        .expect_err("a non-deletable main database target must abort rebuild");
    assert!(error.contains("Cannot remove the existing font-cache database"));
    assert!(
        main_path.is_dir(),
        "failed delete must not be reported as clear success"
    );
}

// ── finalize_drift generation check ──

#[test]
fn finalize_drift_returns_default_when_generation_changed() {
    // Simulates `detect_font_cache_drift` Phase 1 capturing the
    // cached source identities + generation, then
    // `clear_font_cache` republishing a fresh empty cache (which
    // bumps the generation), then Phase 3 calling finalize_drift
    // with a cache reference that no longer matches the snapshot.
    // Without the generation check, the snapshot's sources would
    // leak into `added`, violating the documented "added is always
    // empty for the GUI path" contract. With the check, Phase 3
    // returns DriftReport::default().
    let (_guard, cache) = temp_cache("fin_drift_gen_changed");
    // Pre-clear snapshot: two sources the user previously had in
    // their cache. The fresh post-clear `cache` we pass in does
    // NOT contain them.
    let snapshot = vec![
        shallow_snapshot("/legacy/folder/a", 100),
        shallow_snapshot("/legacy/folder/b", 200),
    ];
    let unreadable = vec![shallow_key("/legacy/folder/a")];
    let report = finalize_drift(Some(&cache), &snapshot, &unreadable, 5, 6).unwrap();
    assert!(
        report.added.is_empty(),
        "stale snapshot must NOT leak into added[]; got {:?}",
        report.added
    );
    assert!(report.modified.is_empty(), "modified must also be empty");
    assert!(report.removed.is_empty(), "removed must also be empty");
}

#[test]
fn finalize_drift_returns_default_when_cache_unavailable() {
    // Pins shape (1): cache slot is None (clear is mid-flight,
    // between `*slot = None` and `*slot = Some(fresh)`).
    // Generation check still happens first, but None is the
    // independent reason for the default return.
    let snapshot = vec![shallow_snapshot("/folder/a", 100)];
    let report = finalize_drift(None, &snapshot, &[], 0, 0).unwrap();
    assert!(report.added.is_empty());
    assert!(report.modified.is_empty());
    assert!(report.removed.is_empty());
}

#[test]
fn finalize_drift_returns_diff_when_generation_matches() {
    // Counter-test: when the generation didn't change between
    // Phase 1 and Phase 3 (no clear interleaved), diff_sources
    // runs and reports real drift. Seeds /folder/a with mtime
    // 100; passes a snapshot with mtime 999 (mtime mismatch
    // → reported as modified).
    let (_guard, mut cache) = temp_cache("fin_drift_gen_matches");
    cache.replace_folder("/folder/a", 100, &[]).unwrap();
    let snapshot = vec![shallow_snapshot("/folder/a", 999)];
    let report = finalize_drift(Some(&cache), &snapshot, &[], 42, 42).unwrap();
    assert_eq!(
        report.modified,
        vec![shallow_key("/folder/a")],
        "mtime mismatch should classify as modified"
    );
    assert!(
        report.added.is_empty(),
        "snapshot path is in cache → not added"
    );
    assert!(
        report.removed.is_empty(),
        "all cache rows present in snapshot"
    );
}
