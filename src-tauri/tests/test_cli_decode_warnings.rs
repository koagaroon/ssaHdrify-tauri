//! Decode quality must remain visible after a successful or failed CLI operation.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const ASS: &str = include_str!("fixtures/utf8.ass");
const NO_FONT_ASS: &str = "[Script Info]\nScriptType: v4.00+\n\n[V4+ Styles]\nFormat: Name, Fontname\n\n[Events]\nFormat: Layer, Start, End, Style, Name, MarginL, MarginR, MarginV, Effect, Text\n";

fn temp_dir(label: &str) -> PathBuf {
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let root = std::env::temp_dir().join(format!(
        "ssahdrify-cli-decode-{label}-{}-{stamp}",
        std::process::id()
    ));
    fs::create_dir_all(&root).unwrap();
    root
}

fn write_ass(root: &Path, content: &str, malformed: bool) -> (PathBuf, Vec<u8>) {
    let path = root.join("episode.ass");
    let mut bytes = b"\xef\xbb\xbf".to_vec();
    bytes.extend_from_slice(content.as_bytes());
    bytes.extend_from_slice(b"\n; source comment: ");
    bytes.extend_from_slice(if malformed { b"\xff" } else { b"valid" });
    bytes.push(b'\n');
    fs::write(&path, &bytes).unwrap();
    (path, bytes)
}

fn run(args: &[&str], input: &Path, json: bool) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_ssahdrify-cli"));
    command.args(["--lang", "en"]);
    if json {
        command.arg("--json");
    }
    command.args(args).arg(input).output().unwrap()
}

fn check_standalone(args: &[&str], content: &str) {
    for malformed in [false, true] {
        for json in [false, true] {
            let root = temp_dir("standalone");
            let (input, before) = write_ass(&root, content, malformed);
            let output = run(args, &input, json);
            let stdout = String::from_utf8_lossy(&output.stdout);
            let stderr = String::from_utf8_lossy(&output.stderr);
            assert!(output.status.success(), "{args:?}: {stdout}\n{stderr}");
            if json {
                let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
                assert_eq!(report["written"], 1, "{report}");
                let warnings = report["results"][0]["warnings"].as_array();
                assert_eq!(
                    warnings.map_or(0, Vec::len),
                    usize::from(malformed),
                    "{report}"
                );
                if malformed {
                    assert!(
                        warnings.unwrap()[0].as_str().unwrap().contains("malformed"),
                        "{report}"
                    );
                    let output_path = report["results"][0]["output"].as_str().unwrap();
                    assert!(fs::read_to_string(output_path)
                        .unwrap()
                        .contains('\u{fffd}'));
                }
            } else {
                assert_eq!(
                    stderr.matches("malformed").count(),
                    usize::from(malformed),
                    "{stderr}"
                );
                assert_eq!(
                    stdout.contains("1 written with warnings / incomplete (1 warning(s))"),
                    malformed,
                    "{stdout}"
                );
            }
            assert_eq!(fs::read(&input).unwrap(), before);
            fs::remove_dir_all(root).unwrap();
        }
    }
}

#[test]
fn hdr_reports_lossy_decode_in_human_and_json_output() {
    check_standalone(&["hdr", "--eotf", "pq"], ASS);
}

#[test]
fn shift_reports_lossy_decode_in_both_template_paths() {
    check_standalone(&["shift", "--offset", "+1s"], ASS);
    check_standalone(
        &[
            "shift",
            "--offset",
            "+1s",
            "--output-template",
            "{name}.{format}.ass",
        ],
        ASS,
    );
}

#[test]
fn embed_reports_lossy_decode_without_font_warnings() {
    check_standalone(&["--no-cache", "embed", "--no-system-fonts"], NO_FONT_ASS);
}

#[test]
fn chain_reports_one_decode_warning_for_multiple_steps() {
    for malformed in [false, true] {
        let root = temp_dir("chain");
        let (input, before) = write_ass(&root, ASS, malformed);
        let output = run(
            &[
                "chain", "hdr", "--eotf", "pq", "+", "shift", "--offset", "+1s",
            ],
            &input,
            false,
        );
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(output.status.success(), "{stdout}\n{stderr}");
        assert_eq!(
            stderr.matches("malformed").count(),
            usize::from(malformed),
            "{stderr}"
        );
        assert_eq!(
            stdout.contains("1 written with warnings / incomplete (1 warning(s))"),
            malformed,
            "{stdout}"
        );
        assert_eq!(fs::read(&input).unwrap(), before);
        fs::remove_dir_all(root).unwrap();
    }
}

#[test]
fn diagnose_fonts_keeps_decode_warning_separate_from_font_qa() {
    let root = temp_dir("diagnose");
    let (input, before) = write_ass(&root, NO_FONT_ASS, true);
    let output = run(
        &["--no-cache", "diagnose-fonts", "--no-system-fonts"],
        &input,
        true,
    );
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["warningCount"], 1, "{report}");
    assert_eq!(
        report["files"][0]["warnings"].as_array().unwrap().len(),
        1,
        "{report}"
    );
    assert_eq!(report["qa"]["status"], "complete", "{report}");
    assert_eq!(fs::read(&input).unwrap(), before);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn failed_conversion_retains_its_decode_warning() {
    let root = temp_dir("failed");
    let (input, before) = write_ass(&root, "not subtitle content", true);
    let output = run(&["shift", "--offset", "+1s"], &input, true);
    assert!(!output.status.success());
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["failed"], 1, "{report}");
    assert_eq!(
        report["results"][0]["warnings"].as_array().unwrap().len(),
        1,
        "{report}"
    );
    assert!(!root.join("episode.shifted.ass").exists());
    assert_eq!(fs::read(&input).unwrap(), before);
    fs::remove_dir_all(root).unwrap();
}
