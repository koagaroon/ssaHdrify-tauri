use std::fs;
use std::path::PathBuf;
use std::process::Command;

const ASS: &str = include_str!("fixtures/utf8.ass");

fn temp_dir(label: &str) -> PathBuf {
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let path = std::env::temp_dir().join(format!(
        "ssahdrify-input-protection-{label}-{}-{stamp}",
        std::process::id()
    ));
    fs::create_dir_all(&path).unwrap();
    path
}

#[test]
fn transforms_never_replace_another_selected_input_even_with_overwrite() {
    let commands: &[(&str, &[&str])] = &[
        (
            "hdr",
            &["hdr", "--eotf", "pq", "--output-template", "protected.ass"],
        ),
        (
            "shift",
            &[
                "shift",
                "--offset",
                "+1s",
                "--output-template",
                "protected.ass",
            ],
        ),
        ("embed", &["embed", "--output-template", "protected.ass"]),
        (
            "chain",
            &[
                "chain",
                "--output-template",
                "protected.ass",
                "shift",
                "--offset",
                "+1s",
            ],
        ),
    ];
    for (name, args) in commands {
        for dry_run in [false, true] {
            let root = temp_dir(name);
            let source = root.join("source.ass");
            let protected = root.join("protected.ass");
            fs::write(&source, ASS).unwrap();
            let protected_content = format!("{ASS}\n; preserve this selected source\n");
            fs::write(&protected, &protected_content).unwrap();
            let mut command = Command::new(env!("CARGO_BIN_EXE_ssahdrify-cli"));
            command.args(["--lang", "en", "--no-cache", "--overwrite"]);
            if dry_run {
                command.arg("--dry-run");
            }
            let output = command
                .args(*args)
                .arg(&source)
                .arg(&protected)
                .output()
                .unwrap();
            let text = format!(
                "{}\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            assert!(!output.status.success(), "{name} dry_run={dry_run}: {text}");
            assert!(
                text.contains("selected input file"),
                "{name} dry_run={dry_run}: {text}"
            );
            assert_eq!(
                fs::read_to_string(&source).unwrap(),
                ASS,
                "{name}: source changed"
            );
            assert_eq!(
                fs::read_to_string(&protected).unwrap(),
                protected_content,
                "{name}: selected output changed"
            );
            fs::remove_dir_all(root).unwrap();
        }
    }
}

#[test]
fn format_dependent_shift_checks_selected_inputs_after_resolving_the_output() {
    let root = temp_dir("detected-format");
    let source = root.join("episode.srt");
    let protected = root.join("episode.shifted.srt");
    let subtitle = "1\n00:00:01,000 --> 00:00:02,000\nsource\n";
    fs::write(&source, subtitle).unwrap();
    fs::write(&protected, subtitle).unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_ssahdrify-cli"))
        .args([
            "--lang",
            "en",
            "--overwrite",
            "shift",
            "--offset",
            "+1s",
            "--output-template",
            "{name}.shifted.{format}",
        ])
        .arg(&source)
        .arg(&protected)
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("selected input file"));
    assert_eq!(fs::read_to_string(&source).unwrap(), subtitle);
    assert_eq!(fs::read_to_string(&protected).unwrap(), subtitle);
    assert!(String::from_utf8_lossy(&output.stderr).contains("same as input"));
    assert_eq!(fs::read_dir(&root).unwrap().count(), 2);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn explicit_overwrite_still_replaces_an_unselected_previous_output() {
    let root = temp_dir("ordinary-overwrite");
    let source = root.join("episode.srt");
    let destination = root.join("episode.shifted.srt");
    let subtitle = "1\n00:00:01,000 --> 00:00:02,000\nsource\n";
    fs::write(&source, subtitle).unwrap();
    fs::write(&destination, "previous output").unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_ssahdrify-cli"))
        .args(["--lang", "en", "--overwrite", "shift", "--offset", "+1s"])
        .arg(&source)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(fs::read_to_string(&source).unwrap(), subtitle);
    assert!(fs::read_to_string(&destination)
        .unwrap()
        .contains("00:00:02,000 --> 00:00:03,000"));
    fs::remove_dir_all(root).unwrap();
}
