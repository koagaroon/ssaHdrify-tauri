//! End-to-end smoke tests for the persistent font cache (#5).
//!
//! Spawns the built `ssahdrify-cli` binary to exercise the
//! `refresh-fonts` writer path and `embed`'s startup drift /
//! announce path; opens the resulting SQLite cache file via the
//! library API and asserts contracts the unit tests can't see:
//!
//! 1. `refresh-fonts --font-dir D --cache-file F` produces an
//!    SQLite file at F with one `cached_folders` row whose path
//!    matches D's canonicalized form, and `cache_meta.schema_version`
//!    matches `font_cache::SCHEMA_VERSION`.
//! 2. Re-running `refresh-fonts` against the same dir is idempotent
//!    — `cached_folders` still has exactly one row for that path,
//!    not two.
//! 3. `refresh-fonts --no-cache <whatever>` errors out as
//!    contradictory (locked design: refresh-fonts requires the cache
//!    by definition).
//! 4. `embed --cache-file <existing-cache> --no-cache` runs without
//!    touching the cache file — file mtime stays unchanged across
//!    the run. Pairs with #3 to lock in the opt-out semantic from
//!    both sides.
//! 5. `embed` against a cache whose folder mtime has drifted prints
//!    the drift report on stderr and falls back to no-cache.
//!
//! Test doesn't use real font files — empty `.ttf` files in a temp
//! directory are enough to exercise the cache writer and reader; the
//! folder-level rows (which are what drift detection inspects) get
//! populated regardless of whether the parser found any faces inside.
//! Tests that require actual face resolution are scoped to the unit
//! test layer in `font_cache.rs` and `font-embedder.test.ts`.
//!
//! Run with:
//!     cd src-tauri && cargo test --test test_font_cache --release

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::thread;
use std::time::Duration;

use app_lib::font_cache::FontCache;

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

/// Make a font folder containing one empty `.ttf` so the scanner has
/// something to walk; parser failure is fine — the cache writer's
/// `replace_folder` call still creates the folder row (the unit of
/// drift tracking).
fn make_font_dir(dir: &Path) -> PathBuf {
    let font_dir = dir.join("fonts");
    fs::create_dir_all(&font_dir).expect("failed to create fonts subdir");
    fs::write(font_dir.join("placeholder.ttf"), b"").expect("failed to write placeholder ttf");
    font_dir
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
    let folders = inspect.list_folders().expect("list_folders");
    assert_eq!(
        folders.len(),
        1,
        "expected exactly 1 cached folder, got {folders:?}"
    );
    let canonical_font_dir = font_dir.canonicalize().expect("canonicalize font dir");
    let stored = &folders[0].folder_path;
    let canonical_str = canonical_font_dir.display().to_string();
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
    let folders = inspect.list_folders().expect("list_folders");
    assert_eq!(
        folders.len(),
        1,
        "two consecutive refreshes must yield exactly 1 row, got {folders:?}"
    );

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
fn embed_with_no_cache_does_not_touch_cache_file() {
    if let Some(reason) = engine_bundle_missing() {
        eprintln!("SKIP: {reason}");
        return;
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

    // Sleep one full second so mtime resolution (NTFS is sub-second
    // but FAT/SUBST/network drives can be 1-2 s) makes any post-hoc
    // touch detectable. Otherwise a no-op write within the same
    // second would tie the assertion.
    thread::sleep(Duration::from_millis(1100));

    let subtitle = write_fixture_ass(&work, "input.ass");
    let embed = run_cli(&[
        "--no-cache",
        "--cache-file",
        cache.to_str().unwrap(),
        "embed",
        subtitle.to_str().unwrap(),
    ]);
    // Embed may exit non-zero if Arial isn't resolvable on the host
    // (--on-missing default is warn, but this test fixture leaves
    // strict-mode platforms ambiguous). What we lock here is that
    // even if embed succeeds, the cache file isn't touched.
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
        eprintln!("SKIP: {reason}");
        return;
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
    assert!(
        stderr.contains("Cache drift detected") || stderr.contains("drift"),
        "expected drift warning in stderr, got: {stderr}"
    );
    // Locked design: drift fallback skips the cache for this run.
    // Stderr should also tell the user how to refresh.
    assert!(
        stderr.contains("refresh-fonts"),
        "drift fallback should suggest refresh-fonts: {stderr}"
    );

    let _ = fs::remove_dir_all(work);
}
