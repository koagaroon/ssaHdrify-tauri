//! CLI diagnostics smoke tests.
//!
//! These tests keep the new diagnostics surface pinned at the process
//! boundary: command-specific `--diagnose`, standalone `diagnose-fonts`,
//! JSON shape opt-in, and the louder partial-success summary.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::thread;
use std::time::Duration;

const MISSING_FONT_ASS: &str = concat!(
    "[Script Info]\n",
    "ScriptType: v4.00+\n",
    "\n",
    "[V4+ Styles]\n",
    "Format: Name, Fontname, Fontsize, PrimaryColour, SecondaryColour, OutlineColour, BackColour, Bold, Italic, Underline, StrikeOut, ScaleX, ScaleY, Spacing, Angle, BorderStyle, Outline, Shadow, Alignment, MarginL, MarginR, MarginV, Encoding\n",
    "Style: Default,DefinitelyMissingSsaHdrifyFont,20,&H00FFFFFF,&H000000FF,&H00000000,&H00000000,0,0,0,0,100,100,0,0,1,2,2,2,10,10,10,1\n",
    "\n",
    "[Events]\n",
    "Format: Layer, Start, End, Style, Name, MarginL, MarginR, MarginV, Effect, Text\n",
    "Dialogue: 0,0:00:01.00,0:00:03.00,Default,,0,0,0,,Hello world\n",
);
const MISSING_FONT_EMBEDDED_NAME: &str = "definitelymissingssahdrifyfont.ttf";

fn cli_path() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_ssahdrify-cli"))
}

fn temp_dir(label: &str) -> PathBuf {
    let pid = std::process::id();
    let nano = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let dir = std::env::temp_dir().join(format!("ssahdrify-cli-diagnostics-{label}-{pid}-{nano}"));
    fs::create_dir_all(&dir).expect("failed to create test temp dir");
    dir
}

fn write_missing_font_ass(dir: &Path) -> PathBuf {
    let path = dir.join("missing-font.ass");
    fs::write(&path, MISSING_FONT_ASS).expect("failed to write fixture ASS");
    path
}

fn make_font_dir(dir: &Path) -> PathBuf {
    let font_dir = dir.join("fonts");
    fs::create_dir_all(&font_dir).expect("failed to create fonts subdir");
    fs::write(font_dir.join("placeholder.ttf"), b"").expect("failed to write placeholder ttf");
    font_dir
}

fn sqlite_sidecar_path(path: &Path, suffix: &str) -> PathBuf {
    let mut sidecar = path.as_os_str().to_os_string();
    sidecar.push(suffix);
    PathBuf::from(sidecar)
}

fn optional_modified(path: &Path) -> Option<std::time::SystemTime> {
    fs::metadata(path)
        .ok()
        .and_then(|metadata| metadata.modified().ok())
}

fn run_cli(args: &[&str]) -> std::process::Output {
    Command::new(cli_path())
        .args(args)
        .output()
        .expect("failed to spawn ssahdrify-cli")
}

fn engine_bundle_missing() -> Option<String> {
    let output = run_cli(&[
        "--lang",
        "en",
        "--no-cache",
        "embed",
        "/nonexistent-test-input-do-not-create.ass",
    ]);
    let stderr = String::from_utf8_lossy(&output.stderr);
    if stderr.contains("Run `npm run build:engine`") {
        Some(format!(
            "engine bundle missing — run `npm run build:engine` first ({stderr})"
        ))
    } else {
        None
    }
}

#[test]
fn embed_diagnose_reports_written_with_warnings() {
    if let Some(reason) = engine_bundle_missing() {
        // Hard-fail instead of skip-and-return: a skip records PASS in Cargo,
        // so a forgotten `npm run build:engine` would ship every diagnostics
        // integration test as a green no-op. Matches the panic guard in
        // test_chain.rs / test_font_cache.rs.
        panic!("engine bundle missing — run `npm run build:engine` first ({reason})");
    }

    let work = temp_dir("embed-warnings");
    let input = write_missing_font_ass(&work);
    let output = run_cli(&[
        "--lang",
        "en",
        "--no-cache",
        "embed",
        "--no-system-fonts",
        "--diagnose",
        input.to_str().unwrap(),
    ]);

    assert!(
        output.status.success(),
        "embed should succeed under default warn mode: stderr={}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("written with warnings / incomplete"),
        "summary should make partial success visible: stdout={stdout}"
    );
    assert!(
        work.join("missing-font.embed.ass").exists(),
        "embed should still write the subtitle under warn mode"
    );

    let _ = fs::remove_dir_all(work);
}

#[test]
fn diagnose_fonts_reports_missing_without_writing_output() {
    if let Some(reason) = engine_bundle_missing() {
        // Hard-fail instead of skip-and-return: a skip records PASS in Cargo,
        // so a forgotten `npm run build:engine` would ship every diagnostics
        // integration test as a green no-op. Matches the panic guard in
        // test_chain.rs / test_font_cache.rs.
        panic!("engine bundle missing — run `npm run build:engine` first ({reason})");
    }

    let work = temp_dir("standalone");
    let input = write_missing_font_ass(&work);
    let output = run_cli(&[
        "--lang",
        "en",
        "--no-cache",
        "diagnose-fonts",
        "--no-system-fonts",
        input.to_str().unwrap(),
    ]);

    assert!(
        output.status.success(),
        "diagnose-fonts should complete for a readable ASS: stderr={}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("DefinitelyMissingSsaHdrifyFont") && stdout.contains("Missing"),
        "standalone diagnostics should name the missing font: stdout={stdout}"
    );
    assert!(
        stdout.contains("Font QA: incomplete")
            && stdout.contains("fonts: 0/1")
            && stdout.contains("missing: 1"),
        "standalone diagnostics should include a package-level QA verdict: stdout={stdout}"
    );
    assert!(
        stdout.contains("embedded label:") && stdout.contains(MISSING_FONT_EMBEDDED_NAME),
        "standalone diagnostics should show the generated ASS [Fonts] label: stdout={stdout}"
    );
    assert!(
        stdout.contains("next actions:")
            && stdout.contains("pass `--font-dir <DIR>` or `--font-file <FILE>`"),
        "standalone diagnostics should suggest a concrete font-source next step: stdout={stdout}"
    );
    assert!(
        !work.join("missing-font.embed.ass").exists(),
        "diagnose-fonts must not write subtitle outputs"
    );

    let _ = fs::remove_dir_all(work);
}

#[test]
fn diagnose_fonts_next_actions_are_localized() {
    if let Some(reason) = engine_bundle_missing() {
        // Hard-fail instead of skip-and-return: a skip records PASS in Cargo,
        // so a forgotten `npm run build:engine` would ship every diagnostics
        // integration test as a green no-op. Matches the panic guard in
        // test_chain.rs / test_font_cache.rs.
        panic!("engine bundle missing — run `npm run build:engine` first ({reason})");
    }

    let work = temp_dir("standalone-next-actions-zh");
    let input = write_missing_font_ass(&work);
    let output = run_cli(&[
        "--lang",
        "zh",
        "--no-cache",
        "diagnose-fonts",
        "--no-system-fonts",
        input.to_str().unwrap(),
    ]);

    assert!(
        output.status.success(),
        "diagnose-fonts should complete for a readable ASS: stderr={}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("下一步建议：") && stdout.contains("请为字体包传入"),
        "next-action guidance should be localized under --lang zh: stdout={stdout}"
    );

    let _ = fs::remove_dir_all(work);
}

#[test]
fn diagnose_fonts_json_reports_successful_files_as_diagnosed() {
    if let Some(reason) = engine_bundle_missing() {
        // Hard-fail instead of skip-and-return: a skip records PASS in Cargo,
        // so a forgotten `npm run build:engine` would ship every diagnostics
        // integration test as a green no-op. Matches the panic guard in
        // test_chain.rs / test_font_cache.rs.
        panic!("engine bundle missing — run `npm run build:engine` first ({reason})");
    }

    let work = temp_dir("standalone-json-status");
    let input = write_missing_font_ass(&work);
    let output = run_cli(&[
        "--lang",
        "en",
        "--json",
        "--no-cache",
        "diagnose-fonts",
        "--no-system-fonts",
        input.to_str().unwrap(),
    ]);

    assert!(
        output.status.success(),
        "diagnose-fonts --json should complete for a readable ASS: stderr={}",
        String::from_utf8_lossy(&output.stderr)
    );
    let value: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("stdout should be JSON");
    assert_eq!(value["files"][0]["status"], "diagnosed");
    assert_eq!(value["files"][0]["output"], serde_json::Value::Null);
    assert_eq!(value["qa"]["status"], "incomplete");
    assert_eq!(value["qa"]["fontReferenceCount"], 1);
    assert_eq!(value["qa"]["missingCount"], 1);
    assert_eq!(
        value["fonts"][0]["embeddedFontName"],
        MISSING_FONT_EMBEDDED_NAME
    );
    assert!(
        !String::from_utf8_lossy(&output.stdout).contains("next actions"),
        "human next-action prose must not be mixed into JSON stdout"
    );

    let _ = fs::remove_dir_all(work);
}

#[test]
fn diagnose_fonts_subset_check_reports_skipped_for_unresolved_without_writing_output() {
    if let Some(reason) = engine_bundle_missing() {
        // Hard-fail instead of skip-and-return: a skip records PASS in Cargo,
        // so a forgotten `npm run build:engine` would ship every diagnostics
        // integration test as a green no-op. Matches the panic guard in
        // test_chain.rs / test_font_cache.rs.
        panic!("engine bundle missing — run `npm run build:engine` first ({reason})");
    }

    let work = temp_dir("subset-check-unresolved");
    let input = write_missing_font_ass(&work);
    let output = run_cli(&[
        "--lang",
        "en",
        "--json",
        "--no-cache",
        "diagnose-fonts",
        "--no-system-fonts",
        "--subset-check",
        input.to_str().unwrap(),
    ]);

    assert!(
        output.status.success(),
        "diagnose-fonts --subset-check should complete for unresolved fonts: stderr={}",
        String::from_utf8_lossy(&output.stderr)
    );
    let value: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("stdout should be JSON");
    assert_eq!(value["files"][0]["status"], "diagnosed");
    assert_eq!(value["qa"]["status"], "incomplete");
    assert_eq!(value["qa"]["subsetSkippedCount"], 1);
    assert_eq!(
        value["fonts"][0]["subsetCheck"]["status"], "skipped",
        "unresolved fonts should report a skipped subset check: {value}"
    );
    assert!(
        value["fonts"][0]["subsetCheck"]["error"]
            .as_str()
            .is_some_and(|error| error.contains("not resolved")),
        "skipped subset check should explain why: {value}"
    );
    assert!(
        !work.join("missing-font.embed.ass").exists(),
        "diagnose-fonts --subset-check must not write subtitle outputs"
    );

    let _ = fs::remove_dir_all(work);
}

#[cfg(target_os = "windows")]
#[test]
fn diagnose_fonts_does_not_validate_embed_output_path() {
    if let Some(reason) = engine_bundle_missing() {
        // Hard-fail instead of skip-and-return: a skip records PASS in Cargo,
        // so a forgotten `npm run build:engine` would ship every diagnostics
        // integration test as a green no-op. Matches the panic guard in
        // test_chain.rs / test_font_cache.rs.
        panic!("engine bundle missing — run `npm run build:engine` first ({reason})");
    }

    let work = temp_dir("standalone-output-free");
    let input = (1..220)
        .map(|len| {
            let stem = "a".repeat(len);
            let input = work.join(format!("{stem}.ass"));
            let output = work.join(format!("{stem}.embed.ass"));
            (input, output)
        })
        .find(|(input, output)| {
            input.to_string_lossy().encode_utf16().count() <= 259
                && output.to_string_lossy().encode_utf16().count() > 259
        })
        .map(|(input, _)| input)
        .expect("test temp path should leave room for a near-MAX_PATH fixture");
    fs::write(&input, MISSING_FONT_ASS).expect("failed to write long-path ASS fixture");

    let output = run_cli(&[
        "--lang",
        "en",
        "--no-cache",
        "diagnose-fonts",
        "--no-system-fonts",
        input.to_str().unwrap(),
    ]);

    assert!(
        output.status.success(),
        "diagnose-fonts should not fail just because the embed output path would be invalid: stderr={}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("DefinitelyMissingSsaHdrifyFont") && stdout.contains("Missing"),
        "standalone diagnostics should still inspect fonts: stdout={stdout}"
    );

    let _ = fs::remove_dir_all(work);
}

#[test]
fn diagnose_fonts_does_not_mutate_cache_file() {
    if let Some(reason) = engine_bundle_missing() {
        // Hard-fail instead of skip-and-return: a skip records PASS in Cargo,
        // so a forgotten `npm run build:engine` would ship every diagnostics
        // integration test as a green no-op. Matches the panic guard in
        // test_chain.rs / test_font_cache.rs.
        panic!("engine bundle missing — run `npm run build:engine` first ({reason})");
    }

    let work = temp_dir("cache-readonly");
    let input = write_missing_font_ass(&work);
    let font_dir = make_font_dir(&work);
    let cache = work.join("cache.sqlite3");
    let refresh = run_cli(&[
        "--lang",
        "en",
        "--cache-file",
        cache.to_str().unwrap(),
        "refresh-fonts",
        "--font-dir",
        font_dir.to_str().unwrap(),
    ]);
    assert!(
        refresh.status.success(),
        "refresh-fonts should create cache: stderr={}",
        String::from_utf8_lossy(&refresh.stderr)
    );
    let before = fs::metadata(&cache)
        .expect("cache metadata before diagnose")
        .modified()
        .expect("cache mtime before diagnose");
    let before_wal = sqlite_sidecar_path(&cache, "-wal");
    let before_shm = sqlite_sidecar_path(&cache, "-shm");
    let before_wal_modified = optional_modified(&before_wal);
    let before_shm_modified = optional_modified(&before_shm);
    thread::sleep(Duration::from_millis(20));

    let diagnose = run_cli(&[
        "--lang",
        "en",
        "--cache-file",
        cache.to_str().unwrap(),
        "diagnose-fonts",
        "--no-system-fonts",
        "--subset-check",
        input.to_str().unwrap(),
    ]);
    assert!(
        diagnose.status.success(),
        "diagnose-fonts should inspect existing cache read-only: stderr={}",
        String::from_utf8_lossy(&diagnose.stderr)
    );
    let after = fs::metadata(&cache)
        .expect("cache metadata after diagnose")
        .modified()
        .expect("cache mtime after diagnose");
    assert_eq!(
        before, after,
        "diagnose-fonts must not mutate the cache file"
    );
    assert_eq!(
        before_wal_modified,
        optional_modified(&before_wal),
        "diagnose-fonts must not create, remove, or modify the cache WAL sidecar"
    );
    assert_eq!(
        before_shm_modified,
        optional_modified(&before_shm),
        "diagnose-fonts must not create, remove, or modify the cache SHM sidecar"
    );

    let _ = fs::remove_dir_all(work);
}

#[test]
fn json_with_diagnose_includes_full_diagnostics() {
    if let Some(reason) = engine_bundle_missing() {
        // Hard-fail instead of skip-and-return: a skip records PASS in Cargo,
        // so a forgotten `npm run build:engine` would ship every diagnostics
        // integration test as a green no-op. Matches the panic guard in
        // test_chain.rs / test_font_cache.rs.
        panic!("engine bundle missing — run `npm run build:engine` first ({reason})");
    }

    let work = temp_dir("json");
    let input = write_missing_font_ass(&work);
    let output = run_cli(&[
        "--lang",
        "en",
        "--json",
        "--no-cache",
        "embed",
        "--no-system-fonts",
        "--diagnose",
        input.to_str().unwrap(),
    ]);

    assert!(
        output.status.success(),
        "embed --json --diagnose should succeed: stderr={}",
        String::from_utf8_lossy(&output.stderr)
    );
    let value: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("stdout should be JSON");
    assert_eq!(value["diagnostics"]["mode"], "summary");
    assert_eq!(value["diagnostics"]["qa"]["status"], "incomplete");
    assert_eq!(value["diagnostics"]["qa"]["missingCount"], 1);
    assert!(
        value["diagnostics"]["files"]
            .as_array()
            .is_some_and(|files| !files.is_empty()),
        "diagnostic JSON should include full per-file details: {value}"
    );
    assert!(
        value["diagnostics"]["fonts"]
            .as_array()
            .is_some_and(|fonts| !fonts.is_empty()),
        "diagnostic JSON should include full font details: {value}"
    );
    assert_eq!(
        value["diagnostics"]["fonts"][0]["embeddedFontName"],
        MISSING_FONT_EMBEDDED_NAME
    );

    let _ = fs::remove_dir_all(work);
}

#[test]
fn embed_diagnose_full_prints_generated_font_label() {
    if let Some(reason) = engine_bundle_missing() {
        // Hard-fail instead of skip-and-return: a skip records PASS in Cargo,
        // so a forgotten `npm run build:engine` would ship every diagnostics
        // integration test as a green no-op. Matches the panic guard in
        // test_chain.rs / test_font_cache.rs.
        panic!("engine bundle missing — run `npm run build:engine` first ({reason})");
    }

    let work = temp_dir("embed-full-font-label");
    let input = write_missing_font_ass(&work);
    let output = run_cli(&[
        "--lang",
        "en",
        "--no-cache",
        "embed",
        "--no-system-fonts",
        "--diagnose=full",
        input.to_str().unwrap(),
    ]);

    assert!(
        output.status.success(),
        "embed --diagnose=full should succeed under default warn mode: stderr={}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("embedded label:") && stderr.contains(MISSING_FONT_EMBEDDED_NAME),
        "full human diagnostics should show the generated ASS [Fonts] label: stderr={stderr}"
    );

    let _ = fs::remove_dir_all(work);
}

#[test]
fn non_font_command_attaches_compact_diagnostics() {
    if let Some(reason) = engine_bundle_missing() {
        // Hard-fail instead of skip-and-return: a skip records PASS in Cargo,
        // so a forgotten `npm run build:engine` would ship every diagnostics
        // integration test as a green no-op. Matches the panic guard in
        // test_chain.rs / test_font_cache.rs.
        panic!("engine bundle missing — run `npm run build:engine` first ({reason})");
    }

    let work = temp_dir("shift");
    let input = write_missing_font_ass(&work);
    let output = run_cli(&[
        "--lang",
        "en",
        "--dry-run",
        "shift",
        "--offset",
        "+1s",
        "--diagnose",
        input.to_str().unwrap(),
    ]);

    assert!(
        output.status.success(),
        "shift --diagnose should keep normal command semantics: stderr={}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("Diagnostics:"),
        "attached diagnostics should appear after the command result: stderr={stderr}"
    );

    let _ = fs::remove_dir_all(work);
}

#[test]
fn embed_diagnose_failed_missing_font_suggests_full_details() {
    if let Some(reason) = engine_bundle_missing() {
        // Hard-fail instead of skip-and-return: a skip records PASS in Cargo,
        // so a forgotten `npm run build:engine` would ship every diagnostics
        // integration test as a green no-op. Matches the panic guard in
        // test_chain.rs / test_font_cache.rs.
        panic!("engine bundle missing — run `npm run build:engine` first ({reason})");
    }

    let work = temp_dir("embed-fail-next-actions");
    let input = write_missing_font_ass(&work);
    let output = run_cli(&[
        "--lang",
        "en",
        "--no-cache",
        "embed",
        "--no-system-fonts",
        "--on-missing",
        "fail",
        "--diagnose",
        input.to_str().unwrap(),
    ]);

    assert!(
        !output.status.success(),
        "embed --on-missing fail should fail for an unresolved font"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("next actions:")
            && stderr.contains("Rerun with `--diagnose=full`")
            && stderr.contains("pass `--font-dir <DIR>` or `--font-file <FILE>`"),
        "compact diagnostics should give actionable guidance even without warnings: stderr={stderr}"
    );

    let _ = fs::remove_dir_all(work);
}

#[test]
fn diagnose_flag_is_rejected_on_unsupported_commands() {
    // Contract (CLI design doc #9): `--diagnose` is supported ONLY on
    // hdr / shift / embed / rename. chain and refresh-fonts must FAIL on it
    // rather than silently ignore it. Both reject before any engine work
    // (chain via its step parser, refresh-fonts via clap), so this needs no
    // engine bundle and no skip/panic guard. The assertion pins "rejected and
    // names the flag" without coupling to either tool's exact wording.
    let chain = run_cli(&["chain", "--diagnose", "a.ass", "hdr"]);
    assert!(
        !chain.status.success(),
        "chain must reject --diagnose, not silently ignore it"
    );
    let chain_err = String::from_utf8_lossy(&chain.stderr);
    assert!(
        chain_err.contains("--diagnose"),
        "chain rejection should name the offending flag; stderr={chain_err}"
    );

    let refresh = run_cli(&["refresh-fonts", "--diagnose", "--font-dir", "."]);
    assert!(
        !refresh.status.success(),
        "refresh-fonts must reject --diagnose, not silently ignore it"
    );
    let refresh_err = String::from_utf8_lossy(&refresh.stderr);
    assert!(
        refresh_err.contains("--diagnose"),
        "refresh-fonts rejection should name the offending flag; stderr={refresh_err}"
    );
}
