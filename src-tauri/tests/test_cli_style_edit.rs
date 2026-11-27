//! End-to-end integration tests for `ssahdrify-cli style-edit`.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

mod common;
use common::TestTempDir;

fn cli_path() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_ssahdrify-cli"))
}

fn temp_dir(label: &str) -> TestTempDir {
    TestTempDir::new("ssahdrify-cli-style-edit", label)
}

const SAMPLE_ASS: &str = "\
[Script Info]
Title: Style Edit Fixture
ScriptType: v4.00+

[V4+ Styles]
Format: Name, Fontname, Fontsize, PrimaryColour, SecondaryColour, OutlineColour, BackColour, Bold, Italic, Underline, StrikeOut, ScaleX, ScaleY, Spacing, Angle, BorderStyle, Outline, Shadow, Alignment, MarginL, MarginR, MarginV, Encoding
Style: Default,Arial,48,&H00FFFFFF,&H000000FF,&H00000000,&H00000000,0,0,0,0,100,100,0,0,1,2,1,2,20,20,20,1
Style: Sign,Noto Sans,40,&H00FFFFFF,&H000000FF,&H00000000,&H00000000,0,0,0,0,100,100,0,0,1,2,1,8,20,20,20,1

[Events]
Format: Layer, Start, End, Style, Name, MarginL, MarginR, MarginV, Effect, Text
Dialogue: 0,0:00:01.00,0:00:03.00,Default,,0,0,0,,{\\fnCustomInline\\fs30}Hello world
";

fn write_utf16le_with_bom(path: &Path, text: &str) {
    let mut bytes = vec![0xFF, 0xFE];
    for unit in text.encode_utf16() {
        bytes.extend_from_slice(&unit.to_le_bytes());
    }
    fs::write(path, bytes).unwrap();
}

fn read_utf16le_with_bom(path: &Path) -> String {
    let bytes = fs::read(path).unwrap();
    assert!(
        bytes.starts_with(&[0xFF, 0xFE]),
        "expected UTF-16LE BOM on {}",
        path.display()
    );
    let units: Vec<u16> = bytes[2..]
        .as_chunks::<2>()
        .0
        .iter()
        .copied()
        .map(u16::from_le_bytes)
        .collect();
    String::from_utf16(&units).unwrap()
}

#[test]
fn style_edit_writes_styled_sibling_preserving_utf16le_bom_and_inline_tags() {
    let dir = temp_dir("utf16le-write");
    let input = dir.join("episode01.ass");
    let expected_out = dir.join("episode01.styled.ass");
    write_utf16le_with_bom(&input, SAMPLE_ASS);

    let output = Command::new(cli_path())
        .args([
            "--lang",
            "en",
            "--json",
            "style-edit",
            "--font-family",
            "LXGW WenKai",
            "--source-family",
            "arial",
            "--font-size",
            "54",
        ])
        .arg(&input)
        .output()
        .expect("failed to execute style-edit");

    assert!(
        output.status.success(),
        "style-edit failed: stdout={}, stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    let report: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("stdout must be valid JSON");
    assert_eq!(report["command"], "style-edit");
    assert_eq!(report["written"], 1);
    assert_eq!(report["skipped"], 0);
    assert_eq!(report["failed"], 0);
    assert_eq!(report["results"][0]["status"], "written");
    assert_eq!(report["results"][0]["encoding"], "UTF-16LE");

    assert!(expected_out.exists(), "expected sibling .styled.ass output");
    let edited = read_utf16le_with_bom(&expected_out);
    assert!(
        edited.contains("Style: Default,LXGW WenKai,54,"),
        "Default style should update family and size: {edited}"
    );
    assert!(
        edited.contains("Style: Sign,Noto Sans,54,"),
        "Sign style should keep family (filtered by --source-family) and update size: {edited}"
    );
    assert!(
        edited.contains("{\\fnCustomInline\\fs30}Hello world"),
        "inline override tags must remain untouched: {edited}"
    );

    let _ = fs::remove_dir_all(dir);
}

#[test]
fn style_edit_supports_dry_run_diagnose_and_skips_noop_inputs() {
    let dir = temp_dir("dry-run-and-noop");
    let changeable = dir.join("ep1.ass");
    let noop = dir.join("ep2.ass");
    fs::write(&changeable, SAMPLE_ASS).unwrap();
    fs::write(
        &noop,
        SAMPLE_ASS
            .replace("Arial", "LXGW WenKai")
            .replace("Noto Sans", "LXGW WenKai"),
    )
    .unwrap();

    let dry_output = Command::new(cli_path())
        .args([
            "--lang",
            "en",
            "--json",
            "--dry-run",
            "style-edit",
            "--diagnose",
            "--font-family",
            "LXGW WenKai",
        ])
        .arg(&changeable)
        .arg(&noop)
        .output()
        .expect("failed to execute style-edit --dry-run");

    assert!(dry_output.status.success());
    let dry_report: serde_json::Value =
        serde_json::from_slice(&dry_output.stdout).expect("dry-run JSON must parse");
    assert_eq!(dry_report["command"], "style-edit");
    assert_eq!(dry_report["planned"], 1);
    assert_eq!(dry_report["skipped"], 1);
    assert_eq!(dry_report["failed"], 0);
    assert_eq!(dry_report["diagnostics"]["mode"], "summary");
    assert!(!dir.join("ep1.styled.ass").exists());
    assert!(!dir.join("ep2.styled.ass").exists());

    // Real run on noop input alone writes nothing and exits 0.
    let noop_output = Command::new(cli_path())
        .args([
            "--lang",
            "en",
            "--json",
            "style-edit",
            "--font-family",
            "LXGW WenKai",
        ])
        .arg(&noop)
        .output()
        .expect("failed to execute noop style-edit");
    assert!(noop_output.status.success());
    let noop_report: serde_json::Value = serde_json::from_slice(&noop_output.stdout).unwrap();
    assert_eq!(noop_report["written"], 0);
    assert_eq!(noop_report["skipped"], 1);
    assert!(!dir.join("ep2.styled.ass").exists());

    let _ = fs::remove_dir_all(dir);
}

#[test]
fn style_edit_rejects_overwrite_output_dir_invalid_ops_and_existing_outputs() {
    let dir = temp_dir("guardrails");
    let input = dir.join("ep1.ass");
    let existing_styled = dir.join("ep1.styled.ass");
    fs::write(&input, SAMPLE_ASS).unwrap();
    fs::write(&existing_styled, "ORIGINAL_EXISTING_OUTPUT").unwrap();

    // 1. --overwrite is rejected at command entry (exit 2)
    let overwrite_out = Command::new(cli_path())
        .args(["--overwrite", "style-edit", "--font-size", "52"])
        .arg(&input)
        .output()
        .unwrap();
    assert_eq!(overwrite_out.status.code(), Some(2));

    // 2. --output-dir is rejected at command entry (exit 2)
    let outdir_out = Command::new(cli_path())
        .arg("--output-dir")
        .arg(&dir)
        .args(["style-edit", "--font-size", "52"])
        .arg(&input)
        .output()
        .unwrap();
    assert_eq!(outdir_out.status.code(), Some(2));

    // 3. Invalid font family containing a comma fails upfront (exit 2)
    let bad_family = Command::new(cli_path())
        .args(["style-edit", "--font-family", "Bad,Family"])
        .arg(&input)
        .output()
        .unwrap();
    assert_eq!(bad_family.status.code(), Some(2));

    // 4. Existing .styled.ass output fails without modifying existing file
    let collision_out = Command::new(cli_path())
        .args(["--json", "style-edit", "--font-size", "52"])
        .arg(&input)
        .output()
        .unwrap();
    assert_eq!(collision_out.status.code(), Some(2));
    let report: serde_json::Value = serde_json::from_slice(&collision_out.stdout).unwrap();
    assert_eq!(report["failed"], 1);
    assert_eq!(
        fs::read_to_string(&existing_styled).unwrap(),
        "ORIGINAL_EXISTING_OUTPUT"
    );

    let _ = fs::remove_dir_all(dir);
}

#[test]
fn style_edit_lossy_refusal_preserves_decode_warnings_in_json_report() {
    let dir = temp_dir("lossy-warnings");
    let input = dir.join("damaged_utf8_bom.ass");
    let mut bytes = vec![0xEF, 0xBB, 0xBF];
    bytes.extend_from_slice(SAMPLE_ASS.as_bytes());
    bytes.extend_from_slice(b"; trailing invalid byte \xff\n");
    fs::write(&input, bytes).unwrap();

    let out = Command::new(cli_path())
        .args(["--lang", "en", "--json", "style-edit", "--font-size", "50"])
        .arg(&input)
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2));
    let report: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(report["failed"], 1);
    assert!(report["results"][0]["error"]
        .as_str()
        .unwrap()
        .contains("source decoding was lossy"));
    let warnings = report["results"][0]["warnings"]
        .as_array()
        .expect("lossy refusal should preserve decode warnings");
    assert!(
        warnings
            .iter()
            .any(|w| w.as_str().is_some_and(|s| s.contains("malformed"))),
        "expected malformed-byte decode warning, got: {warnings:?}"
    );
    assert!(!dir.join("damaged_utf8_bom.styled.ass").exists());
}
