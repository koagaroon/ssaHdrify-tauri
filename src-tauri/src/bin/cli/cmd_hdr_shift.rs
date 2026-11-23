//! `hdr` and `shift` (including `--map` timing maps) subcommand execution.

use super::*;
use std::collections::HashSet;
use std::fs;
use std::path::Path;
use std::process::ExitCode;

pub(super) struct ShiftProcessContext<'a> {
    pub(super) offset_ms: i64,
    pub(super) threshold_ms: Option<i64>,
    pub(super) timing_map_rules: Option<Vec<engine::TimingMapRule>>,
    pub(super) output_dir: Option<&'a Path>,
    pub(super) selected_input_keys: &'a HashSet<String>,
}

pub(super) fn run_hdr(
    globals: &GlobalOptions,
    args: HdrArgs,
    diagnose: Option<DiagnoseMode>,
) -> Result<ExitCode, String> {
    let mut engine = engine::CliEngine::new()?;
    let output_dir = globals
        .output_dir
        .as_deref()
        .map(absolute_path)
        .transpose()?;
    let mut report = CommandReport::new("hdr");
    let selected_inputs = selected_input_keys(&args.files);
    // First-input-wins dedup is intentional for non-destructive
    // transformations (HDR/Shift/Embed): the second input's work is
    // wasted but no source data is lost. Rename takes the opposite
    // all-fail policy (see duplicate_rename_output_keys) because
    // picking a "winner" there could move the wrong file.
    let mut seen_outputs = HashSet::new();

    for (idx, file) in args.files.iter().enumerate() {
        let result = process_hdr_file(
            globals,
            &args,
            output_dir.as_deref(),
            &mut engine,
            file,
            &mut seen_outputs,
            &selected_inputs,
        );
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

    finish_command_report(globals, &mut report, diagnose, Vec::new(), None)?;
    Ok(report.exit_code())
}

pub(super) fn process_hdr_file(
    globals: &GlobalOptions,
    args: &HdrArgs,
    output_dir: Option<&Path>,
    engine: &mut engine::CliEngine,
    file: &Path,
    seen_outputs: &mut HashSet<String>,
    selected_inputs: &HashSet<String>,
) -> FileReport {
    let input_path = match absolute_path(file) {
        Ok(path) => path,
        Err(error) => {
            return failed_report(file, None, None, error);
        }
    };
    let input = display_path(&input_path);

    // Cheap-first ordering: resolve the output path before reading
    // content or running the heavy convert_hdr. Lets dedup and
    // exists-check skip duplicate-output and already-existing-target
    // batches without paying the V8 conversion cost. Both
    // resolve_hdr_output_path and convert_hdr route through the same
    // JS resolveOutputPath helper, so the resolved path is
    // byte-identical to what convert_hdr would have returned.
    let path_request = engine::HdrPathRequest {
        input_path: input.clone(),
        eotf: args.eotf.as_engine_value().to_string(),
        output_template: args.output_template.clone(),
    };
    let resolved_output_path = match engine.resolve_hdr_output_path(&path_request) {
        Ok(path) => path,
        Err(error) => return failed_report(&input_path, None, None, error),
    };

    let output_path = match relocate_output_path(&resolved_output_path, output_dir) {
        Ok(path) => path,
        Err(error) => return failed_report(&input_path, None, None, error),
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
        Err(early) => return *early,
    };

    if globals.dry_run {
        // Dry-run gates BEFORE the read so cheap-first lives up to its
        // name on `--dry-run` invocations: no I/O, no V8 work, just the
        // resolved path. encoding is None because we haven't read —
        // matches the cheap-first contract (Embed already does this).
        reservation.keep();
        return planned_report(&input_path, Some(output), None);
    }

    let read_result = match app_lib::encoding::read_text_detect_encoding_inner(&input, |_| true) {
        Ok(result) => result,
        Err(error) => return failed_report(&input_path, Some(output), None, error),
    };
    let mut warnings = format_decode_warnings(globals, &read_result);

    let request = engine::HdrConversionRequest {
        input_path: input.clone(),
        content: read_result.text,
        eotf: args.eotf.as_engine_value().to_string(),
        brightness: args.nits,
        output_template: args.output_template.clone(),
    };

    let conversion = match engine.convert_hdr(&request) {
        Ok(result) => result,
        Err(error) => {
            let mut report =
                failed_report(&input_path, Some(output), Some(read_result.encoding), error);
            report.warnings = warnings;
            return report;
        }
    };

    if let Some(messages) = format_oversized_caption_warning(
        globals,
        conversion.skipped_count,
        &input,
        OversizedCaptionHandling::Dropped,
    ) {
        warnings.get_or_insert_with(Vec::new).extend(messages);
    }

    // (sibling): attach warnings to the
    // failed_report on write_output failure so the oversized-caption
    // diagnostic isn't silently lost. Same fix shape as
    // process_shift_file_heavy_first's three early-return paths.
    if let Err(error) = write_output(
        globals,
        &output_path,
        &conversion.content,
        globals.overwrite,
    ) {
        let mut report =
            failed_report(&input_path, Some(output), Some(read_result.encoding), error);
        report.warnings = warnings;
        return report;
    }

    reservation.keep();

    FileReport {
        input,
        output: Some(output),
        encoding: Some(read_result.encoding),
        status: FileStatus::Written,
        error: None,
        warnings,
    }
}

pub(super) fn read_timing_map_text(map_path: &Path) -> Result<String, String> {
    let absolute = absolute_path(map_path)?;
    let display = display_path(&absolute);
    let metadata = fs::metadata(&absolute)
        .map_err(|err| format!("failed to read timing map metadata for {display}: {err}"))?;
    if !metadata.is_file() {
        return Err(format!("timing map path is not a regular file: {display}"));
    }
    if metadata.len() > MAX_TIMING_MAP_BYTES {
        return Err(format!(
            "timing map file is too large: {} bytes (max {MAX_TIMING_MAP_BYTES})",
            metadata.len()
        ));
    }
    fs::read_to_string(&absolute)
        .map_err(|err| format!("failed to read timing map as UTF-8 text from {display}: {err}"))
}

pub(super) fn require_enabled_timing_map_rules(
    rules: Vec<engine::TimingMapRule>,
) -> Result<Vec<engine::TimingMapRule>, String> {
    if rules.is_empty() {
        return Err("timing map contains no rules".to_string());
    }
    if !rules.iter().any(|rule| rule.enabled.unwrap_or(true)) {
        return Err("timing map contains no enabled rules".to_string());
    }
    Ok(rules)
}

pub(super) fn load_timing_map_rules(
    engine: &mut engine::CliEngine,
    map_path: &Path,
) -> Result<Vec<engine::TimingMapRule>, String> {
    let content = read_timing_map_text(map_path)?;
    let parsed = engine.parse_timing_map(&engine::TimingMapParseRequest { content })?;
    require_enabled_timing_map_rules(parsed.rules)
}

pub(super) fn run_shift(
    globals: &GlobalOptions,
    args: ShiftArgs,
    diagnose: Option<DiagnoseMode>,
) -> Result<ExitCode, String> {
    let offset_ms = match args.offset.as_deref() {
        Some(offset) => parse_duration_ms(offset)?,
        None => 0,
    };
    let threshold_ms = args.after.as_deref().map(parse_timestamp_ms).transpose()?;
    let mut engine = engine::CliEngine::new()?;
    let timing_map_rules = args
        .map
        .as_deref()
        .map(|path| load_timing_map_rules(&mut engine, path))
        .transpose()?;
    let output_dir = globals
        .output_dir
        .as_deref()
        .map(absolute_path)
        .transpose()?;
    let mut report = CommandReport::new("shift");
    let selected_inputs = selected_input_keys(&args.files);
    // Same first-wins dedup policy as run_hdr. Shift now does
    // cheap-first dedup for the common case (default template, no
    // `{format}` token). Templates that reference `{format}` need
    // parsing content to resolve and fall back to heavy-first ordering
    // inside process_shift_file_heavy_first.
    let mut seen_outputs = HashSet::new();

    for (idx, file) in args.files.iter().enumerate() {
        let result = process_shift_file(
            globals,
            &args,
            &ShiftProcessContext {
                offset_ms,
                threshold_ms,
                timing_map_rules: timing_map_rules.clone(),
                output_dir: output_dir.as_deref(),
                selected_input_keys: &selected_inputs,
            },
            &mut engine,
            file,
            &mut seen_outputs,
        );
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

    finish_command_report(globals, &mut report, diagnose, Vec::new(), None)?;
    Ok(report.exit_code())
}

pub(super) fn process_shift_file(
    globals: &GlobalOptions,
    args: &ShiftArgs,
    context: &ShiftProcessContext<'_>,
    engine: &mut engine::CliEngine,
    file: &Path,
    seen_outputs: &mut HashSet<String>,
) -> FileReport {
    // Dispatch by template shape: `{format}` substitution requires
    // parsing the file (the value comes from shiftSubtitles' detected
    // format), so cheap-first ordering doesn't apply to those. The
    // common case (default template `{name}.shifted{ext}` and any
    // user template lacking `{format}`) goes through the cheap path,
    // mirroring HDR's process_hdr_file.
    if args.output_template.contains("{format}") {
        process_shift_file_heavy_first(globals, args, context, engine, file, seen_outputs)
    } else {
        process_shift_file_cheap_first(globals, args, context, engine, file, seen_outputs)
    }
}

pub(super) fn build_shift_request(
    input: String,
    content: String,
    context: &ShiftProcessContext<'_>,
    output_template: String,
) -> engine::ShiftConversionRequest {
    engine::ShiftConversionRequest {
        input_path: input,
        content,
        offset_ms: context.offset_ms,
        threshold_ms: context.threshold_ms,
        timing_map_rules: context.timing_map_rules.clone(),
        output_template,
    }
}

pub(super) fn process_shift_file_cheap_first(
    globals: &GlobalOptions,
    args: &ShiftArgs,
    context: &ShiftProcessContext<'_>,
    engine: &mut engine::CliEngine,
    file: &Path,
    seen_outputs: &mut HashSet<String>,
) -> FileReport {
    let input_path = match absolute_path(file) {
        Ok(path) => path,
        Err(error) => return failed_report(file, None, None, error),
    };
    let input = display_path(&input_path);

    // Cheap path resolution before any I/O or V8 work.
    let path_request = engine::ShiftPathRequest {
        input_path: input.clone(),
        output_template: args.output_template.clone(),
    };
    let resolved_output_path = match engine.resolve_shift_output_path(&path_request) {
        Ok(path) => path,
        Err(error) => return failed_report(&input_path, None, None, error),
    };

    let output_path = match relocate_output_path(&resolved_output_path, context.output_dir) {
        Ok(path) => path,
        Err(error) => return failed_report(&input_path, None, None, error),
    };
    let output = display_path(&output_path);

    let mut reservation = match reserve_output(
        globals,
        &input_path,
        &output_path,
        &output,
        None,
        seen_outputs,
        context.selected_input_keys,
    ) {
        Ok(reservation) => reservation,
        Err(early) => return *early,
    };

    if globals.dry_run {
        // Dry-run gates BEFORE the read so cheap-first lives up to its
        // name on `--dry-run` invocations. encoding is None because we
        // haven't read — matches the cheap-first contract.
        reservation.keep();
        return planned_report(&input_path, Some(output), None);
    }

    let read_result = match app_lib::encoding::read_text_detect_encoding_inner(&input, |_| true) {
        Ok(result) => result,
        Err(error) => return failed_report(&input_path, Some(output), None, error),
    };
    let mut warnings = format_decode_warnings(globals, &read_result);

    let request = build_shift_request(
        input.clone(),
        read_result.text,
        context,
        args.output_template.clone(),
    );

    let conversion = match engine.convert_shift(&request) {
        Ok(result) => result,
        Err(error) => {
            let mut report =
                failed_report(&input_path, Some(output), Some(read_result.encoding), error);
            report.warnings = warnings;
            return report;
        }
    };

    let format_upper = conversion.format.to_uppercase();
    emit_verbose(
        globals,
        format!(
            "shift: {} captions, {} shifted, format {}",
            conversion.caption_count, conversion.shifted_count, format_upper
        ),
        format!(
            "时间轴偏移：{} 条字幕，{} 条已偏移，格式 {}",
            conversion.caption_count, conversion.shifted_count, format_upper
        ),
    );

    if let Some(messages) = format_oversized_caption_warning(
        globals,
        conversion.skipped_count,
        &input,
        if matches!(conversion.format.as_str(), "ass" | "vtt") {
            OversizedCaptionHandling::Preserved
        } else {
            OversizedCaptionHandling::Dropped
        },
    ) {
        warnings.get_or_insert_with(Vec::new).extend(messages);
    }

    // (sibling): attach warnings to the
    // failed_report on write_output failure so the oversized-caption
    // diagnostic isn't silently lost. Cheap-first dedup happens
    // BEFORE convert, so the dedup early-return path doesn't see
    // warnings — only the write-fail path can lose them here.
    if let Err(error) = write_output(
        globals,
        &output_path,
        &conversion.content,
        globals.overwrite,
    ) {
        let mut report =
            failed_report(&input_path, Some(output), Some(read_result.encoding), error);
        report.warnings = warnings;
        return report;
    }

    reservation.keep();

    FileReport {
        input,
        output: Some(output),
        encoding: Some(read_result.encoding),
        status: FileStatus::Written,
        error: None,
        warnings,
    }
}

pub(super) fn process_shift_file_heavy_first(
    globals: &GlobalOptions,
    args: &ShiftArgs,
    context: &ShiftProcessContext<'_>,
    engine: &mut engine::CliEngine,
    file: &Path,
    seen_outputs: &mut HashSet<String>,
) -> FileReport {
    // Original heavy-first ordering, used only when the template
    // contains `{format}`. Read + parse + shift first; only then
    // resolve and dedup. Wasted work on rerun-skip and dedup-fail
    // batches is accepted because `{format}` templates are rare.
    let input_path = match absolute_path(file) {
        Ok(path) => path,
        Err(error) => return failed_report(file, None, None, error),
    };
    let input = display_path(&input_path);

    let read_result = match app_lib::encoding::read_text_detect_encoding_inner(&input, |_| true) {
        Ok(result) => result,
        Err(error) => return failed_report(&input_path, None, None, error),
    };
    let mut warnings = format_decode_warnings(globals, &read_result);

    let request = build_shift_request(
        input.clone(),
        read_result.text,
        context,
        args.output_template.clone(),
    );

    let conversion = match engine.convert_shift(&request) {
        Ok(result) => result,
        Err(error) => {
            let mut report = failed_report(&input_path, None, Some(read_result.encoding), error);
            report.warnings = warnings;
            return report;
        }
    };

    // The oversized-skipped warning is computed BEFORE the dedup /
    // dry-run / write-fail early returns below, so the warning exists
    // by then. The format helper returns the message via
    // FileReport.warnings + emit_file_report's print loop instead of
    // its own eprintln — but the early-return paths construct
    // FileReport via `failed_report` / `skipped_report` /
    // `planned_report`, all of which set `warnings: None`. Without
    // attaching `warnings` to every FileReport returned from this
    // function (early or final), the dry-run / dedup-skip paths would
    // silently lose the warning.
    if let Some(messages) = format_oversized_caption_warning(
        globals,
        conversion.skipped_count,
        &input,
        if matches!(conversion.format.as_str(), "ass" | "vtt") {
            OversizedCaptionHandling::Preserved
        } else {
            OversizedCaptionHandling::Dropped
        },
    ) {
        warnings.get_or_insert_with(Vec::new).extend(messages);
    }

    let output_path = match relocate_output_path(&conversion.output_path, context.output_dir) {
        Ok(path) => path,
        Err(error) => {
            let mut report = failed_report(&input_path, None, Some(read_result.encoding), error);
            report.warnings = warnings;
            return report;
        }
    };
    let output = display_path(&output_path);

    let mut reservation = match reserve_output(
        globals,
        &input_path,
        &output_path,
        &output,
        Some(&read_result.encoding),
        seen_outputs,
        context.selected_input_keys,
    ) {
        Ok(reservation) => reservation,
        Err(mut early) => {
            early.warnings = warnings.clone();
            return *early;
        }
    };

    // Dry-run gates BEFORE the verbose progress print: a
    // `--dry-run --verbose` invocation should NOT emit the "shift: N
    // captions, M shifted" line because no shift was actually
    // committed. Matches the cheap-first path's ordering.
    if globals.dry_run {
        reservation.keep();
        let mut planned = planned_report(&input_path, Some(output), Some(read_result.encoding));
        planned.warnings = warnings;
        return planned;
    }

    let format_upper = conversion.format.to_uppercase();
    emit_verbose(
        globals,
        format!(
            "shift: {} captions, {} shifted, format {}",
            conversion.caption_count, conversion.shifted_count, format_upper
        ),
        format!(
            "时间轴偏移：{} 条字幕，{} 条已偏移，格式 {}",
            conversion.caption_count, conversion.shifted_count, format_upper
        ),
    );

    if let Err(error) = write_output(
        globals,
        &output_path,
        &conversion.content,
        globals.overwrite,
    ) {
        let mut report =
            failed_report(&input_path, Some(output), Some(read_result.encoding), error);
        report.warnings = warnings;
        return report;
    }

    reservation.keep();

    FileReport {
        input,
        output: Some(output),
        encoding: Some(read_result.encoding),
        status: FileStatus::Written,
        error: None,
        warnings,
    }
}
