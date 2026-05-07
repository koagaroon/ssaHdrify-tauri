use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::{Args, Parser, Subcommand, ValueEnum};
use serde::Serialize;

mod engine;

const MAX_SHIFT_OFFSET_MS: i64 = 365 * 24 * 60 * 60 * 1000;

#[derive(Debug, Parser)]
#[command(
    name = "ssahdrify-cli",
    version,
    about = "Command-line interface for SSA HDRify subtitle workflows",
    arg_required_else_help = true
)]
struct Cli {
    #[command(flatten)]
    globals: GlobalOptions,

    #[command(subcommand)]
    command: Command,
}

#[derive(Args, Debug)]
struct GlobalOptions {
    /// Output directory. Defaults to each input file's directory.
    #[arg(long, global = true, value_name = "DIR")]
    output_dir: Option<PathBuf>,

    /// Replace existing output files instead of skipping them.
    #[arg(long, global = true)]
    overwrite: bool,

    /// Show planned work without writing files.
    #[arg(long, global = true)]
    dry_run: bool,

    /// Suppress normal progress output.
    #[arg(long, global = true, conflicts_with = "verbose")]
    quiet: bool,

    /// Show more progress detail.
    #[arg(long, global = true)]
    verbose: bool,

    /// Emit machine-readable JSON.
    #[arg(long, global = true)]
    json: bool,

    /// Output language. Defaults to OS/terminal locale detection.
    #[arg(long, global = true, value_enum, value_name = "LANG")]
    lang: Option<OutputLang>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
enum OutputLang {
    En,
    Zh,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Convert SDR subtitle colors to HDR.
    Hdr(HdrArgs),
    /// Shift subtitle timings by an offset.
    Shift(ShiftArgs),
    /// Embed fonts into ASS subtitle files.
    Embed(EmbedArgs),
    /// Pair subtitles with videos and rename subtitles to match.
    Rename(RenameArgs),
}

#[derive(Args, Debug)]
struct HdrArgs {
    /// Transfer function.
    #[arg(long, value_enum)]
    eotf: EotfArg,

    /// Target subtitle brightness in nits.
    #[arg(long, default_value_t = 203)]
    nits: u16,

    /// Output filename template.
    #[arg(long, default_value = "{name}.hdr.ass")]
    output_template: String,

    /// Subtitle files to convert.
    #[arg(required = true)]
    files: Vec<PathBuf>,
}

#[derive(Clone, Copy, Debug, ValueEnum)]
enum EotfArg {
    Pq,
    Hlg,
}

impl EotfArg {
    fn as_engine_value(self) -> &'static str {
        match self {
            EotfArg::Pq => "PQ",
            EotfArg::Hlg => "HLG",
        }
    }
}

#[derive(Args, Debug)]
struct ShiftArgs {
    /// Signed duration, for example "+2.5s", "-500ms", or "+1m30s".
    #[arg(long, allow_hyphen_values = true)]
    offset: String,

    /// Shift only entries after this timestamp.
    #[arg(long)]
    after: Option<String>,

    /// Output filename template.
    #[arg(long, default_value = "{name}.shifted{ext}")]
    output_template: String,

    /// Subtitle files to shift.
    #[arg(required = true)]
    files: Vec<PathBuf>,
}

#[derive(Args, Debug)]
struct EmbedArgs {
    /// Local font directory. Can be passed multiple times.
    #[arg(long = "font-dir", value_name = "DIR")]
    font_dirs: Vec<PathBuf>,

    /// Local font file. Can be passed multiple times.
    #[arg(long = "font-file", value_name = "FILE")]
    font_files: Vec<PathBuf>,

    /// Do not use system-installed fonts.
    #[arg(long)]
    no_system_fonts: bool,

    /// Behavior when referenced fonts are missing.
    #[arg(long, value_enum, default_value_t = MissingFontAction::Warn)]
    on_missing: MissingFontAction,

    /// Output filename template.
    #[arg(long, default_value = "{name}.embed.ass")]
    output_template: String,

    /// ASS/SSA files to process.
    #[arg(required = true)]
    files: Vec<PathBuf>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
enum MissingFontAction {
    Warn,
    Fail,
}

#[derive(Args, Debug)]
struct RenameArgs {
    /// Output mode.
    #[arg(long, value_enum, default_value_t = RenameMode::CopyToVideo)]
    mode: RenameMode,

    /// Language selection: auto, all, or a comma-separated list such as sc,jp.
    #[arg(long, default_value = "auto")]
    langs: String,

    /// Video/subtitle files or folders to pair.
    #[arg(required = true)]
    paths: Vec<PathBuf>,
}

#[derive(Clone, Copy, Debug, ValueEnum)]
enum RenameMode {
    Rename,
    CopyToVideo,
    CopyToChosen,
}

impl RenameMode {
    fn as_engine_value(self) -> &'static str {
        match self {
            RenameMode::Rename => "rename",
            RenameMode::CopyToVideo => "copy_to_video",
            RenameMode::CopyToChosen => "copy_to_chosen",
        }
    }

    fn is_copy(self) -> bool {
        matches!(self, RenameMode::CopyToVideo | RenameMode::CopyToChosen)
    }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct CommandReport {
    command: &'static str,
    written: usize,
    planned: usize,
    skipped: usize,
    failed: usize,
    results: Vec<FileReport>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct FileReport {
    input: String,
    output: Option<String>,
    encoding: Option<String>,
    status: FileStatus,
    error: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "kebab-case")]
enum FileStatus {
    Written,
    Planned,
    Skipped,
    Failed,
}

struct ResolvedEmbedFont {
    label: String,
    font_name: String,
    path: String,
    index: u32,
    codepoints: Vec<u32>,
}

struct TempFontDbDir(PathBuf);

impl Drop for TempFontDbDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

impl CommandReport {
    fn new(command: &'static str) -> Self {
        Self {
            command,
            written: 0,
            planned: 0,
            skipped: 0,
            failed: 0,
            results: Vec::new(),
        }
    }

    fn push(&mut self, result: FileReport) {
        match result.status {
            FileStatus::Written => self.written += 1,
            FileStatus::Planned => self.planned += 1,
            FileStatus::Skipped => self.skipped += 1,
            FileStatus::Failed => self.failed += 1,
        }
        self.results.push(result);
    }

    fn exit_code(&self) -> ExitCode {
        if self.failed == 0 {
            return ExitCode::SUCCESS;
        }

        let non_failed = self.written + self.planned + self.skipped;
        if non_failed > 0 {
            ExitCode::from(1)
        } else {
            ExitCode::from(2)
        }
    }
}

fn main() -> ExitCode {
    match run() {
        Ok(code) => code,
        Err(err) => {
            eprintln!("ssahdrify-cli: {err}");
            ExitCode::from(2)
        }
    }
}

fn run() -> Result<ExitCode, String> {
    let Cli { globals, command } = Cli::parse();

    match command {
        Command::Hdr(args) => run_hdr(&globals, args),
        Command::Shift(args) => run_shift(&globals, args),
        Command::Embed(args) => run_embed(&globals, args),
        Command::Rename(args) => run_rename(&globals, args),
    }
}

fn run_hdr(globals: &GlobalOptions, args: HdrArgs) -> Result<ExitCode, String> {
    let mut engine = engine::CliEngine::new()?;
    let output_dir = globals
        .output_dir
        .as_deref()
        .map(absolute_path)
        .transpose()?;
    let mut report = CommandReport::new("hdr");

    for file in &args.files {
        let result = process_hdr_file(globals, &args, output_dir.as_deref(), &mut engine, file);
        emit_file_report(globals, &result);
        report.push(result);
    }

    if globals.json {
        let json = serde_json::to_string_pretty(&report)
            .map_err(|err| format!("failed to encode JSON report: {err}"))?;
        println!("{json}");
    } else if !globals.quiet {
        let message = localize(
            globals,
            format!(
                "Done: {} written, {} planned, {} skipped, {} failed",
                report.written, report.planned, report.skipped, report.failed
            ),
            format!(
                "完成：{} 个已写入，{} 个计划写入，{} 个已跳过，{} 个失败",
                report.written, report.planned, report.skipped, report.failed
            ),
        );
        println!("{message}");
    }

    Ok(report.exit_code())
}

fn process_hdr_file(
    globals: &GlobalOptions,
    args: &HdrArgs,
    output_dir: Option<&Path>,
    engine: &mut engine::CliEngine,
    file: &Path,
) -> FileReport {
    let input_path = match absolute_path(file) {
        Ok(path) => path,
        Err(error) => {
            return failed_report(file, None, None, error);
        }
    };
    let input = display_path(&input_path);

    let read_result = match app_lib::encoding::read_text_detect_encoding(input.clone()) {
        Ok(result) => result,
        Err(error) => return failed_report(&input_path, None, None, error),
    };

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
            return failed_report(&input_path, None, Some(read_result.encoding), error);
        }
    };

    let output_path = match relocate_output_path(&conversion.output_path, output_dir) {
        Ok(path) => path,
        Err(error) => {
            return failed_report(&input_path, None, Some(read_result.encoding), error);
        }
    };
    let output = display_path(&output_path);

    if output_path_exists(&output_path) && !globals.overwrite {
        return FileReport {
            input,
            output: Some(output),
            encoding: Some(read_result.encoding),
            status: FileStatus::Skipped,
            error: Some("output exists; pass --overwrite to replace it".to_string()),
        };
    }

    if globals.dry_run {
        return FileReport {
            input,
            output: Some(output),
            encoding: Some(read_result.encoding),
            status: FileStatus::Planned,
            error: None,
        };
    }

    if let Err(error) = write_output(&output_path, &conversion.content) {
        return failed_report(&input_path, Some(output), Some(read_result.encoding), error);
    }

    FileReport {
        input,
        output: Some(output),
        encoding: Some(read_result.encoding),
        status: FileStatus::Written,
        error: None,
    }
}

fn run_shift(globals: &GlobalOptions, args: ShiftArgs) -> Result<ExitCode, String> {
    let offset_ms = parse_duration_ms(&args.offset)?;
    if offset_ms.abs() > MAX_SHIFT_OFFSET_MS {
        return Err(format!(
            "offset is too large: max supported range is +/-{} ms",
            MAX_SHIFT_OFFSET_MS
        ));
    }
    let threshold_ms = args.after.as_deref().map(parse_timestamp_ms).transpose()?;
    let mut engine = engine::CliEngine::new()?;
    let output_dir = globals
        .output_dir
        .as_deref()
        .map(absolute_path)
        .transpose()?;
    let mut report = CommandReport::new("shift");

    for file in &args.files {
        let result = process_shift_file(
            globals,
            &args,
            offset_ms,
            threshold_ms,
            output_dir.as_deref(),
            &mut engine,
            file,
        );
        emit_file_report(globals, &result);
        report.push(result);
    }

    if globals.json {
        let json = serde_json::to_string_pretty(&report)
            .map_err(|err| format!("failed to encode JSON report: {err}"))?;
        println!("{json}");
    } else if !globals.quiet {
        let message = localize(
            globals,
            format!(
                "Done: {} written, {} planned, {} skipped, {} failed",
                report.written, report.planned, report.skipped, report.failed
            ),
            format!(
                "完成：{} 个已写入，{} 个计划写入，{} 个已跳过，{} 个失败",
                report.written, report.planned, report.skipped, report.failed
            ),
        );
        println!("{message}");
    }

    Ok(report.exit_code())
}

fn process_shift_file(
    globals: &GlobalOptions,
    args: &ShiftArgs,
    offset_ms: i64,
    threshold_ms: Option<i64>,
    output_dir: Option<&Path>,
    engine: &mut engine::CliEngine,
    file: &Path,
) -> FileReport {
    let input_path = match absolute_path(file) {
        Ok(path) => path,
        Err(error) => {
            return failed_report(file, None, None, error);
        }
    };
    let input = display_path(&input_path);

    let read_result = match app_lib::encoding::read_text_detect_encoding(input.clone()) {
        Ok(result) => result,
        Err(error) => return failed_report(&input_path, None, None, error),
    };

    let request = engine::ShiftConversionRequest {
        input_path: input.clone(),
        content: read_result.text,
        offset_ms,
        threshold_ms,
        output_template: args.output_template.clone(),
    };

    let conversion = match engine.convert_shift(&request) {
        Ok(result) => result,
        Err(error) => {
            return failed_report(&input_path, None, Some(read_result.encoding), error);
        }
    };

    let output_path = match relocate_output_path(&conversion.output_path, output_dir) {
        Ok(path) => path,
        Err(error) => {
            return failed_report(&input_path, None, Some(read_result.encoding), error);
        }
    };
    let output = display_path(&output_path);

    if output_path_exists(&output_path) && !globals.overwrite {
        return FileReport {
            input,
            output: Some(output),
            encoding: Some(read_result.encoding),
            status: FileStatus::Skipped,
            error: Some("output exists; pass --overwrite to replace it".to_string()),
        };
    }

    if globals.verbose && !globals.json && !globals.quiet {
        println!(
            "shift: {} captions, {} shifted, format {}",
            conversion.caption_count,
            conversion.shifted_count,
            conversion.format.to_uppercase()
        );
    }

    if globals.dry_run {
        return FileReport {
            input,
            output: Some(output),
            encoding: Some(read_result.encoding),
            status: FileStatus::Planned,
            error: None,
        };
    }

    if let Err(error) = write_output(&output_path, &conversion.content) {
        return failed_report(&input_path, Some(output), Some(read_result.encoding), error);
    }

    FileReport {
        input,
        output: Some(output),
        encoding: Some(read_result.encoding),
        status: FileStatus::Written,
        error: None,
    }
}

fn run_embed(globals: &GlobalOptions, args: EmbedArgs) -> Result<ExitCode, String> {
    app_lib::fonts::init_system_dirs();
    let use_user_fonts = !args.font_dirs.is_empty() || !args.font_files.is_empty();
    let _font_db_dir = if use_user_fonts {
        Some(init_cli_font_sources(globals, &args)?)
    } else {
        None
    };

    let mut engine = engine::CliEngine::new()?;
    let output_dir = globals
        .output_dir
        .as_deref()
        .map(absolute_path)
        .transpose()?;
    let mut report = CommandReport::new("embed");

    for file in &args.files {
        let result = process_embed_file(
            globals,
            &args,
            use_user_fonts,
            output_dir.as_deref(),
            &mut engine,
            file,
        );
        emit_file_report(globals, &result);
        report.push(result);
    }

    emit_report_summary(globals, &report)?;
    Ok(report.exit_code())
}

fn init_cli_font_sources(
    globals: &GlobalOptions,
    args: &EmbedArgs,
) -> Result<TempFontDbDir, String> {
    let db_dir = std::env::temp_dir().join(format!("ssahdrify-cli-font-db-{}", std::process::id()));
    let _ = fs::remove_dir_all(&db_dir);
    app_lib::fonts::init_user_font_db(&db_dir)?;

    for (index, dir) in args.font_dirs.iter().enumerate() {
        let dir = absolute_path(dir)?;
        let source_id = format!("cli-dir-{index}");
        let summary = app_lib::fonts::import_font_directory_for_cli(&dir, &source_id)?;
        emit_font_source_summary(globals, "font dir", &dir, &summary);
    }

    if !args.font_files.is_empty() {
        let paths: Result<Vec<String>, String> = args
            .font_files
            .iter()
            .map(|path| absolute_path(path).map(|path| display_path(&path)))
            .collect();
        let summary = app_lib::fonts::import_font_files_for_cli(paths?, "cli-files")?;
        if globals.verbose && !globals.json && !globals.quiet {
            println!(
                "font files: {} faces scanned, {} added, {} duplicated",
                summary.total, summary.added, summary.duplicated
            );
        }
    }

    Ok(TempFontDbDir(db_dir))
}

fn emit_font_source_summary(
    globals: &GlobalOptions,
    label: &str,
    path: &Path,
    summary: &app_lib::fonts::FontSourceImportSummary,
) {
    if !globals.verbose || globals.json || globals.quiet {
        return;
    }
    let reason = match summary.reason {
        app_lib::fonts::ScanStopReason::Natural => "",
        app_lib::fonts::ScanStopReason::UserCancel => " (cancelled)",
        app_lib::fonts::ScanStopReason::CeilingHit => " (ceiling hit)",
    };
    println!(
        "{label}: {} faces scanned, {} added, {} duplicated{} ({})",
        summary.total,
        summary.added,
        summary.duplicated,
        reason,
        display_path(path)
    );
}

fn process_embed_file(
    globals: &GlobalOptions,
    args: &EmbedArgs,
    use_user_fonts: bool,
    output_dir: Option<&Path>,
    engine: &mut engine::CliEngine,
    file: &Path,
) -> FileReport {
    let input_path = match absolute_path(file) {
        Ok(path) => path,
        Err(error) => return failed_report(file, None, None, error),
    };
    let input = display_path(&input_path);

    if !has_ass_extension(&input_path) {
        return failed_report(
            &input_path,
            None,
            None,
            "font embed only supports ASS/SSA subtitle files".to_string(),
        );
    }

    let read_result = match app_lib::encoding::read_text_detect_encoding(input.clone()) {
        Ok(result) => result,
        Err(error) => return failed_report(&input_path, None, None, error),
    };

    let plan_request = engine::FontEmbedPlanRequest {
        input_path: input.clone(),
        content: read_result.text.clone(),
        output_template: args.output_template.clone(),
    };
    let plan = match engine.plan_font_embed(&plan_request) {
        Ok(result) => result,
        Err(error) => {
            return failed_report(&input_path, None, Some(read_result.encoding), error);
        }
    };

    let output_path = match relocate_output_path(&plan.output_path, output_dir) {
        Ok(path) => path,
        Err(error) => {
            return failed_report(&input_path, None, Some(read_result.encoding), error);
        }
    };
    let output = display_path(&output_path);

    if output_path_exists(&output_path) && !globals.overwrite {
        return FileReport {
            input,
            output: Some(output),
            encoding: Some(read_result.encoding),
            status: FileStatus::Skipped,
            error: Some("output exists; pass --overwrite to replace it".to_string()),
        };
    }

    let resolved_fonts = match resolve_embed_fonts(globals, args, use_user_fonts, &plan.fonts) {
        Ok(fonts) => fonts,
        Err(error) => {
            return failed_report(&input_path, Some(output), Some(read_result.encoding), error);
        }
    };

    if globals.verbose && !globals.json && !globals.quiet {
        let glyph_count: usize = plan.fonts.iter().map(|font| font.glyph_count).sum();
        println!(
            "embed: {} referenced fonts ({} glyphs), {} resolved",
            plan.fonts.len(),
            glyph_count,
            resolved_fonts.len()
        );
    }

    if globals.dry_run {
        return FileReport {
            input,
            output: Some(output),
            encoding: Some(read_result.encoding),
            status: FileStatus::Planned,
            error: None,
        };
    }

    let subset_payloads = match subset_resolved_fonts(globals, args, &resolved_fonts) {
        Ok(payloads) => payloads,
        Err(error) => {
            return failed_report(&input_path, Some(output), Some(read_result.encoding), error);
        }
    };

    let apply_request = engine::FontEmbedApplyRequest {
        content: read_result.text,
        fonts: subset_payloads,
    };
    let applied = match engine.apply_font_embed(&apply_request) {
        Ok(result) => result,
        Err(error) => {
            return failed_report(&input_path, Some(output), Some(read_result.encoding), error);
        }
    };

    if globals.verbose && !globals.json && !globals.quiet {
        println!("embed: {} fonts embedded", applied.embedded_count);
    }

    if let Err(error) = write_output(&output_path, &applied.content) {
        return failed_report(&input_path, Some(output), Some(read_result.encoding), error);
    }

    FileReport {
        input,
        output: Some(output),
        encoding: Some(read_result.encoding),
        status: FileStatus::Written,
        error: None,
    }
}

fn resolve_embed_fonts(
    globals: &GlobalOptions,
    args: &EmbedArgs,
    use_user_fonts: bool,
    fonts: &[engine::FontEmbedUsage],
) -> Result<Vec<ResolvedEmbedFont>, String> {
    let mut resolved = Vec::new();
    let mut missing = Vec::new();

    for font in fonts {
        let lookup = resolve_embed_font(args, use_user_fonts, font);
        let (path, index) = match lookup {
            Ok(Some(found)) => found,
            Ok(None) => {
                missing.push(font.label.clone());
                continue;
            }
            Err(error) => {
                missing.push(format!("{} ({error})", font.label));
                continue;
            }
        };

        resolved.push(ResolvedEmbedFont {
            label: font.label.clone(),
            font_name: font.font_name.clone(),
            path,
            index,
            codepoints: font.codepoints.clone(),
        });
    }

    if !missing.is_empty() {
        if globals.verbose && !globals.json && !globals.quiet {
            eprintln!("embed: missing/skipped fonts: {}", missing.join(", "));
        }
        if args.on_missing == MissingFontAction::Fail {
            return Err(format!("missing/skipped fonts: {}", missing.join(", ")));
        }
    }

    Ok(resolved)
}

fn subset_resolved_fonts(
    globals: &GlobalOptions,
    args: &EmbedArgs,
    fonts: &[ResolvedEmbedFont],
) -> Result<Vec<engine::FontSubsetPayload>, String> {
    let mut payloads = Vec::new();
    let mut skipped = Vec::new();

    for font in fonts {
        match app_lib::fonts::subset_font(font.path.clone(), font.index, font.codepoints.clone()) {
            Ok(data) => payloads.push(engine::FontSubsetPayload {
                font_name: font.font_name.clone(),
                data,
            }),
            Err(error) => skipped.push(format!("{} ({error})", font.label)),
        }
    }

    if !skipped.is_empty() {
        if globals.verbose && !globals.json && !globals.quiet {
            eprintln!("embed: skipped fonts: {}", skipped.join(", "));
        }
        if args.on_missing == MissingFontAction::Fail {
            return Err(format!("skipped fonts: {}", skipped.join(", ")));
        }
    }

    Ok(payloads)
}

fn resolve_embed_font(
    args: &EmbedArgs,
    use_user_fonts: bool,
    font: &engine::FontEmbedUsage,
) -> Result<Option<(String, u32)>, String> {
    if use_user_fonts {
        if let Some(found) =
            app_lib::fonts::resolve_user_font(font.family.clone(), font.bold, font.italic)?
        {
            return Ok(Some((found.path, found.index)));
        }
    }

    if args.no_system_fonts {
        return Ok(None);
    }

    app_lib::fonts::find_system_font(font.family.clone(), font.bold, font.italic)
        .map(|found| Some((found.path, found.index)))
        .or_else(|error| {
            if error.starts_with("Font not found:") {
                Ok(None)
            } else {
                Err(error)
            }
        })
}

fn has_ass_extension(path: &Path) -> bool {
    path.extension()
        .and_then(|ext| ext.to_str())
        .map(|ext| matches!(ext.to_ascii_lowercase().as_str(), "ass" | "ssa"))
        .unwrap_or(false)
}

fn run_rename(globals: &GlobalOptions, args: RenameArgs) -> Result<ExitCode, String> {
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
        _ => {}
    }

    let expanded_paths = expand_rename_inputs(&args.paths)?;
    let mut engine = engine::CliEngine::new()?;
    let request = engine::RenamePlanRequest {
        paths: expanded_paths,
        mode: args.mode.as_engine_value().to_string(),
        output_dir: output_dir.as_deref().map(display_path),
        langs: args.langs.clone(),
    };
    let plan = engine.plan_rename(&request)?;
    let mut report = CommandReport::new("rename");

    if globals.verbose && !globals.json && !globals.quiet {
        println!(
            "rename: {} videos, {} subtitles, {} ignored, {} unknown",
            plan.video_count, plan.subtitle_count, plan.ignored_count, plan.unknown_count
        );
    }

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
        };
        emit_file_report(globals, &result);
        report.push(result);
        emit_report_summary(globals, &report)?;
        return Ok(report.exit_code());
    }

    let mut seen_outputs = HashSet::new();
    for row in &plan.pairings {
        let result = process_rename_pair(globals, &args, row, &mut seen_outputs);
        emit_file_report(globals, &result);
        report.push(result);
    }

    emit_report_summary(globals, &report)?;
    Ok(report.exit_code())
}

fn expand_rename_inputs(paths: &[PathBuf]) -> Result<Vec<String>, String> {
    let absolute_paths: Result<Vec<String>, String> = paths
        .iter()
        .map(|path| absolute_path(path).map(|path| display_path(&path)))
        .collect();
    let expanded = app_lib::dropzone::expand_dropped_paths(absolute_paths?)?;

    if expanded.is_empty() {
        return Err("no regular files found in rename input paths".to_string());
    }
    Ok(expanded)
}

fn process_rename_pair(
    globals: &GlobalOptions,
    args: &RenameArgs,
    row: &engine::RenamePlanRow,
    seen_outputs: &mut HashSet<String>,
) -> FileReport {
    let input_path = PathBuf::from(&row.input_path);
    let output_path = PathBuf::from(&row.output_path);
    let input = display_path(&input_path);
    let output = display_path(&output_path);

    if row.no_op {
        return FileReport {
            input,
            output: Some(output),
            encoding: None,
            status: FileStatus::Skipped,
            error: Some("subtitle already matches the target path".to_string()),
        };
    }

    let output_key = normalize_output_key(&output_path);
    if !seen_outputs.insert(output_key) {
        return failed_report(
            &input_path,
            Some(output),
            None,
            "duplicate output path in planned batch".to_string(),
        );
    }

    if output_path_exists(&output_path) && !globals.overwrite {
        return FileReport {
            input,
            output: Some(output),
            encoding: None,
            status: FileStatus::Skipped,
            error: Some("output exists; pass --overwrite to replace it".to_string()),
        };
    }

    if globals.verbose && !globals.json && !globals.quiet {
        println!(
            "rename: {} -> {} (video: {})",
            display_path(&input_path),
            display_path(&output_path),
            row.video_path
        );
    }

    if globals.dry_run {
        return FileReport {
            input,
            output: Some(output),
            encoding: None,
            status: FileStatus::Planned,
            error: None,
        };
    }

    let operation_result = if args.mode.is_copy() {
        copy_file_output(&input_path, &output_path)
    } else {
        rename_file_output(&input_path, &output_path, globals.overwrite)
    };

    if let Err(error) = operation_result {
        return failed_report(&input_path, Some(output), None, error);
    }

    FileReport {
        input,
        output: Some(output),
        encoding: None,
        status: FileStatus::Written,
        error: None,
    }
}

fn emit_report_summary(globals: &GlobalOptions, report: &CommandReport) -> Result<(), String> {
    if globals.json {
        let json = serde_json::to_string_pretty(report)
            .map_err(|err| format!("failed to encode JSON report: {err}"))?;
        println!("{json}");
    } else if !globals.quiet {
        let message = localize(
            globals,
            format!(
                "Done: {} written, {} planned, {} skipped, {} failed",
                report.written, report.planned, report.skipped, report.failed
            ),
            format!(
                "完成：{} 个已写入，{} 个计划写入，{} 个已跳过，{} 个失败",
                report.written, report.planned, report.skipped, report.failed
            ),
        );
        println!("{message}");
    }
    Ok(())
}

fn emit_file_report(globals: &GlobalOptions, result: &FileReport) {
    if globals.json {
        return;
    }

    if matches!(result.status, FileStatus::Failed) {
        if let Some(error) = &result.error {
            eprintln!("failed: {} ({error})", result.input);
        }
        return;
    }

    if globals.quiet {
        return;
    }

    let Some(output) = &result.output else {
        return;
    };

    match result.status {
        FileStatus::Written => {
            if globals.verbose {
                let encoding = result.encoding.as_deref().unwrap_or("unknown");
                println!("written: {} -> {} ({encoding})", result.input, output);
            } else {
                println!("written: {output}");
            }
        }
        FileStatus::Planned => println!("would write: {output}"),
        FileStatus::Skipped => println!("skipped: {output}"),
        FileStatus::Failed => {}
    }
}

fn failed_report(
    input: impl AsRef<Path>,
    output: Option<String>,
    encoding: Option<String>,
    error: String,
) -> FileReport {
    FileReport {
        input: display_path(input.as_ref()),
        output,
        encoding,
        status: FileStatus::Failed,
        error: Some(error),
    }
}

fn absolute_path(path: &Path) -> Result<PathBuf, String> {
    if path.is_absolute() {
        return Ok(path.to_path_buf());
    }

    std::env::current_dir()
        .map(|cwd| cwd.join(path))
        .map_err(|err| format!("failed to resolve current directory: {err}"))
}

fn relocate_output_path(path: &str, output_dir: Option<&Path>) -> Result<PathBuf, String> {
    let path = PathBuf::from(path);
    let Some(output_dir) = output_dir else {
        return Ok(path);
    };

    let file_name = path
        .file_name()
        .ok_or_else(|| "engine returned an output path without a filename".to_string())?;
    Ok(output_dir.join(file_name))
}

fn output_path_exists(path: &Path) -> bool {
    match fs::metadata(path) {
        Ok(_) => true,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => false,
        Err(_) => true,
    }
}

fn write_output(path: &Path, content: &str) -> Result<(), String> {
    let parent = path
        .parent()
        .ok_or_else(|| "output path has no parent directory".to_string())?;
    fs::create_dir_all(parent)
        .map_err(|err| format!("failed to create output directory: {err}"))?;
    fs::write(path, content.as_bytes()).map_err(|err| format!("failed to write output: {err}"))
}

fn copy_file_output(input: &Path, output: &Path) -> Result<(), String> {
    ensure_output_parent(output)?;
    fs::copy(input, output)
        .map(|_| ())
        .map_err(|err| format!("failed to copy file: {err}"))
}

fn rename_file_output(input: &Path, output: &Path, overwrite: bool) -> Result<(), String> {
    ensure_output_parent(output)?;
    if overwrite && output_path_exists(output) {
        fs::remove_file(output)
            .map_err(|err| format!("failed to remove existing output before rename: {err}"))?;
    }
    fs::rename(input, output).map_err(|err| format!("failed to rename file: {err}"))
}

fn ensure_output_parent(path: &Path) -> Result<(), String> {
    let parent = path
        .parent()
        .ok_or_else(|| "output path has no parent directory".to_string())?;
    fs::create_dir_all(parent).map_err(|err| format!("failed to create output directory: {err}"))
}

fn display_path(path: &Path) -> String {
    let path = path.to_string_lossy().into_owned();
    if cfg!(windows) {
        path.replace('/', "\\")
    } else {
        path
    }
}

fn normalize_output_key(path: &Path) -> String {
    display_path(path).replace('/', "\\").to_lowercase()
}

fn localize(globals: &GlobalOptions, en: String, zh: String) -> String {
    match globals.lang.unwrap_or(OutputLang::En) {
        OutputLang::En => en,
        OutputLang::Zh => zh,
    }
}

fn parse_duration_ms(input: &str) -> Result<i64, String> {
    let mut rest = input.trim();
    if rest.is_empty() {
        return Err("offset cannot be empty".to_string());
    }

    let sign = if let Some(stripped) = rest.strip_prefix('-') {
        rest = stripped;
        -1.0
    } else if let Some(stripped) = rest.strip_prefix('+') {
        rest = stripped;
        1.0
    } else {
        1.0
    };

    if rest.is_empty() {
        return Err("offset has no duration value".to_string());
    }

    let bytes = rest.as_bytes();
    let mut index = 0;
    let mut total = 0.0;

    while index < bytes.len() {
        let value_start = index;
        let mut saw_dot = false;
        while index < bytes.len() {
            let ch = bytes[index] as char;
            if ch.is_ascii_digit() {
                index += 1;
            } else if ch == '.' && !saw_dot {
                saw_dot = true;
                index += 1;
            } else {
                break;
            }
        }
        if value_start == index {
            return Err(format!("invalid duration near '{}'", &rest[value_start..]));
        }

        let value: f64 = rest[value_start..index]
            .parse()
            .map_err(|_| format!("invalid duration value '{}'", &rest[value_start..index]))?;

        let unit_start = index;
        while index < bytes.len() && (bytes[index] as char).is_ascii_alphabetic() {
            index += 1;
        }
        if unit_start == index {
            return Err(format!(
                "missing duration unit after '{}'",
                &rest[value_start..unit_start]
            ));
        }

        let factor = match &rest[unit_start..index].to_ascii_lowercase()[..] {
            "ms" => 1.0,
            "s" => 1000.0,
            "m" => 60_000.0,
            "h" => 3_600_000.0,
            unit => return Err(format!("unsupported duration unit '{unit}'")),
        };
        total += value * factor;
    }

    if !total.is_finite() {
        return Err("offset is not finite".to_string());
    }
    Ok((sign * total).round() as i64)
}

fn parse_timestamp_ms(input: &str) -> Result<i64, String> {
    let trimmed = input.trim();
    let parts: Vec<&str> = trimmed.split(':').collect();
    if parts.len() != 3 {
        return Err(format!(
            "invalid timestamp '{trimmed}'; expected HH:MM:SS or HH:MM:SS.mmm"
        ));
    }

    let hours = parse_timestamp_part(parts[0], "hours")?;
    let minutes = parse_timestamp_part(parts[1], "minutes")?;
    let (seconds_text, millis_text) = parts[2]
        .split_once('.')
        .or_else(|| parts[2].split_once(','))
        .unwrap_or((parts[2], ""));
    let seconds = parse_timestamp_part(seconds_text, "seconds")?;

    if minutes > 59 || seconds > 59 {
        return Err(format!(
            "invalid timestamp '{trimmed}'; minutes and seconds must be 00-59"
        ));
    }

    let millis = if millis_text.is_empty() {
        0
    } else if millis_text.len() <= 3 && millis_text.chars().all(|ch| ch.is_ascii_digit()) {
        millis_text
            .chars()
            .chain(std::iter::repeat('0'))
            .take(3)
            .collect::<String>()
            .parse::<i64>()
            .map_err(|_| format!("invalid millisecond part in timestamp '{trimmed}'"))?
    } else {
        return Err(format!("invalid millisecond part in timestamp '{trimmed}'"));
    };

    Ok(hours * 3_600_000 + minutes * 60_000 + seconds * 1000 + millis)
}

fn parse_timestamp_part(part: &str, label: &str) -> Result<i64, String> {
    if part.is_empty() || !part.chars().all(|ch| ch.is_ascii_digit()) {
        return Err(format!("invalid {label} value '{part}'"));
    }
    part.parse::<i64>()
        .map_err(|_| format!("invalid {label} value '{part}'"))
}

#[cfg(test)]
mod tests {
    use super::{parse_duration_ms, parse_timestamp_ms};

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
}
