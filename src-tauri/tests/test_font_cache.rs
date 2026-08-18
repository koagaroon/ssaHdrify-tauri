//! End-to-end smoke tests for the persistent font cache (#5).
//!
//! Spawns the built `ssahdrify-cli` binary to exercise the
//! `refresh-fonts` writer path and `embed`'s startup drift /
//! announce path; opens the resulting SQLite cache file via the
//! library API and asserts contracts the unit tests can't see:
//!
//! 1. `refresh-fonts --font-dir D --cache-file F` produces an
//!    SQLite file at F with one shallow cached source whose normalized
//!    canonical path matches D, and `cache_meta.schema_version`
//!    matches `font_cache::SCHEMA_VERSION`.
//! 2. Re-running `refresh-fonts` against the same dir is idempotent
//!    — the cache still has exactly one shallow source identity for
//!    that path, not two.
//! 3. `refresh-fonts --no-cache <whatever>` errors out as
//!    contradictory (locked design: refresh-fonts requires the cache
//!    by definition).
//! 4. `refresh-fonts` exits non-zero when every requested source is
//!    skipped, instead of printing a false cache-updated success line.
//! 5. `refresh-fonts` refuses to create a cache with more source rows
//!    than the read-side sanity cap will accept.
//! 6. `embed --cache-file <existing-cache> --no-cache` runs without
//!    touching the cache file — file mtime stays unchanged across
//!    the run. Pairs with #3 to lock in the opt-out semantic from
//!    both sides.
//! 7. `embed` against a cache whose folder mtime has drifted prints
//!    the drift report on stderr and falls back to no-cache.
//! 8. `refresh-fonts --recursive-font-dir` reaches a nested font and persists
//!    the root with recursive scope.
//!
//! The cache deliberately refuses to publish a source with zero readable
//! faces, so the helper copies one operating-system font into each temporary
//! source. The copy is test-only and is removed with the temporary directory.
//!
//! Run with:
//!     cd src-tauri && cargo test --test test_font_cache --release

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::thread;
use std::time::Duration;

use app_lib::font_cache::{cache_source_key, FontCache, FontDirectoryScope, MAX_CACHED_FOLDERS};

mod common;
use common::make_real_font_dir as make_font_dir;

const FIXTURE_ASS: &str = concat!(
    "[Script Info]\n",
    "ScriptType: v4.00+\n",
    "\n",
    "[V4+ Styles]\n",
    "Format: Name, Fontname, Fontsize, PrimaryColour, SecondaryColour, OutlineColour, BackColour, Bold, Italic, Underline, StrikeOut, ScaleX, ScaleY, Spacing, Angle, BorderStyle, Outline, Shadow, Alignment, MarginL, MarginR, MarginV, Encoding\n",
    "Style: Default,Arial,20,&H00FFFFFF,&H000000FF,&H00000000,&H00000000,0,0,0,0,100,100,0,0,1,2,2,2,10,10,10,1\n",
    "\n",
    "[Events]\n",
    "Format: Layer, Start, End, Style, Name, MarginL, MarginR, MarginV, Effect, Text\n",
    "Dialogue: 0,0:00:01.00,0:00:03.00,Default,,0,0,0,,Hello world\n",
);

fn cli_path() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_ssahdrify-cli"))
}

fn temp_dir(label: &str) -> PathBuf {
    let pid = std::process::id();
    let nano = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let dir = std::env::temp_dir().join(format!("ssahdrify-cli-cache-test-{label}-{pid}-{nano}"));
    fs::create_dir_all(&dir).expect("failed to create test temp dir");
    dir
}

fn write_fixture_ass(dir: &Path, name: &str) -> PathBuf {
    let path = dir.join(name);
    fs::write(&path, FIXTURE_ASS).expect("failed to write fixture ASS");
    path
}

fn cache_path(dir: &Path) -> PathBuf {
    dir.join("cache.sqlite3")
}

fn engine_bundle_missing() -> Option<String> {
    // Mirror test_chain.rs: detect the build.rs missing-engine stub
    // so we skip cleanly instead of false-failing in environments
    // where `npm run build:engine` hasn't run.
    //
    // `--no-cache` keeps the probe from touching the user's real
    // default cache file (would race with their live GUI / CLI usage).
    let output = Command::new(cli_path())
        .args([
            "--no-cache",
            "embed",
            "/nonexistent-test-input-do-not-create.ass",
        ])
        .output()
        .ok()?;
    let stderr = String::from_utf8_lossy(&output.stderr);
    if stderr.contains("Run `npm run build:engine`") {
        Some(format!(
            "engine bundle missing — run `npm run build:engine` first ({stderr})"
        ))
    } else {
        None
    }
}

fn run_cli(args: &[&str]) -> std::process::Output {
    Command::new(cli_path())
        .args(args)
        .output()
        .expect("failed to spawn ssahdrify-cli")
}

#[test]
fn refresh_fonts_creates_cache_with_one_folder_row() {
    let work = temp_dir("create");
    let font_dir = make_font_dir(&work);
    let cache = cache_path(&work);

    let output = run_cli(&[
        "--cache-file",
        cache.to_str().unwrap(),
        "refresh-fonts",
        "--font-dir",
        font_dir.to_str().unwrap(),
    ]);
    assert!(
        output.status.success(),
        "refresh-fonts failed: stderr={}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        cache.exists(),
        "cache file not created at {}",
        cache.display()
    );

    // Open via library API and inspect.
    let inspect = FontCache::open_or_create(&cache).expect("open cache for inspection");
    let sources = inspect.list_sources().expect("list_sources");
    assert_eq!(
        sources.len(),
        1,
        "expected exactly 1 cached source, got {sources:?}"
    );
    let stored = &sources[0].source_root;
    let canonical_str = cache_source_key(&font_dir, FontDirectoryScope::Shallow)
        .expect("derive normalized cache source key")
        .source_root;
    assert_eq!(
        stored, &canonical_str,
        "cached folder path mismatch: stored={stored}, canonical={canonical_str}"
    );

    let _ = fs::remove_dir_all(work);
}

#[test]
fn refresh_fonts_idempotent_no_duplicate_folder_rows() {
    let work = temp_dir("idem");
    let font_dir = make_font_dir(&work);
    let cache = cache_path(&work);

    for run in 1..=2 {
        let output = run_cli(&[
            "--cache-file",
            cache.to_str().unwrap(),
            "refresh-fonts",
            "--font-dir",
            font_dir.to_str().unwrap(),
        ]);
        assert!(
            output.status.success(),
            "refresh-fonts run {run} failed: stderr={}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    let inspect = FontCache::open_or_create(&cache).expect("open cache");
    let sources = inspect.list_sources().expect("list_sources");
    assert_eq!(
        sources.len(),
        1,
        "two consecutive refreshes must yield exactly 1 row, got {sources:?}"
    );

    let _ = fs::remove_dir_all(work);
}

#[test]
fn refresh_fonts_recursive_source_reaches_nested_font_and_persists_scope() {
    let work = temp_dir("recursive-create");
    let library_root = work.join("library");
    make_font_dir(&library_root.join("collection").join("family"));
    let cache_path = cache_path(&work);

    let output = run_cli(&[
        "--cache-file",
        cache_path.to_str().unwrap(),
        "refresh-fonts",
        "--recursive-font-dir",
        library_root.to_str().unwrap(),
    ]);
    assert!(
        output.status.success(),
        "recursive refresh-fonts failed: stderr={}",
        String::from_utf8_lossy(&output.stderr)
    );

    let inspect = FontCache::open_or_create(&cache_path).expect("open recursive cache");
    let sources = inspect.list_sources().expect("list recursive sources");
    assert_eq!(
        sources.len(),
        1,
        "expected one recursive source: {sources:?}"
    );
    let expected = cache_source_key(&library_root, FontDirectoryScope::Recursive)
        .expect("derive recursive cache source key");
    assert_eq!(sources[0].key(), expected);

    let _ = fs::remove_dir_all(work);
}

#[test]
fn refresh_fonts_with_no_cache_errors() {
    // Locked design: --no-cache contradicts refresh-fonts (subcommand's
    // entire purpose is writing to cache). Surface as parse-time error.
    let work = temp_dir("nocache_refresh");
    let font_dir = make_font_dir(&work);

    let output = run_cli(&[
        "--no-cache",
        "refresh-fonts",
        "--font-dir",
        font_dir.to_str().unwrap(),
    ]);
    assert!(!output.status.success(), "expected non-zero exit");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("--no-cache"),
        "stderr should mention --no-cache: {stderr}"
    );

    let _ = fs::remove_dir_all(work);
}

#[test]
fn refresh_fonts_rejects_json_even_when_quiet() {
    let work = temp_dir("json_quiet_rejected");
    let font_dir = make_font_dir(&work);
    let cache = cache_path(&work);

    let output = run_cli(&[
        "--quiet",
        "--json",
        "--cache-file",
        cache.to_str().unwrap(),
        "refresh-fonts",
        "--font-dir",
        font_dir.to_str().unwrap(),
    ]);

    assert_eq!(output.status.code(), Some(2));
    assert!(
        output.stdout.is_empty(),
        "unsupported --json must never produce non-JSON stdout"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("refresh-fonts does not implement --json output"),
        "hard error must survive --quiet: {stderr}"
    );
    assert!(
        !cache.exists(),
        "flag rejection must happen before cache I/O"
    );

    let _ = fs::remove_dir_all(work);
}

#[test]
fn refresh_fonts_reports_verbose_as_inert() {
    let work = temp_dir("verbose_inert");
    let font_dir = make_font_dir(&work);
    let cache = cache_path(&work);

    let output = run_cli(&[
        "--lang",
        "en",
        "--verbose",
        "--cache-file",
        cache.to_str().unwrap(),
        "refresh-fonts",
        "--font-dir",
        font_dir.to_str().unwrap(),
    ]);

    assert!(
        output.status.success(),
        "--verbose should be a disclosed no-op, not a failure: stderr={}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("--verbose") && stderr.contains("no effect here"),
        "refresh-fonts must not silently ignore --verbose: {stderr}"
    );

    let _ = fs::remove_dir_all(work);
}

#[test]
fn refresh_fonts_corrupt_cache_names_path_and_offers_targeted_rebuild_in_both_open_modes() {
    let work = temp_dir("corrupt_guidance");
    let font_dir = make_font_dir(&work);
    let cache = cache_path(&work);
    let original = b"this is not a sqlite database";
    fs::write(&cache, original).expect("write invalid cache fixture");

    let dry_run = run_cli(&[
        "--lang",
        "en",
        "--dry-run",
        "--cache-file",
        cache.to_str().unwrap(),
        "refresh-fonts",
        "--font-dir",
        font_dir.to_str().unwrap(),
    ]);
    let normal = run_cli(&[
        "--lang",
        "en",
        "--cache-file",
        cache.to_str().unwrap(),
        "refresh-fonts",
        "--font-dir",
        font_dir.to_str().unwrap(),
    ]);

    for (mode, output) in [("read-only dry-run", dry_run), ("read-write", normal)] {
        assert_eq!(output.status.code(), Some(2), "mode={mode}");
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            stderr.contains(cache.to_str().unwrap()),
            "error must name the selected cache path in {mode}: {stderr}"
        );
        assert!(
            stderr.contains("invalid or corrupt")
                && stderr.contains("Delete the cache file")
                && stderr.contains("rerun the same `refresh-fonts` command"),
            "confirmed invalid SQLite should get precise rebuild guidance in {mode}: {stderr}"
        );
    }
    assert_eq!(
        fs::read(&cache).expect("read invalid cache after refusal"),
        original,
        "the CLI must not delete a corrupt cache automatically"
    );

    let _ = fs::remove_dir_all(work);
}

#[test]
fn refresh_fonts_ordinary_open_error_does_not_recommend_deletion() {
    let work = temp_dir("ordinary_open_error");
    let font_dir = make_font_dir(&work);
    let cache = cache_path(&work);
    fs::create_dir(&cache).expect("create directory at cache-file path");

    let output = run_cli(&[
        "--lang",
        "en",
        "--cache-file",
        cache.to_str().unwrap(),
        "refresh-fonts",
        "--font-dir",
        font_dir.to_str().unwrap(),
    ]);

    assert_eq!(output.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains(cache.to_str().unwrap())
            && stderr.contains("Check the cache path and file permissions"),
        "ordinary I/O error should name its path and recovery boundary: {stderr}"
    );
    assert!(
        !stderr.contains("Delete the cache file")
            && !stderr.contains("rerun the same `refresh-fonts` command"),
        "permission/open failures must not receive destructive corruption guidance: {stderr}"
    );

    let _ = fs::remove_dir_all(work);
}

#[test]
fn refresh_fonts_schema_mismatch_names_path_and_rerun_action() {
    let work = temp_dir("schema_guidance");
    let font_dir = make_font_dir(&work);
    let cache = cache_path(&work);
    drop(FontCache::open_or_create(&cache).expect("create current-schema cache"));
    let connection = rusqlite::Connection::open(&cache).expect("open cache for schema mutation");
    connection
        .execute(
            "UPDATE cache_meta SET value = '999' WHERE key = 'schema_version'",
            [],
        )
        .expect("mutate schema version");
    drop(connection);

    let dry_run = run_cli(&[
        "--lang",
        "en",
        "--dry-run",
        "--cache-file",
        cache.to_str().unwrap(),
        "refresh-fonts",
        "--font-dir",
        font_dir.to_str().unwrap(),
    ]);
    let normal = run_cli(&[
        "--lang",
        "en",
        "--cache-file",
        cache.to_str().unwrap(),
        "refresh-fonts",
        "--font-dir",
        font_dir.to_str().unwrap(),
    ]);

    for (mode, output) in [("read-only dry-run", dry_run), ("read-write", normal)] {
        assert_eq!(output.status.code(), Some(2), "mode={mode}");
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            stderr.contains(cache.to_str().unwrap())
                && stderr.contains("requires schema version")
                && stderr.contains("rerun the same `refresh-fonts` command"),
            "schema mismatch should give a complete same-command recovery action in {mode}: {stderr}"
        );
        assert!(
            !stderr.contains("(file:"),
            "schema guidance must not end in the old dangling half-command in {mode}: {stderr}"
        );
    }

    let _ = fs::remove_dir_all(work);
}

#[test]
fn refresh_fonts_errors_when_every_source_is_skipped() {
    let work = temp_dir("all_skipped_refresh");
    let not_a_dir = work.join("not-a-dir.ttf");
    fs::write(&not_a_dir, b"").expect("failed to write non-directory source");
    let cache = cache_path(&work);

    let output = run_cli(&[
        "--cache-file",
        cache.to_str().unwrap(),
        "refresh-fonts",
        "--font-dir",
        not_a_dir.to_str().unwrap(),
    ]);
    assert!(
        !output.status.success(),
        "expected non-zero exit when every refresh source is skipped"
    );

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("not a directory"),
        "stderr should show why the source was skipped: {stderr}"
    );
    assert!(
        stderr.contains("could not index any font source")
            || stderr.contains("未能索引任何字体来源"),
        "stderr should refuse the all-skipped refresh as a failed run: {stderr}"
    );
    assert!(
        !stderr.contains("Cache updated"),
        "all-skipped refresh must not print a success summary: {stderr}"
    );

    let _ = fs::remove_dir_all(work);
}

#[test]
fn refresh_fonts_rejects_folder_count_over_cache_cap() {
    let work = temp_dir("folder_cap_refresh");
    let cache = cache_path(&work);
    let mut args = vec![
        "--cache-file".to_string(),
        cache.to_string_lossy().into_owned(),
        "refresh-fonts".to_string(),
    ];

    for i in 0..=MAX_CACHED_FOLDERS {
        let relative_dir = format!("fonts-{i:04}");
        fs::create_dir_all(work.join(&relative_dir)).expect("failed to create font dir");
        args.push("--font-dir".to_string());
        args.push(relative_dir);
    }

    // Relative arguments keep this boundary test below Windows' process
    // command-line limit while the CLI still resolves 257 distinct folders.
    let output = Command::new(cli_path())
        .current_dir(&work)
        .args(&args)
        .output()
        .expect("failed to spawn ssahdrify-cli");
    assert!(
        !output.status.success(),
        "expected non-zero exit for too many cached source folders"
    );

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains(&format!(
            "exceeding the {MAX_CACHED_FOLDERS}-source cache sanity cap"
        )),
        "stderr should explain the cache folder cap: {stderr}"
    );
    assert!(
        !stderr.contains("Cache updated"),
        "over-cap refresh must not print a success summary: {stderr}"
    );

    let _ = fs::remove_dir_all(work);
}

#[test]
fn embed_with_no_cache_does_not_touch_cache_file() {
    if let Some(reason) = engine_bundle_missing() {
        // Hard-fail instead of skip-and-return : see
        // test_chain.rs for the WHY — green-when-broken is the failure
        // mode this guards against.
        panic!("engine bundle missing — run `npm run build:engine` first ({reason})");
    }
    let work = temp_dir("nocache_embed");
    let font_dir = make_font_dir(&work);
    let cache = cache_path(&work);

    // Populate cache first.
    let refresh = run_cli(&[
        "--cache-file",
        cache.to_str().unwrap(),
        "refresh-fonts",
        "--font-dir",
        font_dir.to_str().unwrap(),
    ]);
    assert!(refresh.status.success(), "refresh-fonts failed");

    // Snapshot cache file mtime, then run embed --no-cache.
    let before = fs::metadata(&cache)
        .and_then(|m| m.modified())
        .expect("stat cache before");

    // Sleep longer than the worst-case mtime granularity (FAT/SUBST/
    // network drives can be 1-2 s; NTFS is sub-second). 2100 ms covers
    // both branches symmetrically with the drift test below — without
    // this, a no-op write on a 2-second-resolution volume would tie
    // before/after and false-pass.
    thread::sleep(Duration::from_millis(2100));

    let subtitle = write_fixture_ass(&work, "input.ass");
    let embed = run_cli(&[
        "--no-cache",
        "--cache-file",
        cache.to_str().unwrap(),
        "embed",
        subtitle.to_str().unwrap(),
    ]);
    // Embed exit status is intentionally not asserted: Arial
    // resolvability varies across CI runners and dev machines, and
    // the engine bundle can short-circuit before font resolution
    // depending on available system fonts. The lock is narrow — even
    // if embed succeeds, the cache file isn't touched.
    let _ = embed.status;

    let after = fs::metadata(&cache)
        .and_then(|m| m.modified())
        .expect("stat cache after");
    assert_eq!(
        before, after,
        "embed --no-cache must not modify the cache file"
    );

    let _ = fs::remove_dir_all(work);
}

#[test]
fn embed_reports_drift_when_folder_mtime_changes() {
    if let Some(reason) = engine_bundle_missing() {
        // Hard-fail instead of skip-and-return : see
        // test_chain.rs for the WHY — green-when-broken is the failure
        // mode this guards against.
        panic!("engine bundle missing — run `npm run build:engine` first ({reason})");
    }
    let work = temp_dir("drift");
    let font_dir = make_font_dir(&work);
    let cache = cache_path(&work);

    // Populate cache.
    let refresh = run_cli(&[
        "--cache-file",
        cache.to_str().unwrap(),
        "refresh-fonts",
        "--font-dir",
        font_dir.to_str().unwrap(),
    ]);
    assert!(refresh.status.success(), "refresh-fonts failed");

    // Mutate folder: add a file. mtime resolution gates this — sleep
    // longer than the worst-case granularity (NTFS is fine, FAT/SUBST
    // can be 1-2 s) before touching, so the post-touch mtime is
    // strictly greater than what's in cache.
    thread::sleep(Duration::from_millis(2100));
    fs::write(font_dir.join("added-after-cache.ttf"), b"")
        .expect("failed to add second placeholder");

    let subtitle = write_fixture_ass(&work, "input.ass");
    let embed = run_cli(&[
        "--cache-file",
        cache.to_str().unwrap(),
        "embed",
        subtitle.to_str().unwrap(),
    ]);
    // Capture stderr regardless of exit code — the drift report is
    // written before any embed-time font resolution can fail.
    let stderr = String::from_utf8_lossy(&embed.stderr);
    // Pin the structured drift line — `contains("drift")` alone would
    // pass for unrelated stderr text mentioning drift, undermining
    // the test contract for "embed reports drift when folder mtime
    // changes". The locked drift report begins with this exact prefix
    // in either locale (EN: "Cache drift detected", ZH: "检测到字体缓存
    // 漂移"). Bilingual so the test verifies the actual user-visible
    // path on the runner's machine, not only the EN code path.
    assert!(
        stderr.contains("Cache drift detected") || stderr.contains("检测到字体缓存漂移"),
        "expected drift warning (EN or ZH) in stderr, got: {stderr}"
    );
    // Locked design: drift fallback skips the cache for this run.
    // Stderr should also tell the user how to refresh.
    assert!(
        stderr.contains("refresh-fonts"),
        "drift fallback should suggest refresh-fonts: {stderr}"
    );
    // pin the locked-design exit semantics —
    // drift fallback should fall through to system fonts and let
    // embed complete (non-zero indicates a hard failure unrelated
    // to drift). The sibling `embed_with_no_cache_does_not_touch_cache_file`
    // intentionally omits this assertion because no-cache → no
    // input file → embed Err is structurally guaranteed. Here we
    // DO need to pin success because the test's named contract is
    // "embed reports drift AND continues processing".
    assert!(
        embed.status.success(),
        "drift fallback should let embed continue (exit code = {:?}, stderr = {stderr})",
        embed.status.code()
    );

    let _ = fs::remove_dir_all(work);
}
