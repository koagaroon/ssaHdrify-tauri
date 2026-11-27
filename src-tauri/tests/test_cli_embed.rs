//! Standalone end-to-end integration tests for `ssahdrify-cli embed`.

use std::fs;
use std::path::{Path, PathBuf};

mod common;
use common::{assert_engine_bundle_ready, make_real_font_dir, run_cli, TestTempDir};

const BASE_ASS: &str = concat!(
    "[Script Info]\n",
    "ScriptType: v4.00+\n",
    "\n",
    "[V4+ Styles]\n",
    "Format: Name, Fontname, Fontsize, PrimaryColour, SecondaryColour, OutlineColour, BackColour, Bold, Italic, Underline, StrikeOut, ScaleX, ScaleY, Spacing, Angle, BorderStyle, Outline, Shadow, Alignment, MarginL, MarginR, MarginV, Encoding\n",
    "Style: Default,__FAMILY__,24,&H00FFFFFF,&H000000FF,&H00000000,&H00000000,__BOLD__,__ITALIC__,0,0,100,100,0,0,1,2,1,2,10,10,10,1\n",
    "\n",
    "[Events]\n",
    "Format: Layer, Start, End, Style, Name, MarginL, MarginR, MarginV, Effect, Text\n",
    "Dialogue: 0,0:00:01.00,0:00:03.00,Default,,0,0,0,,Hello font subset 123\n",
);

fn temp_dir(label: &str) -> TestTempDir {
    TestTempDir::new("ssahdrify-cli-embed", label)
}

fn first_indexed_entry(font_dir: &Path) -> (PathBuf, app_lib::fonts::LocalFontEntry) {
    let font_file = fs::read_dir(font_dir)
        .expect("read font_dir")
        .flatten()
        .map(|e| e.path())
        .find(|p| p.is_file())
        .expect("copied font file in font_dir");
    let entries =
        app_lib::fonts::scan_directory_collecting(font_dir).expect("scan test font directory");
    let entry = entries
        .into_iter()
        .find(|entry| !entry.families.is_empty())
        .expect("copied OS font must expose at least one family name");
    (font_file, entry)
}

fn write_ass_for_entry(dir: &Path, name: &str, entry: &app_lib::fonts::LocalFontEntry) -> PathBuf {
    let family = entry.families.first().expect("family name");
    let bold_flag = if entry.bold { "-1" } else { "0" };
    let italic_flag = if entry.italic { "-1" } else { "0" };
    let content = BASE_ASS
        .replace("__FAMILY__", family)
        .replace("__BOLD__", bold_flag)
        .replace("__ITALIC__", italic_flag);
    let path = dir.join(name);
    fs::write(&path, content).expect("write ASS fixture");
    path
}

#[test]
fn embed_subsets_and_writes_fonts_section_from_font_dir_and_font_file() {
    assert_engine_bundle_ready();
    let work = temp_dir("happy-path");
    let font_dir = make_real_font_dir(&work);
    let (font_file, entry) = first_indexed_entry(&font_dir);

    // 1. Resolve via --font-dir (with --no-system-fonts --no-cache --on-missing fail)
    let sub_dir = write_ass_for_entry(&work, "from_dir.ass", &entry);
    let out_dir = run_cli(&[
        "--lang",
        "en",
        "--json",
        "--no-cache",
        "embed",
        "--no-system-fonts",
        "--on-missing",
        "fail",
        "--font-dir",
        font_dir.to_str().unwrap(),
        sub_dir.to_str().unwrap(),
    ]);
    assert!(
        out_dir.status.success(),
        "embed --font-dir failed: stdout={}, stderr={}",
        String::from_utf8_lossy(&out_dir.stdout),
        String::from_utf8_lossy(&out_dir.stderr)
    );
    let report: serde_json::Value = serde_json::from_slice(&out_dir.stdout).unwrap();
    assert_eq!(report["command"], "embed");
    assert_eq!(report["written"], 1);
    assert_eq!(report["failed"], 0);
    let embedded_ass = fs::read_to_string(work.join("from_dir.embed.ass")).unwrap();
    assert!(embedded_ass.contains("[Fonts]\nfontname: "));

    // 2. Resolve via explicit --font-file
    let sub_file = write_ass_for_entry(&work, "from_file.ass", &entry);
    let out_file = run_cli(&[
        "--lang",
        "en",
        "--json",
        "--no-cache",
        "embed",
        "--no-system-fonts",
        "--on-missing",
        "fail",
        "--font-file",
        font_file.to_str().unwrap(),
        sub_file.to_str().unwrap(),
    ]);
    assert!(
        out_file.status.success(),
        "embed --font-file failed: stderr={}",
        String::from_utf8_lossy(&out_file.stderr)
    );
    let embedded_from_file = fs::read_to_string(work.join("from_file.embed.ass")).unwrap();
    assert!(embedded_from_file.contains("[Fonts]\nfontname: "));
}

#[test]
fn embed_on_missing_warn_vs_fail_dry_run_and_non_ass_rejection() {
    assert_engine_bundle_ready();
    let work = temp_dir("modes-and-gates");
    let missing_ass = work.join("missing.ass");
    fs::write(
        &missing_ass,
        BASE_ASS
            .replace("__FAMILY__", "DefinitelyNonExistentFontFamilyXYZ")
            .replace("__BOLD__", "0")
            .replace("__ITALIC__", "0"),
    )
    .unwrap();

    // 1. --dry-run --json plans output without writing or resolving fonts
    let dry = run_cli(&[
        "--lang",
        "en",
        "--json",
        "--dry-run",
        "--no-cache",
        "embed",
        "--no-system-fonts",
        "--on-missing",
        "fail",
        missing_ass.to_str().unwrap(),
    ]);
    assert!(dry.status.success());
    let dry_json: serde_json::Value = serde_json::from_slice(&dry.stdout).unwrap();
    assert_eq!(dry_json["planned"], 1);
    assert!(!work.join("missing.embed.ass").exists());

    // 2. --on-missing fail exits 2 and writes nothing
    let strict = run_cli(&[
        "--lang",
        "en",
        "--json",
        "--no-cache",
        "embed",
        "--no-system-fonts",
        "--on-missing",
        "fail",
        missing_ass.to_str().unwrap(),
    ]);
    assert_eq!(strict.status.code(), Some(2));
    let strict_json: serde_json::Value = serde_json::from_slice(&strict.stdout).unwrap();
    assert_eq!(strict_json["failed"], 1);
    assert!(!work.join("missing.embed.ass").exists());

    // 3. --on-missing warn writes output with warnings and exits 0
    let warn = run_cli(&[
        "--lang",
        "en",
        "--json",
        "--no-cache",
        "embed",
        "--no-system-fonts",
        "--on-missing",
        "warn",
        missing_ass.to_str().unwrap(),
    ]);
    assert!(warn.status.success());
    let warn_json: serde_json::Value = serde_json::from_slice(&warn.stdout).unwrap();
    assert_eq!(warn_json["written"], 1);
    assert!(warn_json["results"][0]["warnings"]
        .as_array()
        .is_some_and(|w| !w.is_empty()));
    assert!(work.join("missing.embed.ass").exists());

    // 4. Non-ASS input is rejected cleanly
    let srt = work.join("plain.srt");
    fs::write(&srt, "1\n00:00:01,000 --> 00:00:02,000\nHi\n").unwrap();
    let bad_ext = run_cli(&[
        "--lang",
        "en",
        "--json",
        "--no-cache",
        "embed",
        srt.to_str().unwrap(),
    ]);
    assert_eq!(bad_ext.status.code(), Some(2));
    let bad_json: serde_json::Value = serde_json::from_slice(&bad_ext.stdout).unwrap();
    assert_eq!(bad_json["results"][0]["status"], "failed");
    assert!(bad_json["results"][0]["error"]
        .as_str()
        .unwrap()
        .contains("only supports ASS/SSA"));
}
