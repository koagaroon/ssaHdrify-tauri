//! `style-edit` subcommand execution — structured ASS/SSA `Style:` font-family
//! and font-size editing with non-destructive `.styled.ass` / `.styled.ssa`
//! sibling outputs, source-encoding preservation, and stale-source guards.

use super::*;
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

pub(super) fn run_style_edit(
    globals: &GlobalOptions,
    args: StyleEditArgs,
    diagnose: Option<DiagnoseMode>,
) -> Result<ExitCode, String> {
    if globals.overwrite {
        return Err(
            "style-edit never overwrites existing files; remove --overwrite \
             (outputs are always new .styled.ass / .styled.ssa sibling files)"
                .to_string(),
        );
    }
    if globals.output_dir.is_some() {
        return Err(
            "style-edit always writes sibling .styled.ass / .styled.ssa files \
             next to each source; --output-dir is not supported"
                .to_string(),
        );
    }

    let mut engine = engine::CliEngine::new()?;
    let ops_request = engine::StyleEditOperationsRequest {
        font_family: args.font_family.clone(),
        source_family: args.source_family.clone(),
        font_size: args.font_size.clone(),
    };
    engine.validate_style_edit_operations(&ops_request)?;

    let mut report = CommandReport::new("style-edit");
    let selected_inputs = selected_input_keys(&args.files);
    let mut seen_outputs = HashSet::new();

    for (idx, file) in args.files.iter().enumerate() {
        let result = process_style_edit_file(
            globals,
            &args,
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

fn process_style_edit_file(
    globals: &GlobalOptions,
    args: &StyleEditArgs,
    engine: &mut engine::CliEngine,
    file: &Path,
    seen_outputs: &mut HashSet<String>,
    selected_inputs: &HashSet<String>,
) -> FileReport {
    let input_path = match absolute_path(file) {
        Ok(path) => path,
        Err(error) => return failed_report(file, None, None, error),
    };
    let input = display_path(&input_path);

    let resolved_output_path =
        match engine.resolve_style_edit_output_path(&engine::StyleEditPathRequest {
            input_path: input.clone(),
        }) {
            Ok(path) => path,
            Err(error) => return failed_report(&input_path, None, None, error),
        };
    let output_path = PathBuf::from(&resolved_output_path);
    let output = display_path(&output_path);

    if output_matches_selected_input(&output_path, selected_inputs) {
        return failed_report(
            &input_path,
            Some(output),
            None,
            "output path matches a selected input file".to_string(),
        );
    }

    let key = normalize_output_key(&output_path);
    if !seen_outputs.insert(key.clone()) {
        return failed_report(
            &input_path,
            Some(output),
            None,
            "duplicate output path in planned batch".to_string(),
        );
    }
    let mut reservation = OutputReservation::new(seen_outputs, key);

    let read_result = match app_lib::encoding::read_text_detect_encoding_inner(&input, |_| true) {
        Ok(result) => result,
        Err(error) => return failed_report(&input_path, Some(output), None, error),
    };
    let warnings = format_decode_warnings(globals, &read_result);
    if read_result.lossy {
        let mut report = failed_report(
            &input_path,
            Some(output),
            Some(read_result.encoding),
            "source decoding was lossy; refusing to write an irreversible style edit".to_string(),
        );
        report.warnings = warnings;
        return report;
    }

    let applied = match engine.apply_style_edit(&engine::StyleEditCliRequest {
        input_path: input.clone(),
        content: read_result.text,
        font_family: args.font_family.clone(),
        source_family: args.source_family.clone(),
        font_size: args.font_size.clone(),
    }) {
        Ok(result) => result,
        Err(error) => {
            let mut report =
                failed_report(&input_path, Some(output), Some(read_result.encoding), error);
            report.warnings = warnings;
            return report;
        }
    };

    if applied.changed_style_count == 0 {
        let mut report = skipped_report(
            &input_path,
            Some(output),
            Some(read_result.encoding),
            "no matching styles would change".to_string(),
        );
        report.warnings = warnings;
        return report;
    }

    if output_path_exists(globals, &output_path) {
        let mut report = failed_report(
            &input_path,
            Some(output),
            Some(read_result.encoding),
            "output already exists; style-edit never overwrites existing .styled.ass / .styled.ssa files"
                .to_string(),
        );
        report.warnings = warnings;
        return report;
    }

    if globals.dry_run {
        reservation.keep();
        let mut report = planned_report(&input_path, Some(output), Some(read_result.encoding));
        report.warnings = warnings;
        return report;
    }

    if let Err(error) = app_lib::safe_io::safe_write_style_edit_output_inner(
        &input,
        &read_result.source_revision,
        &output,
        &applied.content,
        |_| true,
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
