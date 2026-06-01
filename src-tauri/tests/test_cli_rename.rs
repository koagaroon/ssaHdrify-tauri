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

fn run_cli(args: &[&str]) -> std::process::Output {
    Command::new(cli_path())
        .args(args)
        .output()
        .expect("failed to spawn ssahdrify-cli")
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
