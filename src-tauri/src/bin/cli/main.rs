use std::fs;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::{Args, Parser, Subcommand, ValueEnum};
use serde::Serialize;

mod engine;

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
    #[arg(long)]
    offset: String,

    /// Shift only entries after this timestamp.
    #[arg(long)]
    after: Option<String>,

    /// Output filename template.
    #[arg(long, default_value = "{name}.shifted.ass")]
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

#[derive(Clone, Copy, Debug, ValueEnum)]
enum MissingFontAction {
    Warn,
    Fail,
}

#[derive(Args, Debug)]
struct RenameArgs {
    /// Output mode.
    #[arg(long, value_enum, default_value_t = RenameMode::CopyToVideo)]
    mode: RenameMode,

    /// Required when --mode copy-to-chosen.
    #[arg(long, value_name = "DIR")]
    output_dir: Option<PathBuf>,

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
        Command::Shift(_) => unsupported_command(&globals, "shift"),
        Command::Embed(_) => unsupported_command(&globals, "embed"),
        Command::Rename(_) => unsupported_command(&globals, "rename"),
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

fn unsupported_command(globals: &GlobalOptions, command: &'static str) -> Result<ExitCode, String> {
    if globals.json {
        let mut report = CommandReport::new(command);
        report.push(FileReport {
            input: String::new(),
            output: None,
            encoding: None,
            status: FileStatus::Failed,
            error: Some("command is not implemented yet".to_string()),
        });
        let json = serde_json::to_string_pretty(&report)
            .map_err(|err| format!("failed to encode JSON report: {err}"))?;
        println!("{json}");
    } else if !globals.quiet {
        eprintln!("ssahdrify-cli: '{command}' is not implemented yet");
    }
    Ok(ExitCode::from(2))
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

fn display_path(path: &Path) -> String {
    let path = path.to_string_lossy().into_owned();
    if cfg!(windows) {
        path.replace('/', "\\")
    } else {
        path
    }
}

fn localize(globals: &GlobalOptions, en: String, zh: String) -> String {
    match globals.lang.unwrap_or(OutputLang::En) {
        OutputLang::En => en,
        OutputLang::Zh => zh,
    }
}
