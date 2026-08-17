//! End-to-end coverage for CLI path normalization, output reservations,
//! zero-cue behavior, and BOM-less UTF-16 warning propagation.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

mod common;
use common::make_real_font_dir;

const VALID_SRT: &str = "1\n00:00:01,000 --> 00:00:02,000\nhello\n";
const VALID_VTT: &str = "WEBVTT\n\n00:00:01.000 --> 00:00:02.000\nhello\n";

fn cli_path() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_ssahdrify-cli"))
}

fn temp_dir(label: &str) -> PathBuf {
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir = std::env::temp_dir().join(format!(
        "ssahdrify-cli-shift-{label}-{}-{stamp}",
        std::process::id()
    ));
    fs::create_dir_all(&dir).unwrap();
    dir
}

fn write_file(parent: &Path, name: &str, content: &str) -> PathBuf {
    fs::create_dir_all(parent).unwrap();
    let path = parent.join(name);
    fs::write(&path, content).unwrap();
    path
}

fn write_bomless_utf16le(parent: &Path, name: &str, content: &str) -> PathBuf {
    fs::create_dir_all(parent).unwrap();
    let path = parent.join(name);
    let bytes: Vec<u8> = content.encode_utf16().flat_map(u16::to_le_bytes).collect();
    fs::write(&path, bytes).unwrap();
    path
}

fn run_shift(cwd: &Path, global_args: &[&str], inputs: &[&Path]) -> Output {
    let mut command = Command::new(cli_path());
    command.current_dir(cwd).args(["--lang", "en"]);
    command.args(global_args);
    command.args(["shift", "--offset", "+1s"]);
    command.args(inputs);
    command.output().expect("failed to run shift command")
}

#[test]
fn standalone_shift_reports_inert_cache_flags_without_corrupting_json_stdout() {
    let root = temp_dir("inert-cache-flags");
    let input = write_file(&root, "episode.srt", VALID_SRT);
    let cache = root.join("unused.sqlite3");

    let output = run_shift(
        &root,
        &[
            "--json",
            "--dry-run",
            "--no-cache",
            "--cache-file",
            cache.to_str().unwrap(),
        ],
        &[&input],
    );

    assert!(output.status.success(), "{}", combined_output(&output));
    serde_json::from_slice::<serde_json::Value>(&output.stdout)
        .expect("cache-flag notice must not corrupt JSON stdout");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("shift does not use the persistent font cache"));
    assert!(stderr.contains("--no-cache"));
    assert!(stderr.contains("--cache-file"));
    assert_eq!(
        stderr.matches("have no effect here").count(),
        1,
        "both inert flags should share one notice: {stderr}"
    );
    assert!(!cache.exists(), "inert cache path must not be created");
    assert!(!root.join("episode.shifted.srt").exists());
    let _ = fs::remove_dir_all(root);
}

fn combined_output(output: &Output) -> String {
    format!(
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

#[test]
fn shift_normalizes_parent_components_in_input_and_output_directory() {
    let root = temp_dir("relative-paths");
    let run_dir = root.join("run");
    let subs_dir = root.join("subs");
    let out_dir = root.join("out");
    fs::create_dir_all(&run_dir).unwrap();
    write_file(&subs_dir, "episode.srt", VALID_SRT);

    let output = run_shift(
        &run_dir,
        &["--output-dir", "../out"],
        &[Path::new("../subs/episode.srt")],
    );

    assert!(output.status.success(), "{}", combined_output(&output));
    let shifted = fs::read_to_string(out_dir.join("episode.shifted.srt")).unwrap();
    assert!(shifted.contains("00:00:02,000 --> 00:00:03,000"));
    let _ = fs::remove_dir_all(root);
}

#[test]
fn existing_output_skip_releases_the_reservation_for_later_inputs() {
    let root = temp_dir("skip-release");
    let out_dir = root.join("out");
    let first = write_file(&root.join("one"), "episode.srt", VALID_SRT);
    let second = write_file(&root.join("two"), "episode.srt", VALID_SRT);
    write_file(&out_dir, "episode.shifted.srt", "pre-existing");

    let output = run_shift(
        &root,
        &["--output-dir", out_dir.to_str().unwrap()],
        &[&first, &second],
    );
    let combined = combined_output(&output);

    assert!(output.status.success(), "{combined}");
    assert!(combined.contains("2 skipped"), "{combined}");
    assert!(!combined.contains("duplicate output path"), "{combined}");
    let _ = fs::remove_dir_all(root);
}

#[test]
fn failed_conversion_releases_the_reservation_for_a_later_valid_input() {
    let root = temp_dir("failure-release");
    let out_dir = root.join("out");
    fs::create_dir_all(&out_dir).unwrap();
    let first = write_file(
        &root.join("one"),
        "episode.vtt",
        "WEBVTT\n\nNOTE no shiftable cue\ncomment\n",
    );
    let second = write_file(&root.join("two"), "episode.vtt", VALID_VTT);

    let output = run_shift(
        &root,
        &["--output-dir", out_dir.to_str().unwrap()],
        &[&first, &second],
    );
    let combined = combined_output(&output);

    assert!(
        !output.status.success(),
        "one failed input should keep exit nonzero"
    );
    assert!(
        combined.contains("No shiftable subtitle cues"),
        "{combined}"
    );
    assert!(!combined.contains("duplicate output path"), "{combined}");
    let shifted = fs::read_to_string(out_dir.join("episode.shifted.vtt")).unwrap();
    assert!(shifted.contains("00:00:02.000 --> 00:00:03.000"));
    let _ = fs::remove_dir_all(root);
}

#[test]
fn planned_and_written_outputs_keep_their_batch_reservations() {
    for (label, global_mode) in [("planned", "--dry-run"), ("written", "--overwrite")] {
        let root = temp_dir(label);
        let out_dir = root.join("out");
        fs::create_dir_all(&out_dir).unwrap();
        let first = write_file(&root.join("one"), "episode.srt", VALID_SRT);
        let second = write_file(&root.join("two"), "episode.srt", VALID_SRT);

        let output = run_shift(
            &root,
            &[global_mode, "--output-dir", out_dir.to_str().unwrap()],
            &[&first, &second],
        );
        let combined = combined_output(&output);

        assert!(!output.status.success(), "{combined}");
        assert!(combined.contains("duplicate output path"), "{combined}");
        if global_mode == "--dry-run" {
            assert!(!out_dir.join("episode.shifted.srt").exists());
        } else {
            assert!(out_dir.join("episode.shifted.srt").exists());
        }
        let _ = fs::remove_dir_all(root);
    }
}

#[test]
fn format_template_heavy_shift_keeps_planned_and_written_reservations() {
    for (label, global_mode) in [
        ("heavy-planned", "--dry-run"),
        ("heavy-written", "--overwrite"),
    ] {
        let root = temp_dir(label);
        let out_dir = root.join("out");
        fs::create_dir_all(&out_dir).unwrap();
        let first = write_file(&root.join("one"), "episode.srt", VALID_SRT);
        let second = write_file(&root.join("two"), "episode.srt", VALID_SRT);

        let output = Command::new(cli_path())
            .current_dir(&root)
            .args([
                "--lang",
                "en",
                global_mode,
                "--output-dir",
                out_dir.to_str().unwrap(),
                "shift",
                "--offset",
                "+1s",
                "--output-template",
                "{name}.{format}.shifted{ext}",
            ])
            .args([&first, &second])
            .output()
            .expect("failed to run heavy shift command");
        let combined = combined_output(&output);

        assert!(!output.status.success(), "{combined}");
        assert!(combined.contains("duplicate output path"), "{combined}");
        let expected = out_dir.join("episode.srt.shifted.srt");
        assert_eq!(expected.exists(), global_mode == "--overwrite");
        let _ = fs::remove_dir_all(root);
    }
}

#[test]
fn failed_hdr_conversion_releases_its_output_reservation() {
    let root = temp_dir("hdr-failure-release");
    let out_dir = root.join("out");
    fs::create_dir_all(&out_dir).unwrap();
    let first = write_file(
        &root.join("one"),
        "episode.vtt",
        "WEBVTT\n\nNOTE no convertible cue\ncomment\n",
    );
    let second = write_file(&root.join("two"), "episode.srt", VALID_SRT);

    let output = Command::new(cli_path())
        .current_dir(&root)
        .args([
            "--lang",
            "en",
            "--output-dir",
            out_dir.to_str().unwrap(),
            "hdr",
            "--eotf",
            "pq",
        ])
        .args([&first, &second])
        .output()
        .expect("failed to run HDR command");
    let combined = combined_output(&output);

    assert!(!output.status.success(), "{combined}");
    assert!(combined.contains("No subtitle cues detected"), "{combined}");
    assert!(!combined.contains("duplicate output path"), "{combined}");
    assert!(out_dir.join("episode.hdr.ass").exists());
    let _ = fs::remove_dir_all(root);
}

#[test]
fn comment_only_vtt_fails_without_creating_an_output() {
    let root = temp_dir("zero-cue");
    let input = write_file(
        &root,
        "empty.vtt",
        "WEBVTT\n\nNOTE no shiftable cue\ncomment\n",
    );

    let output = run_shift(&root, &[], &[&input]);

    assert!(!output.status.success());
    assert!(combined_output(&output).contains("No shiftable subtitle cues"));
    assert!(!root.join("empty.shifted.vtt").exists());
    let _ = fs::remove_dir_all(root);
}

#[test]
fn bomless_utf16_inference_is_reported_and_converted_safely() {
    let root = temp_dir("bomless-utf16");
    let input = write_bomless_utf16le(&root, "episode.srt", VALID_SRT);

    let output = run_shift(&root, &["--json"], &[&input]);

    assert!(output.status.success(), "{}", combined_output(&output));
    let report: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("stdout should be JSON");
    assert_eq!(
        report["results"][0]["encoding"],
        "UTF-16LE (inferred, no BOM)"
    );
    assert!(
        report["results"][0]["warnings"]
            .as_array()
            .is_some_and(|warnings| warnings.iter().any(|warning| warning
                .as_str()
                .is_some_and(|text| text.contains("best-effort inference")))),
        "inference warning should be visible in JSON: {report}"
    );
    let shifted = fs::read_to_string(root.join("episode.shifted.srt"))
        .expect("default output should be valid UTF-8");
    assert!(shifted.contains("00:00:02,000 --> 00:00:03,000"));
    let _ = fs::remove_dir_all(root);
}

#[test]
fn refresh_fonts_normalizes_relative_source_and_cache_paths() {
    let root = temp_dir("relative-font-cache");
    let run_dir = root.join("run");
    fs::create_dir_all(&run_dir).unwrap();
    make_real_font_dir(&root);

    let output = Command::new(cli_path())
        .current_dir(&run_dir)
        .args([
            "--lang",
            "en",
            "--cache-file",
            "../cache.sqlite3",
            "refresh-fonts",
            "--font-dir",
            "../fonts",
        ])
        .output()
        .expect("failed to run refresh-fonts");

    assert!(output.status.success(), "{}", combined_output(&output));
    assert!(root.join("cache.sqlite3").exists());
    let _ = fs::remove_dir_all(root);
}

#[test]
fn chain_normalizes_relative_input_and_output_directory() {
    let root = temp_dir("relative-chain-paths");
    let run_dir = root.join("run");
    fs::create_dir_all(&run_dir).unwrap();
    write_file(&root.join("subs"), "episode.srt", VALID_SRT);

    let output = Command::new(cli_path())
        .current_dir(&run_dir)
        .args([
            "--lang",
            "en",
            "--no-cache",
            "--output-dir",
            "../out",
            "chain",
            "shift",
            "--offset",
            "+1s",
            "../subs/episode.srt",
        ])
        .output()
        .expect("failed to run chain");

    assert!(output.status.success(), "{}", combined_output(&output));
    let shifted = fs::read_to_string(root.join("out").join("episode.shifted.ass")).unwrap();
    assert!(shifted.contains("00:00:02,000 --> 00:00:03,000"));
    let _ = fs::remove_dir_all(root);
}
