use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Args, Parser, Subcommand, ValueEnum};

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

#[derive(Clone, Copy, Debug, ValueEnum)]
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

fn main() -> ExitCode {
    let cli = Cli::parse();
    let command_name = match cli.command {
        Command::Hdr(_) => "hdr",
        Command::Shift(_) => "shift",
        Command::Embed(_) => "embed",
        Command::Rename(_) => "rename",
    };

    if !cli.globals.quiet {
        eprintln!("ssahdrify-cli: '{command_name}' command is scaffolded, not implemented yet");
    }
    ExitCode::from(2)
}
