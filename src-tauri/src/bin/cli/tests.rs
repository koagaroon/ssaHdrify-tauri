#[test]
fn subset_retention_budget_accepts_boundary_and_rejects_excess_or_overflow() {
    assert_eq!(
        super::checked_subset_total(super::MAX_V8_SUBSET_TOTAL_BYTES - 1, 1).unwrap(),
        super::MAX_V8_SUBSET_TOTAL_BYTES
    );
    assert!(super::checked_subset_total(super::MAX_V8_SUBSET_TOTAL_BYTES, 1).is_err());
    assert!(super::checked_subset_total(usize::MAX, 1).is_err());
}

use super::{
    absolute_path, apply_effective_embedded_font_names, apply_subset_checks_to_diagnostics,
    build_font_qa_summary, check_cache_drift, classify_locale, copy_file_output,
    create_cli_font_db_dir, diagnostic_next_actions, display_path, duplicate_rename_output_keys,
    engine, format_refresh_cache_access_error, format_refresh_cache_schema_error,
    group_resolved_fonts_by_face, normalize_output_key, parse_duration_ms, parse_timestamp_ms,
    predict_chain_output_path, read_timing_map_text, refresh_font_directory_sources,
    relocate_output_path, require_complete_cli_font_source, require_enabled_timing_map_rules,
    resolve_embed_fonts, sanitize_for_display, substitute_template, write_output, CacheDiagnostic,
    CacheDiagnosticStatus, Cli, Command, CommandDiagnostics, CommandReport, DiagnoseMode,
    DiagnosticSubsetBudget, EmbedArgs, FileDiagnostic, FileReport, FileStatus, FontDiagnostic,
    FontQaStatus, FontSubsetCheckDiagnostic, FontSubsetCheckStatus, GlobalOptions,
    MissingFontAction, OutputLang, RefreshFontsArgs, ResolvedEmbedFont, TempFontDbDir,
    MAX_DIAGNOSTIC_SUBSET_CALLS, MAX_DIAGNOSTIC_SUBSET_TOTAL_BYTES, MAX_RESOLVED_FONT_CODEPOINTS,
    MAX_SHIFT_OFFSET_MS, MAX_SUBSET_CODEPOINTS_FOR_DEDUP, MAX_TIMING_MAP_BYTES,
};
// Import the canonical filename literal directly from app_lib so the
// test pins the same name `TempFontDbDir::drop`'s remove_dir_all
// sees on disk. No `as USER_FONT_DB_FILENAME` alias here — the
// test doesn't pretend to have its own filename literal;
// `USER_FONT_DB_FILENAME` is the canonical name everywhere, and an
// alias would only add indirection.
use app_lib::fonts::USER_FONT_DB_FILENAME;
use clap::error::ErrorKind;
use clap::{CommandFactory, Parser};
use std::fs;
use std::path::{Path, PathBuf};

fn timing_map_rule(enabled: Option<bool>) -> engine::TimingMapRule {
    engine::TimingMapRule {
        start_ms: 0,
        end_ms: None,
        offset_ms: 1,
        enabled,
        label: None,
    }
}

#[test]
fn timing_map_file_accepts_one_mib_and_rejects_one_byte_over() {
    let root = create_cli_font_db_dir().expect("temporary test directory");
    let map_path = root.join("boundary-map.csv");

    fs::write(&map_path, vec![b' '; MAX_TIMING_MAP_BYTES as usize])
        .expect("write at-limit timing map");
    let at_limit = read_timing_map_text(&map_path).expect("one MiB should be accepted");
    assert_eq!(at_limit.len(), MAX_TIMING_MAP_BYTES as usize);

    fs::write(&map_path, vec![b' '; MAX_TIMING_MAP_BYTES as usize + 1])
        .expect("write over-limit timing map");
    let error = read_timing_map_text(&map_path).expect_err("one MiB plus one must be refused");
    assert!(
        error.contains("timing map file is too large")
            && error.contains(&(MAX_TIMING_MAP_BYTES + 1).to_string())
            && error.contains(&format!("max {MAX_TIMING_MAP_BYTES}")),
        "size refusal should report the actual and accepted byte counts: {error}"
    );

    fs::remove_dir_all(root).expect("remove timing-map test directory");
}

#[test]
fn timing_map_rules_require_at_least_one_rule() {
    let error = require_enabled_timing_map_rules(Vec::new())
        .expect_err("an empty parsed map must be refused");

    assert_eq!(error, "timing map contains no rules");
}

#[test]
fn timing_map_rules_require_at_least_one_enabled_rule() {
    let error = require_enabled_timing_map_rules(vec![
        timing_map_rule(Some(false)),
        timing_map_rule(Some(false)),
    ])
    .expect_err("an all-disabled parsed map must be refused");
    assert_eq!(error, "timing map contains no enabled rules");

    for enabled in [None, Some(true)] {
        let rules = require_enabled_timing_map_rules(vec![timing_map_rule(enabled)])
            .expect("omitted and explicit true both mean enabled");
        assert_eq!(rules.len(), 1);
    }
}

#[test]
fn absolute_path_folds_cli_dot_and_parent_components_without_touching_names() {
    let cwd = std::env::current_dir().unwrap();
    let normalized = absolute_path(Path::new("one/./two/../three/file..name.ass")).unwrap();

    assert_eq!(
        normalized,
        cwd.join("one").join("three").join("file..name.ass")
    );
    assert!(normalized.is_absolute());
    assert!(!normalized
        .components()
        .any(|component| matches!(component, std::path::Component::ParentDir)));
}

#[test]
fn absolute_path_clamps_parent_components_at_the_filesystem_root() {
    let cwd = std::env::current_dir().unwrap();
    let root = cwd.ancestors().last().unwrap();

    assert_eq!(
        absolute_path(&root.join("..").join("output.ass")).unwrap(),
        root.join("output.ass")
    );
    assert_eq!(
        absolute_path(&root.join("folder").join("..").join("..").join("output.ass")).unwrap(),
        root.join("output.ass")
    );
}

#[test]
fn absolute_path_handles_rooted_windows_paths_and_rejects_drive_relative_paths() {
    let cwd = std::env::current_dir().unwrap();
    let root = cwd.ancestors().last().unwrap();

    assert_eq!(
        absolute_path(Path::new(r"\ssahdrify-test\file.ass")).unwrap(),
        root.join("ssahdrify-test").join("file.ass")
    );
    let error = absolute_path(Path::new(r"C:ssahdrify-test\file.ass")).unwrap_err();
    assert!(error.contains("drive-relative paths are ambiguous"));
}

/// pin the cross-module constant equality. The CLI's
/// dedup cap MUST equal the IPC cap inside fonts.rs — the dedup
/// decision (merge union vs per-alias fallback) is bounded by
/// what subset_font itself accepts. A unilateral bump on either
/// side would silently break the b8aa3fd fallback contract; this
/// test fails the build instead. TS sibling
/// (`MAX_SUBSET_CODEPOINTS_FOR_DEDUP` in font-embedder.ts) carries
/// a WHY comment naming `app_lib::fonts::MAX_SUBSET_CODEPOINTS` as
/// the source of truth.
#[test]
fn dedup_cap_matches_ipc_cap() {
    assert_eq!(
        MAX_SUBSET_CODEPOINTS_FOR_DEDUP,
        app_lib::fonts::MAX_SUBSET_CODEPOINTS,
        "CLI dedup cap must equal the fonts.rs IPC cap; the dedup \
         fallback path is bounded by what subset_font accepts. \
         If you change one, change both AND update the TS sibling \
         at src/features/font-embed/font-embedder.ts."
    );
}

/// Pin the per-alias resolved cap < dedup cap inequality the
/// fallback path's safety claim depends on. The cap-busting
/// fallback subsets each alias independently — each alias's
/// codepoints is bounded by `MAX_RESOLVED_FONT_CODEPOINTS`
/// upstream in `resolve_embed_fonts`, and that bound MUST be
/// strictly less than the subset cap or the per-alias call could
/// itself overflow `subset_font`. Same cross-language drift
/// defense rationale that motivated `dedup_cap_matches_ipc_cap`:
/// the relationship is load-bearing, so this test target carries a
/// compile-time assertion rather than relying on prose alone.
#[test]
fn resolved_font_cap_fits_subset_cap() {
    const _: () = assert!(
        MAX_RESOLVED_FONT_CODEPOINTS < MAX_SUBSET_CODEPOINTS_FOR_DEDUP,
        "MAX_RESOLVED_FONT_CODEPOINTS must stay strictly less than \
         MAX_SUBSET_CODEPOINTS_FOR_DEDUP so each fallback subset fits \
         under the cap."
    );
}

#[test]
fn global_help_stays_user_facing() {
    let help = Cli::command().render_long_help().to_string();

    assert!(help.contains("Skip the persistent font cache for this run."));
    assert!(help.contains("Override the default font cache file path."));

    for internal_phrase in [
        "global = true",
        "drift surface",
        "no-silent-action violation",
        "The clap idiom",
    ] {
        assert!(
            !help.contains(internal_phrase),
            "CLI help leaked internal implementation rationale: {internal_phrase}"
        );
    }
}

#[test]
fn diagnose_without_value_parses_as_summary() {
    let cli = Cli::try_parse_from([
        "ssahdrify-cli",
        "hdr",
        "--eotf",
        "pq",
        "--diagnose",
        "input.ass",
    ])
    .expect("bare --diagnose should parse");

    match cli.command {
        Command::Hdr(args) => assert_eq!(args.diagnose.mode(), Some(DiagnoseMode::Summary)),
        other => panic!("expected hdr command, got {other:?}"),
    }
}

#[test]
fn diagnose_full_parses() {
    let cli = Cli::try_parse_from([
        "ssahdrify-cli",
        "embed",
        "--diagnose=full",
        "--no-system-fonts",
        "input.ass",
    ])
    .expect("--diagnose=full should parse");

    match cli.command {
        Command::Embed(args) => assert_eq!(args.diagnose.mode(), Some(DiagnoseMode::Full)),
        other => panic!("expected embed command, got {other:?}"),
    }
}

#[test]
fn diagnose_suffix_after_input_parses() {
    let cli = Cli::try_parse_from(["ssahdrify-cli", "embed", "input.ass", "--diagnose=full"])
        .expect("diagnose should parse after command inputs");

    match cli.command {
        Command::Embed(args) => assert_eq!(args.diagnose.mode(), Some(DiagnoseMode::Full)),
        other => panic!("expected embed command, got {other:?}"),
    }
}

#[test]
fn invalid_diagnose_value_fails_at_parse_time() {
    let err = Cli::try_parse_from([
        "ssahdrify-cli",
        "shift",
        "--offset",
        "+1s",
        "--diagnose=short",
        "input.ass",
    ])
    .expect_err("invalid diagnose value should fail");

    assert_eq!(err.kind(), ErrorKind::InvalidValue);
}

#[test]
fn hdr_nits_accepts_documented_boundaries_and_rejects_values_outside_them() {
    for value in ["1", "10000"] {
        let cli = Cli::try_parse_from([
            "ssahdrify-cli",
            "hdr",
            "--eotf",
            "pq",
            "--nits",
            value,
            "input.ass",
        ])
        .unwrap_or_else(|error| panic!("--nits {value} should parse: {error}"));
        let Command::Hdr(args) = cli.command else {
            panic!("expected hdr command");
        };
        assert_eq!(args.args.nits.to_string(), value);
    }

    for value in ["0", "10001"] {
        let error = Cli::try_parse_from([
            "ssahdrify-cli",
            "hdr",
            "--eotf",
            "pq",
            "--nits",
            value,
            "input.ass",
        ])
        .expect_err("out-of-range --nits must fail at parse time");
        assert!(
            error.to_string().contains("1..=10000"),
            "range error should name the accepted boundary: {error}"
        );
    }
}

#[test]
fn refresh_fonts_rejects_diagnose_option() {
    let err = Cli::try_parse_from([
        "ssahdrify-cli",
        "refresh-fonts",
        "--diagnose",
        "--font-dir",
        "fonts",
    ])
    .expect_err("unsupported command should reject --diagnose");

    assert_eq!(err.kind(), ErrorKind::UnknownArgument);
}

#[test]
fn embed_parses_repeatable_recursive_font_dirs() {
    let cli = Cli::try_parse_from([
        "ssahdrify-cli",
        "embed",
        "--recursive-font-dir",
        "library-a",
        "--recursive-font-dir",
        "library-b",
        "input.ass",
    ])
    .expect("recursive font library flags should be repeatable");

    let Command::Embed(args) = cli.command else {
        panic!("expected embed command");
    };
    assert_eq!(
        args.args.recursive_font_dirs,
        vec![PathBuf::from("library-a"), PathBuf::from("library-b")]
    );
    assert!(args.args.font_dirs.is_empty());
    assert!(args.args.has_user_font_sources());
}

#[test]
fn diagnose_fonts_parses_recursive_and_shallow_sources_together() {
    let cli = Cli::try_parse_from([
        "ssahdrify-cli",
        "diagnose-fonts",
        "--recursive-font-dir",
        "library",
        "--font-dir",
        "flat",
        "input.ass",
    ])
    .expect("diagnose-fonts should accept mixed directory scopes");

    let Command::DiagnoseFonts(args) = cli.command else {
        panic!("expected diagnose-fonts command");
    };
    assert_eq!(args.recursive_font_dirs, vec![PathBuf::from("library")]);
    assert_eq!(args.font_dirs, vec![PathBuf::from("flat")]);
}

#[test]
fn refresh_fonts_accepts_recursive_source_without_shallow_source() {
    let cli = Cli::try_parse_from([
        "ssahdrify-cli",
        "refresh-fonts",
        "--recursive-font-dir",
        "library",
    ])
    .expect("either directory-source flag should satisfy refresh-fonts");

    let Command::RefreshFonts(args) = cli.command else {
        panic!("expected refresh-fonts command");
    };
    assert!(args.font_dirs.is_empty());
    assert_eq!(args.recursive_font_dirs, vec![PathBuf::from("library")]);
}

#[test]
fn refresh_fonts_keeps_legacy_shallow_source_valid() {
    let cli = Cli::try_parse_from(["ssahdrify-cli", "refresh-fonts", "--font-dir", "flat"])
        .expect("the existing shallow flag must remain valid by itself");

    let Command::RefreshFonts(args) = cli.command else {
        panic!("expected refresh-fonts command");
    };
    assert_eq!(args.font_dirs, vec![PathBuf::from("flat")]);
    assert!(args.recursive_font_dirs.is_empty());
}

#[test]
fn refresh_fonts_accepts_mixed_directory_scopes() {
    let cli = Cli::try_parse_from([
        "ssahdrify-cli",
        "refresh-fonts",
        "--recursive-font-dir",
        "library",
        "--font-dir",
        "flat",
    ])
    .expect("refresh-fonts should accept mixed directory scopes");

    let Command::RefreshFonts(args) = cli.command else {
        panic!("expected refresh-fonts command");
    };
    assert_eq!(args.recursive_font_dirs, vec![PathBuf::from("library")]);
    assert_eq!(args.font_dirs, vec![PathBuf::from("flat")]);
}

#[test]
fn refresh_source_iteration_prioritizes_recursive_before_shallow() {
    let args = RefreshFontsArgs {
        font_dirs: vec![PathBuf::from("flat-a"), PathBuf::from("flat-b")],
        recursive_font_dirs: vec![PathBuf::from("library-a"), PathBuf::from("library-b")],
    };
    let sources: Vec<(PathBuf, app_lib::fonts::FontDirectoryScope)> =
        refresh_font_directory_sources(&args)
            .map(|(path, scope, _flag)| (path.to_path_buf(), scope))
            .collect();

    assert_eq!(
        sources,
        vec![
            (
                PathBuf::from("library-a"),
                app_lib::fonts::FontDirectoryScope::Recursive,
            ),
            (
                PathBuf::from("library-b"),
                app_lib::fonts::FontDirectoryScope::Recursive,
            ),
            (
                PathBuf::from("flat-a"),
                app_lib::fonts::FontDirectoryScope::Shallow,
            ),
            (
                PathBuf::from("flat-b"),
                app_lib::fonts::FontDirectoryScope::Shallow,
            ),
        ]
    );
}

#[test]
fn refresh_fonts_rejects_missing_directory_source() {
    let err = Cli::try_parse_from(["ssahdrify-cli", "refresh-fonts"])
        .expect_err("refresh-fonts requires one directory-source flag");

    assert_eq!(err.kind(), ErrorKind::MissingRequiredArgument);
}

#[test]
fn refresh_cache_errors_sanitize_paths_and_limit_delete_guidance() {
    let hostile = Path::new("cache\u{202e}\u{001b}[2J.sqlite3");

    let schema = format_refresh_cache_schema_error(hostile, 4, 6);
    assert!(!schema.contains('\u{202e}') && !schema.contains('\u{001b}'));
    assert!(schema.contains("cache[2J.sqlite3"));
    assert!(schema.contains("rerun the same `refresh-fonts` command"));

    let invalid = format_refresh_cache_access_error(
        hostile,
        "Opening",
        app_lib::font_cache::CacheError::InvalidDatabase("file is not a database".to_string()),
    );
    assert!(!invalid.contains('\u{202e}') && !invalid.contains('\u{001b}'));
    assert!(invalid.contains("Delete the cache file"));
    assert!(invalid.contains("rerun the same `refresh-fonts` command"));

    let ordinary = format_refresh_cache_access_error(
        hostile,
        "Opening",
        app_lib::font_cache::CacheError::Io("permission denied".to_string()),
    );
    assert!(!ordinary.contains('\u{202e}') && !ordinary.contains('\u{001b}'));
    assert!(ordinary.contains("Check the cache path and file permissions"));
    assert!(!ordinary.contains("Delete the cache file"));
    assert!(!ordinary.contains("rerun the same `refresh-fonts` command"));
}

/// Construct a default GlobalOptions for tests that need to call
/// fs-touching helpers (write_output / copy_file_output etc.) which
/// take `&GlobalOptions` for stat-failure warning localization.
fn test_globals() -> GlobalOptions {
    GlobalOptions {
        output_dir: None,
        overwrite: false,
        dry_run: false,
        quiet: true,
        verbose: false,
        json: false,
        lang: Some(OutputLang::En),
        no_cache: false,
        cache_file: None,
        fail_fast: false,
    }
}

#[test]
fn decode_warning_flags_are_independent_and_counted_once() {
    for inferred in [false, true] {
        for lossy in [false, true] {
            let decoded = app_lib::encoding::ReadTextResult {
                text: String::new(),
                encoding: "UTF-16LE".into(),
                encoding_id: "UTF-16LE".into(),
                had_bom: false,
                lossy,
                inferred_without_bom: inferred,
                source_revision: String::new(),
                source_byte_length: 0,
            };
            let warnings = super::format_decode_warnings(&test_globals(), &decoded);
            let expected = usize::from(inferred) + usize::from(lossy);
            assert_eq!(warnings.as_ref().map_or(0, Vec::len), expected);
            let messages = warnings
                .as_ref()
                .map(|items| items.join("\n"))
                .unwrap_or_default();
            assert_eq!(
                messages.matches("best-effort inference").count(),
                usize::from(inferred)
            );
            assert_eq!(messages.matches("malformed").count(), usize::from(lossy));
            let mut report = CommandReport::new("shift");
            report.push(FileReport {
                input: "source.ass".into(),
                output: Some("source.shifted.ass".into()),
                encoding: Some(decoded.encoding),
                status: FileStatus::Written,
                error: None,
                warnings,
            });
            let files = usize::from(expected > 0);
            assert_eq!(super::warning_counts(&report), (files, expected, files));
        }
    }
}

#[test]
fn rename_preflight_requires_unpaired_subtitle_identity_but_excludes_videos() {
    let root = create_cli_font_db_dir().unwrap();
    let source = root.join("source.ass");
    fs::write(&source, "source-original").unwrap();
    let mut rows = vec![engine::RenamePlanRow {
        input_path: display_path(&source),
        output_path: display_path(&root.join("target.ass")),
        video_path: display_path(&root.join("unavailable.mkv")),
        source: "regex".into(),
        no_op: false,
        input_conflict: false,
    }];
    let mut selected = vec![display_path(&source), rows[0].video_path.clone()];
    super::protect_rename_selected_inputs(&selected, &mut rows).unwrap();
    assert!(!rows[0].input_conflict);
    selected.push(display_path(&root.join("unavailable.sup")));
    let error = super::protect_rename_selected_inputs(&selected, &mut rows).unwrap_err();
    assert!(
        error.contains("Failed to resolve selected subtitle input"),
        "{error}"
    );
    assert_eq!(fs::read_to_string(source).unwrap(), "source-original");
    assert!(!root.join("target.ass").exists());
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn cache_drift_json_keeps_raw_paths_and_adds_scope_aware_sources() {
    let modified = app_lib::font_cache::CacheSourceKey {
        source_root: "C:\\Fonts\\Library".to_string(),
        scope: app_lib::font_cache::FontDirectoryScope::Recursive,
    };
    let removed = app_lib::font_cache::CacheSourceKey {
        source_root: "C:\\Fonts\\Flat".to_string(),
        scope: app_lib::font_cache::FontDirectoryScope::Shallow,
    };
    let mut diagnostic = CacheDiagnostic::new(
        None,
        CacheDiagnosticStatus::Drift,
        Some("changed".to_string()),
    );
    diagnostic.modified_folders = vec![modified.source_root.clone()];
    diagnostic.removed_folders = vec![removed.source_root.clone()];
    diagnostic.modified_sources = vec![modified.clone()];
    diagnostic.removed_sources = vec![removed.clone()];

    let json = serde_json::to_value(diagnostic).unwrap();
    assert_eq!(json["modifiedFolders"][0], modified.source_root);
    assert_eq!(json["removedFolders"][0], removed.source_root);
    assert_eq!(json["modifiedSources"][0]["scope"], "recursive");
    assert_eq!(json["removedSources"][0]["scope"], "shallow");
    assert!(
        !json["modifiedFolders"][0]
            .as_str()
            .unwrap()
            .contains("[recursive]"),
        "machine-readable paths must not contain terminal decoration"
    );
}

#[test]
fn incomplete_cli_font_imports_are_rejected_instead_of_used_partially() {
    let globals = test_globals();
    for reason in [
        app_lib::fonts::ScanStopReason::UserCancel,
        app_lib::fonts::ScanStopReason::CeilingHit,
        app_lib::fonts::ScanStopReason::IncompleteIo,
    ] {
        let summary = app_lib::fonts::FontSourceImportSummary {
            total: 12,
            added: 10,
            duplicated: 2,
            reason,
        };
        let error = require_complete_cli_font_source(
            &globals,
            "recursive font library",
            "递归字体库",
            Some(Path::new("library")),
            &summary,
        )
        .expect_err("partial non-interactive imports must fail closed");
        assert!(error.contains("partial font results were discarded"));
    }
}

#[test]
fn cli_drift_marks_existing_unreadable_recursive_source_modified() {
    let guard = TempFontDbDir(create_cli_font_db_dir().unwrap());
    let root = guard.0.join("library");
    fs::create_dir_all(&root).unwrap();
    let original = app_lib::font_cache::snapshot_source_directories(
        &root,
        app_lib::font_cache::FontDirectoryScope::Recursive,
    )
    .unwrap();
    let cache_path = guard.0.join("drift-cache.sqlite3");
    let mut cache = app_lib::font_cache::FontCache::open_or_create(&cache_path).unwrap();
    cache.replace_source(&original, &[]).unwrap();
    fs::write(root.join("evil\u{202e}name.ttf"), b"candidate").unwrap();

    let report = check_cache_drift(&cache).unwrap();
    assert_eq!(report.modified, vec![original.key()]);
    assert!(report.removed.is_empty());
}

#[test]
fn recursive_snapshot_and_cache_identity_include_nested_candidate() {
    let guard = TempFontDbDir(create_cli_font_db_dir().unwrap());
    let root = guard.0.join("library");
    let nested = root.join("nested").join("deeper");
    fs::create_dir_all(&nested).unwrap();
    fs::write(
        nested.join("candidate.ttf"),
        b"not parsed in this metadata test",
    )
    .unwrap();
    let args = RefreshFontsArgs {
        font_dirs: Vec::new(),
        recursive_font_dirs: vec![root.clone()],
    };
    let (source_path, scope, flag) = refresh_font_directory_sources(&args).next().unwrap();
    assert_eq!(flag, "--recursive-font-dir");
    let snapshot = app_lib::font_cache::snapshot_source_directories(source_path, scope).unwrap();
    assert_eq!(
        snapshot.scope,
        app_lib::font_cache::FontDirectoryScope::Recursive
    );
    assert_eq!(snapshot.files.len(), 1);

    let cache_path = guard.0.join("recursive-cache.sqlite3");
    let mut cache = app_lib::font_cache::FontCache::open_or_create(&cache_path).unwrap();
    cache.replace_source(&snapshot, &[]).unwrap();
    let sources = cache.list_sources().unwrap();
    assert_eq!(sources.len(), 1);
    assert_eq!(
        sources[0].scope,
        app_lib::font_cache::FontDirectoryScope::Recursive
    );
    assert_eq!(sources[0].files.len(), 1);
}

#[test]
fn non_font_warnings_do_not_suggest_strict_embed_policy() {
    let globals = test_globals();
    let diagnostics = CommandDiagnostics {
        mode: DiagnoseMode::Summary,
        files_with_warnings: 1,
        warning_count: 1,
        notes: Vec::new(),
        files: vec![FileDiagnostic {
            input: "input.ass".to_string(),
            output: None,
            encoding: None,
            status: FileStatus::Written,
            error: None,
            warnings: vec!["non-font warning".to_string()],
        }],
        cache: None,
        qa: None,
        fonts: Vec::new(),
    };

    let actions = diagnostic_next_actions(&globals, &diagnostics, true);

    assert!(
        actions
            .iter()
            .any(|action| action.contains("--diagnose=full")),
        "warning diagnostics should still point users to full detail: {actions:?}"
    );
    assert!(
        actions
            .iter()
            .all(|action| !action.contains("embed --on-missing")),
        "non-font warnings should not suggest strict font-embedding policy: {actions:?}"
    );
}

fn qa_usage(label: &str) -> engine::FontEmbedUsage {
    engine::FontEmbedUsage {
        family: label.to_string(),
        bold: false,
        italic: false,
        label: label.to_string(),
        font_name: format!("{label}.ttf"),
        glyph_count: 1,
        codepoints: vec![0x41],
    }
}

fn qa_report(status: FileStatus) -> CommandReport {
    let mut report = CommandReport::new("diagnose-fonts");
    report.push(FileReport {
        input: "input.ass".to_string(),
        output: None,
        encoding: Some("utf-8".to_string()),
        status,
        error: None,
        warnings: None,
    });
    report
}

#[test]
fn font_qa_summary_marks_complete_when_all_fonts_resolve() {
    let mut resolved = FontDiagnostic::new(&qa_usage("Resolved"));
    resolved.mark_resolved("/fonts/resolved.ttf".to_string(), 0);

    let qa = build_font_qa_summary(&qa_report(FileStatus::Diagnosed), &[resolved]);

    assert_eq!(qa.status, FontQaStatus::Complete);
    assert_eq!(qa.resolved_count, 1);
    assert_eq!(qa.missing_count, 0);
}

#[test]
fn font_qa_summary_marks_incomplete_for_missing_fonts() {
    let missing = FontDiagnostic::new(&qa_usage("Missing"));

    let qa = build_font_qa_summary(&qa_report(FileStatus::Diagnosed), &[missing]);

    assert_eq!(qa.status, FontQaStatus::Incomplete);
    assert_eq!(qa.missing_count, 1);
}

#[test]
fn font_qa_summary_marks_blocked_for_subset_failures() {
    let mut resolved = FontDiagnostic::new(&qa_usage("BrokenSubset"));
    resolved.mark_resolved("/fonts/broken.ttf".to_string(), 0);
    resolved.subset_check = Some(FontSubsetCheckDiagnostic {
        status: FontSubsetCheckStatus::Failed,
        bytes: None,
        error: Some("subset failed".to_string()),
    });

    let qa = build_font_qa_summary(&qa_report(FileStatus::Diagnosed), &[resolved]);

    assert_eq!(qa.status, FontQaStatus::Blocked);
    assert_eq!(qa.subset_failed_count, 1);
}

#[test]
fn font_qa_summary_ignores_non_font_warnings() {
    let mut resolved = FontDiagnostic::new(&qa_usage("Resolved"));
    resolved.mark_resolved("/fonts/resolved.ttf".to_string(), 0);
    let mut report = qa_report(FileStatus::Diagnosed);
    report.results[0].warnings = Some(vec![
        "BOM-less UTF-16 encoding was inferred; verify the output".to_string(),
    ]);

    let qa = build_font_qa_summary(&report, &[resolved]);

    assert_eq!(
        qa.status,
        FontQaStatus::Complete,
        "encoding and other file warnings must not downgrade font QA"
    );
}

#[test]
fn shift_request_omits_absent_threshold_from_json_wire_shape() {
    let request = engine::ShiftConversionRequest {
        input_path: "C:\\subs\\episode.ass".to_string(),
        content: "[Script Info]\n".to_string(),
        offset_ms: 1000,
        threshold_ms: None,
        timing_map_rules: None,
        output_template: "{name}.shifted{ext}".to_string(),
    };

    let json = serde_json::to_value(request).expect("request should serialize");
    assert!(
        json.get("thresholdMs").is_none(),
        "None threshold must be omitted, not serialized as thresholdMs:null"
    );
}

#[test]
fn resolve_embed_fonts_drops_diagnostics_when_not_requested() {
    let globals = test_globals();
    let args = EmbedArgs {
        font_dirs: Vec::new(),
        recursive_font_dirs: Vec::new(),
        font_files: Vec::new(),
        no_system_fonts: true,
        on_missing: MissingFontAction::Warn,
        output_template: "{name}.embed.ass".to_string(),
        files: Vec::new(),
    };
    let usage = engine::FontEmbedUsage {
        family: "Definitely Missing".to_string(),
        bold: false,
        italic: false,
        label: "Definitely Missing".to_string(),
        font_name: "Definitely Missing".to_string(),
        glyph_count: 1,
        codepoints: vec![0x41],
    };

    let no_diagnostics = resolve_embed_fonts(
        &globals,
        &args,
        false,
        None,
        false,
        std::slice::from_ref(&usage),
    )
    .expect("warn mode should return a skipped-font warning");
    assert!(no_diagnostics.diagnostics.is_empty());
    assert_eq!(
        no_diagnostics.warnings,
        vec!["missing font: Definitely Missing".to_string()]
    );

    let with_diagnostics = resolve_embed_fonts(&globals, &args, false, None, true, &[usage])
        .expect("diagnostic mode should still return warn-mode outcome");
    assert_eq!(with_diagnostics.diagnostics.len(), 1);
}

#[test]
fn classify_locale_picks_zh_for_chinese_tags() {
    for tag in [
        "zh",
        "zh-CN",
        "zh_CN",
        "zh_TW.UTF-8",
        "zh-Hans-CN",
        "ZH",
        "zh@pinyin",
    ] {
        assert_eq!(classify_locale(tag), OutputLang::Zh, "tag = {tag}");
    }
}

#[test]
fn classify_locale_falls_back_to_en_for_others_and_garbage() {
    for tag in [
        "",
        "en",
        "en-US",
        "en_US.UTF-8",
        "C",
        "POSIX",
        "ja-JP",
        "-zh",
        ".",
    ] {
        assert_eq!(classify_locale(tag), OutputLang::En, "tag = {tag}");
    }
}

#[test]
fn parses_signed_duration_examples() {
    assert_eq!(parse_duration_ms("+2.5s").unwrap(), 2500);
    assert_eq!(parse_duration_ms("-500ms").unwrap(), -500);
    assert_eq!(parse_duration_ms("+1m30s").unwrap(), 90_000);
    assert_eq!(parse_duration_ms("2h").unwrap(), 7_200_000);
}

#[test]
fn rejects_invalid_duration() {
    assert!(parse_duration_ms("").is_err());
    assert!(parse_duration_ms("+").is_err());
    assert!(parse_duration_ms("10").is_err());
    assert!(parse_duration_ms("1week").is_err());
}

#[test]
fn rejects_repeated_or_out_of_order_units() {
    // Same-unit repetition: guards against a silent-sum bug
    // where `1s2s` would parse as 3 seconds.
    assert!(parse_duration_ms("+1s2s").is_err());
    assert!(parse_duration_ms("-30s1s").is_err());
    assert!(parse_duration_ms("+1m1m").is_err());
    // Out-of-order: smaller unit cannot precede a larger one.
    assert!(parse_duration_ms("+30s1m").is_err());
    assert!(parse_duration_ms("+500ms2s").is_err());
    assert!(parse_duration_ms("+1m1h").is_err());
    // Canonical descending form still works.
    assert_eq!(parse_duration_ms("+1h30m45s500ms").unwrap(), 5_445_500);
}

#[test]
fn parses_threshold_timestamps() {
    assert_eq!(parse_timestamp_ms("00:10:00").unwrap(), 600_000);
    assert_eq!(parse_timestamp_ms("01:02:03.4").unwrap(), 3_723_400);
    assert_eq!(parse_timestamp_ms("01:02:03.045").unwrap(), 3_723_045);
}

#[test]
fn rejects_invalid_threshold_timestamps() {
    assert!(parse_timestamp_ms("10:00").is_err());
    assert!(parse_timestamp_ms("00:60:00").is_err());
    assert!(parse_timestamp_ms("00:00:00.1234").is_err());
}

#[test]
fn parse_duration_ms_caps_extreme_values() {
    // Far-future hours: f64 multiplication produces a value beyond
    // MAX_SHIFT_OFFSET_MS — bound check fires before the as-cast
    // saturates. Pre-N4 fix this would have wrapped via
    // i64::MIN.abs() and bypassed the cap.
    assert!(parse_duration_ms("+9999999999999h").is_err());
    // Negative analogue (the original wrap path).
    assert!(parse_duration_ms("-9999999999999h").is_err());
    // Above-cap seconds.
    assert!(parse_duration_ms("+999999999999s").is_err());
}

// at-limit / over-limit boundary pair for MAX_SHIFT_OFFSET_MS.
// When a test name promises a cap, pair the at-limit test (must
// accept) with an over-limit counter-test (must reject), so a
// refactor that loosens the cap in either direction surfaces. The
// existing `parse_duration_ms_caps_extreme_values` test above only
// exercises far-over-cap inputs; the at-cap boundary was unpinned.
#[test]
fn parse_duration_ms_at_max_shift_offset_accepts() {
    // MAX_SHIFT_OFFSET_MS = 365 * 24 * 60 * 60 * 1000 ms (= 1 year
    // in ms). At the boundary, parse_duration_ms must accept.
    // 1 year = 8760 hours exactly.
    let at_cap_str = "+8760h";
    let got =
        parse_duration_ms(at_cap_str).expect("at-cap +8760h (= MAX_SHIFT_OFFSET_MS) must parse");
    assert_eq!(got, MAX_SHIFT_OFFSET_MS);
}

#[test]
fn parse_duration_ms_one_ms_over_max_shift_offset_rejects() {
    // 1 ms over the cap must reject (counter-test paired with
    // the at-cap acceptance test above). Using the smallest
    // ms-precision step above the cap so the boundary is
    // pinned at exactly ±1 ms.
    let over_cap_str = "+31536000001ms"; // MAX_SHIFT_OFFSET_MS + 1
    assert!(
        parse_duration_ms(over_cap_str).is_err(),
        "one ms over MAX_SHIFT_OFFSET_MS must reject"
    );
}

#[test]
fn parse_timestamp_ms_caps_extreme_hours() {
    // 100k hours (~11 years) is the upper bound; above it the cap
    // fires before hours * 3_600_000 can wrap i64.
    assert!(parse_timestamp_ms("100001:00:00").is_err());
    assert!(parse_timestamp_ms("9999999999999:00:00").is_err());
    // Just under the cap still parses cleanly.
    assert_eq!(
        parse_timestamp_ms("100000:00:00").unwrap(),
        100_000_i64 * 3_600_000
    );
}

#[test]
fn rename_dedup_flags_non_no_op_against_no_op_with_same_target() {
    // Concrete repro: row 0 is a no-op (subtitle
    // already correctly named), row 1 wants to rename a different
    // subtitle onto that same target. The dedup must flag the
    // collision so process_rename_pair's --overwrite path doesn't
    // silently destroy row 0's existing file.
    let rows = vec![
        engine::RenamePlanRow {
            input_path: "C:\\Subs\\Episode.ass".to_string(),
            output_path: "C:\\Subs\\Episode.ass".to_string(),
            video_path: "C:\\Subs\\Episode.mkv".to_string(),
            source: "regex".to_string(),
            no_op: true,
            input_conflict: false,
        },
        engine::RenamePlanRow {
            input_path: "C:\\Subs\\episode.tc.ass".to_string(),
            output_path: "C:\\Subs\\Episode.ass".to_string(),
            video_path: "C:\\Subs\\Episode.mkv".to_string(),
            source: "regex".to_string(),
            no_op: false,
            input_conflict: false,
        },
    ];

    let duplicates = duplicate_rename_output_keys(&rows);
    let expected_key = "c:/subs/episode.ass";
    assert!(
        duplicates.contains(expected_key),
        "no-op row's target should be claimed in the seen set"
    );
}

#[test]
fn rename_dedup_allows_distinct_language_suffix_outputs() {
    let rows = vec![
        engine::RenamePlanRow {
            input_path: "C:\\Subs\\episode.sc.ass".to_string(),
            output_path: "C:\\Subs\\Episode.sc.ass".to_string(),
            video_path: "C:\\Subs\\Episode.mkv".to_string(),
            source: "regex".to_string(),
            no_op: false,
            input_conflict: false,
        },
        engine::RenamePlanRow {
            input_path: "C:\\Subs\\episode.tc.ass".to_string(),
            output_path: "C:\\Subs\\Episode.tc.ass".to_string(),
            video_path: "C:\\Subs\\Episode.mkv".to_string(),
            source: "regex".to_string(),
            no_op: false,
            input_conflict: false,
        },
    ];

    let duplicates = duplicate_rename_output_keys(&rows);
    assert!(
        duplicates.is_empty(),
        "distinct canonical language outputs must not be blocked as duplicates"
    );
}

#[test]
fn rename_dedup_flags_canonical_alias_duplicate_outputs() {
    let rows = vec![
        engine::RenamePlanRow {
            input_path: "C:\\Subs\\episode.sc.ass".to_string(),
            output_path: "C:\\Subs\\Episode.sc.ass".to_string(),
            video_path: "C:\\Subs\\Episode.mkv".to_string(),
            source: "regex".to_string(),
            no_op: false,
            input_conflict: false,
        },
        engine::RenamePlanRow {
            input_path: "C:\\Subs\\episode.zh-CN.ass".to_string(),
            output_path: "C:\\Subs\\Episode.sc.ass".to_string(),
            video_path: "C:\\Subs\\Episode.mkv".to_string(),
            source: "regex".to_string(),
            no_op: false,
            input_conflict: false,
        },
    ];

    let duplicates = duplicate_rename_output_keys(&rows);
    let expected_key = "c:/subs/episode.sc.ass";
    assert!(
        duplicates.contains(expected_key),
        "canonical aliases that resolve to the same output must be blocked"
    );
}

#[test]
fn detects_duplicate_rename_outputs_before_writes() {
    let rows = vec![
        engine::RenamePlanRow {
            input_path: "C:\\Subs\\episode.sc.ass".to_string(),
            output_path: "C:\\Subs\\Episode.ass".to_string(),
            video_path: "C:\\Subs\\Episode.mkv".to_string(),
            source: "regex".to_string(),
            no_op: false,
            input_conflict: false,
        },
        engine::RenamePlanRow {
            input_path: "C:\\Subs\\episode.tc.ass".to_string(),
            output_path: "C:\\Subs\\Episode.ass".to_string(),
            video_path: "C:\\Subs\\Episode.mkv".to_string(),
            source: "regex".to_string(),
            no_op: false,
            input_conflict: false,
        },
        engine::RenamePlanRow {
            input_path: "C:\\Subs\\already.ass".to_string(),
            output_path: "C:\\Subs\\Episode.ass".to_string(),
            video_path: "C:\\Subs\\Episode.mkv".to_string(),
            source: "regex".to_string(),
            no_op: true,
            input_conflict: false,
        },
    ];

    let duplicates = duplicate_rename_output_keys(&rows);
    assert_eq!(duplicates.len(), 1);
    let expected_key = "c:/subs/episode.ass";
    assert!(duplicates.contains(expected_key));
}

#[test]
fn rename_dedup_excludes_rows_already_blocked_by_input_conflicts() {
    let rows = vec![
        engine::RenamePlanRow {
            input_path: "C:\\Subs\\unsafe.ass".to_string(),
            output_path: "C:\\Subs\\shared.ass".to_string(),
            video_path: "C:\\Subs\\Unsafe.mkv".to_string(),
            source: "regex".to_string(),
            no_op: false,
            input_conflict: true,
        },
        engine::RenamePlanRow {
            input_path: "C:\\Subs\\safe.ass".to_string(),
            output_path: "C:\\Subs\\shared.ass".to_string(),
            video_path: "C:\\Subs\\Safe.mkv".to_string(),
            source: "regex".to_string(),
            no_op: false,
            input_conflict: false,
        },
    ];

    assert!(duplicate_rename_output_keys(&rows).is_empty());
}

#[test]
fn output_keys_fold_slashes_and_unicode_normalization() {
    let decomposed = normalize_output_key(Path::new("C:\\Subs\\Cafe\u{301}.ass"));
    let precomposed = normalize_output_key(Path::new("C:/Subs/Caf\u{00e9}.ass"));
    assert_eq!(decomposed, precomposed);

    assert_eq!(precomposed, "c:/subs/caf\u{00e9}.ass");
}

#[test]
fn write_output_uses_create_new_and_explicit_overwrite() {
    let globals = test_globals();
    let dir = create_cli_font_db_dir().unwrap();
    let output = dir.join("out.ass");

    fs::write(&output, b"old").unwrap();
    assert!(write_output(&globals, &output, "new", false).is_err());
    assert_eq!(fs::read_to_string(&output).unwrap(), "old");

    write_output(&globals, &output, "new", true).unwrap();
    assert_eq!(fs::read_to_string(&output).unwrap(), "new");

    let _ = fs::remove_file(&output);
    let _ = fs::remove_dir(&dir);
}

#[test]
fn copy_file_output_uses_create_new_and_explicit_overwrite() {
    let globals = test_globals();
    let dir = create_cli_font_db_dir().unwrap();
    let input = dir.join("in.ass");
    let output = dir.join("out.ass");

    fs::write(&input, b"copied").unwrap();
    fs::write(&output, b"old").unwrap();
    assert!(copy_file_output(&globals, &input, &output, false).is_err());
    assert_eq!(fs::read_to_string(&output).unwrap(), "old");

    copy_file_output(&globals, &input, &output, true).unwrap();
    assert_eq!(fs::read_to_string(&output).unwrap(), "copied");

    let _ = fs::remove_file(&input);
    let _ = fs::remove_file(&output);
    let _ = fs::remove_dir(&dir);
}

#[test]
fn relocate_output_path_returns_input_when_no_output_dir() {
    let result = relocate_output_path("C:\\subs\\episode.shifted.ass", None).unwrap();
    assert_eq!(result, PathBuf::from("C:\\subs\\episode.shifted.ass"));
}

#[test]
fn relocate_output_path_joins_filename_with_output_dir() {
    let out_dir = PathBuf::from("D:\\out");
    let result = relocate_output_path("C:\\subs\\episode.shifted.ass", Some(&out_dir)).unwrap();
    assert_eq!(result, out_dir.join("episode.shifted.ass"));
}

#[test]
fn relocate_output_path_rejects_intermediate_trailing_ascii_dot() {
    let out_dir = PathBuf::from(r"D:\real-output.");
    let err = relocate_output_path(r"C:\subs\episode.shifted.ass", Some(&out_dir)).unwrap_err();
    assert!(
        err.contains("directory component ending in an unsupported ASCII dot"),
        "got: {err}"
    );
}

#[test]
fn relocate_output_path_preserves_verbatim_intermediate_trailing_dot() {
    let out_dir = PathBuf::from(r"\\?\C:\real-output.");
    let result = relocate_output_path(r"C:\subs\episode.shifted.ass", Some(&out_dir));
    assert_eq!(result.unwrap(), out_dir.join("episode.shifted.ass"));

    let slash_out_dir = PathBuf::from(r"//?/C:/real-output.");
    let result = relocate_output_path(r"C:\subs\episode.shifted.ass", Some(&slash_out_dir));
    assert_eq!(result.unwrap(), slash_out_dir.join("episode.shifted.ass"));
}

#[test]
fn relocate_output_path_accepts_ramdisk_shaped_output_dirs() {
    let drive_root = PathBuf::from("R:\\");
    let root_result =
        relocate_output_path("C:\\subs\\episode.embed.ass", Some(&drive_root)).unwrap();
    assert_eq!(root_result, drive_root.join("episode.embed.ass"));

    let spaced_cjk_dir = PathBuf::from("R:\\ass out 中文");
    let spaced_result =
        relocate_output_path("C:\\subs\\episode.embed.ass", Some(&spaced_cjk_dir)).unwrap();
    assert_eq!(spaced_result, spaced_cjk_dir.join("episode.embed.ass"));
}

#[test]
fn relocate_output_path_rejects_overlong_relocated_path() {
    // A 300-char path comfortably exceeds the 259-char cap.
    let long_dir_name: String = "a".repeat(300);
    let out_dir = PathBuf::from(format!("C:\\{long_dir_name}"));
    let err = relocate_output_path("C:\\subs\\episode.shifted.ass", Some(&out_dir)).unwrap_err();
    assert!(
        err.contains("relocated output path is too long"),
        "got: {err}"
    );
}

#[test]
fn relocate_output_path_counts_utf16_units_not_utf8_bytes_for_cjk() {
    // A CJK directory path is 200
    // UTF-16 code units (well under 259) but ~600 UTF-8 bytes
    // (over 259). The cap must accept this.
    let cjk_dir: String = "字".repeat(200);
    let out_dir = PathBuf::from(format!("C:\\{cjk_dir}"));
    // 200 + drive prefix + filename ≈ 215 UTF-16 cu — within cap.
    let result = relocate_output_path("C:\\subs\\episode.shifted.ass", Some(&out_dir));
    assert!(
        result.is_ok(),
        "CJK path within UTF-16 cap should pass; got: {result:?}"
    );
}

#[test]
fn relocate_output_path_relaxes_cap_for_long_local_paths() {
    // \\?\ prefix gets the 32766 cap. A 1000-char path exceeds
    // the standard 259 cap but is well under the long-local cap.
    let long_dir_name: String = "a".repeat(1000);
    let out_dir = PathBuf::from(format!("\\\\?\\C:\\{long_dir_name}"));
    let result = relocate_output_path("C:\\subs\\episode.shifted.ass", Some(&out_dir));
    assert!(
        result.is_ok(),
        "long-local path under 32766 cap should pass; got: {result:?}"
    );
}

#[test]
fn cli_font_db_temp_dir_is_create_only_and_cleanup_is_narrow() {
    let dir = create_cli_font_db_dir().unwrap();
    assert!(dir.is_dir());
    assert!(dir
        .file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name.starts_with(super::CLI_FONT_DB_DIR_PREFIX)));

    fs::write(dir.join(USER_FONT_DB_FILENAME), b"db").unwrap();
    fs::write(dir.join(format!("{USER_FONT_DB_FILENAME}-wal")), b"wal").unwrap();
    app_lib::fonts::init_user_font_db(&dir).unwrap();
    let guard = TempFontDbDir(dir.clone());
    drop(guard);

    assert!(!dir.exists());
    let result = app_lib::fonts::resolve_user_font("Arial".to_string(), false, false);
    let Err(error) = result else {
        panic!("dropped CLI temp DB guard should clear the global DB path");
    };
    assert!(
        error.contains("not initialized"),
        "dropped CLI temp DB guard should fail shut after cleanup: {error}"
    );
}

// ── substitute_template — regression coverage ──

#[test]
fn substitute_template_preserves_double_dots_inside_user_content() {
    // Old blanket `replace("..", ".")` mangled this; segment-based
    // substitution keeps user-content `..` intact.
    let got = substitute_template(
        "{name}.shifted{ext}",
        &[("name", "Show..special"), ("ext", ".ass")],
    );
    assert_eq!(got.as_deref(), Some("Show..special.shifted.ass"));
}

#[test]
fn substitute_template_collapses_boundary_double_dots() {
    // Template-side dot + ext-leading dot at the seam → drop one.
    let got = substitute_template("{name}.{ext}", &[("name", "Show"), ("ext", ".ass")]);
    assert_eq!(got.as_deref(), Some("Show.ass"));
}

#[test]
fn substitute_template_collapses_template_literal_dot_runs() {
    // `..` inside the user-typed template (typo) collapses, but
    // user-content `..` would not (covered by the preserve test).
    let got = substitute_template("{name}..shifted{ext}", &[("name", "Show"), ("ext", ".ass")]);
    assert_eq!(got.as_deref(), Some("Show.shifted.ass"));
}

#[test]
fn substitute_template_dollar_in_value_is_literal() {
    // Rust's str::replace is already literal (the TS bug was JS-
    // specific regex backreferences), but pin parity so a future
    // refactor that uses regex-based substitution doesn't regress.
    let got = substitute_template(
        "{name}.shifted{ext}",
        &[("name", "Show$1$&"), ("ext", ".ass")],
    );
    assert_eq!(got.as_deref(), Some("Show$1$&.shifted.ass"));
}

#[test]
fn substitute_template_unknown_token_returns_none() {
    // Unknown token (vars has name + ext, template uses {lang})
    // returns None. Chain parsing now rejects this before runtime,
    // but the helper still fails closed if called directly.
    let got = substitute_template("{name}.{lang}{ext}", &[("name", "Show"), ("ext", ".ass")]);
    assert!(got.is_none());
}

#[test]
fn substitute_template_leaves_unknown_braces_intact() {
    // Token shape doesn't match (uppercase) → kept as literal text.
    // Uppercase `{NAME}` falls through the lexer; downstream the
    // brace gate in `assertSafeOutputFilename` (TS) /
    // predict_chain_output_path's per-char reject (Rust) catches it.
    let got = substitute_template("{NAME}.{ext}", &[("name", "Show"), ("ext", ".ass")]);
    assert_eq!(got.as_deref(), Some("{NAME}.ass"));
}

#[test]
fn substitute_template_token_at_32_char_cap_matches() {
    // Boundary-pin pair (a): 32-char identifier matches the lexer
    // (first char + 31 subsequent chars = 32 total = cap). Vars
    // contain the token → substitutes normally. Pre-fix the lexer
    // was unbounded; this pins the inclusive boundary.
    let long_name = "a".repeat(32);
    let template = format!("{{{long_name}}}.ass");
    let got = substitute_template(&template, &[(long_name.as_str(), "value")]);
    assert_eq!(got.as_deref(), Some("value.ass"));
}

#[test]
fn substitute_template_token_at_32_char_cap_unknown_returns_none() {
    // Boundary-pin pair (b): 32-char identifier matches the lexer
    // and is NOT in vars → unknown-token rejection fires.
    let long_name = "a".repeat(32);
    let template = format!("{{{long_name}}}.ass");
    let got = substitute_template(&template, &[("name", "Show")]);
    assert!(got.is_none());
}

#[test]
fn substitute_template_token_over_cap_falls_through_as_literal() {
    // Boundary-pin pair (c): 33-char identifier exceeds the lexer
    // bound → not matched as a token → stays as literal text.
    // An unbounded lexer would silently collapse this; with the
    // bounded lexer the literal `{aaa...}` survives so the
    // downstream brace-reject path surfaces the failure.
    let long_name = "a".repeat(33);
    let template = format!("{{{long_name}}}.ass");
    let expected = format!("{{{long_name}}}.ass");
    let got = substitute_template(&template, &[("name", "Show")]);
    assert_eq!(got.as_deref(), Some(expected.as_str()));
}

// ── predict_chain_output_path — end-to-end with the fix ──

#[test]
fn predict_chain_output_path_preserves_double_dots() {
    let input = PathBuf::from("/subs/Show..special.ass");
    let predicted = predict_chain_output_path(&input, "{name}.shifted{ext}", None)
        .expect("prediction should produce a path");
    let file_name = predicted
        .file_name()
        .and_then(|n| n.to_str())
        .expect("file_name str");
    assert_eq!(file_name, "Show..special.shifted.ass");
}

// Each test below pins one rejection CODEPOINT-CLASS — the input
// exercises a distinct character / structural class
// (Windows-reserved, superscript-COM, drive-letter, NTFS-illegal,
// control, BiDi, zero-width, U+2028, whitespace-only, `..`,
// empty post-substitution). Several of these classes collapse
// onto the same internal boolean gate inside `predict_chain_output_path`
// (e.g. the matches!()-block control + BiDi + NTFS-illegal arms
// all flip `illegal_in_filename`), so this suite does NOT pin
// "every distinct gate branch" — that's a different invariant
// and isn't covered here. The codepoint-class structure IS what
// changes most often in practice: a refactor that drops a
// superscript variant or relaxes a BiDi codepoint slips past a
// single positive-acceptance test, which is exactly what these
// pins guard against.

#[test]
fn predict_chain_output_path_rejects_windows_reserved_device_name() {
    // {name} = "CON" → predicted name "CON.ass" → Win32 device
    // path. Predictor rejects → V8/TS handles authoritative msg.
    let input = PathBuf::from("/subs/CON.ass");
    let predicted = predict_chain_output_path(&input, "{name}{ext}", None);
    assert!(
        predicted.is_none(),
        "CON.ass should be rejected as a Windows reserved device name; got {predicted:?}"
    );
}

#[test]
fn predict_chain_output_path_rejects_superscript_com_variant() {
    // Unicode superscript COM¹ is a Win32 device-alias on some
    // reparse layers — TS-side `assertSafeOutputFilename`
    // rejects it explicitly; Rust predictor is documented to
    // omit this superset and let V8/TS handle. Pin the predictor
    // returning None (so the V8/TS path is reached).
    let input = PathBuf::from("/subs/COM\u{00B9}.ass");
    let predicted = predict_chain_output_path(&input, "{name}{ext}", None);
    assert!(
        predicted.is_none(),
        "COM¹.ass should be rejected; got {predicted:?}"
    );
}

#[test]
fn predict_chain_output_path_rejects_drive_letter_prefix_in_template() {
    // Template-output that starts with a drive-letter prefix
    // (`C:foo.ass`) is rejected at the predictor — chain output
    // is a single filename in input's directory, never a path.
    let input = PathBuf::from("/subs/Show.ass");
    let predicted = predict_chain_output_path(&input, "C:{name}{ext}", None);
    assert!(
        predicted.is_none(),
        "drive-letter prefix in template should reject; got {predicted:?}"
    );
}

#[test]
fn predict_chain_output_path_rejects_ntfs_illegal_punctuation() {
    // `<`, `>`, `:`, `"`, `|`, `?`, `*` are NTFS-illegal. TS
    // `assertSafeOutputFilename` rejects them; predictor mirrors.
    let input = PathBuf::from("/subs/Show?wild.ass");
    let predicted = predict_chain_output_path(&input, "{name}{ext}", None);
    assert!(
        predicted.is_none(),
        "NTFS-illegal `?` should reject; got {predicted:?}"
    );
}

#[test]
fn predict_chain_output_path_rejects_control_char_in_name() {
    // `\u{001B}` ESC is a C0 control. Predictor must reject;
    // V8 stringification round-trip is unsafe.
    let input = PathBuf::from("/subs/Show\u{001B}.ass");
    let predicted = predict_chain_output_path(&input, "{name}{ext}", None);
    assert!(
        predicted.is_none(),
        "ESC control char should reject; got {predicted:?}"
    );
}

#[test]
fn predict_chain_output_path_rejects_bidi_override_in_name() {
    // U+202E RIGHT-TO-LEFT OVERRIDE — well-known display-flip
    // exploit primitive. TS-side rejects via `unicode-controls`;
    // predictor mirrors.
    let input = PathBuf::from("/subs/evil\u{202E}.ass");
    let predicted = predict_chain_output_path(&input, "{name}{ext}", None);
    assert!(
        predicted.is_none(),
        "U+202E should reject; got {predicted:?}"
    );
}

#[test]
fn predict_chain_output_path_rejects_zero_width_in_name() {
    // U+200B ZERO WIDTH SPACE — invisible-character class.
    let input = PathBuf::from("/subs/Show\u{200B}.ass");
    let predicted = predict_chain_output_path(&input, "{name}{ext}", None);
    assert!(
        predicted.is_none(),
        "U+200B should reject; got {predicted:?}"
    );
}

#[test]
fn predict_chain_output_path_rejects_unicode_line_separator() {
    // U+2028 LINE SEPARATOR — would wrap stderr/println output
    // across lines if it reached display.
    let input = PathBuf::from("/subs/Show\u{2028}.ass");
    let predicted = predict_chain_output_path(&input, "{name}{ext}", None);
    assert!(
        predicted.is_none(),
        "U+2028 should reject; got {predicted:?}"
    );
}

#[test]
fn predict_chain_output_path_rejects_whitespace_only_name() {
    // Template `{name}` with a whitespace-only stem produces
    // a whitespace-only output_name. TS rejects via
    // `!filename.trim()`; predictor mirrors. (Template must
    // omit `{ext}` so the trim-empty branch fires — `"   .ass"`
    // would trim to `".ass"` not empty.)
    let input = PathBuf::from("/subs/   .ass");
    let predicted = predict_chain_output_path(&input, "{name}", None);
    assert!(
        predicted.is_none(),
        "whitespace-only name should reject; got {predicted:?}"
    );
}

#[test]
fn predict_chain_output_path_rejects_bare_double_dot() {
    // `..` as the entire output filename is a parent-dir
    // traversal at the path-resolution layer. Predictor rejects
    // via the `.` / `..` gate documented in the doc-comment.
    let input = PathBuf::from("/subs/Show.ass");
    let predicted = predict_chain_output_path(&input, "..", None);
    assert!(
        predicted.is_none(),
        "bare `..` template should reject; got {predicted:?}"
    );
}

#[test]
fn predict_chain_output_path_abstains_on_trailing_ascii_dot_or_space() {
    let input = PathBuf::from("/subs/Show.ass");
    for output_template in ["{name}{ext}.", "{name}{ext} "] {
        let predicted = predict_chain_output_path(&input, output_template, None);
        assert!(
            predicted.is_none(),
            "Trailing ASCII dot/space template should defer to V8/TS; \
             template={output_template:?}, got {predicted:?}"
        );
    }
}

#[test]
fn predict_chain_output_path_rejects_empty_output_name_after_substitution() {
    // The `output_name.is_empty()` gate at the head of
    // predict_chain_output_path needs explicit coverage: an empty
    // template substitutes to `""` for any input (no tokens to
    // expand). The predictor must reject so the caller falls back
    // to V8 + TS for the authoritative empty-name error — without
    // this test, a refactor that dropped the `.is_empty()` arm
    // would let the cheap-first path build a `parent/` path which
    // `parent.join("")` normalizes back to `parent`, and the
    // existence check would skip the whole batch as "already
    // exists."
    let input = PathBuf::from("/subs/Show.ass");
    let predicted = predict_chain_output_path(&input, "", None);
    assert!(
        predicted.is_none(),
        "empty template should reject; got {predicted:?}"
    );
}

// ── display_path is a PURE FORMATTER (operational) ──

#[test]
fn display_path_preserves_control_chars_for_filesystem_correctness() {
    // display_path's return value is used operationally — as the
    // input to `read_text_detect_encoding_inner` and as the
    // `HdrPathRequest.input_path` field sent to the engine. If a
    // path contains a control char or U+202E, stripping at
    // display_path would make the CLI open a
    // DIFFERENT file (or the same name minus the special char,
    // which may also exist) — a path-confusion bug. Display-time
    // sanitization belongs in `sanitize_for_display` and is
    // applied only at print sites; display_path itself stays
    // operational-pure.
    let evil = PathBuf::from("/subs/evil\u{202e}.ass");
    let got = display_path(&evil);
    assert!(
        got.contains('\u{202e}'),
        "display_path must preserve U+202E for filesystem correctness; got {got:?}"
    );
    let esc = PathBuf::from("/subs/x\u{001b}[2J.ass");
    let got = display_path(&esc);
    assert!(
        got.contains('\u{001b}'),
        "display_path must preserve ESC for filesystem correctness; got {got:?}"
    );
}

#[test]
fn sanitize_for_display_strips_control_and_bidi() {
    // The display-time sanitizer companion to display_path. Used
    // at `emit_file_report` and `format_oversized_caption_warning`
    // to laundering paths before stderr/println interpolation,
    // so a crafted argv filename can't corrupt the terminal.
    assert_eq!(
        sanitize_for_display("/subs/evil\u{202e}.ass"),
        "/subs/evil.ass"
    );
    assert_eq!(
        sanitize_for_display("/subs/x\u{001b}[2J.ass"),
        "/subs/x[2J.ass"
    );
    // Normal paths pass through unchanged.
    assert_eq!(
        sanitize_for_display("/home/u/episode.ass"),
        "/home/u/episode.ass"
    );
}

fn make_resolved_with_font_name(
    label: &str,
    font_name: &str,
    path: &str,
    index: u32,
    codepoints: &[u32],
) -> ResolvedEmbedFont {
    ResolvedEmbedFont {
        label: label.to_string(),
        font_name: font_name.to_string(),
        path: path.to_string(),
        index,
        bold: false,
        italic: false,
        codepoints: codepoints.to_vec(),
    }
}

fn make_resolved_with_style(
    label: &str,
    path: &str,
    index: u32,
    bold: bool,
    italic: bool,
    codepoints: &[u32],
) -> ResolvedEmbedFont {
    ResolvedEmbedFont {
        label: label.to_string(),
        font_name: format!("{label}.ttf"),
        path: path.to_string(),
        index,
        bold,
        italic,
        codepoints: codepoints.to_vec(),
    }
}

fn make_resolved(label: &str, path: &str, index: u32, codepoints: &[u32]) -> ResolvedEmbedFont {
    make_resolved_with_font_name(label, &format!("{label}.ttf"), path, index, codepoints)
}

fn diagnostic_for_resolved(font: &ResolvedEmbedFont) -> FontDiagnostic {
    let usage = engine::FontEmbedUsage {
        family: font.label.clone(),
        bold: font.bold,
        italic: font.italic,
        label: font.label.clone(),
        font_name: font.font_name.clone(),
        glyph_count: font.codepoints.len(),
        codepoints: font.codepoints.clone(),
    };
    let mut diagnostic = FontDiagnostic::new(&usage);
    diagnostic.mark_resolved(font.path.clone(), font.index);
    diagnostic
}

#[test]
fn group_resolved_fonts_collapses_aliases_on_same_face() {
    // Canonical alias case: English + Chinese family names both
    // resolved to msyh.ttc face 0. Pre-fix this produced two
    // byte-identical subset calls embedded under different
    // filenames; post-fix it produces one entry with the union
    // of both aliases' codepoints.
    let aliases = vec![
        make_resolved("Microsoft YaHei", "/fonts/msyh.ttc", 0, &[0x41, 0x42]),
        make_resolved("微软雅黑", "/fonts/msyh.ttc", 0, &[0x4f60, 0x597d]),
    ];
    let groups = group_resolved_fonts_by_face(&aliases);
    assert_eq!(groups.len(), 1, "two aliases on same face must collapse");
    let group = groups.values().next().expect("the single group");
    // Both labels are preserved so subset-failure diagnostics
    // can name every Style affected by a failure, not just
    // first-seen.
    assert_eq!(group.labels.len(), 2);
    assert!(group.labels.contains(&"Microsoft YaHei".to_string()));
    assert!(group.labels.contains(&"微软雅黑".to_string()));
    // Aliases list preserves both insertions in order; the
    // merged codepoints (computed on demand) is the union.
    assert_eq!(group.aliases.len(), 2);
    let merged = group.merged_codepoints();
    assert_eq!(merged, [0x41, 0x42, 0x4f60, 0x597d].into_iter().collect());
    // Template is first-occurrence — drives font_name + path
    // for the subsequent subset_font call.
    assert_eq!(group.template().label, "Microsoft YaHei");
}

#[test]
fn effective_embedded_font_name_matches_dedup_template_for_same_face_aliases() {
    let aliases = vec![
        make_resolved_with_font_name(
            "Microsoft YaHei",
            "microsoft_yahei.ttf",
            "/fonts/msyh.ttc",
            0,
            &[0x41, 0x42],
        ),
        make_resolved_with_font_name(
            "微软雅黑",
            "font_1eda5db2.ttf",
            "/fonts/msyh.ttc",
            0,
            &[0x4f60, 0x597d],
        ),
    ];
    let mut diagnostics: Vec<_> = aliases.iter().map(diagnostic_for_resolved).collect();

    apply_effective_embedded_font_names(&aliases, &mut diagnostics);

    assert_eq!(diagnostics[0].embedded_font_name, "microsoft_yahei.ttf");
    assert_eq!(diagnostics[0].requested_embedded_font_name, None);
    assert_eq!(diagnostics[1].embedded_font_name, "microsoft_yahei.ttf");
    assert_eq!(
        diagnostics[1].requested_embedded_font_name.as_deref(),
        Some("font_1eda5db2.ttf")
    );
}

#[test]
fn effective_embedded_font_name_keeps_alias_labels_for_over_cap_fallback() {
    let aliases = vec![
        make_resolved_range("A", "/fonts/face.ttf", 0x010000, 70_000),
        make_resolved_range("B", "/fonts/face.ttf", 0x020000, 70_000),
        make_resolved_range("C", "/fonts/face.ttf", 0x030000, 70_000),
    ];
    let mut diagnostics: Vec<_> = aliases.iter().map(diagnostic_for_resolved).collect();

    apply_effective_embedded_font_names(&aliases, &mut diagnostics);

    assert_eq!(diagnostics[0].embedded_font_name, "A.ttf");
    assert_eq!(diagnostics[1].embedded_font_name, "B.ttf");
    assert_eq!(diagnostics[2].embedded_font_name, "C.ttf");
    assert!(
        diagnostics
            .iter()
            .all(|diagnostic| diagnostic.requested_embedded_font_name.is_none()),
        "over-cap fallback emits per-alias labels, so diagnostics must not rewrite them"
    );
}

#[test]
fn diagnostic_subset_budget_skips_when_call_cap_is_reached() {
    let mut budget = DiagnosticSubsetBudget {
        calls: MAX_DIAGNOSTIC_SUBSET_CALLS,
        bytes: 0,
        exhausted_reason: None,
    };

    let check = budget
        .begin_call()
        .expect("at-cap budget should skip before another subset call");

    assert_eq!(check.status, FontSubsetCheckStatus::Skipped);
    assert!(
        check
            .error
            .as_deref()
            .is_some_and(|error| error.contains("call budget exceeded")),
        "skip reason should mention call budget: {check:?}"
    );
}

#[test]
fn diagnostic_subset_budget_skips_when_byte_cap_would_be_exceeded() {
    let mut budget = DiagnosticSubsetBudget {
        calls: 0,
        bytes: MAX_DIAGNOSTIC_SUBSET_TOTAL_BYTES - 1,
        exhausted_reason: None,
    };

    budget.finish_bytes(2);
    let check = budget
        .begin_call()
        .expect("later checks should skip after byte budget exhaustion");

    assert_eq!(check.status, FontSubsetCheckStatus::Skipped);
    assert!(
        check
            .error
            .as_deref()
            .is_some_and(|error| error.contains("byte budget exceeded")),
        "skip reason should mention byte budget: {check:?}"
    );
}

#[test]
fn subset_check_budget_is_shared_across_diagnostic_files() {
    let first = vec![make_resolved("First", "/fonts/first.ttf", 0, &[0x41])];
    let second = vec![make_resolved("Second", "/fonts/second.ttf", 0, &[0x42])];
    let mut first_diagnostics: Vec<_> = first.iter().map(diagnostic_for_resolved).collect();
    let mut second_diagnostics: Vec<_> = second.iter().map(diagnostic_for_resolved).collect();
    let mut budget = DiagnosticSubsetBudget {
        calls: MAX_DIAGNOSTIC_SUBSET_CALLS,
        bytes: 0,
        exhausted_reason: None,
    };
    let mut emitted = false;

    let first_warnings = apply_subset_checks_to_diagnostics(
        &first,
        &mut first_diagnostics,
        &mut budget,
        &mut emitted,
    );
    let second_warnings = apply_subset_checks_to_diagnostics(
        &second,
        &mut second_diagnostics,
        &mut budget,
        &mut emitted,
    );

    assert_eq!(first_warnings.len(), 1);
    assert!(
        first_warnings[0].contains("call budget exceeded"),
        "first file should surface the command-level budget cap: {first_warnings:?}"
    );
    assert!(
        second_warnings.is_empty(),
        "shared warning gate should avoid one warning per input file"
    );
    for diagnostic in first_diagnostics.iter().chain(second_diagnostics.iter()) {
        let check = diagnostic
            .subset_check
            .as_ref()
            .expect("resolved diagnostics should receive a subset-check result");
        assert_eq!(check.status, FontSubsetCheckStatus::Skipped);
        assert!(
            check
                .error
                .as_deref()
                .is_some_and(|error| error.contains("call budget exceeded")),
            "all skipped diagnostics should cite the shared cap: {check:?}"
        );
    }
}

#[test]
fn group_resolved_fonts_keeps_distinct_face_indices() {
    // Same TTC file, different face_index — these are genuinely
    // distinct faces (face 0 = Microsoft YaHei, face 1 =
    // Microsoft YaHei UI inside msyh.ttc) and must NOT collapse.
    // If a future bug keys dedup on just `path` without `index`,
    // this test catches the regression.
    let inputs = vec![
        make_resolved("Microsoft YaHei", "/fonts/msyh.ttc", 0, &[0x41]),
        make_resolved("Microsoft YaHei UI", "/fonts/msyh.ttc", 1, &[0x42]),
    ];
    let groups = group_resolved_fonts_by_face(&inputs);
    assert_eq!(groups.len(), 2);
    // Both face indices present as map keys.
    let indices: std::collections::BTreeSet<u32> = groups.keys().map(|(_, i, _, _)| *i).collect();
    assert_eq!(indices, [0u32, 1u32].into_iter().collect());
}

#[test]
fn group_resolved_fonts_keeps_style_flags_distinct_on_same_face() {
    let inputs = vec![
        make_resolved_with_style(
            "Face Regular",
            "/fonts/family.ttc",
            0,
            false,
            false,
            &[0x41],
        ),
        make_resolved_with_style("Face Bold", "/fonts/family.ttc", 0, true, false, &[0x42]),
        make_resolved_with_style("Face Italic", "/fonts/family.ttc", 0, false, true, &[0x43]),
    ];

    let groups = group_resolved_fonts_by_face(&inputs);

    assert_eq!(
        groups.len(),
        3,
        "same path/index with different ASS style flags must not dedup"
    );
    let styles: std::collections::BTreeSet<(bool, bool)> = groups
        .keys()
        .map(|(_, _, bold, italic)| (*bold, *italic))
        .collect();
    assert_eq!(
        styles,
        [(false, false), (true, false), (false, true)]
            .into_iter()
            .collect()
    );
}

#[test]
fn group_resolved_fonts_deduplicates_repeated_label_for_same_face() {
    // Defensive: if the same label appears multiple times for
    // the same face (shouldn't happen in practice — resolve_embed_fonts
    // emits one ResolvedEmbedFont per ASS-referenced family — but
    // a future refactor could let it through), the labels list
    // must NOT accumulate duplicates. Codepoints still union.
    let inputs = vec![
        make_resolved("Arial", "/fonts/arial.ttf", 0, &[0x41]),
        make_resolved("Arial", "/fonts/arial.ttf", 0, &[0x42]),
    ];
    let groups = group_resolved_fonts_by_face(&inputs);
    assert_eq!(groups.len(), 1);
    let group = groups.values().next().expect("single group");
    assert_eq!(group.labels, vec!["Arial".to_string()]);
    // Aliases list keeps both insertions (labels dedup, aliases don't);
    // the union is still the same set.
    assert_eq!(group.aliases.len(), 2);
    let merged = group.merged_codepoints();
    assert_eq!(merged, [0x41u32, 0x42u32].into_iter().collect());
}

#[test]
fn group_resolved_fonts_preserves_template_codepoints_for_singleton() {
    // Singleton input (one ResolvedEmbedFont, no aliases) — the
    // helper still goes through the BTreeMap path; verify the
    // codepoint set survives intact and labels has exactly one
    // entry. Boundary test for "doesn't break the common case
    // where dedup is a no-op."
    let inputs = vec![make_resolved(
        "Roboto",
        "/fonts/roboto.ttf",
        0,
        &[0x30, 0x31, 0x32],
    )];
    let groups = group_resolved_fonts_by_face(&inputs);
    assert_eq!(groups.len(), 1);
    let group = groups.values().next().expect("single group");
    assert_eq!(group.labels, vec!["Roboto".to_string()]);
    assert_eq!(group.aliases.len(), 1);
    let merged: Vec<u32> = group.merged_codepoints().into_iter().collect();
    assert_eq!(merged, vec![0x30, 0x31, 0x32]);
}

#[test]
fn group_resolved_fonts_empty_input_yields_empty_map() {
    let groups = group_resolved_fonts_by_face(&[]);
    assert_eq!(groups.len(), 0);
}

// ── Boundary-pin for the dedup-vs-fallback cap ──

fn make_resolved_range(label: &str, path: &str, start: u32, count: u32) -> ResolvedEmbedFont {
    let codepoints: Vec<u32> = (0..count).map(|i| start + i).collect();
    ResolvedEmbedFont {
        label: label.to_string(),
        font_name: format!("{label}.ttf"),
        path: path.to_string(),
        index: 0,
        bold: false,
        italic: false,
        codepoints,
    }
}

#[test]
fn merged_codepoints_at_cap_stays_within_subset_limit() {
    // 4 aliases × 50,000 disjoint codepoints each = 200,000
    // exactly at MAX_SUBSET_CODEPOINTS_FOR_DEDUP. The dedup
    // decision uses `> cap`, so at-cap takes the dedup path —
    // one subset_font call with the unioned set. This pins the
    // boundary: a regression that flipped the comparison to
    // `>=` would route at-cap groups to the fallback path
    // unnecessarily and lose the dedup win for inputs that
    // legitimately fit the cap.
    let aliases = vec![
        make_resolved_range("A", "/fonts/face.ttf", 0x010000, 50_000),
        make_resolved_range("B", "/fonts/face.ttf", 0x020000, 50_000),
        make_resolved_range("C", "/fonts/face.ttf", 0x030000, 50_000),
        make_resolved_range("D", "/fonts/face.ttf", 0x040000, 50_000),
    ];
    let groups = group_resolved_fonts_by_face(&aliases);
    assert_eq!(groups.len(), 1);
    let group = groups.values().next().expect("single group");
    let merged = group
        .merged_codepoints_with_cap(MAX_SUBSET_CODEPOINTS_FOR_DEDUP)
        .expect("at-cap union should stay under cap");
    assert_eq!(merged.len(), 200_000);
    assert!(merged.len() <= MAX_SUBSET_CODEPOINTS_FOR_DEDUP);
}

#[test]
fn merged_codepoints_over_cap_signals_fallback() {
    // 4 aliases × 50,001 disjoint codepoints each = 200,004,
    // but the bounded merge must stop at cap+1. The actual
    // subset_font fallback can't be unit-tested without the IPC-
    // bypass setup that the env-var-gated integration test
    // (subset_with_index_handles_ttc) already covers. This test
    // guards both the early-stop resource bound and the contract
    // that per-alias detail survives for fallback.
    let aliases = vec![
        make_resolved_range("A", "/fonts/face.ttf", 0x010000, 50_001),
        make_resolved_range("B", "/fonts/face.ttf", 0x020000, 50_001),
        make_resolved_range("C", "/fonts/face.ttf", 0x030000, 50_001),
        make_resolved_range("D", "/fonts/face.ttf", 0x040000, 50_001),
    ];
    let groups = group_resolved_fonts_by_face(&aliases);
    assert_eq!(groups.len(), 1);
    let group = groups.values().next().expect("single group");
    let over_cap_len = group
        .merged_codepoints_with_cap(MAX_SUBSET_CODEPOINTS_FOR_DEDUP)
        .expect_err("over-cap union should signal fallback");
    assert_eq!(
        over_cap_len,
        MAX_SUBSET_CODEPOINTS_FOR_DEDUP + 1,
        "over-cap union should stop as soon as fallback is known"
    );
    // Aliases preserved so subset_resolved_fonts can iterate
    // them for the per-alias fallback path. Each alias's
    // codepoint count stays under the cap individually.
    assert_eq!(group.aliases.len(), 4);
    for alias in &group.aliases {
        assert!(
            alias.codepoints.len() <= MAX_SUBSET_CODEPOINTS_FOR_DEDUP,
            "fallback subset call must stay within the cap"
        );
    }
}

#[test]
fn build_rs_missing_engine_stub_covers_all_probed_engine_methods() {
    let build_rs = include_str!("../../../build.rs");
    for method in engine::REQUIRED_ENGINE_METHODS {
        let needle = format!("\"{method}\"");
        assert!(
            build_rs.contains(&needle),
            "build.rs missing_engine_stub FUNCTIONS must include {needle} to match engine.rs probe"
        );
    }
}

#[test]
fn parse_timestamp_ms_rejects_trailing_decimal_or_comma_separator() {
    assert_eq!(parse_timestamp_ms("00:01:02").unwrap(), 62_000);
    assert_eq!(parse_timestamp_ms("00:01:02.5").unwrap(), 62_500);
    assert_eq!(parse_timestamp_ms("00:01:02,05").unwrap(), 62_050);
    assert!(
        parse_timestamp_ms("00:01:02.").is_err(),
        "trailing dot with empty fractional part must be rejected"
    );
    assert!(
        parse_timestamp_ms("00:01:02,").is_err(),
        "trailing comma with empty fractional part must be rejected"
    );
}
