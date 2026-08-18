//! CLI Batch Rename process-boundary tests.
//!
//! These pin the embedded engine bundle behavior after `npm run build:engine`:
//! `--langs all` must preserve real language suffixes for multi-subtitle
//! sidecars, while duplicate canonical outputs fail before any write.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

fn cli_path() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_ssahdrify-cli"))
}

fn temp_dir(label: &str) -> PathBuf {
    let pid = std::process::id();
    let nano = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let dir = std::env::temp_dir().join(format!("ssahdrify-cli-rename-{label}-{pid}-{nano}"));
    fs::create_dir_all(&dir).expect("failed to create test temp dir");
    dir
}

fn touch(dir: &Path, name: &str) {
    fs::write(dir.join(name), b"").expect("failed to write rename fixture");
}

fn write_fixture(dir: &Path, name: &str, content: &str) -> PathBuf {
    let path = dir.join(name);
    fs::write(&path, content.as_bytes()).expect("failed to write rename fixture");
    path
}

fn run_cli(args: &[&str]) -> std::process::Output {
    Command::new(cli_path())
        .args(args)
        .output()
        .expect("failed to spawn ssahdrify-cli")
}

fn run_cli_owned(args: &[String]) -> std::process::Output {
    Command::new(cli_path())
        .args(args)
        .output()
        .expect("failed to spawn ssahdrify-cli")
}

fn push_path(args: &mut Vec<String>, path: &Path) {
    args.push(path.to_string_lossy().into_owned());
}

fn parse_json_output(output: &std::process::Output) -> serde_json::Value {
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !stderr.contains("Run `npm run build:engine`"),
        "engine bundle missing; run `npm run build:engine` first: {stderr}"
    );
    serde_json::from_slice(&output.stdout).expect("stdout should be JSON")
}

fn normalize(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

fn json_outputs(value: &serde_json::Value) -> Vec<String> {
    let mut outputs: Vec<String> = value["results"]
        .as_array()
        .expect("results should be an array")
        .iter()
        .map(|row| {
            row["output"]
                .as_str()
                .expect("rename row output should be a string")
                .replace('\\', "/")
        })
        .collect();
    outputs.sort();
    outputs
}

#[test]
fn rename_langs_all_dry_run_keeps_distinct_ass_and_sup_language_outputs() {
    let work = temp_dir("langs-all");
    let video = "[RawsX][Show Title][01][1080P][BDRip].mkv";
    touch(&work, video);
    touch(&work, "[SubsA][Show Title][01][1080P][BDRip].sc.ass");
    touch(&work, "[SubsA][Show Title][01][1080P][BDRip].tc.ass");
    touch(&work, "[SubsA][Show Title][01][1080P][BDRip].sc.sup");
    touch(&work, "[SubsA][Show Title][01][1080P][BDRip].tc.sup");

    let work_arg = work.to_string_lossy().to_string();
    let output = run_cli(&[
        "--lang",
        "en",
        "--json",
        "--dry-run",
        "rename",
        "--langs",
        "all",
        &work_arg,
    ]);

    assert!(
        output.status.success(),
        "rename --langs all dry-run should succeed: stderr={}",
        String::from_utf8_lossy(&output.stderr)
    );
    let value = parse_json_output(&output);

    assert_eq!(value["planned"], 4);
    assert_eq!(value["failed"], 0);
    assert_eq!(
        json_outputs(&value),
        vec![
            normalize(&work.join("[RawsX][Show Title][01][1080P][BDRip].sc.ass")),
            normalize(&work.join("[RawsX][Show Title][01][1080P][BDRip].sc.sup")),
            normalize(&work.join("[RawsX][Show Title][01][1080P][BDRip].tc.ass")),
            normalize(&work.join("[RawsX][Show Title][01][1080P][BDRip].tc.sup")),
        ]
    );

    let _ = fs::remove_dir_all(work);
}

#[test]
fn rename_langs_all_alias_collision_fails_before_writes() {
    let work = temp_dir("alias-collision");
    touch(&work, "[RawsX][Show Title][01][1080P][BDRip].mkv");
    touch(&work, "[SubsA][Show Title][01][1080P][BDRip].sc.ass");
    touch(&work, "[SubsB][Show Title][01][1080P][BDRip].zh-CN.ass");

    let work_arg = work.to_string_lossy().to_string();
    let output = run_cli(&[
        "--lang",
        "en",
        "--json",
        "--dry-run",
        "rename",
        "--langs",
        "all",
        &work_arg,
    ]);
    let value = parse_json_output(&output);

    assert!(
        !output.status.success(),
        "alias duplicate outputs should fail the command"
    );
    assert_eq!(value["failed"], 2);
    assert!(
        value["results"]
            .as_array()
            .expect("results should be an array")
            .iter()
            .all(|row| row["error"] == "duplicate output path in planned batch"),
        "all duplicate participants should fail before writes: {value:#}"
    );

    let _ = fs::remove_dir_all(work);
}

#[test]
fn swap_conflicts_fail_before_io_for_all_modes_and_overwrite_settings() {
    for mode in ["rename", "copy-to-video", "copy-to-chosen"] {
        for overwrite in [false, true] {
            let work = temp_dir(&format!("swap-{mode}-{overwrite}"));
            let video_1080 = write_fixture(&work, "[Raw][Show][01][1080p].mkv", "video-1080");
            let video_720 = write_fixture(&work, "[Raw][Show][01][720p].mkv", "video-720");
            let sub_720 =
                write_fixture(&work, "[Raw][Show][01][720p].ass", "subtitle-720-original");
            let sub_1080 = write_fixture(
                &work,
                "[Raw][Show][01][1080p].ass",
                "subtitle-1080-original",
            );

            let mut args = vec!["--lang".to_string(), "en".to_string(), "--json".to_string()];
            if overwrite {
                args.push("--overwrite".to_string());
            }
            if mode == "copy-to-chosen" {
                args.push("--output-dir".to_string());
                push_path(&mut args, &work);
            }
            args.extend(["rename".to_string(), "--mode".to_string(), mode.to_string()]);
            push_path(&mut args, &video_1080);
            push_path(&mut args, &video_720);
            // Deliberately reverse the subtitle order. Both videos have the
            // same episode key, so index pairing plans a two-file swap.
            push_path(&mut args, &sub_720);
            push_path(&mut args, &sub_1080);

            let output = run_cli_owned(&args);
            assert!(
                !output.status.success(),
                "unsafe {mode} swap must fail (overwrite={overwrite}): stderr={}",
                String::from_utf8_lossy(&output.stderr)
            );
            let value = parse_json_output(&output);
            assert_eq!(value["written"], 0, "{value:#}");
            assert_eq!(value["planned"], 0, "{value:#}");
            assert_eq!(value["skipped"], 0, "{value:#}");
            assert_eq!(value["failed"], 2, "{value:#}");
            assert!(
                value["results"]
                    .as_array()
                    .expect("results should be an array")
                    .iter()
                    .all(|row| row["status"] == "failed"
                        && row["error"]
                            == "subtitle is part of a planned conflict where an output targets a loaded subtitle input; no files in that conflicting chain were changed"),
                "every swap participant should report the preflight conflict: {value:#}"
            );
            assert!(
                value["results"]
                    .as_array()
                    .expect("results should be an array")
                    .iter()
                    .all(|row| row["warnings"]
                        .as_array()
                        .is_some_and(|warnings| warnings.iter().any(|warning| {
                            warning
                                == "pairing is ambiguous because multiple videos share the same season and episode; verify the selected subtitle"
                        }))),
                "the TypeScript pairing source should survive into JSON diagnostics: {value:#}"
            );
            assert_eq!(
                fs::read_to_string(&sub_720).unwrap(),
                "subtitle-720-original",
                "{mode} overwrite={overwrite} changed the first source"
            );
            assert_eq!(
                fs::read_to_string(&sub_1080).unwrap(),
                "subtitle-1080-original",
                "{mode} overwrite={overwrite} changed the second source"
            );

            let _ = fs::remove_dir_all(work);
        }
    }
}

#[test]
fn three_row_chain_reports_every_participant_and_preserves_all_bytes() {
    let work = temp_dir("three-chain-human");
    let video_b = write_fixture(&work, "[RawB][Show][01][1080p].mkv", "video-b");
    let video_c = write_fixture(&work, "[RawC][Show][01][720p].mkv", "video-c");
    let video_d = write_fixture(&work, "[RawD][Show][01][480p].mkv", "video-d");
    let sub_a = write_fixture(&work, "[RawA][Show][01][2160p].ass", "subtitle-a-original");
    let sub_b = write_fixture(&work, "[RawB][Show][01][1080p].ass", "subtitle-b-original");
    let sub_c = write_fixture(&work, "[RawC][Show][01][720p].ass", "subtitle-c-original");
    let final_output = work.join("[RawD][Show][01][480p].ass");

    let mut args = vec![
        "--lang".to_string(),
        "en".to_string(),
        "--overwrite".to_string(),
        "rename".to_string(),
        "--mode".to_string(),
        "rename".to_string(),
    ];
    for path in [&video_b, &video_c, &video_d, &sub_a, &sub_b, &sub_c] {
        push_path(&mut args, path);
    }

    let output = run_cli_owned(&args);
    assert!(!output.status.success(), "unsafe chain must fail");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stdout.contains("Done: 0 written, 0 planned, 0 skipped, 3 failed"),
        "human summary should report every failed chain participant: {stdout}"
    );
    assert_eq!(
        stderr
            .matches("planned conflict where an output targets a loaded subtitle input")
            .count(),
        3,
        "each chain participant should have a human-readable failure: {stderr}"
    );
    assert_eq!(
        stderr
            .matches(
                "pairing is ambiguous because multiple videos share the same season and episode"
            )
            .count(),
        3,
        "the human report should surface every ambiguous pairing: {stderr}"
    );
    assert_eq!(fs::read_to_string(&sub_a).unwrap(), "subtitle-a-original");
    assert_eq!(fs::read_to_string(&sub_b).unwrap(), "subtitle-b-original");
    assert_eq!(fs::read_to_string(&sub_c).unwrap(), "subtitle-c-original");
    assert!(
        !final_output.exists(),
        "chain tail output must not be created"
    );

    let _ = fs::remove_dir_all(work);
}

#[test]
fn dry_run_still_fails_an_input_conflict_instead_of_planning_it() {
    let work = temp_dir("swap-dry-run");
    let video_1080 = write_fixture(&work, "[Raw][Show][01][1080p].mkv", "video-1080");
    let video_720 = write_fixture(&work, "[Raw][Show][01][720p].mkv", "video-720");
    let sub_720 = write_fixture(&work, "[Raw][Show][01][720p].ass", "subtitle-720-original");
    let sub_1080 = write_fixture(
        &work,
        "[Raw][Show][01][1080p].ass",
        "subtitle-1080-original",
    );
    let mut args = vec![
        "--lang".to_string(),
        "en".to_string(),
        "--json".to_string(),
        "--dry-run".to_string(),
        "rename".to_string(),
        "--mode".to_string(),
        "copy-to-video".to_string(),
    ];
    for path in [&video_1080, &video_720, &sub_720, &sub_1080] {
        push_path(&mut args, path);
    }

    let output = run_cli_owned(&args);
    let value = parse_json_output(&output);
    assert!(!output.status.success());
    assert_eq!(value["planned"], 0, "{value:#}");
    assert_eq!(value["failed"], 2, "{value:#}");
    assert_eq!(
        fs::read_to_string(&sub_720).unwrap(),
        "subtitle-720-original"
    );
    assert_eq!(
        fs::read_to_string(&sub_1080).unwrap(),
        "subtitle-1080-original"
    );

    let _ = fs::remove_dir_all(work);
}

#[test]
fn fail_fast_blocks_a_threatened_endpoint_before_it_can_move() {
    let work = temp_dir("fail-fast-endpoint-first");
    let video_d = write_fixture(&work, "[RawD][Show][01][480p].mkv", "video-d");
    let video_b = write_fixture(&work, "[RawB][Show][01][1080p].mkv", "video-b");
    let sub_b = write_fixture(&work, "[RawB][Show][01][1080p].ass", "subtitle-b-original");
    let sub_a = write_fixture(&work, "[RawA][Show][01][2160p].ass", "subtitle-a-original");
    let tail_output = work.join("[RawD][Show][01][480p].ass");

    let mut args = vec![
        "--lang".to_string(),
        "en".to_string(),
        "--json".to_string(),
        "--overwrite".to_string(),
        "--fail-fast".to_string(),
        "rename".to_string(),
        "--mode".to_string(),
        "rename".to_string(),
    ];
    // Pair by index as B -> D first, then A -> B. Whole-plan preflight must
    // flag the threatened B row before fail-fast enters the execution loop;
    // otherwise it would move B to D before the later writer is rejected.
    for path in [&video_d, &video_b, &sub_b, &sub_a] {
        push_path(&mut args, path);
    }

    let output = run_cli_owned(&args);
    let value = parse_json_output(&output);
    assert!(!output.status.success());
    assert_eq!(value["failed"], 1, "{value:#}");
    assert_eq!(value["written"], 0, "{value:#}");
    assert_eq!(value["abortedByFailFast"], true, "{value:#}");
    assert_eq!(
        value["results"].as_array().map(Vec::len),
        Some(1),
        "fail-fast should stop after the preflight-failed endpoint: {value:#}"
    );
    assert_eq!(fs::read_to_string(&sub_b).unwrap(), "subtitle-b-original");
    assert_eq!(fs::read_to_string(&sub_a).unwrap(), "subtitle-a-original");
    assert!(
        !tail_output.exists(),
        "the first endpoint must not be moved"
    );

    let _ = fs::remove_dir_all(work);
}
