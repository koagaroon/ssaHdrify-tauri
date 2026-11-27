//! Standalone end-to-end integration tests for `ssahdrify-cli hdr`.

use std::fs;

mod common;
use common::{assert_engine_bundle_ready, run_cli, TestTempDir};

const SAMPLE_ASS: &str = concat!(
    "[Script Info]\n",
    "ScriptType: v4.00+\n",
    "\n",
    "[V4+ Styles]\n",
    "Format: Name, Fontname, Fontsize, PrimaryColour, SecondaryColour, OutlineColour, BackColour, Bold, Italic, Underline, StrikeOut, ScaleX, ScaleY, Spacing, Angle, BorderStyle, Outline, Shadow, Alignment, MarginL, MarginR, MarginV, Encoding\n",
    "Style: Default,Arial,24,&H00FFFFFF,&H000000FF,&H00000000,&H00000000,0,0,0,0,100,100,0,0,1,2,1,2,10,10,10,1\n",
    "\n",
    "[Events]\n",
    "Format: Layer, Start, End, Style, Name, MarginL, MarginR, MarginV, Effect, Text\n",
    "Dialogue: 0,0:00:01.00,0:00:03.00,Default,,0,0,0,,{\\c&HFFFFFF&}Bright HDR line\n",
);

const SAMPLE_SRT: &str = "1\n00:00:01,000 --> 00:00:03,000\nConverted from SRT\n";

fn temp_dir(label: &str) -> TestTempDir {
    TestTempDir::new("ssahdrify-cli-hdr", label)
}

#[test]
fn hdr_converts_ass_and_srt_for_pq_and_hlg_and_respects_overwrite() {
    assert_engine_bundle_ready();
    let work = temp_dir("pq-hlg-conversion");
    let ass_in = work.join("ep01.ass");
    let srt_in = work.join("ep02.srt");
    fs::write(&ass_in, SAMPLE_ASS).unwrap();
    fs::write(&srt_in, SAMPLE_SRT).unwrap();

    // 1. PQ conversion on ASS + SRT in one batch
    let pq = run_cli(&[
        "--lang",
        "en",
        "--json",
        "hdr",
        "--eotf",
        "pq",
        "--nits",
        "203",
        ass_in.to_str().unwrap(),
        srt_in.to_str().unwrap(),
    ]);
    assert!(
        pq.status.success(),
        "hdr pq failed: stderr={}",
        String::from_utf8_lossy(&pq.stderr)
    );
    let pq_json: serde_json::Value = serde_json::from_slice(&pq.stdout).unwrap();
    assert_eq!(pq_json["command"], "hdr");
    assert_eq!(pq_json["written"], 2);

    let ass_out = fs::read_to_string(work.join("ep01.hdr.ass")).unwrap();
    assert!(
        !ass_out.contains("&H00FFFFFF") && !ass_out.contains("{\\c&HFFFFFF&}"),
        "SDR pure white should be tone-mapped for PQ: {ass_out}"
    );
    let srt_out = fs::read_to_string(work.join("ep02.hdr.ass")).unwrap();
    assert!(srt_out.contains("[V4+ Styles]") && srt_out.contains("Converted from SRT"));

    // 2. Re-run without --overwrite skips existing outputs
    let skip_run = run_cli(&[
        "--lang",
        "en",
        "--json",
        "hdr",
        "--eotf",
        "pq",
        ass_in.to_str().unwrap(),
    ]);
    assert!(skip_run.status.success());
    let skip_json: serde_json::Value = serde_json::from_slice(&skip_run.stdout).unwrap();
    assert_eq!(skip_json["skipped"], 1);

    // 3. Re-run WITH --overwrite and HLG replaces output
    let hlg_run = run_cli(&[
        "--lang",
        "en",
        "--json",
        "--overwrite",
        "hdr",
        "--eotf",
        "hlg",
        "--nits",
        "150",
        ass_in.to_str().unwrap(),
    ]);
    assert!(hlg_run.status.success());
    let hlg_json: serde_json::Value = serde_json::from_slice(&hlg_run.stdout).unwrap();
    assert_eq!(hlg_json["written"], 1);
    let hlg_out = fs::read_to_string(work.join("ep01.hdr.ass")).unwrap();
    assert_ne!(
        ass_out, hlg_out,
        "HLG 150 nits output must differ from PQ 203 nits"
    );
}

#[test]
fn hdr_supports_dry_run_and_fail_fast_batch_abort() {
    assert_engine_bundle_ready();
    let work = temp_dir("dry-run-and-fail-fast");
    let valid = work.join("valid.ass");
    let missing = work.join("missing.ass");
    fs::write(&valid, SAMPLE_ASS).unwrap();

    // 1. --dry-run plans without writing
    let dry = run_cli(&[
        "--lang",
        "en",
        "--json",
        "--dry-run",
        "hdr",
        "--eotf",
        "pq",
        valid.to_str().unwrap(),
    ]);
    assert!(dry.status.success());
    let dry_json: serde_json::Value = serde_json::from_slice(&dry.stdout).unwrap();
    assert_eq!(dry_json["planned"], 1);
    assert!(!work.join("valid.hdr.ass").exists());

    // 2. --fail-fast aborts after first failed input without touching second input
    let ff = run_cli(&[
        "--lang",
        "en",
        "--json",
        "--fail-fast",
        "hdr",
        "--eotf",
        "pq",
        missing.to_str().unwrap(),
        valid.to_str().unwrap(),
    ]);
    assert_eq!(ff.status.code(), Some(2));
    let ff_json: serde_json::Value = serde_json::from_slice(&ff.stdout).unwrap();
    assert_eq!(ff_json["failed"], 1);
    assert_eq!(ff_json["abortedByFailFast"], true);
    assert_eq!(ff_json["results"].as_array().unwrap().len(), 1);
    assert!(!work.join("valid.hdr.ass").exists());
}
