use std::collections::HashSet;
use std::fs;
use std::io::{ErrorKind, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::OnceLock;
use std::time::{SystemTime, UNIX_EPOCH};

use clap::{Args, Parser, Subcommand, ValueEnum};
use serde::Serialize;
use unicode_normalization::UnicodeNormalization;

mod engine;

const MAX_SHIFT_OFFSET_MS: i64 = 365 * 24 * 60 * 60 * 1000;
const CLI_FONT_DB_DIR_PREFIX: &str = "ssahdrify-cli-font-db";
const CLI_FONT_DB_FILENAME: &str = "user-font-sources.session.sqlite3";

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

    /// Output language. Defaults to OS locale (zh* → zh, otherwise en).
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
    /// Non-fatal warnings carried alongside a successful or planned
    /// result. Currently used by embed under `--on-missing warn` to
    /// surface unresolved / failed-to-subset fonts to JSON consumers
    /// (without warnings here, JSON callers couldn't distinguish
    /// "all fonts embedded" from "embedded what we found").
    /// `serde(skip_serializing_if)` keeps the JSON shape backward-
    /// compatible: the field is absent unless something actually
    /// warned.
    #[serde(skip_serializing_if = "Option::is_none")]
    warnings: Option<Vec<String>>,
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

struct ShiftProcessContext<'a> {
    offset_ms: i64,
    threshold_ms: Option<i64>,
    output_dir: Option<&'a Path>,
}

struct TempFontDbDir(PathBuf);

impl Drop for TempFontDbDir {
    fn drop(&mut self) {
        for suffix in ["", "-journal", "-wal", "-shm"] {
            let _ = fs::remove_file(self.0.join(format!("{CLI_FONT_DB_FILENAME}{suffix}")));
        }
        let _ = fs::remove_dir(&self.0);
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
    init_logger();
    match run() {
        Ok(code) => code,
        Err(err) => {
            eprintln!("ssahdrify-cli: {err}");
            ExitCode::from(2)
        }
    }
}

// Wire a stderr-targeted env_logger so library-side `log::warn!` and
// `log::error!` calls (dropzone path-rejections, font-scan canonicalize
// failures, font-kit lookup details) become visible to CLI users.
// Without an init, the log crate's default null logger discards every
// message, leaving the user blind to "why did expand_dropped_paths
// return empty?"-class issues. Default level `warn` keeps the happy
// path quiet; `RUST_LOG=info` opens diagnostic detail.
fn init_logger() {
    let _ = env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("warn"))
        .target(env_logger::Target::Stderr)
        .try_init();
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
    // First-input-wins dedup is intentional for non-destructive
    // transformations (HDR/Shift/Embed): the second input's work is
    // wasted but no source data is lost. Rename takes the opposite
    // all-fail policy (see duplicate_rename_output_keys) because
    // picking a "winner" there could move the wrong file. See
    // ssahdrify_cli_design.md § Cross-cutting 行为.
    let mut seen_outputs = HashSet::new();

    for file in &args.files {
        let result = process_hdr_file(
            globals,
            &args,
            output_dir.as_deref(),
            &mut engine,
            file,
            &mut seen_outputs,
        );
        emit_file_report(globals, &result);
        report.push(result);
    }

    emit_report_summary(globals, &report)?;
    Ok(report.exit_code())
}

fn process_hdr_file(
    globals: &GlobalOptions,
    args: &HdrArgs,
    output_dir: Option<&Path>,
    engine: &mut engine::CliEngine,
    file: &Path,
    seen_outputs: &mut HashSet<String>,
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

    if let Some(early) = dedup_and_exists_check(
        globals,
        &input_path,
        &output_path,
        &output,
        None,
        seen_outputs,
    ) {
        return early;
    }

    if globals.dry_run {
        // Dry-run gates BEFORE the read so cheap-first lives up to its
        // name on `--dry-run` invocations: no I/O, no V8 work, just the
        // resolved path. encoding is None because we haven't read —
        // matches the cheap-first contract (Embed already does this).
        return planned_report(&input_path, Some(output), None);
    }

    let read_result = match app_lib::encoding::read_text_detect_encoding(input.clone()) {
        Ok(result) => result,
        Err(error) => return failed_report(&input_path, Some(output), None, error),
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
            return failed_report(&input_path, Some(output), Some(read_result.encoding), error);
        }
    };

    if let Err(error) = write_output(
        globals,
        &output_path,
        &conversion.content,
        globals.overwrite,
    ) {
        return failed_report(&input_path, Some(output), Some(read_result.encoding), error);
    }

    FileReport {
        input,
        output: Some(output),
        encoding: Some(read_result.encoding),
        status: FileStatus::Written,
        error: None,
        warnings: None,
    }
}

fn run_shift(globals: &GlobalOptions, args: ShiftArgs) -> Result<ExitCode, String> {
    let offset_ms = parse_duration_ms(&args.offset)?;
    let threshold_ms = args.after.as_deref().map(parse_timestamp_ms).transpose()?;
    let mut engine = engine::CliEngine::new()?;
    let output_dir = globals
        .output_dir
        .as_deref()
        .map(absolute_path)
        .transpose()?;
    let mut report = CommandReport::new("shift");
    // Same first-wins dedup policy as run_hdr. Shift now does
    // cheap-first dedup for the common case (default template, no
    // `{format}` token). Templates that reference `{format}` need
    // parsing content to resolve and fall back to heavy-first ordering
    // inside process_shift_file_heavy_first.
    let mut seen_outputs = HashSet::new();

    for file in &args.files {
        let result = process_shift_file(
            globals,
            &args,
            &ShiftProcessContext {
                offset_ms,
                threshold_ms,
                output_dir: output_dir.as_deref(),
            },
            &mut engine,
            file,
            &mut seen_outputs,
        );
        emit_file_report(globals, &result);
        report.push(result);
    }

    emit_report_summary(globals, &report)?;
    Ok(report.exit_code())
}

fn process_shift_file(
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

// Shared post-resolve check used by HDR, Shift (cheap + heavy), and
// Embed dispatchers. Returns `Some(FileReport)` when the file should
// short-circuit (duplicate output in the same batch, or pre-existing
// output without --overwrite), `None` to proceed. Encoding is taken by
// reference so the caller can pass `Some(&read.encoding)` (heavy-first,
// after read) or `None` (cheap-first, before read). Returns `Option`
// rather than `Result` because FileReport is large (>128 bytes); a
// Result variant would trip clippy::result_large_err.
fn dedup_and_exists_check(
    globals: &GlobalOptions,
    input_path: &Path,
    output_path: &Path,
    output: &str,
    encoding: Option<&str>,
    seen_outputs: &mut HashSet<String>,
) -> Option<FileReport> {
    let cloned_encoding = || encoding.map(|s| s.to_string());
    if !seen_outputs.insert(normalize_output_key(output_path)) {
        return Some(failed_report(
            input_path,
            Some(output.to_string()),
            cloned_encoding(),
            "duplicate output path in planned batch".to_string(),
        ));
    }
    if output_path_exists(globals, output_path) && !globals.overwrite {
        return Some(skipped_report(
            input_path,
            Some(output.to_string()),
            cloned_encoding(),
            "output exists; pass --overwrite to replace it".to_string(),
        ));
    }
    None
}

fn build_shift_request(
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
        output_template,
    }
}

fn process_shift_file_cheap_first(
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

    if let Some(early) = dedup_and_exists_check(
        globals,
        &input_path,
        &output_path,
        &output,
        None,
        seen_outputs,
    ) {
        return early;
    }

    if globals.dry_run {
        // Dry-run gates BEFORE the read so cheap-first lives up to its
        // name on `--dry-run` invocations. encoding is None because we
        // haven't read — matches the cheap-first contract.
        return planned_report(&input_path, Some(output), None);
    }

    let read_result = match app_lib::encoding::read_text_detect_encoding(input.clone()) {
        Ok(result) => result,
        Err(error) => return failed_report(&input_path, Some(output), None, error),
    };

    let request = build_shift_request(
        input.clone(),
        read_result.text,
        context,
        args.output_template.clone(),
    );

    let conversion = match engine.convert_shift(&request) {
        Ok(result) => result,
        Err(error) => {
            return failed_report(&input_path, Some(output), Some(read_result.encoding), error);
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

    if let Err(error) = write_output(
        globals,
        &output_path,
        &conversion.content,
        globals.overwrite,
    ) {
        return failed_report(&input_path, Some(output), Some(read_result.encoding), error);
    }

    FileReport {
        input,
        output: Some(output),
        encoding: Some(read_result.encoding),
        status: FileStatus::Written,
        error: None,
        warnings: None,
    }
}

fn process_shift_file_heavy_first(
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

    let read_result = match app_lib::encoding::read_text_detect_encoding(input.clone()) {
        Ok(result) => result,
        Err(error) => return failed_report(&input_path, None, None, error),
    };

    let request = build_shift_request(
        input.clone(),
        read_result.text,
        context,
        args.output_template.clone(),
    );

    let conversion = match engine.convert_shift(&request) {
        Ok(result) => result,
        Err(error) => {
            return failed_report(&input_path, None, Some(read_result.encoding), error);
        }
    };

    let output_path = match relocate_output_path(&conversion.output_path, context.output_dir) {
        Ok(path) => path,
        Err(error) => {
            return failed_report(&input_path, None, Some(read_result.encoding), error);
        }
    };
    let output = display_path(&output_path);

    if let Some(early) = dedup_and_exists_check(
        globals,
        &input_path,
        &output_path,
        &output,
        Some(&read_result.encoding),
        seen_outputs,
    ) {
        return early;
    }

    // Dry-run gates BEFORE the verbose progress print: a
    // `--dry-run --verbose` invocation should NOT emit the "shift: N
    // captions, M shifted" line because no shift was actually
    // committed. Matches the cheap-first path's ordering.
    if globals.dry_run {
        return planned_report(&input_path, Some(output), Some(read_result.encoding));
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
        return failed_report(&input_path, Some(output), Some(read_result.encoding), error);
    }

    FileReport {
        input,
        output: Some(output),
        encoding: Some(read_result.encoding),
        status: FileStatus::Written,
        error: None,
        warnings: None,
    }
}

fn run_embed(globals: &GlobalOptions, args: EmbedArgs) -> Result<ExitCode, String> {
    app_lib::fonts::init_system_dirs();
    let use_user_fonts = !args.font_dirs.is_empty() || !args.font_files.is_empty();
    // Skip the user-font scan in dry-run mode. process_embed_file's
    // cheap-first ordering returns Planned BEFORE reading content or
    // resolving fonts, so dry-run never needs the SQLite source index.
    // Saves a 17k-font-folder scan when the user passes --font-dir
    // alongside --dry-run just to preview output paths.
    let _font_db_dir = if use_user_fonts && !globals.dry_run {
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
    // Same first-wins dedup policy as run_hdr. Embed already orders
    // dedup correctly (cheap plan_font_embed → dedup → expensive
    // subset+apply), so no JS work is wasted on duplicate batches.
    let mut seen_outputs = HashSet::new();

    for file in &args.files {
        let result = process_embed_file(
            globals,
            &args,
            use_user_fonts,
            output_dir.as_deref(),
            &mut engine,
            file,
            &mut seen_outputs,
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
    // Wrap the temp dir in TempFontDbDir IMMEDIATELY so any `?` in the
    // init/import sequence below drops the guard and runs the cleanup.
    // The earlier shape (return Ok(TempFontDbDir(db_dir)) only at the
    // end) leaked the directory on every failure between create and
    // return.
    let guard = TempFontDbDir(create_cli_font_db_dir()?);
    app_lib::fonts::init_user_font_db(&guard.0)?;

    for (index, dir) in args.font_dirs.iter().enumerate() {
        let dir = absolute_path(dir)?;
        let source_id = format!("cli-dir-{index}");
        let summary = app_lib::fonts::import_font_directory_for_cli(&dir, &source_id)?;
        emit_font_source_summary(globals, "font dir", "字体目录", Some(&dir), &summary);
    }

    if !args.font_files.is_empty() {
        let paths: Result<Vec<String>, String> = args
            .font_files
            .iter()
            .map(|path| absolute_path(path).map(|path| display_path(&path)))
            .collect();
        let summary = app_lib::fonts::import_font_files_for_cli(paths?, "cli-files")?;
        // Funnel through emit_font_source_summary so ScanStopReason
        // (UserCancel / CeilingHit) surfaces with the same suffix as
        // font-dir summaries. Path is None — font files are a flat
        // list without a single "source path."
        emit_font_source_summary(globals, "font files", "字体文件", None, &summary);
    }

    Ok(guard)
}

fn create_cli_font_db_dir() -> Result<PathBuf, String> {
    let base = std::env::temp_dir();
    let pid = std::process::id();
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();

    for attempt in 0..1000u16 {
        let candidate = base.join(format!("{CLI_FONT_DB_DIR_PREFIX}-{pid}-{stamp}-{attempt}"));
        match fs::create_dir(&candidate) {
            Ok(()) => return Ok(candidate),
            Err(error) if error.kind() == ErrorKind::AlreadyExists => continue,
            Err(error) => {
                return Err(format!(
                    "failed to create temporary font database directory: {error}"
                ));
            }
        }
    }

    Err("failed to allocate a unique temporary font database directory".to_string())
}

fn emit_font_source_summary(
    globals: &GlobalOptions,
    label_en: &str,
    label_zh: &str,
    path: Option<&Path>,
    summary: &app_lib::fonts::FontSourceImportSummary,
) {
    if !globals.verbose || globals.json || globals.quiet {
        return;
    }
    let (reason_en, reason_zh) = match summary.reason {
        app_lib::fonts::ScanStopReason::Natural => ("", ""),
        app_lib::fonts::ScanStopReason::UserCancel => (" (cancelled)", "（已取消）"),
        app_lib::fonts::ScanStopReason::CeilingHit => (" (ceiling hit)", "（已达上限）"),
    };
    // Path suffix is optional: font dirs always have one; font files
    // are a flat list with no single "source path" so the suffix is
    // omitted in that case.
    let (path_suffix_en, path_suffix_zh) = match path {
        Some(p) => {
            let display = display_path(p);
            (format!(" ({display})"), format!("（{display}）"))
        }
        None => (String::new(), String::new()),
    };
    println!(
        "{}",
        localize(
            globals,
            format!(
                "{label_en}: {} faces scanned, {} added, {} duplicated{reason_en}{path_suffix_en}",
                summary.total, summary.added, summary.duplicated
            ),
            format!(
                "{label_zh}：扫描 {} 个字体，{} 个已添加，{} 个已去重{reason_zh}{path_suffix_zh}",
                summary.total, summary.added, summary.duplicated
            ),
        )
    );
}

fn process_embed_file(
    globals: &GlobalOptions,
    args: &EmbedArgs,
    use_user_fonts: bool,
    output_dir: Option<&Path>,
    engine: &mut engine::CliEngine,
    file: &Path,
    seen_outputs: &mut HashSet<String>,
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
        Err(error) => return failed_report(&input_path, None, None, error),
    };

    let output_path = match relocate_output_path(&resolved_output_path, output_dir) {
        Ok(path) => path,
        Err(error) => return failed_report(&input_path, None, None, error),
    };
    let output = display_path(&output_path);

    if let Some(early) = dedup_and_exists_check(
        globals,
        &input_path,
        &output_path,
        &output,
        None,
        seen_outputs,
    ) {
        return early;
    }

    if globals.dry_run {
        // Dry-run for embed reports the planned output path without
        // doing font discovery or content parsing — matches HDR/Shift
        // dry-run behavior and avoids the surprise of "dry-run scanned
        // 17k fonts then planned no actual write."
        return planned_report(&input_path, Some(output), None);
    }

    let read_result = match app_lib::encoding::read_text_detect_encoding(input.clone()) {
        Ok(result) => result,
        Err(error) => return failed_report(&input_path, Some(output), None, error),
    };

    let plan_request = engine::FontEmbedPlanRequest {
        input_path: input.clone(),
        content: read_result.text.clone(),
        output_template: args.output_template.clone(),
    };
    let plan = match engine.plan_font_embed(&plan_request) {
        Ok(result) => result,
        Err(error) => {
            return failed_report(&input_path, Some(output), Some(read_result.encoding), error);
        }
    };

    let mut warnings: Vec<String> = Vec::new();

    let resolved_fonts = match resolve_embed_fonts(globals, args, use_user_fonts, &plan.fonts) {
        Ok((fonts, mut resolve_warnings)) => {
            warnings.append(&mut resolve_warnings);
            fonts
        }
        Err(error) => {
            return failed_report(&input_path, Some(output), Some(read_result.encoding), error);
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

    let subset_payloads = match subset_resolved_fonts(globals, args, &resolved_fonts) {
        Ok((payloads, mut subset_warnings)) => {
            warnings.append(&mut subset_warnings);
            payloads
        }
        Err(error) => {
            return failed_report(&input_path, Some(output), Some(read_result.encoding), error);
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
        let apply_request = engine::FontEmbedApplyRequest {
            content: read_result.text,
            fonts: subset_payloads,
        };
        match engine.apply_font_embed(&apply_request) {
            Ok(result) => result,
            Err(error) => {
                return failed_report(&input_path, Some(output), Some(read_result.encoding), error);
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
        return failed_report(&input_path, Some(output), Some(read_result.encoding), error);
    }

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
    }
}

/// Resolve fonts; under `--on-missing warn`, returns the resolved
/// list AND the missing-font diagnostics so the caller can surface
/// them in `FileReport.warnings` (not just on stderr).
/// Under `--on-missing fail`, returns Err on any missing font.
fn resolve_embed_fonts(
    globals: &GlobalOptions,
    args: &EmbedArgs,
    use_user_fonts: bool,
    fonts: &[engine::FontEmbedUsage],
) -> Result<(Vec<ResolvedEmbedFont>, Vec<String>), String> {
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
        let joined = missing.join(", ");
        emit_verbose_err(
            globals,
            format!("embed: missing/skipped fonts: {joined}"),
            format!("字体嵌入：缺失/跳过的字体：{joined}"),
        );
        if args.on_missing == MissingFontAction::Fail {
            return Err(format!("missing/skipped fonts: {joined}"));
        }
    }

    let warnings = missing
        .into_iter()
        .map(|m| format!("missing font: {m}"))
        .collect();
    Ok((resolved, warnings))
}

/// Subset fonts; under `--on-missing warn`, returns successful
/// payloads AND the skipped-font diagnostics for `FileReport.warnings`.
fn subset_resolved_fonts(
    globals: &GlobalOptions,
    args: &EmbedArgs,
    fonts: &[ResolvedEmbedFont],
) -> Result<(Vec<engine::FontSubsetPayload>, Vec<String>), String> {
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
            // String-coupled to fonts.rs's `format!("Font not found: ...)`.
            // Any change to that prefix in fonts.rs MUST update this
            // matcher; otherwise a "miss" becomes a hard Err and breaks
            // --on-missing warn semantics. fonts.rs has the matching
            // WHY comment at the format-string site.
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
        (RenameMode::Rename | RenameMode::CopyToVideo, None) => {}
        (RenameMode::CopyToChosen, Some(_)) => {}
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
        emit_report_summary(globals, &report)?;
        return Ok(report.exit_code());
    }

    let duplicate_outputs = duplicate_rename_output_keys(&plan.pairings);
    for row in &plan.pairings {
        let result = process_rename_pair(globals, &args, row, &duplicate_outputs);
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
    duplicate_outputs: &HashSet<String>,
) -> FileReport {
    let input_path = PathBuf::from(&row.input_path);
    let output_path = PathBuf::from(&row.output_path);
    let input = display_path(&input_path);
    let output = display_path(&output_path);

    if row.no_op {
        return skipped_report(
            &input_path,
            Some(output),
            None,
            "subtitle already matches the target path".to_string(),
        );
    }

    let output_key = normalize_output_key(&output_path);
    if duplicate_outputs.contains(&output_key) {
        return failed_report(
            &input_path,
            Some(output),
            None,
            "duplicate output path in planned batch".to_string(),
        );
    }

    if output_path_exists(globals, &output_path) && !globals.overwrite {
        return skipped_report(
            &input_path,
            Some(output),
            None,
            "output exists; pass --overwrite to replace it".to_string(),
        );
    }

    let from = display_path(&input_path);
    let to = display_path(&output_path);
    let video = &row.video_path;
    emit_verbose(
        globals,
        format!("rename: {from} -> {to} (video: {video})"),
        format!("重命名：{from} -> {to}（视频：{video}）"),
    );

    if globals.dry_run {
        return planned_report(&input_path, Some(output), None);
    }

    let operation_result = if args.mode.is_copy() {
        copy_file_output(globals, &input_path, &output_path, globals.overwrite)
    } else {
        rename_file_output(globals, &input_path, &output_path, globals.overwrite)
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
        warnings: None,
    }
}

fn duplicate_rename_output_keys(rows: &[engine::RenamePlanRow]) -> HashSet<String> {
    // All-fail dedup (not first-wins) for rename: rename is destructive,
    // so picking a "winner" among duplicates risks moving the wrong
    // file into a stable name. Every participant in a duplicate set is
    // flagged here and refuses to act in process_rename_pair. See
    // ssahdrify_cli_design.md § Cross-cutting 行为.
    //
    // No-op rows DO claim their output key — a no-op row's output is a
    // real file already on disk, so a non-no-op row targeting the same
    // key would silently overwrite it under --overwrite. The no-op row
    // itself is still skipped at process_rename_pair (no_op branch
    // returns Skipped before the duplicate check); the conflict signal
    // lands on the colliding non-no-op rows.
    let mut seen = HashSet::new();
    let mut duplicates = HashSet::new();

    for row in rows {
        let key = normalize_output_key(Path::new(&row.output_path));
        if !seen.insert(key.clone()) {
            duplicates.insert(key);
        }
    }

    duplicates
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

    // Status line first. Failed always surfaces to stderr regardless
    // of --quiet (it's an error, not output); other statuses respect
    // --quiet.
    if matches!(result.status, FileStatus::Failed) {
        if let Some(error) = &result.error {
            eprintln!(
                "{}",
                localize(
                    globals,
                    format!("failed: {} ({error})", result.input),
                    format!("失败：{}（{error}）", result.input),
                )
            );
        }
    } else if !globals.quiet {
        if let Some(output) = &result.output {
            match result.status {
                FileStatus::Written => {
                    if globals.verbose {
                        let encoding = result.encoding.as_deref().unwrap_or("unknown");
                        println!(
                            "{}",
                            localize(
                                globals,
                                format!("written: {} -> {} ({encoding})", result.input, output),
                                format!("已写入：{} -> {}（{encoding}）", result.input, output),
                            )
                        );
                    } else {
                        println!(
                            "{}",
                            localize(
                                globals,
                                format!("written: {output}"),
                                format!("已写入：{output}"),
                            )
                        );
                    }
                }
                FileStatus::Planned => println!(
                    "{}",
                    localize(
                        globals,
                        format!("would write: {output}"),
                        format!("将写入：{output}"),
                    )
                ),
                FileStatus::Skipped => println!(
                    "{}",
                    localize(
                        globals,
                        format!("skipped: {output}"),
                        format!("已跳过：{output}"),
                    )
                ),
                FileStatus::Failed => {}
            }
        }
    }

    // Warnings: stderr, after the status line for ANY status — moved
    // out of the Failed early-return scope so a future
    // failed-with-warnings path (e.g., partial-success-with-critical-
    // error) doesn't silently drop them. Currently no failed_report
    // caller sets warnings; the structure is preventive. JSON mode
    // already returned; --quiet suppresses warnings too because the
    // user explicitly silenced output (Failed errors still surface
    // above as a hard exception to that rule).
    if !globals.quiet {
        if let Some(warnings) = &result.warnings {
            for warning in warnings {
                eprintln!(
                    "  {}",
                    localize(
                        globals,
                        format!("warning: {warning}"),
                        format!("警告：{warning}"),
                    )
                );
            }
        }
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
        warnings: None,
    }
}

fn skipped_report(
    input: impl AsRef<Path>,
    output: Option<String>,
    encoding: Option<String>,
    error: String,
) -> FileReport {
    FileReport {
        input: display_path(input.as_ref()),
        output,
        encoding,
        status: FileStatus::Skipped,
        error: Some(error),
        warnings: None,
    }
}

fn planned_report(
    input: impl AsRef<Path>,
    output: Option<String>,
    encoding: Option<String>,
) -> FileReport {
    FileReport {
        input: display_path(input.as_ref()),
        output,
        encoding,
        status: FileStatus::Planned,
        error: None,
        warnings: None,
    }
}

// Trust model: --output-dir is user-controlled CLI argument. We
// normalize it to absolute form here but DO NOT canonicalize (which
// would resolve symlinks). On Windows, fs::canonicalize returns the
// `\\?\C:\...` extended-path form which surprises downstream tools;
// on POSIX it would silently follow symlinks the user may have set
// up intentionally. The trust boundary is "the user supplied this
// path" — any symlinks they set up are theirs to manage.
fn absolute_path(path: &Path) -> Result<PathBuf, String> {
    if path.is_absolute() {
        return Ok(path.to_path_buf());
    }

    std::env::current_dir()
        .map(|cwd| cwd.join(path))
        .map_err(|err| format!("failed to resolve current directory: {err}"))
}

// Cap on the relocated output path — same 259-char buffer-fitting
// limit the JS validators apply (per the GUI design doc's path-
// validation extraction). A user-supplied --output-dir that's longer
// than the input dir can push the relocated path past MAX_PATH even
// if the JS resolver's pre-relocation path was within bounds.
// Long-local paths (`\\?\C:\...`) get the extended cap. UNC long
// paths keep the standard cap because the server side may not.
const RELOCATED_PATH_MAX_LEN: usize = 259;
const RELOCATED_LONG_PATH_MAX_LEN: usize = 32766;

fn relocate_output_path(path: &str, output_dir: Option<&Path>) -> Result<PathBuf, String> {
    let path = PathBuf::from(path);
    let Some(output_dir) = output_dir else {
        return Ok(path);
    };

    let file_name = path
        .file_name()
        .ok_or_else(|| "engine returned an output path without a filename".to_string())?;
    let relocated = output_dir.join(file_name);

    // Re-validate length on the relocated path. The JS validators saw
    // the pre-relocation path and signed off; relocation can grow it
    // beyond MAX_PATH if --output-dir itself is long.
    //
    // Count UTF-16 code units (NOT UTF-8 bytes), matching Windows
    // MAX_PATH semantics. CJK characters take 3 bytes in UTF-8 but
    // typically 1 UTF-16 code unit, so a `display.len()` (byte count)
    // would over-restrict CJK paths the OS would happily accept.
    let display = relocated.to_string_lossy();
    let lower = display.to_lowercase();
    let is_long_local = (lower.starts_with("\\\\?\\") && !lower.starts_with("\\\\?\\unc\\"))
        || (lower.starts_with("//?/") && !lower.starts_with("//?/unc/"));
    let cap = if is_long_local {
        RELOCATED_LONG_PATH_MAX_LEN
    } else {
        RELOCATED_PATH_MAX_LEN
    };
    let len = display.encode_utf16().count();
    if len > cap {
        return Err(format!(
            "relocated output path is too long ({len} chars, max {cap}); shorten --output-dir"
        ));
    }
    Ok(relocated)
}

fn output_path_exists(globals: &GlobalOptions, path: &Path) -> bool {
    match fs::metadata(path) {
        Ok(_) => true,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => false,
        Err(err) => {
            // Treat non-NotFound errors as "exists" so we never silently
            // overwrite a file we couldn't stat (restrictive ACLs,
            // network shares with metadata-read denied). Surface a
            // stderr warning so the user sees the real cause instead of
            // a misleading "skipped: output exists" diagnostic.
            let display = path.display();
            eprintln!(
                "{}",
                localize(
                    globals,
                    format!("warning: stat({display}) failed: {err}; treating as 'output exists'"),
                    format!("警告：stat({display}) 失败：{err}；按「输出存在」处理"),
                )
            );
            true
        }
    }
}

// TOCTOU note (applies to write_output / copy_file_output /
// rename_file_output): there's a small window between the
// `output_path_exists` skip-check or the `remove_file` overwrite step
// and the `OpenOptions::create_new(true).open(path)` below where
// another process in the same user context could swap the path. The
// window is bounded and the consequences are limited:
//   - `create_new(true)` is atomic at the OS level — refuses if the
//     path now exists, regardless of symlink. No through-symlink
//     write.
//   - On race, we get `ErrorKind::AlreadyExists` → "failed to create
//     output" — surfaced cleanly, no data corruption.
//   - The non-overwrite skip path returns early before any write
//     attempt, so no race there.
// Single-user desktop scope makes this acceptable; documented for
// future adversarial-review eyes.
fn write_output(
    globals: &GlobalOptions,
    path: &Path,
    content: &str,
    overwrite: bool,
) -> Result<(), String> {
    let parent = path
        .parent()
        .ok_or_else(|| "output path has no parent directory".to_string())?;
    fs::create_dir_all(parent)
        .map_err(|err| format!("failed to create output directory: {err}"))?;
    if overwrite && output_path_exists(globals, path) {
        fs::remove_file(path)
            .map_err(|err| format!("failed to remove existing output before write: {err}"))?;
    }

    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(|err| format!("failed to create output: {err}"))?;
    file.write_all(content.as_bytes())
        .map_err(|err| format!("failed to write output: {err}"))
}

fn copy_file_output(
    globals: &GlobalOptions,
    input: &Path,
    output: &Path,
    overwrite: bool,
) -> Result<(), String> {
    ensure_output_parent(output)?;

    if overwrite && output_path_exists(globals, output) {
        fs::remove_file(output)
            .map_err(|err| format!("failed to remove existing output before copy: {err}"))?;
    }

    let mut source = fs::File::open(input).map_err(|err| format!("failed to open input: {err}"))?;
    let mut destination = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(output)
        .map_err(|err| format!("failed to create output: {err}"))?;

    std::io::copy(&mut source, &mut destination)
        .map(|_| ())
        .map_err(|err| format!("failed to copy file: {err}"))
}

fn rename_file_output(
    globals: &GlobalOptions,
    input: &Path,
    output: &Path,
    overwrite: bool,
) -> Result<(), String> {
    ensure_output_parent(output)?;
    if overwrite && output_path_exists(globals, output) {
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
    let normalized = path
        .to_string_lossy()
        .replace('\\', "/")
        .nfc()
        .collect::<String>();
    if cfg!(windows) {
        normalized.to_lowercase()
    } else {
        normalized
    }
}

fn localize(globals: &GlobalOptions, en: String, zh: String) -> String {
    match globals.lang.unwrap_or_else(detect_os_locale) {
        OutputLang::En => en,
        OutputLang::Zh => zh,
    }
}

fn emit_verbose(globals: &GlobalOptions, en: String, zh: String) {
    if globals.verbose && !globals.json && !globals.quiet {
        println!("{}", localize(globals, en, zh));
    }
}

fn emit_verbose_err(globals: &GlobalOptions, en: String, zh: String) {
    if globals.verbose && !globals.json && !globals.quiet {
        eprintln!("{}", localize(globals, en, zh));
    }
}

// Detect OS UI locale once per process and cache it. sys-locale reads env
// vars (LC_ALL / LC_MESSAGES / LANG) on Unix and calls
// GetUserDefaultLocaleName on Windows — the same surface every other CLI
// tool uses. Empty / malformed locales fall through to En, matching the
// behavior users got before this detection landed.
fn detect_os_locale() -> OutputLang {
    static CACHED: OnceLock<OutputLang> = OnceLock::new();
    *CACHED.get_or_init(|| {
        sys_locale::get_locale()
            .map(|raw| classify_locale(&raw))
            .unwrap_or(OutputLang::En)
    })
}

// Classify a BCP-47 / POSIX locale tag by its primary subtag. We treat
// any tag whose primary is `zh` (zh, zh-CN, zh_TW.UTF-8, zh-Hans, ...)
// as Chinese; everything else, including empty strings, falls back to
// English. Split point chars cover `-`, `_`, `.` (POSIX charset suffix),
// and `@` (POSIX modifier).
fn classify_locale(raw: &str) -> OutputLang {
    let primary = raw
        .split(['-', '_', '.', '@'])
        .next()
        .unwrap_or("")
        .to_ascii_lowercase();
    if primary == "zh" {
        OutputLang::Zh
    } else {
        OutputLang::En
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
    // Bound BEFORE casting to i64. f64 -> i64 saturates at i64::MIN/MAX,
    // and i64::MIN.abs() wraps to i64::MIN in release mode — so a cap
    // check after the cast can be silently bypassed by inputs that
    // round to i64 saturation. Bounding the f64 first closes that path.
    let signed = sign * total;
    if signed.abs() > MAX_SHIFT_OFFSET_MS as f64 {
        return Err(format!(
            "offset is too large: max supported range is +/-{} ms",
            MAX_SHIFT_OFFSET_MS
        ));
    }
    Ok(signed.round() as i64)
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
    // Bound hours so the multiply below cannot wrap i64. 100k hours
    // (~11 years) is generous beyond any subtitle reality and keeps
    // hours * 3_600_000 well within i64 range. Without this cap a
    // pathological --after value like "9999999999999:00:00" would
    // panic in debug builds and silently wrap in release.
    if hours > 100_000 {
        return Err(format!(
            "invalid timestamp '{trimmed}'; hours value too large"
        ));
    }
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
    // i64::MAX is 19 digits (9223372036854775807). Anything > 19
    // digits unconditionally overflows; 19-digit values may or may
    // not fit (e.g., 9999999999999999999 is 19 digits and overflows).
    // Both branches surface "out of range" — the > 19 pre-check just
    // shortcuts the parse for clearly-too-large inputs.
    if part.len() > 19 {
        return Err(format!("{label} value '{part}' is out of range"));
    }
    part.parse::<i64>()
        .map_err(|_| format!("{label} value '{part}' is out of range"))
}

#[cfg(test)]
mod tests {
    use super::{
        classify_locale, copy_file_output, create_cli_font_db_dir, duplicate_rename_output_keys,
        engine, normalize_output_key, parse_duration_ms, parse_timestamp_ms, relocate_output_path,
        write_output, GlobalOptions, OutputLang, TempFontDbDir, CLI_FONT_DB_FILENAME,
    };
    use std::fs;
    use std::path::{Path, PathBuf};

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
        }
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
        // Concrete repro for round-2 N-R2-1: row 0 is a no-op (subtitle
        // already correctly named), row 1 wants to rename a different
        // subtitle onto that same target. The dedup must flag the
        // collision so process_rename_pair's --overwrite path doesn't
        // silently destroy row 0's existing file.
        let rows = vec![
            engine::RenamePlanRow {
                input_path: "C:\\Subs\\Episode.ass".to_string(),
                output_path: "C:\\Subs\\Episode.ass".to_string(),
                video_path: "C:\\Subs\\Episode.mkv".to_string(),
                no_op: true,
            },
            engine::RenamePlanRow {
                input_path: "C:\\Subs\\episode.tc.ass".to_string(),
                output_path: "C:\\Subs\\Episode.ass".to_string(),
                video_path: "C:\\Subs\\Episode.mkv".to_string(),
                no_op: false,
            },
        ];

        let duplicates = duplicate_rename_output_keys(&rows);
        let expected_key = if cfg!(windows) {
            "c:/subs/episode.ass"
        } else {
            "C:/Subs/Episode.ass"
        };
        assert!(
            duplicates.contains(expected_key),
            "no-op row's target should be claimed in the seen set"
        );
    }

    #[test]
    fn detects_duplicate_rename_outputs_before_writes() {
        let rows = vec![
            engine::RenamePlanRow {
                input_path: "C:\\Subs\\episode.sc.ass".to_string(),
                output_path: "C:\\Subs\\Episode.ass".to_string(),
                video_path: "C:\\Subs\\Episode.mkv".to_string(),
                no_op: false,
            },
            engine::RenamePlanRow {
                input_path: "C:\\Subs\\episode.tc.ass".to_string(),
                output_path: "C:\\Subs\\Episode.ass".to_string(),
                video_path: "C:\\Subs\\Episode.mkv".to_string(),
                no_op: false,
            },
            engine::RenamePlanRow {
                input_path: "C:\\Subs\\already.ass".to_string(),
                output_path: "C:\\Subs\\Episode.ass".to_string(),
                video_path: "C:\\Subs\\Episode.mkv".to_string(),
                no_op: true,
            },
        ];

        let duplicates = duplicate_rename_output_keys(&rows);
        assert_eq!(duplicates.len(), 1);
        let expected_key = if cfg!(windows) {
            "c:/subs/episode.ass"
        } else {
            "C:/Subs/Episode.ass"
        };
        assert!(duplicates.contains(expected_key));
    }

    #[test]
    fn output_keys_fold_slashes_and_unicode_normalization() {
        let decomposed = normalize_output_key(Path::new("C:\\Subs\\Cafe\u{301}.ass"));
        let precomposed = normalize_output_key(Path::new("C:/Subs/Caf\u{00e9}.ass"));
        assert_eq!(decomposed, precomposed);

        if cfg!(windows) {
            assert_eq!(precomposed, "c:/subs/caf\u{00e9}.ass");
        }
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
    fn relocate_output_path_rejects_overlong_relocated_path() {
        // A 300-char path comfortably exceeds the 259-char cap.
        let long_dir_name: String = "a".repeat(300);
        let out_dir = PathBuf::from(format!("C:\\{long_dir_name}"));
        let err =
            relocate_output_path("C:\\subs\\episode.shifted.ass", Some(&out_dir)).unwrap_err();
        assert!(
            err.contains("relocated output path is too long"),
            "got: {err}"
        );
    }

    #[test]
    fn relocate_output_path_counts_utf16_units_not_utf8_bytes_for_cjk() {
        // Pin the round-4 N-R4-1 fix: a CJK directory path is 200
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

        fs::write(dir.join(CLI_FONT_DB_FILENAME), b"db").unwrap();
        fs::write(dir.join(format!("{CLI_FONT_DB_FILENAME}-wal")), b"wal").unwrap();
        let guard = TempFontDbDir(dir.clone());
        drop(guard);

        assert!(!dir.exists());
    }
}
