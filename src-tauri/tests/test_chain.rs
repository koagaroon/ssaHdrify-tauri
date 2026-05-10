//! End-to-end smoke test for the `chain` subcommand.
//!
//! Runs the built `ssahdrify-cli` binary against fixture ASS content
//! and asserts the round-trip contracts:
//!
//! 1. `chain hdr + shift` produces output byte-identical to running
//!    `hdr` then `shift` as separate standalone invocations. This is
//!    the load-bearing equivalence — without it, chain semantics
//!    drift from per-feature semantics.
//! 2. `chain --dry-run` prints a plan without writing files.
//! 3. Multi-file batches process every input.
//! 4. `--overwrite` toggles skip-on-exists vs replace.
//!
//! **Prerequisite**: `npm run build:engine` must have run first to
//! produce `dist-engine/engine.js`. Without it, build.rs falls back
//! to a stub that throws "Run `npm run build:engine`" on any chain
//! invocation. The first test reads the stderr from a chain attempt
//! and skips the suite with a clear message if it detects the stub.
//!
//! Run with:
//!     cd src-tauri && cargo test --test test_chain --release

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

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
    "Dialogue: 0,0:00:04.00,0:00:06.00,Default,,0,0,0,,Second line\n",
);

fn cli_path() -> PathBuf {
    // Cargo sets CARGO_BIN_EXE_<binname> for integration tests in
    // `tests/`. The binary is built automatically before tests run,
    // but the engine bundle (dist-engine/engine.js) is NOT — that
    // requires `npm run build:engine` separately.
    PathBuf::from(env!("CARGO_BIN_EXE_ssahdrify-cli"))
}

fn temp_dir(label: &str) -> PathBuf {
    let pid = std::process::id();
    let nano = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let dir = std::env::temp_dir().join(format!("ssahdrify-cli-chain-test-{label}-{pid}-{nano}"));
    fs::create_dir_all(&dir).expect("failed to create test temp dir");
    dir
}

fn write_fixture(dir: &Path, name: &str) -> PathBuf {
    let path = dir.join(name);
    fs::write(&path, FIXTURE_ASS).expect("failed to write fixture");
    path
}

/// Returns Some(reason) if the engine bundle is the missing-bundle
/// stub from build.rs (which throws "Run `npm run build:engine`").
/// Returns None when the real bundle is loaded and tests can proceed.
fn engine_bundle_missing() -> Option<String> {
    // A chain invocation that would otherwise succeed surfaces the
    // stub error if engine.js wasn't built. We pass a non-existent
    // file so non-stub builds also fail (with a different error
    // about the missing input file), letting us distinguish the two.
    //
    // `--no-cache` keeps the probe from touching the user's real
    // default cache file (would race with their live GUI / CLI usage).
    let output = Command::new(cli_path())
        .args([
            "--no-cache",
            "chain",
            "hdr",
            "--eotf",
            "pq",
            "+",
            "shift",
            "--offset",
            "+2s",
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

#[test]
fn chain_hdr_shift_byte_equals_sequential_runs() {
    if let Some(reason) = engine_bundle_missing() {
        eprintln!("SKIP: {reason}");
        return;
    }

    let chain_dir = temp_dir("chain");
    let seq_dir = temp_dir("seq");
    let chain_input = write_fixture(&chain_dir, "cat.ass");
    let seq_input = write_fixture(&seq_dir, "cat.ass");

    // Chain: HDR + Shift in one invocation.
    let chain_status = Command::new(cli_path())
        .args([
            "chain", "hdr", "--eotf", "pq", "+", "shift", "--offset", "+2s",
        ])
        .arg(&chain_input)
        .status()
        .expect("failed to run chain");
    assert!(chain_status.success(), "chain run failed");

    // Sequential: hdr standalone, then shift standalone on the hdr output.
    let hdr_status = Command::new(cli_path())
        .args(["hdr", "--eotf", "pq"])
        .arg(&seq_input)
        .status()
        .expect("failed to run hdr");
    assert!(hdr_status.success(), "hdr run failed");
    let hdr_output = seq_dir.join("cat.hdr.ass");
    let shift_status = Command::new(cli_path())
        .args(["shift", "--offset", "+2s"])
        .arg(&hdr_output)
        .status()
        .expect("failed to run shift");
    assert!(shift_status.success(), "shift run failed");

    // Both flows produce <name>.hdr.shifted.ass.
    let chain_out =
        fs::read_to_string(chain_dir.join("cat.hdr.shifted.ass")).expect("chain output not found");
    let seq_out = fs::read_to_string(seq_dir.join("cat.hdr.shifted.ass"))
        .expect("sequential output not found");
    assert_eq!(
        chain_out, seq_out,
        "chain output must be byte-identical to sequential standalone runs"
    );

    let _ = fs::remove_dir_all(chain_dir);
    let _ = fs::remove_dir_all(seq_dir);
}

#[test]
fn chain_dry_run_prints_plan_without_writing() {
    if let Some(reason) = engine_bundle_missing() {
        eprintln!("SKIP: {reason}");
        return;
    }

    let dir = temp_dir("dryrun");
    let input = write_fixture(&dir, "cat.ass");

    let output = Command::new(cli_path())
        .args([
            "--dry-run",
            "chain",
            "hdr",
            "--eotf",
            "pq",
            "+",
            "shift",
            "--offset",
            "+2s",
        ])
        .arg(&input)
        .output()
        .expect("failed to run chain");
    assert!(output.status.success(), "dry-run should succeed");

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("Plan (no files written)"),
        "stdout: {stdout}"
    );
    assert!(stdout.contains("hdr"), "stdout: {stdout}");
    assert!(stdout.contains("shift"), "stdout: {stdout}");

    // No output file should exist.
    assert!(
        !dir.join("cat.hdr.shifted.ass").exists(),
        "dry-run must not write files"
    );

    let _ = fs::remove_dir_all(dir);
}

#[test]
fn chain_multi_file_batch_processes_all_inputs() {
    if let Some(reason) = engine_bundle_missing() {
        eprintln!("SKIP: {reason}");
        return;
    }

    let dir = temp_dir("multi");
    let input_a = write_fixture(&dir, "a.ass");
    let input_b = write_fixture(&dir, "b.ass");

    let status = Command::new(cli_path())
        .args([
            "chain", "hdr", "--eotf", "pq", "+", "shift", "--offset", "+2s",
        ])
        .arg(&input_a)
        .arg(&input_b)
        .status()
        .expect("failed to run chain");
    assert!(status.success(), "chain run failed");

    assert!(dir.join("a.hdr.shifted.ass").exists(), "a output missing");
    assert!(dir.join("b.hdr.shifted.ass").exists(), "b output missing");

    let _ = fs::remove_dir_all(dir);
}

#[test]
fn chain_overwrite_toggles_skip_vs_replace() {
    if let Some(reason) = engine_bundle_missing() {
        eprintln!("SKIP: {reason}");
        return;
    }

    let dir = temp_dir("overwrite");
    let input = write_fixture(&dir, "cat.ass");

    // First run: writes the output.
    let first = Command::new(cli_path())
        .args([
            "chain", "hdr", "--eotf", "pq", "+", "shift", "--offset", "+2s",
        ])
        .arg(&input)
        .status()
        .expect("first run failed to spawn");
    assert!(first.success(), "first chain run failed");
    let output_path = dir.join("cat.hdr.shifted.ass");
    assert!(output_path.exists(), "first run didn't produce output");
    let first_content = fs::read_to_string(&output_path).unwrap();

    // Second run without --overwrite: skips.
    let second_output = Command::new(cli_path())
        .args([
            "chain", "hdr", "--eotf", "pq", "+", "shift", "--offset", "+2s",
        ])
        .arg(&input)
        .output()
        .expect("second run failed to spawn");
    assert!(
        second_output.status.success(),
        "second run should still exit 0"
    );
    let stdout = String::from_utf8_lossy(&second_output.stdout);
    // Pin the skip evidence: the per-file "⊘ ... already exists ..."
    // line AND the summary's "0 written, 1 skipped" reading. Substring
    // "skipped" alone would also pass on a partial-write that emits
    // unrelated "skipped" text (e.g., "skipped (placeholder)"); the
    // pair pins exactly the contract we want to test.
    assert!(
        stdout.contains("already exists (use --overwrite to replace)"),
        "expected skip explanation in stdout: {stdout}"
    );
    assert!(
        stdout.contains("0 written, 1 skipped, 0 failed"),
        "expected skip in summary line: {stdout}"
    );
    let unchanged = fs::read_to_string(&output_path).unwrap();
    assert_eq!(first_content, unchanged, "skip path mutated the file");

    // Third run WITH --overwrite: rewrites (content same so byte
    // equality holds, but the operation should report 1 written
    // not 1 skipped).
    let third_output = Command::new(cli_path())
        .args([
            "--overwrite",
            "chain",
            "hdr",
            "--eotf",
            "pq",
            "+",
            "shift",
            "--offset",
            "+2s",
        ])
        .arg(&input)
        .output()
        .expect("third run failed to spawn");
    assert!(third_output.status.success(), "third run should succeed");
    let stdout = String::from_utf8_lossy(&third_output.stdout);
    assert!(
        stdout.contains("1 written"),
        "expected 'written' in --overwrite stdout: {stdout}"
    );

    let _ = fs::remove_dir_all(dir);
}

#[test]
fn chain_rejects_in_step_output_template() {
    // Locked design: --output-template inside any step segment is a
    // parse-time error. Pin exit code = 2 (the conventional clap /
    // CLI usage-error code; matches what the CLI returns for the
    // analogous parse failure surface).
    let output = Command::new(cli_path())
        .args([
            "chain",
            "hdr",
            "--eotf",
            "pq",
            "--output-template",
            "ignored.ass",
            "+",
            "shift",
            "--offset",
            "+2s",
            "/nonexistent.ass",
        ])
        .output()
        .expect("failed to spawn");
    assert_eq!(
        output.status.code(),
        Some(2),
        "expected exit code 2 (parse error); got {:?}, stderr={}",
        output.status.code(),
        String::from_utf8_lossy(&output.stderr)
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("--output-template") && stderr.contains("chain-level"),
        "stderr should explain chain-level requirement: {stderr}"
    );
}
