//! End-to-end coverage for `ssahdrify-cli shift --map`.
//!
//! The timing-map parser lives in the shared TypeScript engine, but
//! the CLI owns the user-facing contract: parse the map once before
//! the batch loop, reject invalid maps before any writes, then apply
//! the same validated rules to every input.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

const FIXTURE_SRT: &str = concat!(
    "1\n",
    "00:00:01,000 --> 00:00:02,000\n",
    "one\n",
    "\n",
    "2\n",
    "00:00:05,000 --> 00:00:06,000\n",
    "two\n",
    "\n",
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
    let dir = std::env::temp_dir().join(format!("ssahdrify-cli-timing-map-{label}-{pid}-{nano}"));
    fs::create_dir_all(&dir).expect("failed to create test temp dir");
    dir
}

fn write_fixture(dir: &Path, name: &str) -> PathBuf {
    let path = dir.join(name);
    fs::write(&path, FIXTURE_SRT).expect("failed to write SRT fixture");
    path
}

fn engine_bundle_missing() -> Option<String> {
    let output = Command::new(cli_path())
        .args([
            "--lang",
            "en",
            "shift",
            "--offset",
            "+1s",
            "/nonexistent-test-input-do-not-create.srt",
        ])
        .output()
        .ok()?;
    let stderr = String::from_utf8_lossy(&output.stderr);
    if stderr.contains("Run `npm run build:engine`") {
        Some(format!(
            "engine bundle missing - run `npm run build:engine` first ({stderr})"
        ))
    } else {
        None
    }
}

#[test]
fn shift_map_applies_json_rules() {
    if let Some(reason) = engine_bundle_missing() {
        panic!("{reason}");
    }

    let dir = temp_dir("json");
    let input = write_fixture(&dir, "episode.srt");
    let map = dir.join("timing-map.json");
    fs::write(
        &map,
        r#"{
  "rules": [
    { "start": "00:00:00.000", "end": "00:00:05.000", "offset": "+1s", "label": "opening" },
    { "startMs": 5000, "offsetMs": -500, "label": "main" }
  ]
}"#,
    )
    .expect("failed to write timing map");

    let output = Command::new(cli_path())
        .args(["--lang", "en", "shift", "--map"])
        .arg(&map)
        .arg(&input)
        .output()
        .expect("failed to run shift --map");

    assert!(
        output.status.success(),
        "shift --map should succeed: stderr={}",
        String::from_utf8_lossy(&output.stderr)
    );
    let shifted =
        fs::read_to_string(dir.join("episode.shifted.srt")).expect("shifted output missing");
    assert!(shifted.contains("00:00:02,000 --> 00:00:03,000"));
    assert!(shifted.contains("00:00:04,500 --> 00:00:05,500"));

    let _ = fs::remove_dir_all(dir);
}

#[test]
fn shift_map_dry_run_still_validates_map_before_planning() {
    if let Some(reason) = engine_bundle_missing() {
        panic!("{reason}");
    }

    let dir = temp_dir("invalid-dry-run");
    let input = write_fixture(&dir, "episode.srt");
    let map = dir.join("bad-map.csv");
    fs::write(&map, "00:60:00.000,,+1s").expect("failed to write invalid timing map");

    let output = Command::new(cli_path())
        .args(["--lang", "en", "--dry-run", "shift", "--map"])
        .arg(&map)
        .arg(&input)
        .output()
        .expect("failed to run shift --map dry-run");

    assert!(
        !output.status.success(),
        "invalid map should fail even in dry-run mode"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("Timing map") && stderr.contains("below 60"),
        "stderr should explain the map parse failure: {stderr}"
    );
    assert!(
        !dir.join("episode.shifted.srt").exists(),
        "invalid dry-run map must not write output"
    );

    let _ = fs::remove_dir_all(dir);
}

#[test]
fn shift_map_rejects_global_offset_mix() {
    let dir = temp_dir("conflict");
    let input = write_fixture(&dir, "episode.srt");
    let map = dir.join("timing-map.csv");
    fs::write(&map, "00:00:00.000,,+1s").expect("failed to write timing map");

    let output = Command::new(cli_path())
        .args(["shift", "--map"])
        .arg(&map)
        .args(["--offset", "+1s"])
        .arg(&input)
        .output()
        .expect("failed to spawn conflict case");

    assert!(
        !output.status.success(),
        "--map and --offset must be mutually exclusive"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("--map") && stderr.contains("--offset"),
        "stderr should name the conflicting options: {stderr}"
    );

    let _ = fs::remove_dir_all(dir);
}
