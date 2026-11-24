//! `rename` subcommand execution, input protection, and collision checks.

use super::*;
use std::collections::HashMap;
use std::collections::HashSet;
use std::fs;
use std::io::ErrorKind;
use std::path::Path;
use std::path::PathBuf;
use std::process::ExitCode;

pub(super) fn run_rename(
    globals: &GlobalOptions,
    args: RenameArgs,
    diagnose: Option<DiagnoseMode>,
) -> Result<ExitCode, String> {
    let output_dir = globals
        .output_dir
        .as_deref()
        .map(absolute_path)
        .transpose()?;

    match (args.mode, output_dir.as_deref()) {
        (RenameMode::CopyToChosen, None) => {
            return Err("--output-dir is required with rename --mode copy-to-chosen".to_string());
        }
        (RenameMode::Rename | RenameMode::CopyToVideo, Some(_)) => {
            return Err(
                "--output-dir can only be used with rename --mode copy-to-chosen".to_string(),
            );
        }
        (RenameMode::Rename | RenameMode::CopyToVideo, None) => {}
        (RenameMode::CopyToChosen, Some(_)) => {}
    }

    let expanded_paths = expand_rename_inputs(globals, &args.paths)?;
    let mut engine = engine::CliEngine::new()?;
    let request = engine::RenamePlanRequest {
        paths: expanded_paths,
        mode: args.mode.as_engine_value().to_string(),
        output_dir: output_dir.as_deref().map(display_path),
        langs: args.langs.clone(),
    };
    let mut plan = engine.plan_rename(&request)?;
    protect_rename_selected_inputs(&request.paths, &mut plan.pairings)?;
    let mut report = CommandReport::new("rename");

    let v = plan.video_count;
    let s = plan.subtitle_count;
    let i = plan.ignored_count;
    let u = plan.unknown_count;
    emit_verbose(
        globals,
        format!("rename: {v} videos, {s} subtitles, {i} ignored, {u} unknown"),
        format!("重命名：{v} 个视频，{s} 个字幕，{i} 个忽略，{u} 个未知"),
    );

    if plan.pairings.is_empty() {
        let result = FileReport {
            input: "<batch>".to_string(),
            output: None,
            encoding: None,
            status: FileStatus::Failed,
            error: Some(format!(
                "no subtitle/video pairs found ({} videos, {} subtitles, {} unknown)",
                plan.video_count, plan.subtitle_count, plan.unknown_count
            )),
            warnings: None,
        };
        emit_file_report(globals, &result);
        report.push(result);
        finish_command_report(globals, &mut report, diagnose, Vec::new(), None)?;
        return Ok(report.exit_code());
    }

    let duplicate_outputs = duplicate_rename_output_keys(&plan.pairings);
    for (idx, row) in plan.pairings.iter().enumerate() {
        let result = process_rename_pair(globals, &args, row, &duplicate_outputs);
        let failed = result.status == FileStatus::Failed;
        emit_file_report(globals, &result);
        report.push(result);
        if globals.fail_fast && failed {
            let remaining = plan.pairings.len().saturating_sub(idx + 1);
            emit_fail_fast_abort_notice(globals, remaining);
            report.mark_fail_fast_abort();
            break;
        }
    }

    finish_command_report(globals, &mut report, diagnose, Vec::new(), None)?;
    Ok(report.exit_code())
}

pub(super) fn expand_rename_inputs(
    globals: &GlobalOptions,
    paths: &[PathBuf],
) -> Result<Vec<String>, String> {
    let absolute_paths: Result<Vec<String>, String> = paths
        .iter()
        .map(|path| absolute_path(path).map(|path| display_path(&path)))
        .collect();
    // CLI argv paths are user-authored and have no Tauri AppHandle. Bypass
    // only the GUI filesystem scope while retaining validation, reparse-point
    // rejection, one-level walking, and both path-count caps.
    let expanded = app_lib::dropzone::expand_dropped_paths_inner(absolute_paths?, |_| true)?;

    if expanded.files.is_empty() {
        return Err("no regular files found in rename input paths".to_string());
    }
    // CLI surfaces truncation via stderr — the user's drop got
    // partially processed, and they should know before reviewing
    // the output. GUI side surfaces this through useFolderDrop's
    // onError consumer . The cap value comes from
    // the dropzone module so a future bump there flows here
    // automatically. Gate on --quiet so the user who opted into a
    // diagnostics-free run doesn't get this stderr line — same posture
    // as every other informational stderr in the CLI.
    if expanded.truncated && !globals.quiet {
        let cap = app_lib::dropzone::MAX_RESULT_FILES;
        eprintln!(
            "{}",
            localize(
                globals,
                format!(
                    "⚠ Dropped path expansion hit the {cap} file cap; remainder ignored. \
                     Drop fewer files per batch."
                ),
                format!("⚠ 拖入路径展开触及 {cap} 个文件上限；剩余忽略。请减少单次拖入数量。"),
            )
        );
    }
    Ok(expanded.files)
}

pub(super) fn protect_rename_selected_inputs(
    selected_paths: &[String],
    rows: &mut [engine::RenamePlanRow],
) -> Result<(), String> {
    if rows.is_empty() {
        return Ok(());
    }

    let mut input_identities = HashMap::new();
    for input in selected_paths {
        let path = Path::new(input);
        if !app_lib::safe_io::is_rename_subtitle_path(path)
            || input_identities.contains_key(input.as_str())
        {
            continue;
        }
        let canonical = path.canonicalize().map_err(|error| {
            format!(
                "Failed to resolve selected subtitle input {}: {error}",
                path.display()
            )
        })?;
        let metadata = fs::metadata(&canonical)
            .map_err(|error| format!("Failed to inspect selected subtitle input: {error}"))?;
        if !metadata.is_file() {
            return Err(format!(
                "Selected subtitle input is not a regular file: {}",
                path.display()
            ));
        }
        input_identities.insert(input.as_str(), normalize_output_key(&canonical));
    }
    let protected_identities: HashSet<&str> =
        input_identities.values().map(String::as_str).collect();
    let mut input_owners: HashMap<&str, Vec<usize>> = HashMap::new();
    let mut row_identities = Vec::with_capacity(rows.len());
    for (index, row) in rows.iter().enumerate() {
        let identity = input_identities
            .get(row.input_path.as_str())
            .ok_or_else(|| {
                format!(
                    "Rename plan input is not a selected subtitle: {}",
                    row.input_path
                )
            })?;
        input_owners.entry(identity).or_default().push(index);
        row_identities.push(identity.as_str());
    }

    // Resolve the whole plan before executing any row: an earlier source
    // must stay in place when a later output reaches it through an alias.
    let mut conflicts = vec![false; rows.len()];
    let mut no_ops = vec![false; rows.len()];
    for (index, row) in rows.iter().enumerate() {
        let canonical = match Path::new(&row.output_path).canonicalize() {
            Ok(path) => path,
            Err(error) if error.kind() == ErrorKind::NotFound => continue,
            Err(error) => return Err(format!("Failed to resolve rename output path: {error}")),
        };
        let identity = normalize_output_key(&canonical);
        if identity == row_identities[index] {
            no_ops[index] = true;
            continue;
        }
        if protected_identities.contains(identity.as_str()) {
            conflicts[index] = true;
            if let Some(owners) = input_owners.get(identity.as_str()) {
                for &owner in owners {
                    conflicts[owner] = true;
                }
            }
        }
    }
    for (index, row) in rows.iter_mut().enumerate() {
        row.input_conflict = conflicts[index];
        row.no_op |= no_ops[index];
    }
    Ok(())
}

pub(super) fn process_rename_pair(
    globals: &GlobalOptions,
    args: &RenameArgs,
    row: &engine::RenamePlanRow,
    duplicate_outputs: &HashSet<String>,
) -> FileReport {
    let input_path = PathBuf::from(&row.input_path);
    let output_path = PathBuf::from(&row.output_path);
    let input = display_path(&input_path);
    let output = display_path(&output_path);

    // Native preflight computes this flag against every selected subtitle
    // identity (not only executable rows) before this loop. Keep this
    // guard before no-op, duplicate, exists, dry-run, and overwrite handling:
    // every participant in a swap/chain must fail without touching the disk.
    if row.input_conflict {
        return attach_rename_pairing_warning(
            globals,
            row,
            failed_report(
                &input_path,
                Some(output),
                None,
                "subtitle is part of a planned conflict where an output targets a loaded subtitle input; no files in that conflicting chain were changed"
                    .to_string(),
            ),
        );
    }

    if row.no_op {
        return attach_rename_pairing_warning(
            globals,
            row,
            skipped_report(
                &input_path,
                Some(output),
                None,
                "subtitle already matches the target path".to_string(),
            ),
        );
    }

    let output_key = normalize_output_key(&output_path);
    if duplicate_outputs.contains(&output_key) {
        return attach_rename_pairing_warning(
            globals,
            row,
            failed_report(
                &input_path,
                Some(output),
                None,
                "duplicate output path in planned batch".to_string(),
            ),
        );
    }

    if !globals.overwrite && output_path_exists(globals, &output_path) {
        return attach_rename_pairing_warning(
            globals,
            row,
            skipped_report(
                &input_path,
                Some(output),
                None,
                "output exists; pass --overwrite to replace it".to_string(),
            ),
        );
    }

    // Sanitize before stdout interpolation. `from` / `to` / `video`
    // are all raw operational path strings; emit_verbose calls println
    // via localize, so control chars would reach the terminal verbatim.
    let from = sanitize_for_display(&display_path(&input_path));
    let to = sanitize_for_display(&display_path(&output_path));
    let video = sanitize_for_display(&row.video_path);
    emit_verbose(
        globals,
        format!("rename: {from} -> {to} (video: {video})"),
        format!("重命名：{from} -> {to}（视频：{video}）"),
    );

    if globals.dry_run {
        return attach_rename_pairing_warning(
            globals,
            row,
            planned_report(&input_path, Some(output), None),
        );
    }

    let operation_result = if args.mode.is_copy() {
        copy_file_output(globals, &input_path, &output_path, globals.overwrite)
    } else {
        rename_file_output(globals, &input_path, &output_path, globals.overwrite)
    };

    if let Err(error) = operation_result {
        return attach_rename_pairing_warning(
            globals,
            row,
            failed_report(&input_path, Some(output), None, error),
        );
    }

    attach_rename_pairing_warning(
        globals,
        row,
        FileReport {
            input,
            output: Some(output),
            encoding: None,
            status: FileStatus::Written,
            error: None,
            warnings: None,
        },
    )
}

pub(super) fn attach_rename_pairing_warning(
    globals: &GlobalOptions,
    row: &engine::RenamePlanRow,
    mut report: FileReport,
) -> FileReport {
    if row.source == "warning" {
        report.warnings = Some(vec![localize(
            globals,
            "pairing is ambiguous because multiple videos share the same season and episode; verify the selected subtitle"
                .to_string(),
            "多个视频具有相同的季与集编号，配对存在歧义；请核对所选字幕".to_string(),
        )]);
    }
    report
}

pub(super) fn duplicate_rename_output_keys(rows: &[engine::RenamePlanRow]) -> HashSet<String> {
    // All-fail dedup (not first-wins) for rename: rename is destructive,
    // so picking a "winner" among duplicates risks moving the wrong
    // file into a stable name. Every participant in a duplicate set is
    // flagged here and refuses to act in process_rename_pair.
    //
    // Rows already blocked by the stronger input-conflict preflight do not
    // claim outputs: they cannot write, and excluding them keeps GUI/CLI
    // behavior aligned when an otherwise-safe row shares their target.
    //
    // No-op rows DO claim their output key — a no-op row's output is a
    // real file already on disk, so a non-no-op row targeting the same
    // key would silently overwrite it under --overwrite. The no-op row
    // itself is still skipped at process_rename_pair (no_op branch
    // returns Skipped before the duplicate check); the conflict signal
    // lands on the colliding non-no-op rows.
    let mut seen = HashSet::new();
    let mut duplicates = HashSet::new();

    for row in rows.iter().filter(|row| !row.input_conflict) {
        let key = normalize_output_key(Path::new(&row.output_path));
        if !seen.insert(key.clone()) {
            duplicates.insert(key);
        }
    }

    duplicates
}
