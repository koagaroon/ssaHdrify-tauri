//! `embed` and `diagnose-fonts` commands plus shared font resolution,
//! alias-group deduplication, and budgeted subsetting.

use super::*;
use std::collections::HashSet;
use std::path::Path;
use std::process::ExitCode;

pub(super) struct FontLookupOutcome {
    pub(super) found: Option<(String, u32)>,
    pub(super) error: Option<String>,
    pub(super) diagnostic: FontDiagnostic,
}

pub(super) struct ResolveEmbedFontsOutcome {
    pub(super) resolved: Vec<ResolvedEmbedFont>,
    pub(super) warnings: Vec<String>,
    pub(super) diagnostics: Vec<FontDiagnostic>,
}

#[derive(Debug)]
pub(super) struct ResolveEmbedFontsError {
    pub(super) error: String,
    pub(super) diagnostics: Vec<FontDiagnostic>,
}

pub(super) type EmbedFileOutcome = (FileReport, Vec<FontDiagnostic>);

pub(super) struct ResolvedEmbedFont {
    pub(super) label: String,
    pub(super) font_name: String,
    pub(super) path: String,
    pub(super) index: u32,
    pub(super) bold: bool,
    pub(super) italic: bool,
    pub(super) codepoints: Vec<u32>,
}

pub(super) fn run_embed(
    globals: &GlobalOptions,
    args: EmbedArgs,
    diagnose: Option<DiagnoseMode>,
) -> Result<ExitCode, String> {
    app_lib::fonts::init_system_dirs();
    let use_user_fonts = args.has_user_font_sources();
    // Skip the user-font scan in dry-run mode. process_embed_file's
    // cheap-first ordering returns Planned BEFORE reading content or
    // resolving fonts, so dry-run never needs the SQLite source index.
    // Saves a large directory scan when the user passes an explicit
    // font source alongside --dry-run just to preview output paths.
    let _font_db_dir = if use_user_fonts && !globals.dry_run {
        Some(init_cli_font_sources(globals, &args)?)
    } else {
        None
    };

    // Open the persistent font cache per the locked design.
    // Sequence:
    //   1. If --no-cache, skip outright.
    //   2. If --dry-run, skip — cheap-first ordering doesn't reach the
    //      font-resolution step under dry-run, so cache I/O would be
    //      wasted work.
    //   3. Resolve cache path; if file doesn't exist, no cache for
    //      this run (announce, but no fallback to declare).
    //   4. Open. If schema version mismatch, surface as drift-equiv
    //      and fall back to no-cache for this run.
    //   5. Drift-check by listing cached folders + stat()-ing each.
    //      If drift detected, verbose stderr report, fall back to
    //      no-cache for this run, suggest `refresh-fonts`.
    let prepared_cache = prepare_font_cache_for_resolution(globals, &args, true, false)?;
    let cache = prepared_cache.cache.as_ref();

    let mut engine = engine::CliEngine::new()?;
    let output_dir = globals
        .output_dir
        .as_deref()
        .map(absolute_path)
        .transpose()?;
    let mut report = CommandReport::new("embed");
    let selected_inputs = selected_input_keys(&args.files);
    let collect_font_diagnostics = diagnose.is_some();
    let mut font_diagnostics = Vec::new();
    // Same first-wins dedup policy as run_hdr. Embed already orders
    // dedup correctly (cheap plan_font_embed → dedup → expensive
    // subset+apply), so no JS work is wasted on duplicate batches.
    let mut seen_outputs = HashSet::new();

    for (idx, file) in args.files.iter().enumerate() {
        let (result, mut diagnostics) = process_embed_file(
            globals,
            &args,
            use_user_fonts,
            cache,
            collect_font_diagnostics,
            output_dir.as_deref(),
            &mut engine,
            file,
            &mut seen_outputs,
            &selected_inputs,
        );
        if collect_font_diagnostics {
            for diagnostic in &mut diagnostics {
                diagnostic.file = Some(result.input.clone());
            }
            font_diagnostics.append(&mut diagnostics);
        }
        let failed = result.status == FileStatus::Failed;
        emit_file_report(globals, &result);
        report.push(result);
        if globals.fail_fast && failed {
            let remaining = args.files.len().saturating_sub(idx + 1);
            emit_fail_fast_abort_notice(globals, remaining);
            report.mark_fail_fast_abort();
            break;
        }
    }

    finish_command_report(
        globals,
        &mut report,
        diagnose,
        font_diagnostics,
        Some(prepared_cache.diagnostic),
    )?;
    Ok(report.exit_code())
}

pub(super) fn run_diagnose_fonts(
    globals: &GlobalOptions,
    args: DiagnoseFontsArgs,
) -> Result<ExitCode, String> {
    validate_diagnose_fonts_globals(globals)?;
    app_lib::fonts::init_system_dirs();
    let embed_args = args.to_embed_args();
    let use_user_fonts = embed_args.has_user_font_sources();
    let _font_db_dir = if use_user_fonts {
        Some(init_cli_font_sources(globals, &embed_args)?)
    } else {
        None
    };
    let prepared_cache = prepare_font_cache_for_resolution(globals, &embed_args, false, true)?;
    let cache = prepared_cache.cache.as_ref();

    let mut engine = engine::CliEngine::new()?;
    let mut report = CommandReport::new("diagnose-fonts");
    let mut font_diagnostics = Vec::new();
    let mut subset_budget = DiagnosticSubsetBudget::default();
    let mut subset_budget_warning_emitted = false;

    let mut diagnose_context = DiagnoseFontContext {
        globals,
        args: &embed_args,
        use_user_fonts,
        cache,
        engine: &mut engine,
        subset_check: args.subset_check,
        subset_budget: &mut subset_budget,
        subset_budget_warning_emitted: &mut subset_budget_warning_emitted,
    };

    for file in &embed_args.files {
        let (result, mut diagnostics) = diagnose_font_file(&mut diagnose_context, file);
        for diagnostic in &mut diagnostics {
            diagnostic.file = Some(result.input.clone());
        }
        font_diagnostics.append(&mut diagnostics);
        report.push(result);
    }

    let diagnostics = build_command_diagnostics(
        &report,
        DiagnoseMode::Full,
        font_diagnostics,
        Some(prepared_cache.diagnostic),
    );

    if globals.json {
        let json = serde_json::to_string_pretty(&diagnostics)
            .map_err(|err| format!("failed to encode JSON diagnostics: {err}"))?;
        println!("{json}");
    } else {
        emit_standalone_font_diagnostics(globals, &diagnostics);
    }

    Ok(report.exit_code())
}

pub(super) struct DiagnoseFontContext<'a> {
    pub(super) globals: &'a GlobalOptions,
    pub(super) args: &'a EmbedArgs,
    pub(super) use_user_fonts: bool,
    pub(super) cache: Option<&'a app_lib::font_cache::FontCache>,
    pub(super) engine: &'a mut engine::CliEngine,
    pub(super) subset_check: bool,
    pub(super) subset_budget: &'a mut DiagnosticSubsetBudget,
    pub(super) subset_budget_warning_emitted: &'a mut bool,
}

pub(super) fn validate_diagnose_fonts_globals(globals: &GlobalOptions) -> Result<(), String> {
    if globals.output_dir.is_some() {
        return Err(
            "diagnose-fonts is read-only and does not write subtitle outputs; remove --output-dir"
                .to_string(),
        );
    }
    if globals.overwrite {
        return Err(
            "diagnose-fonts is read-only and does not write subtitle outputs; remove --overwrite"
                .to_string(),
        );
    }
    if globals.dry_run {
        return Err("diagnose-fonts is already read-only; remove --dry-run".to_string());
    }
    if globals.fail_fast {
        return Err(
            "diagnose-fonts reports every input it can inspect; remove --fail-fast".to_string(),
        );
    }
    Ok(())
}

pub(super) fn diagnose_font_file(
    ctx: &mut DiagnoseFontContext<'_>,
    file: &Path,
) -> EmbedFileOutcome {
    let input_path = match absolute_path(file) {
        Ok(path) => path,
        Err(error) => return (failed_report(file, None, None, error), Vec::new()),
    };
    let input = display_path(&input_path);

    if !has_ass_extension(&input_path) {
        return (
            failed_report(
                &input_path,
                None,
                None,
                "font diagnostics only supports ASS/SSA subtitle files".to_string(),
            ),
            Vec::new(),
        );
    }

    let read_result = match app_lib::encoding::read_text_detect_encoding_inner(&input, |_| true) {
        Ok(result) => result,
        Err(error) => return (failed_report(&input_path, None, None, error), Vec::new()),
    };
    let decode_warnings = format_decode_warnings(ctx.globals, &read_result);

    let plan_request = engine::FontDiagnosticsPlanRequest {
        content: read_result.text,
    };
    let plan = match ctx.engine.plan_font_diagnostics(&plan_request) {
        Ok(result) => result,
        Err(error) => {
            let mut report = failed_report(&input_path, None, Some(read_result.encoding), error);
            report.warnings = decode_warnings;
            return (report, Vec::new());
        }
    };

    match resolve_embed_fonts(
        ctx.globals,
        ctx.args,
        ctx.use_user_fonts,
        ctx.cache,
        true,
        &plan.fonts,
    ) {
        Ok(outcome) => {
            let mut diagnostics = outcome.diagnostics;
            let mut warnings = decode_warnings.unwrap_or_default();
            warnings.extend(outcome.warnings);
            if ctx.subset_check {
                warnings.extend(apply_subset_checks_to_diagnostics(
                    &outcome.resolved,
                    &mut diagnostics,
                    ctx.subset_budget,
                    ctx.subset_budget_warning_emitted,
                ));
            }
            (
                FileReport {
                    input,
                    output: None,
                    encoding: Some(read_result.encoding),
                    status: FileStatus::Diagnosed,
                    error: None,
                    warnings: if warnings.is_empty() {
                        None
                    } else {
                        Some(warnings)
                    },
                },
                diagnostics,
            )
        }
        Err(error) => {
            let mut report =
                failed_report(&input_path, None, Some(read_result.encoding), error.error);
            report.warnings = decode_warnings;
            (report, error.diagnostics)
        }
    }
}

pub(super) fn apply_subset_checks_to_diagnostics(
    resolved: &[ResolvedEmbedFont],
    diagnostics: &mut [FontDiagnostic],
    budget: &mut DiagnosticSubsetBudget,
    budget_warning_emitted: &mut bool,
) -> Vec<String> {
    for diagnostic in diagnostics.iter_mut() {
        if diagnostic.result != FontResolutionResult::Resolved {
            diagnostic.subset_check = Some(FontSubsetCheckDiagnostic {
                status: FontSubsetCheckStatus::Skipped,
                bytes: None,
                error: Some("font was not resolved".to_string()),
            });
        }
    }

    let mut warnings = Vec::new();
    let face_groups = group_resolved_fonts_by_face(resolved);
    for group in face_groups.values() {
        let merged = match group.merged_codepoints_with_cap(MAX_SUBSET_CODEPOINTS_FOR_DEDUP) {
            Ok(merged) => merged,
            Err(_) => {
                for alias in &group.aliases {
                    let check = run_one_subset_check(alias, budget);
                    if check.status == FontSubsetCheckStatus::Failed {
                        if let Some(error) = &check.error {
                            warnings.push(format!(
                                "font subset check failed: {} ({error})",
                                alias.label
                            ));
                        }
                    }
                    push_subset_budget_warning_once(&mut warnings, budget, budget_warning_emitted);
                    set_subset_check_for_resolved_font(diagnostics, alias, check);
                }
                continue;
            }
        };

        let codepoints: Vec<u32> = merged.into_iter().collect();
        let template = group.template();
        let check =
            run_budgeted_subset_check(template.path.clone(), template.index, codepoints, budget);
        if check.status == FontSubsetCheckStatus::Failed {
            if let Some(error) = &check.error {
                warnings.push(format!(
                    "font subset check failed: {} ({error})",
                    group.labels.join(" / ")
                ));
            }
        }
        push_subset_budget_warning_once(&mut warnings, budget, budget_warning_emitted);

        for alias in &group.aliases {
            set_subset_check_for_resolved_font(diagnostics, alias, check.clone());
        }
    }

    warnings
}

pub(super) fn run_budgeted_subset_check(
    path: String,
    index: u32,
    codepoints: Vec<u32>,
    budget: &mut DiagnosticSubsetBudget,
) -> FontSubsetCheckDiagnostic {
    if let Some(skipped) = budget.begin_call() {
        return skipped;
    }
    match app_lib::fonts::subset_font(path, index, codepoints) {
        Ok(data) => {
            let len = data.len();
            budget.finish_bytes(len);
            FontSubsetCheckDiagnostic {
                status: FontSubsetCheckStatus::Ok,
                bytes: Some(len),
                error: None,
            }
        }
        Err(error) => FontSubsetCheckDiagnostic {
            status: FontSubsetCheckStatus::Failed,
            bytes: None,
            error: Some(error),
        },
    }
}

pub(super) fn run_one_subset_check(
    font: &ResolvedEmbedFont,
    budget: &mut DiagnosticSubsetBudget,
) -> FontSubsetCheckDiagnostic {
    run_budgeted_subset_check(
        font.path.clone(),
        font.index,
        font.codepoints.clone(),
        budget,
    )
}

pub(super) fn push_subset_budget_warning_once(
    warnings: &mut Vec<String>,
    budget: &DiagnosticSubsetBudget,
    emitted: &mut bool,
) {
    if *emitted {
        return;
    }
    let Some(error) = &budget.exhausted_reason else {
        return;
    };
    warnings.push(format!("font subset check budget exhausted: {error}"));
    *emitted = true;
}

pub(super) fn set_subset_check_for_resolved_font(
    diagnostics: &mut [FontDiagnostic],
    font: &ResolvedEmbedFont,
    check: FontSubsetCheckDiagnostic,
) {
    if let Some(diagnostic) = diagnostics.iter_mut().find(|diagnostic| {
        let requested_name = diagnostic
            .requested_embedded_font_name
            .as_deref()
            .unwrap_or(diagnostic.embedded_font_name.as_str());
        diagnostic.subset_check.is_none()
            && diagnostic.label == font.label
            && requested_name == font.font_name
            && diagnostic.path.as_deref() == Some(font.path.as_str())
            && diagnostic.index == Some(font.index)
            && diagnostic.bold == font.bold
            && diagnostic.italic == font.italic
    }) {
        diagnostic.subset_check = Some(check);
    }
}

pub(super) fn format_subset_check_summary(check: &FontSubsetCheckDiagnostic) -> String {
    match check.status {
        FontSubsetCheckStatus::Ok => match check.bytes {
            Some(bytes) => format!("ok ({bytes} bytes)"),
            None => "ok".to_string(),
        },
        FontSubsetCheckStatus::Failed => match &check.error {
            Some(error) => format!("failed ({})", sanitize_for_display(error)),
            None => "failed".to_string(),
        },
        FontSubsetCheckStatus::Skipped => match &check.error {
            Some(error) => format!("skipped ({})", sanitize_for_display(error)),
            None => "skipped".to_string(),
        },
    }
}

// 9 args: globals + args + use_user_fonts + cache + diagnostic flag +
// output_dir + engine + file + seen_outputs. The cache and use_user_fonts
// could be folded into a per-run state struct, but the existing run_embed
// already passes them as parallel locals; bundling here would just shift
// the boilerplate. Allowing this one lint locally.
#[allow(clippy::too_many_arguments)]
pub(super) fn process_embed_file(
    globals: &GlobalOptions,
    args: &EmbedArgs,
    use_user_fonts: bool,
    cache: Option<&app_lib::font_cache::FontCache>,
    collect_font_diagnostics: bool,
    output_dir: Option<&Path>,
    engine: &mut engine::CliEngine,
    file: &Path,
    seen_outputs: &mut HashSet<String>,
    selected_inputs: &HashSet<String>,
) -> EmbedFileOutcome {
    let input_path = match absolute_path(file) {
        Ok(path) => path,
        Err(error) => return (failed_report(file, None, None, error), Vec::new()),
    };
    let input = display_path(&input_path);

    if !has_ass_extension(&input_path) {
        return (
            failed_report(
                &input_path,
                None,
                None,
                "font embed only supports ASS/SSA subtitle files".to_string(),
            ),
            Vec::new(),
        );
    }

    // Cheap-first ordering (mirrors process_hdr_file). Resolve output
    // path BEFORE reading content or running plan_font_embed (which
    // parses the entire ASS via ass-compiler — non-trivial cost on
    // large files). Saves the read + parse + V8 round-trip on dedup,
    // exists-skip, and dry-run paths. Both resolve_embed_output_path
    // and plan_font_embed route through the same JS resolveOutputPath
    // helper with identical defaults, so the resolved path is byte-
    // identical to what plan_font_embed would have returned.
    let path_request = engine::EmbedPathRequest {
        input_path: input.clone(),
        output_template: args.output_template.clone(),
    };
    let resolved_output_path = match engine.resolve_embed_output_path(&path_request) {
        Ok(path) => path,
        Err(error) => return (failed_report(&input_path, None, None, error), Vec::new()),
    };

    let output_path = match relocate_output_path(&resolved_output_path, output_dir) {
        Ok(path) => path,
        Err(error) => return (failed_report(&input_path, None, None, error), Vec::new()),
    };
    let output = display_path(&output_path);

    let mut reservation = match reserve_output(
        globals,
        &input_path,
        &output_path,
        &output,
        None,
        seen_outputs,
        selected_inputs,
    ) {
        Ok(reservation) => reservation,
        Err(early) => return (*early, Vec::new()),
    };

    if globals.dry_run {
        // Dry-run for embed reports the planned output path without
        // doing font discovery or content parsing — matches HDR/Shift
        // dry-run behavior and avoids the surprise of "dry-run scanned
        // 17k fonts then planned no actual write."
        reservation.keep();
        return (planned_report(&input_path, Some(output), None), Vec::new());
    }

    let read_result = match app_lib::encoding::read_text_detect_encoding_inner(&input, |_| true) {
        Ok(result) => result,
        Err(error) => {
            return (
                failed_report(&input_path, Some(output), None, error),
                Vec::new(),
            )
        }
    };
    let mut warnings = format_decode_warnings(globals, &read_result).unwrap_or_default();

    let plan_request = engine::FontEmbedPlanRequest {
        input_path: input.clone(),
        content: read_result.text.clone(),
        output_template: args.output_template.clone(),
    };
    let plan = match engine.plan_font_embed(&plan_request) {
        Ok(result) => result,
        Err(error) => {
            let mut report =
                failed_report(&input_path, Some(output), Some(read_result.encoding), error);
            if !warnings.is_empty() {
                report.warnings = Some(warnings);
            }
            return (report, Vec::new());
        }
    };

    let mut font_diagnostics = Vec::new();
    let resolved_fonts = match resolve_embed_fonts(
        globals,
        args,
        use_user_fonts,
        cache,
        collect_font_diagnostics,
        &plan.fonts,
    ) {
        Ok(mut outcome) => {
            warnings.append(&mut outcome.warnings);
            font_diagnostics.append(&mut outcome.diagnostics);
            outcome.resolved
        }
        Err(mut error) => {
            font_diagnostics.append(&mut error.diagnostics);
            let mut report = failed_report(
                &input_path,
                Some(output),
                Some(read_result.encoding),
                error.error,
            );
            if !warnings.is_empty() {
                report.warnings = Some(warnings);
            }
            return (report, font_diagnostics);
        }
    };

    let glyph_count: usize = plan.fonts.iter().map(|font| font.glyph_count).sum();
    let referenced = plan.fonts.len();
    let resolved_count = resolved_fonts.len();
    emit_verbose(
        globals,
        format!(
            "embed: {referenced} referenced fonts ({glyph_count} glyphs), {resolved_count} resolved"
        ),
        format!(
            "字体嵌入：{referenced} 个引用字体（{glyph_count} 个字符），{resolved_count} 个已解析"
        ),
    );

    // Subset / apply / write Err paths must attach the accumulated
    // `warnings` vec (resolve_warnings, plus subset_warnings on the
    // apply/write paths) to the FileReport. A bare
    // `failed_report(..., error)` sets `warnings: None`, so any
    // missing-font / subset-failure diagnostics already gathered would
    // be silently dropped. The helper closure mirrors the shape used
    // for the standalone shift early-returns; one allocation per
    // early path is acceptable (these are failure paths, not the hot
    // path).
    let attach_warnings = |mut report: FileReport, warnings: &Vec<String>| -> FileReport {
        if !warnings.is_empty() {
            report.warnings = Some(warnings.clone());
        }
        report
    };

    let subset_payloads = match subset_resolved_fonts(globals, args, &resolved_fonts) {
        Ok((payloads, mut subset_warnings)) => {
            warnings.append(&mut subset_warnings);
            payloads
        }
        Err(error) => {
            return (
                attach_warnings(
                    failed_report(&input_path, Some(output), Some(read_result.encoding), error),
                    &warnings,
                ),
                font_diagnostics,
            );
        }
    };

    let applied = if subset_payloads.is_empty() {
        // No fonts left to embed (all referenced fonts missing under
        // --on-missing warn). Skip the V8 round-trip — applyFontEmbed
        // JS-side does the same short-circuit, but avoiding the call
        // saves work on batches with many no-resolve files.
        engine::FontEmbedApplyResult {
            content: read_result.text,
            embedded_count: 0,
        }
    } else {
        let total_subset_bytes: usize = subset_payloads.iter().map(|s| s.data.len()).sum();
        if total_subset_bytes > MAX_EMBED_SUBSET_TOTAL_BYTES {
            return (
                attach_warnings(
                    failed_report(
                        &input_path,
                        Some(output),
                        Some(read_result.encoding),
                        format!(
                            "embed subsets total {total_subset_bytes} bytes exceeds the \
                             {MAX_EMBED_SUBSET_TOTAL_BYTES}-byte cap; reduce per-input font \
                             count or split the subtitle before embedding"
                        ),
                    ),
                    &warnings,
                ),
                font_diagnostics,
            );
        }

        let apply_request = engine::FontEmbedApplyRequest {
            content: read_result.text,
            fonts: subset_payloads,
        };
        match engine.apply_font_embed(&apply_request) {
            Ok(result) => result,
            Err(error) => {
                return (
                    attach_warnings(
                        failed_report(&input_path, Some(output), Some(read_result.encoding), error),
                        &warnings,
                    ),
                    font_diagnostics,
                );
            }
        }
    };

    let n = applied.embedded_count;
    emit_verbose(
        globals,
        format!("embed: {n} fonts embedded"),
        format!("字体嵌入：{n} 个字体已嵌入"),
    );

    if let Err(error) = write_output(globals, &output_path, &applied.content, globals.overwrite) {
        return (
            attach_warnings(
                failed_report(&input_path, Some(output), Some(read_result.encoding), error),
                &warnings,
            ),
            font_diagnostics,
        );
    }

    reservation.keep();

    (
        FileReport {
            input,
            output: Some(output),
            encoding: Some(read_result.encoding),
            status: FileStatus::Written,
            error: None,
            warnings: if warnings.is_empty() {
                None
            } else {
                Some(warnings)
            },
        },
        font_diagnostics,
    )
}

/// Pre-resolve fonts for an embed step in a chain. Reuses the same
/// plan_font_embed → resolve_embed_fonts → subset_resolved_fonts
/// pipeline as the standalone `embed` subcommand. Returns the
/// subset payloads ready for injection into the chain's runtime
/// payload.
///
/// HDR/Shift do not modify [V4+ Styles] Fontname or dialogue \fn
/// references, so planning against the original input content is
/// safe — we get the same font list as if we'd planned against the
/// post-HDR/Shift content. This lets the chain runtime stay
/// synchronous (no async TS→Rust callbacks mid-chain).
///
/// Return shape: Ok = (subsets, warnings); Err = (error, partial_warnings)
/// — partial_warnings carries diagnostics collected before the failing
/// step, so the caller can still surface them through ChainFileOutcome.
///
/// **Reachability note**: under current control flow the
/// partial_warnings Vec is always empty on the Err path.
/// `resolve_embed_fonts` Errs only under `--on-missing fail` when ANY
/// font is missing, BEFORE constructing missing_warnings;
/// `subset_resolved_fonts` Errs only under fail mode AFTER subset
/// failures, but at that point any missing-font would already have
/// Err'd in resolve. So the only Err arms that fire have empty
/// missing_warnings to propagate. The field is kept for architectural
/// consistency with `ChainFileOutcome::Failed/Skipped(_, vec)` — a
/// future Err path addition (e.g., `resolve_embed_fonts` surfacing a
/// non-final Err with partial missing_warnings) won't silently lose
/// them.
pub(super) type ChainEmbedSubsetsResult =
    Result<(Vec<engine::FontSubsetPayload>, Vec<String>), (String, Vec<String>)>;

pub(super) fn resolve_chain_embed_subsets(
    engine: &mut engine::CliEngine,
    globals: &GlobalOptions,
    embed_args: &EmbedArgs,
    input_path: &str,
    content: &str,
) -> ChainEmbedSubsetsResult {
    let use_user_fonts = embed_args.has_user_font_sources();

    // output_template is unused at the chain level (the chain-global
    // template wins) but plan_font_embed expects one. The default
    // satisfies the schema; the returned outputPath gets ignored.
    let plan_request = engine::FontEmbedPlanRequest {
        input_path: input_path.to_string(),
        content: content.to_string(),
        output_template: "{name}.embed.ass".to_string(),
    };
    // Err arm now carries (error, partial_warnings)
    // so missing_warnings collected from resolve_embed_fonts aren't
    // silently dropped when a downstream step Errs. plan_font_embed +
    // resolve_embed_fonts Err paths have no partial warnings (their
    // Err strings carry the relevant context themselves);
    // subset_resolved_fonts Err path can lose missing_warnings already
    // gathered from resolve_embed_fonts. Tuple Err type instead of a
    // named struct because this function has a single caller
    // (process_one_chain_input) and the partial-warnings shape is local.
    let plan_result = engine
        .plan_font_embed(&plan_request)
        .map_err(|e| (e, Vec::new()))?;

    // Chain's embed step doesn't use the persistent cache (yet) — chain
    // pre-resolution runs against the input content with whatever
    // explicit font sources the embed step itself was given. Cache integration
    // for chain is a future expansion; for now, pass None.
    //
    // Propagate both warning lists to the caller so chain mode and
    // standalone embed produce equivalent diagnostics. Standalone embed
    // surfaces these as FileReport.warnings; chain wraps them into
    // ChainFileOutcome::Written(_, warnings).
    let resolved_outcome = resolve_embed_fonts(
        globals,
        embed_args,
        use_user_fonts,
        None,
        false,
        &plan_result.fonts,
    )
    .map_err(|e| (e.error, Vec::new()))?;
    let (subsets, skipped_warnings) =
        match subset_resolved_fonts(globals, embed_args, &resolved_outcome.resolved) {
            Ok(result) => result,
            Err(e) => return Err((e, resolved_outcome.warnings)),
        };
    let mut warnings = resolved_outcome.warnings;
    warnings.extend(skipped_warnings);
    Ok((subsets, warnings))
}

/// Resolve fonts; under `--on-missing warn`, returns the resolved
/// list and missing-font warnings. Detailed `FontDiagnostic` rows are
/// collected only when the caller requested diagnostics; ordinary
/// embed/chain runs must not retain batch-wide diagnostic objects.
/// Under `--on-missing fail`, returns Err on any missing font.
pub(super) fn resolve_embed_fonts(
    globals: &GlobalOptions,
    args: &EmbedArgs,
    use_user_fonts: bool,
    cache: Option<&app_lib::font_cache::FontCache>,
    collect_diagnostics: bool,
    fonts: &[engine::FontEmbedUsage],
) -> Result<ResolveEmbedFontsOutcome, ResolveEmbedFontsError> {
    let mut resolved = Vec::new();
    let mut missing = Vec::new();
    let mut diagnostics = Vec::new();

    for font in fonts {
        // Cap font.codepoints BEFORE the lookup + clone into
        // ResolvedEmbedFont. `font` flows
        // from V8/TS-parsed ASS (attacker-influenced), so a crafted
        // subtitle declaring a million codepoints per font would
        // retain a 4 MB Vec<u32> per ResolvedEmbedFont entry until
        // subset_font runs — multiplied across many fonts. subset_font's
        // own MAX_SUBSET_CODEPOINTS (200_000) refuses the actual subset
        // call; this earlier cap bounds the intermediate retention.
        // Treat over-cap as missing/skipped so it flows through the
        // existing --on-missing surface (warning + counted in skipped
        // / Failed under `fail`).
        if font.codepoints.len() > MAX_RESOLVED_FONT_CODEPOINTS {
            let error = format!(
                "too many codepoints: {} > cap {}",
                font.codepoints.len(),
                MAX_RESOLVED_FONT_CODEPOINTS
            );
            if collect_diagnostics {
                let mut diagnostic = FontDiagnostic::new(font);
                diagnostic.mark_error(error.clone());
                diagnostics.push(diagnostic);
            }
            missing.push(format!(
                "{} (too many codepoints: {} > cap {})",
                font.label,
                font.codepoints.len(),
                MAX_RESOLVED_FONT_CODEPOINTS
            ));
            continue;
        }
        let lookup = resolve_embed_font(args, use_user_fonts, cache, font);
        let (path, index) = match (lookup.found.clone(), lookup.error.clone()) {
            (Some(found), _) => found,
            (None, None) => {
                missing.push(font.label.clone());
                if collect_diagnostics {
                    diagnostics.push(lookup.diagnostic);
                }
                continue;
            }
            (None, Some(error)) => {
                missing.push(format!("{} ({error})", font.label));
                if collect_diagnostics {
                    diagnostics.push(lookup.diagnostic);
                }
                continue;
            }
        };
        if collect_diagnostics {
            diagnostics.push(lookup.diagnostic);
        }

        resolved.push(ResolvedEmbedFont {
            label: font.label.clone(),
            font_name: font.font_name.clone(),
            path,
            index,
            bold: font.bold,
            italic: font.italic,
            codepoints: font.codepoints.clone(),
        });
    }

    if !missing.is_empty() {
        let joined = missing.join(", ");
        emit_verbose_err(
            globals,
            format!("embed: missing/skipped fonts: {joined}"),
            format!("字体嵌入：缺失/跳过的字体：{joined}"),
        );
        if args.on_missing == MissingFontAction::Fail {
            return Err(ResolveEmbedFontsError {
                error: format!("missing/skipped fonts: {joined}"),
                diagnostics,
            });
        }
    }

    if collect_diagnostics {
        apply_effective_embedded_font_names(&resolved, &mut diagnostics);
    }

    let warnings = missing
        .into_iter()
        .map(|m| format!("missing font: {m}"))
        .collect();
    Ok(ResolveEmbedFontsOutcome {
        resolved,
        warnings,
        diagnostics,
    })
}

pub(super) fn apply_effective_embedded_font_names(
    resolved: &[ResolvedEmbedFont],
    diagnostics: &mut [FontDiagnostic],
) {
    let face_groups = group_resolved_fonts_by_face(resolved);
    for group in face_groups.values() {
        if group
            .merged_codepoints_with_cap(MAX_SUBSET_CODEPOINTS_FOR_DEDUP)
            .is_err()
        {
            continue;
        }
        let effective_name = group.template().font_name.as_str();
        for alias in &group.aliases {
            if let Some(diagnostic) = diagnostics.iter_mut().find(|diagnostic| {
                diagnostic.requested_embedded_font_name.is_none()
                    && diagnostic.label == alias.label
                    && diagnostic.embedded_font_name == alias.font_name
                    && diagnostic.path.as_deref() == Some(alias.path.as_str())
                    && diagnostic.index == Some(alias.index)
                    && diagnostic.bold == alias.bold
                    && diagnostic.italic == alias.italic
            }) {
                diagnostic.mark_effective_embedded_font_name(effective_name);
            }
        }
    }
}

/// Subset fonts. Under `--on-missing warn`, returns the payloads
/// that successfully subset AND a list of skipped-font diagnostics
/// for inclusion in `FileReport.warnings`. Under `--on-missing fail`,
/// returns Err on the first subset-failure batch (any font in
/// `fonts` failing fontcull parse / subsetting), so the caller
/// upgrades the file's status to Failed and the batch's exit code
/// to non-zero. With `--fail-fast` on top, the batch also
/// short-circuits at the per-file boundary.
///
/// Dedup by resolved `(path, index, bold, italic)` face/style tuple:
/// family aliases that
/// resolve to the same face (e.g., the English `Microsoft YaHei`
/// and Chinese `微软雅黑` both pointing at `msyh.ttc` face 0)
/// otherwise subset twice and embed byte-identical payloads under
/// different `fontname:` filenames. The preserved name-table records
/// in `app_lib::fonts::subset_with_index` (every name ID, every
/// language) let libass match every original family alias to the
/// single deduped entry. `font-embedder.ts` has the parallel TS
/// dedup for the GUI's standalone embed path; this Rust dedup
/// serves the CLI + chain paths which call subset_font directly
/// without routing through `embedFonts()`.
/// Output of `group_resolved_fonts_by_face`: one entry per unique
/// resolved face. `aliases` holds every ResolvedEmbedFont that
/// resolved to this `(path, index, bold, italic)` tuple (in insertion order, so
/// `aliases[0]` is the first-seen alias and serves as the dedup
/// template — its `font_name` drives the eventual `[Fonts]` entry
/// filename in the dedup-happy path). `labels` is the deduplicated
/// list of alias labels for failure-diagnostic strings that name
/// every Style affected by a subset failure (not just the
/// first-seen one).
///
/// Per-alias entries are preserved (instead of eagerly unioning
/// codepoints) so the main-pass fallback can subset each alias
/// separately when the merged-union codepoint count would exceed
/// the downstream `subset_font` cap. See
/// `MAX_SUBSET_CODEPOINTS_FOR_DEDUP` in `subset_resolved_fonts`.
pub(super) struct ResolvedFaceGroup<'a> {
    pub(super) aliases: Vec<&'a ResolvedEmbedFont>,
    pub(super) labels: Vec<String>,
}

impl<'a> ResolvedFaceGroup<'a> {
    /// First-seen alias — its `font_name` / `path` / `index` drive
    /// the dedup-happy-path subset call. Group is non-empty by
    /// construction of `group_resolved_fonts_by_face` (a group is
    /// only inserted when there's at least one alias to seed it).
    pub(super) fn template(&self) -> &'a ResolvedEmbedFont {
        self.aliases[0]
    }

    /// Lazy union of every alias's codepoints. Returns `Err(len)` as
    /// soon as the merged set exceeds `cap`, so the fallback decision
    /// does not allocate/work through the rest of a hostile over-cap
    /// alias union.
    pub(super) fn merged_codepoints_with_cap(
        &self,
        cap: usize,
    ) -> Result<std::collections::BTreeSet<u32>, usize> {
        let mut set: std::collections::BTreeSet<u32> = std::collections::BTreeSet::new();
        for alias in &self.aliases {
            for codepoint in &alias.codepoints {
                set.insert(*codepoint);
                if set.len() > cap {
                    return Err(set.len());
                }
            }
        }
        Ok(set)
    }

    #[cfg(test)]
    pub(super) fn merged_codepoints(&self) -> std::collections::BTreeSet<u32> {
        self.merged_codepoints_with_cap(usize::MAX)
            .expect("usize::MAX cap cannot be exceeded")
    }
}

/// Group resolved fonts by `(path, index, bold, italic)` so family
/// aliases that resolve to the same underlying face/style produce one subset call
/// instead of N byte-identical ones. Pure helper extracted for
/// unit-test access; see the test module for boundary coverage
/// (single alias, two aliases collapse, TTC face 0 vs 1 stay
/// distinct, style flags stay distinct, label list preserves every
/// alias for diagnostics).
///
/// BTreeMap keeps deterministic iteration so `[Fonts]` entries are
/// stable across runs on the same input — easier to diff outputs
/// when investigating embed differences across versions.
pub(super) fn group_resolved_fonts_by_face(
    fonts: &[ResolvedEmbedFont],
) -> std::collections::BTreeMap<(String, u32, bool, bool), ResolvedFaceGroup<'_>> {
    let mut face_groups: std::collections::BTreeMap<(String, u32, bool, bool), ResolvedFaceGroup> =
        std::collections::BTreeMap::new();
    for font in fonts {
        let key = (font.path.clone(), font.index, font.bold, font.italic);
        match face_groups.get_mut(&key) {
            Some(group) => {
                group.aliases.push(font);
                if !group.labels.contains(&font.label) {
                    group.labels.push(font.label.clone());
                }
            }
            None => {
                face_groups.insert(
                    key,
                    ResolvedFaceGroup {
                        aliases: vec![font],
                        labels: vec![font.label.clone()],
                    },
                );
            }
        }
    }
    face_groups
}

/// MUST equal `app_lib::fonts::MAX_SUBSET_CODEPOINTS` (the per-call
/// codepoint cap in `subset_font`, also 200,000). The dedup decision
/// below checks the merged-union size against this cap BEFORE
/// calling subset_font; if the union would overflow, we fall back to
/// per-alias subsetting for that group only — the dedup byte-
/// reduction win is given up for the cap-busting case, the per-call
/// defense-in-depth stays at 200k for every individual `subset_font`
/// call. The TS sibling in `font-embedder.ts` (named
/// `MAX_SUBSET_CODEPOINTS_FOR_DEDUP`) tracks the same value; its WHY
/// comment names fonts.rs as the source of truth.
///
/// CLI lens: `subset_font` is an in-process Rust call from this
/// binary, not an IPC boundary. The "IPC cap" framing only applies
/// on the GUI / Tauri path where `subset_font_bytes` returns raw bytes.
///
/// Cross-language drift defense: `dedup_cap_matches_ipc_cap` in
/// `mod tests` pins the equality with
/// `app_lib::fonts::MAX_SUBSET_CODEPOINTS` so a unilateral bump of
/// the subset cap (the only realistic regression shape — TS↔Rust
/// drift surfaces at the user-facing dedup decision) fails the test
/// instead of shipping silently.
pub(super) const MAX_SUBSET_CODEPOINTS_FOR_DEDUP: usize = 200_000;

pub(super) fn checked_subset_total(current: usize, additional: usize) -> Result<usize, String> {
    current
        .checked_add(additional)
        .filter(|total| *total <= MAX_V8_SUBSET_TOTAL_BYTES)
        .ok_or_else(|| format!(
            "font subsets exceed the {MAX_V8_SUBSET_TOTAL_BYTES}-byte total cap; reduce per-input font count or split the input"
        ))
}

pub(super) fn subset_resolved_fonts(
    globals: &GlobalOptions,
    args: &EmbedArgs,
    fonts: &[ResolvedEmbedFont],
) -> Result<(Vec<engine::FontSubsetPayload>, Vec<String>), String> {
    let face_groups = group_resolved_fonts_by_face(fonts);

    let mut payloads = Vec::new();
    let mut skipped = Vec::new();
    let mut total_bytes = 0;

    for group in face_groups.values() {
        let merged = match group.merged_codepoints_with_cap(MAX_SUBSET_CODEPOINTS_FOR_DEDUP) {
            Ok(merged) => merged,
            Err(_) => {
                // Cap-busting fallback: subset each alias independently.
                // Each alias's codepoints is bounded by
                // `MAX_RESOLVED_FONT_CODEPOINTS` (100,000) upstream in
                // `resolve_embed_fonts`, which is strictly less than the
                // subset cap, so individual subset calls always pass.
                // Output for this face reverts to pre-dedup shape (one
                // [Fonts] entry per alias, byte-identical payloads under
                // different filenames); the dedup byte-reduction win is
                // given up only for this specific group. The face's
                // family-name records are still preserved at the subset
                // layer, so libass's per-glyph fallback can traverse the
                // N entries to find any requested glyph.
                for alias in &group.aliases {
                    match app_lib::fonts::subset_font(
                        alias.path.clone(),
                        alias.index,
                        alias.codepoints.clone(),
                    ) {
                        Ok(data) => {
                            total_bytes = checked_subset_total(total_bytes, data.len())?;
                            payloads.push(engine::FontSubsetPayload {
                                font_name: alias.font_name.clone(),
                                data,
                            });
                        }
                        Err(error) => {
                            skipped.push(format!("{} ({error})", alias.label));
                        }
                    }
                }
                continue;
            }
        };

        let codepoints: Vec<u32> = merged.into_iter().collect();
        let template = group.template();
        match app_lib::fonts::subset_font(template.path.clone(), template.index, codepoints) {
            Ok(data) => {
                total_bytes = checked_subset_total(total_bytes, data.len())?;
                payloads.push(engine::FontSubsetPayload {
                    font_name: template.font_name.clone(),
                    data,
                });
            }
            Err(error) => {
                // Surface every alias that hit this face — the user
                // needs to know all the Styles that lost their font,
                // not just the first-seen family name.
                let aliases = group.labels.join(" / ");
                skipped.push(format!("{aliases} ({error})"));
            }
        }
    }

    if !skipped.is_empty() {
        let joined = skipped.join(", ");
        emit_verbose_err(
            globals,
            format!("embed: skipped fonts: {joined}"),
            format!("字体嵌入：跳过的字体：{joined}"),
        );
        if args.on_missing == MissingFontAction::Fail {
            return Err(format!("skipped fonts: {joined}"));
        }
    }

    let warnings = skipped
        .into_iter()
        .map(|s| format!("font subset failed: {s}"))
        .collect();
    Ok((payloads, warnings))
}

pub(super) fn resolve_embed_font(
    args: &EmbedArgs,
    use_user_fonts: bool,
    cache: Option<&app_lib::font_cache::FontCache>,
    font: &engine::FontEmbedUsage,
) -> FontLookupOutcome {
    let mut diagnostic = FontDiagnostic::new(font);

    // validate font.family once upfront.
    // The GUI sibling `font_cache_commands::lookup_font_family`
    // validates at the IPC boundary; the CLI's resolve_embed_font is
    // the equivalent boundary on the CLI side (font.family flows from
    // TS-engine V8-extracted ASS `\fn` content, which is attacker-
    // influenced). Without this upfront validation, tier-2
    // `c.lookup_family` would be the odd tier — tier-1
    // `resolve_user_font` and tier-3 `find_system_font` validate
    // internally, but a cache row keyed by a hostile name could
    // resolve and flow into `register_cache_provenance` (which
    // validates path but not family). Validating here keeps the trust
    // boundary uniform across all three tiers; the tier-1 / tier-3
    // internal validate calls become redundant but stay as
    // defense-in-depth.
    if let Err(error) = app_lib::util::validate_font_family(&font.family) {
        diagnostic.mark_error(error.clone());
        return FontLookupOutcome {
            found: None,
            error: Some(error),
            diagnostic,
        };
    }

    // Lookup tier 1: session DB populated by explicit font sources for THIS run
    // (Situation A's explicit "merge in these dirs" inputs).
    if use_user_fonts {
        match app_lib::fonts::resolve_user_font(font.family.clone(), font.bold, font.italic) {
            Ok(Some(found)) => {
                diagnostic.add_tier(
                    FontResolveTier::Local,
                    FontTierStatus::Hit,
                    Some(found.path.clone()),
                    Some(found.index),
                    None,
                );
                diagnostic.mark_resolved(found.path.clone(), found.index);
                return FontLookupOutcome {
                    found: Some((found.path, found.index)),
                    error: None,
                    diagnostic,
                };
            }
            Ok(None) => {
                diagnostic.add_tier(
                    FontResolveTier::Local,
                    FontTierStatus::Miss,
                    None,
                    None,
                    None,
                );
            }
            Err(error) => {
                diagnostic.add_tier(
                    FontResolveTier::Local,
                    FontTierStatus::Error,
                    None,
                    None,
                    Some(error.clone()),
                );
                diagnostic.mark_error(error.clone());
                return FontLookupOutcome {
                    found: None,
                    error: Some(error),
                    diagnostic,
                };
            }
        }
    } else {
        diagnostic.add_tier(
            FontResolveTier::Local,
            FontTierStatus::Disabled,
            None,
            None,
            Some(
                "no --font-dir, --recursive-font-dir, or --font-file source was provided"
                    .to_string(),
            ),
        );
    }

    // Lookup tier 2: persistent cache. Implements Situation A's
    // "merge with cache" semantic (when an explicit source is also provided,
    // cache fills in fonts the user didn't explicitly hand) and
    // Situation B's "implicit cache use" (when no explicit source, cache
    // is the primary source). Cache is None when --no-cache is set,
    // when the cache file doesn't exist, or when drift detection
    // fell us back to no-cache for this run.
    if let Some(c) = cache {
        match c.lookup_family(&font.family, font.bold, font.italic) {
            Ok(Some(result)) => {
                // Register the cache hit in the in-process provenance
                // set so subset_font's gate accepts the returned path.
                // Without this, the cache-hit path breaks: cache
                // returns the path, subset_font rejects as "Font path
                // was not discovered by a scan command".
                //
                // registration failure (BiDi /
                // control char / `..` in a hostile cache row) →
                // fall through to system fonts rather than returning
                // the unsafe path. `register_cache_provenance` runs
                // `validate_ipc_path`; previously we returned
                // `Ok(Some((font_path, face_index)))` after the WARN,
                // letting the crafted path flow into verbose logs /
                // FileReport.warnings / stderr (Trojan-Source dialog
                // injection surface) before subset_font's re-validation
                // could refuse it. Same shape as the GUI fix at
                // font_cache_commands::lookup_font_family.
                // register takes `&FontLookupResult`
                // directly — the type's pub(crate) fields restrict
                // construction to `FontCache::lookup_family`, so this
                // call enforces "only lookup hits register" at the
                // type layer. `into_parts` extracts the owned tuple
                // for the return value and rejects negative
                // face_index via try_from.
                match app_lib::fonts::register_cache_provenance(&result) {
                    Ok(()) => match result.into_parts() {
                        Ok((path, index)) => {
                            diagnostic.add_tier(
                                FontResolveTier::Cache,
                                FontTierStatus::Hit,
                                Some(path.clone()),
                                Some(index),
                                None,
                            );
                            diagnostic.mark_resolved(path.clone(), index);
                            return FontLookupOutcome {
                                found: Some((path, index)),
                                error: None,
                                diagnostic,
                            };
                        }
                        Err(e) => {
                            diagnostic.add_tier(
                                FontResolveTier::Cache,
                                FontTierStatus::Error,
                                None,
                                None,
                                Some(format!("malformed cache result: {e}")),
                            );
                            log::warn!(
                                "Font '{}' cache lookup returned a malformed result; \
                                 falling back to system fonts: {e}",
                                font.family
                            );
                        }
                    },
                    Err(e) => {
                        diagnostic.add_tier(
                            FontResolveTier::Cache,
                            FontTierStatus::Error,
                            None,
                            None,
                            Some(format!("cache path failed provenance validation: {e}")),
                        );
                        log::warn!(
                            "Font '{}' cache lookup hit a path that failed provenance \
                             validation; falling back to system fonts: {e}",
                            font.family
                        );
                        // Intentional fall-through to the system-font
                        // tier below.
                    }
                }
            }
            Ok(None) => {
                diagnostic.add_tier(
                    FontResolveTier::Cache,
                    FontTierStatus::Miss,
                    None,
                    None,
                    None,
                );
                // Cache miss; fall through to system fonts.
            }
            Err(e) => {
                diagnostic.add_tier(
                    FontResolveTier::Cache,
                    FontTierStatus::Error,
                    None,
                    None,
                    Some(e.to_string()),
                );
                // Cache read error; log but don't fail the whole
                // embed — fall through to system fonts.
                log::warn!("font cache lookup failed for {}: {e}", font.family);
            }
        }
    } else {
        diagnostic.add_tier(
            FontResolveTier::Cache,
            FontTierStatus::Unavailable,
            None,
            None,
            Some("no usable cache for this run".to_string()),
        );
    }

    if args.no_system_fonts {
        diagnostic.add_tier(
            FontResolveTier::System,
            FontTierStatus::Disabled,
            None,
            None,
            Some("--no-system-fonts was set".to_string()),
        );
        return FontLookupOutcome {
            found: None,
            error: None,
            diagnostic,
        };
    }

    match app_lib::fonts::find_system_font(font.family.clone(), font.bold, font.italic) {
        Ok(found) => {
            diagnostic.add_tier(
                FontResolveTier::System,
                FontTierStatus::Hit,
                Some(found.path.clone()),
                Some(found.index),
                None,
            );
            diagnostic.mark_resolved(found.path.clone(), found.index);
            FontLookupOutcome {
                found: Some((found.path, found.index)),
                error: None,
                diagnostic,
            }
        }
        Err(error) => {
            // String-coupled to fonts.rs's `format!("Font not found: ...)`.
            // Any change to that prefix in fonts.rs MUST update this
            // matcher; otherwise a "miss" becomes a hard Err and breaks
            // --on-missing warn semantics. fonts.rs has the matching
            // WHY comment at the format-string site.
            if error.starts_with("Font not found:") {
                diagnostic.add_tier(
                    FontResolveTier::System,
                    FontTierStatus::Miss,
                    None,
                    None,
                    Some(error),
                );
                FontLookupOutcome {
                    found: None,
                    error: None,
                    diagnostic,
                }
            } else {
                diagnostic.add_tier(
                    FontResolveTier::System,
                    FontTierStatus::Error,
                    None,
                    None,
                    Some(error.clone()),
                );
                diagnostic.mark_error(error.clone());
                FontLookupOutcome {
                    found: None,
                    error: Some(error),
                    diagnostic,
                }
            }
        }
    }
}
