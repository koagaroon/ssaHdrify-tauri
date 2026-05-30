//! Env-gated compatibility checks for a representative real font pack.
//!
//! These tests intentionally do not ship font binaries. Point
//! `SSAHDRIFY_TEST_FONT_ROOT` at a local font pack, then run:
//!
//!   SSAHDRIFY_TEST_FONT_ROOT="C:/path/to/font-pack" \
//!   cargo test --manifest-path src-tauri/Cargo.toml --test test_real_fonts -- --ignored --nocapture

use std::ffi::{OsStr, OsString};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use app_lib::font_cache::FontCache;

const FONT_ROOT_ENV: &str = "SSAHDRIFY_TEST_FONT_ROOT";
const DREAM_HAN_ROOT_ENV: &str = "SSAHDRIFY_TEST_DREAMHAN_ROOT";

const CJK_LOOKUP_CANDIDATES: &[&str] = &[
    "Source Han Serif SC",
    "Source Han Serif CN",
    "Noto Serif CJK SC",
    "Noto Sans CJK SC",
    "思源宋体 SC",
    "思源宋体 CN",
    "思源黑体 CN",
];

const DREAM_HAN_LOOKUP_CANDIDATES: &[&str] = &[
    "Dream Han Serif SC W22",
    "DreamHanSerifSC-W22",
    "Dream Han Serif SC",
    "梦源宋体 SC",
];

fn cli_path() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_ssahdrify-cli"))
}

fn temp_dir(label: &str) -> PathBuf {
    let pid = std::process::id();
    let nano = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let dir = std::env::temp_dir().join(format!("ssahdrify-real-fonts-{label}-{pid}-{nano}"));
    fs::create_dir_all(&dir).expect("failed to create test temp dir");
    dir
}

fn real_font_root() -> Option<PathBuf> {
    let Some(root) = std::env::var_os(FONT_ROOT_ENV).map(PathBuf::from) else {
        eprintln!("{FONT_ROOT_ENV} not set; skipping real-font compatibility gate");
        return None;
    };
    assert!(
        root.is_dir(),
        "{FONT_ROOT_ENV} must point to a readable font-pack directory: {}",
        root.display()
    );
    Some(root)
}

fn dream_han_root() -> Option<PathBuf> {
    let Some(root) = std::env::var_os(DREAM_HAN_ROOT_ENV).map(PathBuf::from) else {
        eprintln!("{DREAM_HAN_ROOT_ENV} not set; skipping Dream Han compatibility gate");
        return None;
    };
    if !root.is_dir() {
        eprintln!(
            "{DREAM_HAN_ROOT_ENV} does not point to a readable directory: {}; skipping",
            root.display()
        );
        return None;
    }
    Some(root)
}

fn require_font_file(path: PathBuf) -> PathBuf {
    assert!(
        path.is_file(),
        "expected local real-font fixture at {}",
        path.display()
    );
    path
}

fn is_font_file(path: &Path) -> bool {
    matches!(
        path.extension()
            .and_then(OsStr::to_str)
            .map(str::to_ascii_lowercase)
            .as_deref(),
        Some("otf" | "ttf" | "ttc")
    )
}

fn collect_font_leaf_dirs(root: &Path) -> Vec<PathBuf> {
    let mut stack = vec![root.to_path_buf()];
    let mut dirs = Vec::new();

    while let Some(dir) = stack.pop() {
        let entries = match fs::read_dir(&dir) {
            Ok(entries) => entries,
            Err(error) => panic!("failed to read {}: {error}", dir.display()),
        };
        let mut contains_font = false;
        for entry in entries {
            let entry = entry.expect("failed to read directory entry");
            let path = entry.path();
            let file_type = entry
                .file_type()
                .expect("failed to inspect directory entry");
            if file_type.is_dir() {
                stack.push(path);
            } else if file_type.is_file() && is_font_file(&path) {
                contains_font = true;
            }
        }
        if contains_font {
            dirs.push(dir);
        }
    }

    dirs.sort();
    dirs.dedup();
    dirs
}

fn representative_font_dirs(root: &Path) -> Vec<PathBuf> {
    let compact_root = root.join("精简包");
    assert!(
        compact_root.is_dir(),
        "expected compact font pack at {}",
        compact_root.display()
    );

    let mut dirs = collect_font_leaf_dirs(&compact_root);
    for targeted in [
        root.join("完整包").join("Adobe").join("CJK"),
        root.join("完整包").join("Google（谷歌）").join("CJK"),
    ] {
        if targeted.is_dir() {
            dirs.push(targeted);
        }
    }
    dirs.sort();
    dirs.dedup();
    assert!(
        !dirs.is_empty(),
        "expected at least one representative font directory under {}",
        root.display()
    );
    dirs
}

fn os(value: impl AsRef<OsStr>) -> OsString {
    value.as_ref().to_os_string()
}

fn run_cli(args: &[OsString]) -> std::process::Output {
    Command::new(cli_path())
        .args(args)
        .output()
        .expect("failed to spawn ssahdrify-cli")
}

fn write_ass_fixture_with_bold(dir: &Path, file_name: &str, family: &str, bold: i32) -> PathBuf {
    let path = dir.join(file_name);
    let content = format!(
        concat!(
            "[Script Info]\n",
            "ScriptType: v4.00+\n",
            "\n",
            "[V4+ Styles]\n",
            "Format: Name, Fontname, Fontsize, PrimaryColour, SecondaryColour, OutlineColour, BackColour, Bold, Italic, Underline, StrikeOut, ScaleX, ScaleY, Spacing, Angle, BorderStyle, Outline, Shadow, Alignment, MarginL, MarginR, MarginV, Encoding\n",
            "Style: Default,{family},20,&H00FFFFFF,&H000000FF,&H00000000,&H00000000,{bold},0,0,0,100,100,0,0,1,2,2,2,10,10,10,1\n",
            "\n",
            "[Events]\n",
            "Format: Layer, Start, End, Style, Name, MarginL, MarginR, MarginV, Effect, Text\n",
            "Dialogue: 0,0:00:01.00,0:00:03.00,Default,,0,0,0,,Real font gate 真实字体门禁\n",
        ),
        family = family,
        bold = bold
    );
    fs::write(&path, content).expect("failed to write ASS fixture");
    path
}

fn write_ass_fixture(dir: &Path, file_name: &str, family: &str) -> PathBuf {
    write_ass_fixture_with_bold(dir, file_name, family, 0)
}

fn write_multi_family_ass_fixture(dir: &Path, file_name: &str, families: &[String]) -> PathBuf {
    let path = dir.join(file_name);
    let mut content = String::from(concat!(
        "[Script Info]\n",
        "ScriptType: v4.00+\n",
        "\n",
        "[V4+ Styles]\n",
        "Format: Name, Fontname, Fontsize, PrimaryColour, SecondaryColour, OutlineColour, BackColour, Bold, Italic, Underline, StrikeOut, ScaleX, ScaleY, Spacing, Angle, BorderStyle, Outline, Shadow, Alignment, MarginL, MarginR, MarginV, Encoding\n",
    ));
    for (index, family) in families.iter().enumerate() {
        content.push_str(&format!(
            "Style: S{index},{family},20,&H00FFFFFF,&H000000FF,&H00000000,&H00000000,0,0,0,0,100,100,0,0,1,2,2,2,10,10,10,1\n"
        ));
    }
    content.push_str(concat!(
        "\n",
        "[Events]\n",
        "Format: Layer, Start, End, Style, Name, MarginL, MarginR, MarginV, Effect, Text\n",
    ));
    for index in 0..families.len() {
        content.push_str(&format!(
            "Dialogue: 0,0:00:01.00,0:00:03.00,S{index},,0,0,0,,Dream Han weight gate\n"
        ));
    }
    fs::write(&path, content).expect("failed to write multi-family ASS fixture");
    path
}

fn ass_font_uudecode(encoded: &str) -> Vec<u8> {
    let mut out = Vec::new();
    for group in encoded.as_bytes().chunks(4) {
        assert!(
            group.len() >= 2,
            "ASS embedded-font payload ended with an invalid 1-char group"
        );
        let mut vals = [0u8; 4];
        for (index, byte) in group.iter().enumerate() {
            vals[index] = byte
                .checked_sub(33)
                .expect("ASS embedded-font byte below printable alphabet");
        }

        let decoded = [
            (vals[0] << 2) | (vals[1] >> 4),
            ((vals[1] & 0x0f) << 4) | (vals[2] >> 2),
            ((vals[2] & 0x03) << 6) | vals[3],
        ];
        let count = if group.len() == 4 { 3 } else { group.len() - 1 };
        out.extend_from_slice(&decoded[..count]);
    }
    out
}

fn first_embedded_font(path: &Path) -> (String, Vec<u8>) {
    let text = fs::read_to_string(path).expect("read embedded ASS output");
    let mut in_fonts = false;
    let mut font_name: Option<String> = None;
    let mut encoded = String::new();

    for line in text.lines() {
        if line == "[Fonts]" {
            in_fonts = true;
            continue;
        }
        if !in_fonts {
            continue;
        }
        if line.starts_with('[') {
            break;
        }
        if let Some(name) = line.strip_prefix("fontname: ") {
            if font_name.is_some() {
                break;
            }
            font_name = Some(name.to_string());
            continue;
        }
        if font_name.is_some() && !line.is_empty() {
            encoded.push_str(line);
        }
    }

    let font_name = font_name.expect("embedded ASS output should contain a fontname header");
    assert!(
        !encoded.is_empty(),
        "embedded ASS output should contain encoded font data for {font_name}"
    );
    (font_name, ass_font_uudecode(&encoded))
}

fn utf16be_pattern(value: &str) -> Vec<u8> {
    value
        .encode_utf16()
        .flat_map(|unit| unit.to_be_bytes())
        .collect()
}

fn contains_bytes(haystack: &[u8], needle: &[u8]) -> bool {
    !needle.is_empty()
        && haystack
            .windows(needle.len())
            .any(|window| window == needle)
}

fn assert_dream_han_ttc_embeds_as_named_single_face_subset(output: &Path) {
    let (font_name, data) = first_embedded_font(output);
    assert_eq!(
        font_name, "dream_han_serif_sc_w22.ttf",
        "the visible ASS fontname is a generated attachment label"
    );
    assert!(
        data.starts_with(b"\0\x01\0\0"),
        "Dream Han W22 TTC should embed as a single-face TrueType subset, not a TTC collection"
    );
    assert!(
        !data.starts_with(b"ttcf"),
        "embedded Dream Han payload must not remain a TTC collection"
    );
    for name in ["Dream Han Serif SC W22", "DreamHanSerifSC-W22"] {
        let pattern = utf16be_pattern(name);
        assert!(
            contains_bytes(&data, &pattern),
            "embedded Dream Han subset should preserve name-table alias {name}"
        );
    }
}

fn lookup_first(cache: &FontCache, families: &[&str]) -> Option<String> {
    families
        .iter()
        .find_map(|family| match cache.lookup_family(family, false, false) {
            Ok(Some(found)) => {
                eprintln!(
                    "resolved {family} via cache: {}#{}",
                    found.font_path(),
                    found.face_index()
                );
                Some((*family).to_string())
            }
            Ok(None) => None,
            Err(error) => panic!("cache lookup failed for {family}: {error}"),
        })
}

fn diagnostic_for_family<'a>(value: &'a serde_json::Value, family: &str) -> &'a serde_json::Value {
    let fonts = value["fonts"]
        .as_array()
        .expect("diagnostics JSON should include fonts[]");
    fonts
        .iter()
        .find(|font| font["family"] == family)
        .unwrap_or_else(|| panic!("diagnostics JSON did not include family {family}: {value}"))
}

fn assert_diagnose_resolved_in_tier(output: &std::process::Output, family: &str, tier_name: &str) {
    assert!(
        output.status.success(),
        "diagnose-fonts should succeed: stderr={}",
        String::from_utf8_lossy(&output.stderr)
    );

    let value: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("diagnose stdout should be JSON");
    let diagnostic = diagnostic_for_family(&value, family);
    assert_eq!(diagnostic["result"], "resolved");
    assert!(
        diagnostic["path"]
            .as_str()
            .is_some_and(|path| !path.is_empty()),
        "resolved diagnostic should carry the font path: {diagnostic}"
    );
    assert!(
        diagnostic["tiers"].as_array().is_some_and(|tiers| {
            tiers
                .iter()
                .any(|tier| tier["tier"] == tier_name && tier["status"] == "hit")
        }),
        "diagnostic should prove the {tier_name} tier resolved the font: {diagnostic}"
    );
}

fn assert_diagnose_many_resolved_in_tier(
    output: &std::process::Output,
    families: &[String],
    tier_name: &str,
) {
    assert!(
        output.status.success(),
        "diagnose-fonts should succeed: stderr={}",
        String::from_utf8_lossy(&output.stderr)
    );

    let value: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("diagnose stdout should be JSON");
    for family in families {
        let diagnostic = diagnostic_for_family(&value, family);
        assert_eq!(diagnostic["result"], "resolved", "{diagnostic}");
        assert!(
            diagnostic["tiers"].as_array().is_some_and(|tiers| {
                tiers
                    .iter()
                    .any(|tier| tier["tier"] == tier_name && tier["status"] == "hit")
            }),
            "diagnostic should prove the {tier_name} tier resolved {family}: {diagnostic}"
        );
    }
}

fn assert_diagnose_resolved(output: &std::process::Output, family: &str) {
    assert!(
        output.status.success(),
        "diagnose-fonts should succeed: stderr={}",
        String::from_utf8_lossy(&output.stderr)
    );

    let value: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("diagnose stdout should be JSON");
    assert_eq!(value["files"][0]["status"], "diagnosed");
    assert_eq!(value["cache"]["status"], "usable");

    let diagnostic = diagnostic_for_family(&value, family);
    assert_eq!(diagnostic["result"], "resolved");
    assert!(
        diagnostic["path"]
            .as_str()
            .is_some_and(|path| !path.is_empty()),
        "resolved diagnostic should carry the font path: {diagnostic}"
    );
    assert!(
        diagnostic["tiers"].as_array().is_some_and(|tiers| {
            tiers
                .iter()
                .any(|tier| tier["tier"] == "cache" && tier["status"] == "hit")
        }),
        "diagnostic should prove the cache tier resolved the font: {diagnostic}"
    );
}

fn assert_diagnose_missing(output: &std::process::Output, family: &str) {
    assert!(
        output.status.success(),
        "diagnose-fonts should succeed for missing-font diagnostics: stderr={}",
        String::from_utf8_lossy(&output.stderr)
    );

    let value: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("diagnose stdout should be JSON");
    let diagnostic = diagnostic_for_family(&value, family);
    assert_eq!(diagnostic["result"], "missing");
}

#[test]
#[ignore = "requires SSAHDRIFY_TEST_FONT_ROOT pointing at a representative local font pack"]
fn real_font_package_refreshes_cache_and_diagnoses_cjk_fonts() {
    let Some(root) = real_font_root() else {
        return;
    };
    let work = temp_dir("refresh-diagnose");
    let cache_path = work.join("cache.sqlite3");
    let font_dirs = representative_font_dirs(&root);

    let mut refresh_args = vec![
        os("--lang"),
        os("en"),
        os("--cache-file"),
        os(cache_path.as_os_str()),
        os("refresh-fonts"),
    ];
    for dir in &font_dirs {
        refresh_args.push(os("--font-dir"));
        refresh_args.push(os(dir.as_os_str()));
    }

    let refresh = run_cli(&refresh_args);
    assert!(
        refresh.status.success(),
        "refresh-fonts should index representative real font dirs: stderr={}",
        String::from_utf8_lossy(&refresh.stderr)
    );
    assert!(cache_path.exists(), "refresh-fonts did not create cache");

    let cache = FontCache::open_existing_read_only(&cache_path).expect("open cache read-only");
    let cached_folders = cache.list_folders().expect("list cached folders");
    assert_eq!(
        cached_folders.len(),
        font_dirs.len(),
        "refresh-fonts should write one row per representative font dir"
    );
    for dir in &font_dirs {
        let canonical = dir.canonicalize().expect("canonicalize font dir");
        let canonical = canonical.display().to_string();
        assert!(
            cached_folders
                .iter()
                .any(|folder| folder.folder_path == canonical),
            "cache did not include expected font dir: {canonical}"
        );
    }

    let cjk_family = lookup_first(&cache, CJK_LOOKUP_CANDIDATES)
        .expect("expected Source Han or Noto CJK family to resolve from the real font pack");
    let cjk_ass = write_ass_fixture(&work, "real-cjk.ass", &cjk_family);
    let diagnose_cjk = run_cli(&[
        os("--lang"),
        os("en"),
        os("--json"),
        os("--cache-file"),
        os(cache_path.as_os_str()),
        os("diagnose-fonts"),
        os("--no-system-fonts"),
        os(cjk_ass.as_os_str()),
    ]);
    assert_diagnose_resolved(&diagnose_cjk, &cjk_family);
    assert!(
        !work.join("real-cjk.embed.ass").exists(),
        "diagnose-fonts must not write subtitle outputs"
    );

    let missing_family = "DefinitelyMissingSsaHdrifyRealFontGate";
    let missing_ass = write_ass_fixture(&work, "missing.ass", missing_family);
    let diagnose_missing = run_cli(&[
        os("--lang"),
        os("en"),
        os("--json"),
        os("--cache-file"),
        os(cache_path.as_os_str()),
        os("diagnose-fonts"),
        os("--no-system-fonts"),
        os(missing_ass.as_os_str()),
    ]);
    assert_diagnose_missing(&diagnose_missing, missing_family);

    match lookup_first(&cache, DREAM_HAN_LOOKUP_CANDIDATES) {
        Some(family) => eprintln!("Dream Han available in local pack as {family}"),
        None => eprintln!("Dream Han Serif SC W22 not present in the local pack cache"),
    }

    let _ = fs::remove_dir_all(work);
}

#[test]
#[ignore = "requires SSAHDRIFY_TEST_DREAMHAN_ROOT pointing at local Dream Han fixtures"]
fn dream_han_serif_face_names_resolve_across_local_cache_and_embed_paths() {
    let Some(root) = dream_han_root() else {
        return;
    };
    let serif_dir = root.join("DreamHanSerif");
    let sans_dir = root.join("DreamHanSans");
    let serif_w22 = require_font_file(serif_dir.join("DreamHanSerif-W22.ttc"));
    let serif_cn_w22 =
        require_font_file(root.join("DreamHanSerifCN").join("DreamHanSerifCN-W22.ttf"));
    let _sans_w22 = require_font_file(sans_dir.join("DreamHanSans-W22.ttc"));

    let work = temp_dir("dream-han");
    let cache_path = work.join("dream-han-cache.sqlite3");
    let out_normal = work.join("normal-out");
    let out_space_cjk = work.join("ramdisk shaped output 中文");
    let out_nested_missing = work.join("missing-parent").join("ass_out");

    let sc_plain = write_ass_fixture(&work, "dreamhan-sc-w22.ass", "Dream Han Serif SC W22");
    let sc_bold = write_ass_fixture_with_bold(
        &work,
        "dreamhan-sc-w22-bold.ass",
        "Dream Han Serif SC W22",
        -1,
    );
    let postscript = write_ass_fixture(
        &work,
        "dreamhan-sc-w22-postscript.ass",
        "DreamHanSerifSC-W22",
    );
    let cn_plain = write_ass_fixture(&work, "dreamhan-cn-w22.ass", "Dream Han Serif CN W22");
    let sans_plain = write_ass_fixture(&work, "dreamhan-sans-sc-w22.ass", "Dream Han Sans SC W22");

    let local_file_diagnose = run_cli(&[
        os("--lang"),
        os("en"),
        os("--json"),
        os("--no-cache"),
        os("diagnose-fonts"),
        os("--font-file"),
        os(serif_w22.as_os_str()),
        os("--no-system-fonts"),
        os(sc_plain.as_os_str()),
    ]);
    assert_diagnose_resolved_in_tier(&local_file_diagnose, "Dream Han Serif SC W22", "local");

    let local_dir_diagnose = run_cli(&[
        os("--lang"),
        os("en"),
        os("--json"),
        os("--no-cache"),
        os("diagnose-fonts"),
        os("--font-dir"),
        os(serif_dir.as_os_str()),
        os("--no-system-fonts"),
        os(sc_plain.as_os_str()),
    ]);
    assert_diagnose_resolved_in_tier(&local_dir_diagnose, "Dream Han Serif SC W22", "local");

    let local_ttf_diagnose = run_cli(&[
        os("--lang"),
        os("en"),
        os("--json"),
        os("--no-cache"),
        os("diagnose-fonts"),
        os("--font-file"),
        os(serif_cn_w22.as_os_str()),
        os("--no-system-fonts"),
        os(cn_plain.as_os_str()),
    ]);
    assert_diagnose_resolved_in_tier(&local_ttf_diagnose, "Dream Han Serif CN W22", "local");

    let refresh = run_cli(&[
        os("--lang"),
        os("en"),
        os("--cache-file"),
        os(cache_path.as_os_str()),
        os("refresh-fonts"),
        os("--font-dir"),
        os(serif_dir.as_os_str()),
        os("--font-dir"),
        os(sans_dir.as_os_str()),
    ]);
    assert!(
        refresh.status.success(),
        "refresh-fonts should index Dream Han dirs: stderr={}",
        String::from_utf8_lossy(&refresh.stderr)
    );

    let cache_sc_diagnose = run_cli(&[
        os("--lang"),
        os("en"),
        os("--json"),
        os("--cache-file"),
        os(cache_path.as_os_str()),
        os("diagnose-fonts"),
        os("--no-system-fonts"),
        os(sc_plain.as_os_str()),
    ]);
    assert_diagnose_resolved_in_tier(&cache_sc_diagnose, "Dream Han Serif SC W22", "cache");

    let cache_bold_diagnose = run_cli(&[
        os("--lang"),
        os("en"),
        os("--json"),
        os("--cache-file"),
        os(cache_path.as_os_str()),
        os("diagnose-fonts"),
        os("--no-system-fonts"),
        os(sc_bold.as_os_str()),
    ]);
    assert_diagnose_resolved_in_tier(&cache_bold_diagnose, "Dream Han Serif SC W22", "cache");

    let cache_postscript_diagnose = run_cli(&[
        os("--lang"),
        os("en"),
        os("--json"),
        os("--cache-file"),
        os(cache_path.as_os_str()),
        os("diagnose-fonts"),
        os("--no-system-fonts"),
        os(postscript.as_os_str()),
    ]);
    assert_diagnose_resolved_in_tier(&cache_postscript_diagnose, "DreamHanSerifSC-W22", "cache");

    let cache_sans_diagnose = run_cli(&[
        os("--lang"),
        os("en"),
        os("--json"),
        os("--cache-file"),
        os(cache_path.as_os_str()),
        os("diagnose-fonts"),
        os("--no-system-fonts"),
        os(sans_plain.as_os_str()),
    ]);
    assert_diagnose_resolved_in_tier(&cache_sans_diagnose, "Dream Han Sans SC W22", "cache");

    let serif_weights: Vec<String> = (1..=27)
        .map(|weight| format!("Dream Han Serif SC W{weight}"))
        .collect();
    let weights_ass =
        write_multi_family_ass_fixture(&work, "dreamhan-serif-sc-all-weights.ass", &serif_weights);
    let weights_diagnose = run_cli(&[
        os("--lang"),
        os("en"),
        os("--json"),
        os("--cache-file"),
        os(cache_path.as_os_str()),
        os("diagnose-fonts"),
        os("--no-system-fonts"),
        os(weights_ass.as_os_str()),
    ]);
    assert_diagnose_many_resolved_in_tier(&weights_diagnose, &serif_weights, "cache");

    for output_dir in [&out_normal, &out_space_cjk, &out_nested_missing] {
        let embed = run_cli(&[
            os("--lang"),
            os("en"),
            os("--cache-file"),
            os(cache_path.as_os_str()),
            os("--fail-fast"),
            os("--output-dir"),
            os(output_dir.as_os_str()),
            os("embed"),
            os("--no-system-fonts"),
            os("--on-missing"),
            os("fail"),
            os(sc_plain.as_os_str()),
        ]);
        assert!(
            embed.status.success(),
            "embed should write Dream Han output to {}: stderr={}",
            output_dir.display(),
            String::from_utf8_lossy(&embed.stderr)
        );
        assert!(
            output_dir.join("dreamhan-sc-w22.embed.ass").exists(),
            "embed output was not written under {}",
            output_dir.display()
        );
    }
    assert_dream_han_ttc_embeds_as_named_single_face_subset(
        &out_normal.join("dreamhan-sc-w22.embed.ass"),
    );

    let _ = fs::remove_dir_all(work);
}
