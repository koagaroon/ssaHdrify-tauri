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

mod chain;
mod engine;

const MAX_SHIFT_OFFSET_MS: i64 = 365 * 24 * 60 * 60 * 1000;
const CLI_FONT_DB_DIR_PREFIX: &str = "ssahdrify-cli-font-db";
const CLI_FONT_DB_FILENAME: &str = "user-font-sources.session.sqlite3";

/// Command-line interface for SSA HDRify subtitle workflows.
///
/// SSA HDRify 字幕工作流命令行工具。
#[derive(Debug, Parser)]
#[command(name = "ssahdrify-cli", version, arg_required_else_help = true)]
struct Cli {
    #[command(flatten)]
    globals: GlobalOptions,

    #[command(subcommand)]
    command: Command,
}

#[derive(Args, Debug)]
struct GlobalOptions {
    /// Output directory. Defaults to each input file's directory. 输出目录；不指定时为每个输入文件所在目录。
    #[arg(long, global = true, value_name = "DIR")]
    output_dir: Option<PathBuf>,

    /// Replace existing output files instead of skipping them. 覆盖已存在的输出文件而非跳过。
    #[arg(long, global = true)]
    overwrite: bool,

    /// Show planned work without writing files. 预演计划工作但不写入文件。
    #[arg(long, global = true)]
    dry_run: bool,

    /// Suppress normal progress output. 抑制常规进度输出。
    #[arg(long, global = true, conflicts_with = "verbose")]
    quiet: bool,

    /// Show more progress detail. 显示更多进度细节。
    #[arg(long, global = true)]
    verbose: bool,

    /// Emit machine-readable JSON. 输出机器可读的 JSON。
    #[arg(long, global = true)]
    json: bool,

    /// Output language. Defaults to OS locale (zh* → zh, otherwise en). 输出语言；不指定时按系统区域设置自动检测。
    #[arg(long, global = true, value_enum, value_name = "LANG")]
    lang: Option<OutputLang>,

    /// Skip the persistent font cache for this run. Cache file is left
    /// untouched. Use when you want a fresh scan without affecting
    /// the cached state. 本次运行跳过持久化字体缓存；缓存文件保持不变。
    #[arg(long, global = true)]
    no_cache: bool,

    /// Override the default font cache file path. Default location
    /// follows each OS's user-data convention: `%APPDATA%/ssahdrify/`
    /// on Windows, `$XDG_DATA_HOME/ssahdrify/` or `~/.local/share/ssahdrify/`
    /// on Linux, `~/Library/Application Support/ssahdrify/` on macOS,
    /// always named `cli_font_cache.sqlite3`. Useful for testing or
    /// non-default layouts. 覆盖字体缓存文件路径。
    #[arg(long, global = true, value_name = "PATH")]
    cache_file: Option<PathBuf>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
enum OutputLang {
    En,
    Zh,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Convert SDR subtitle colors to HDR. 将 SDR 字幕颜色转换为 HDR。
    Hdr(HdrArgs),
    /// Shift subtitle timings by an offset. 按偏移量平移字幕时间轴。
    Shift(ShiftArgs),
    /// Embed fonts into ASS subtitle files. 将字体嵌入 ASS 字幕文件。
    ///
    /// Tips / 提示:
    ///   --font-dir and --font-file are repeatable — pass once per folder/file.
    ///   --font-dir 与 --font-file 可重复传入：每个目录或文件传一次。
    ///
    ///   Font cache (when present): used automatically. With --font-dir,
    ///   the cache merges with the dirs you supplied; without --font-dir,
    ///   the cache is the primary source. Pass --no-cache to skip the
    ///   cache for one run, or run `refresh-fonts` to rebuild it.
    ///   字体缓存（如已存在）：自动使用。提供 --font-dir 时与缓存合并；
    ///   不提供时缓存为主源。--no-cache 跳过本次；refresh-fonts 重建。
    Embed(EmbedArgs),
    /// Pair subtitles with videos and rename subtitles to match. 配对视频和字幕，按视频名重命名字幕。
    Rename(RenameArgs),

    /// Build or refresh the persistent font cache. Always requires
    /// at least one --font-dir (cache-recorded source roots are not
    /// auto-rescanned; user must specify).
    /// 构建或刷新持久化字体缓存。始终必须传至少一个 --font-dir
    /// （缓存记录的 source roots 不会自动 rescan，用户必须显式指定）。
    ///
    /// Each --font-dir is treated as a flat font folder (one level,
    /// non-recursive) — same semantics as `embed`'s --font-dir. To
    /// index a tree, pass each leaf folder explicitly.
    ///
    /// Example / 示例:
    ///   ssahdrify-cli refresh-fonts --font-dir ./Fonts/Anime --font-dir ./Fonts/Latin
    RefreshFonts(RefreshFontsArgs),

    /// Chain multiple steps in one invocation; only the terminal step writes to disk.
    /// 将多个步骤串联执行，仅终端步骤写盘。
    ///
    /// Available steps / 可用步骤:
    ///   hdr     Convert SDR subtitle colors to HDR. SDR 字幕颜色转 HDR。
    ///   shift   Shift subtitle timings by an offset. 按偏移量平移字幕时间轴。
    ///   embed   Embed fonts into ASS subtitle files. 将字体嵌入 ASS 字幕文件。
    ///
    /// Step separator is `+`. The chain-global `--output-dir` and
    /// `--output-template` apply at the terminal step only; passing
    /// `--output-template` inside any step segment is a parse-time error.
    /// 步骤分隔符为 `+`。链全局 `--output-dir` 与 `--output-template` 仅
    /// 在终端步骤应用；将 `--output-template` 放在步骤内部为 parse-time 错误。
    ///
    /// Example / 示例:
    ///   ssahdrify-cli chain hdr --eotf pq + shift --offset +2s + embed --font-dir ./fonts cat.ass
    Chain(ChainArgs),
}

#[derive(Args, Debug)]
pub(crate) struct HdrArgs {
    /// Transfer function. EOTF 曲线（PQ / HLG）。
    #[arg(long, value_enum)]
    eotf: EotfArg,

    /// Target subtitle brightness in nits. 字幕目标亮度（nits）。
    #[arg(long, default_value_t = 203)]
    nits: u16,

    /// Output filename template. 输出文件名模板。
    #[arg(long, default_value = "{name}.hdr.ass")]
    output_template: String,

    /// Subtitle files to convert. 要转换的字幕文件。
    // pub(crate) so the chain module's parser can take/clear this
    // field after parsing each step segment. Other fields stay
    // private until a callsite needs them.
    #[arg(required = true)]
    pub(crate) files: Vec<PathBuf>,
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
pub(crate) struct ShiftArgs {
    /// Signed duration, for example "+2.5s", "-500ms", or "+1m30s". 带符号的偏移量，如 "+2.5s"、"-500ms" 或 "+1m30s"。
    #[arg(long, allow_hyphen_values = true)]
    offset: String,

    /// Shift only entries after this timestamp.
    /// Format: HH:MM:SS, HH:MM:SS.mmm, or HH:MM:SS,mmm (ISO 8601 comma form).
    /// 仅平移此时间戳之后的字幕条目。格式：HH:MM:SS、HH:MM:SS.mmm 或 HH:MM:SS,mmm。
    #[arg(long)]
    after: Option<String>,

    /// Output filename template. 输出文件名模板。
    #[arg(long, default_value = "{name}.shifted{ext}")]
    output_template: String,

    /// Subtitle files to shift. 要平移的字幕文件。
    // See note on HdrArgs.files.
    #[arg(required = true)]
    pub(crate) files: Vec<PathBuf>,
}

#[derive(Args, Debug)]
pub(crate) struct EmbedArgs {
    /// Add a font folder (repeatable). 添加字体目录（可重复传入）。
    ///
    /// Pass once per folder; ssahdrify-cli scans all of them and embeds
    /// whatever the subtitle references.
    /// 每个目录传一次；ssahdrify-cli 会全部扫描并嵌入字幕引用到的字体。
    ///
    /// Example / 示例:
    ///   ssahdrify-cli embed --font-dir ./fonts --font-dir C:/MyFonts subs.ass
    #[arg(long = "font-dir", value_name = "DIR")]
    font_dirs: Vec<PathBuf>,

    /// Add a specific font file (repeatable). 添加具体字体文件（可重复传入）。
    ///
    /// Pass once per file; useful for embedding a single TTF/OTF without
    /// scanning a whole directory.
    /// 每个文件传一次；适合只嵌入单个 TTF/OTF 而不扫描整个目录。
    ///
    /// Example / 示例:
    ///   ssahdrify-cli embed --font-file ./SmileySans.ttf --font-file ./MyFont.otf subs.ass
    #[arg(long = "font-file", value_name = "FILE")]
    font_files: Vec<PathBuf>,

    /// Do not use system-installed fonts. 不使用系统已安装的字体。
    #[arg(long)]
    no_system_fonts: bool,

    /// Behavior when referenced fonts are missing. 缺失字体时的行为。
    #[arg(long, value_enum, default_value_t = MissingFontAction::Warn)]
    on_missing: MissingFontAction,

    /// Output filename template. 输出文件名模板。
    #[arg(long, default_value = "{name}.embed.ass")]
    output_template: String,

    /// ASS/SSA files to process. 要处理的 ASS/SSA 文件。
    // See note on HdrArgs.files.
    #[arg(required = true)]
    pub(crate) files: Vec<PathBuf>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
enum MissingFontAction {
    Warn,
    Fail,
}

#[derive(Args, Debug)]
struct RenameArgs {
    /// Output mode. 输出模式。
    #[arg(long, value_enum, default_value_t = RenameMode::CopyToVideo)]
    mode: RenameMode,

    /// Language selection: auto, all, or a comma-separated list such as sc,jp. 语言选择：auto、all 或逗号分隔列表（如 sc,jp）。
    #[arg(long, default_value = "auto")]
    langs: String,

    /// Video/subtitle files or folders to pair. 要配对的视频/字幕文件或文件夹。
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

#[derive(Args, Debug)]
struct RefreshFontsArgs {
    /// Add a font folder to scan (repeatable). Required — pass at
    /// least once. 添加要扫描的字体目录（可重复传入），必须至少传一次。
    ///
    /// Each folder is scanned one level deep (non-recursive); same
    /// semantics as `embed --font-dir`. To index a tree of fonts,
    /// pass each leaf folder explicitly.
    /// 每个目录扫描一层（不递归）；与 `embed --font-dir` 语义一致。
    /// 树状字体目录请逐层显式传入。
    #[arg(long = "font-dir", value_name = "DIR", required = true)]
    font_dirs: Vec<PathBuf>,
}

#[derive(Args, Debug)]
struct ChainArgs {
    /// Chain-global output filename template applied at the terminal step.
    /// Defaults to a stacked-suffix form (`{name}.<step1>.<step2>...<stepN>.ass`).
    /// 链全局输出文件名模板，仅终端步骤应用；缺省按各步后缀堆叠。
    #[arg(long)]
    output_template: Option<String>,

    /// Steps and input files: `<step1> + <step2> + ... <stepN> file...`.
    /// 步骤与输入文件：`<step1> + <step2> + ... <stepN> file...`。
    ///
    // `trailing_var_arg` captures everything after the first positional
    // (which is also the first step's keyword). `allow_hyphen_values`
    // is required because step segments contain `--eotf`-style flags
    // that would otherwise be interpreted as ChainArgs's own flags.
    #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
    raw_argv: Vec<String>,
}

// ── Chain serialization ─────────────────────────────────────
//
// Each per-feature Args struct provides a `to_chain_step` method
// that produces a JSON shape matching the TS-side `ChainStep`
// discriminated union (see src/features/chain/chain-types.ts).
// Living in main.rs keeps the field-access privacy minimal — chain
// step variants need to read `eotf`, `nits`, `offset`, `after`,
// `font_dirs`, etc., which are private to main.rs.
//
// The TS side (`runChain` registry) is the contract: any drift in
// field naming, optionality, or value form will fail at runtime. The
// `to_runtime_payload` helper in chain.rs has unit tests pinning the
// JSON shape so changes here that miss the TS side surface fast.

impl HdrArgs {
    pub(crate) fn to_chain_step(&self) -> serde_json::Value {
        // `nits` here maps to TS-side `brightness` — the existing CLI
        // surface uses `--nits` for UX (matches HDR signaling vocabulary)
        // while the engine API was named `brightness` from the Python
        // original. Renaming either side is more disruptive than a
        // single-point translation here.
        serde_json::json!({
            "kind": "hdr",
            "params": {
                "eotf": self.eotf.as_engine_value(),
                "brightness": self.nits,
            },
        })
    }
}

impl ShiftArgs {
    pub(crate) fn to_chain_step(&self) -> Result<serde_json::Value, String> {
        let offset_ms = parse_duration_ms(&self.offset)?;
        let threshold_ms = match &self.after {
            Some(text) => Some(parse_timestamp_ms(text)?),
            None => None,
        };
        let mut params = serde_json::json!({ "offsetMs": offset_ms });
        if let Some(t) = threshold_ms {
            params["thresholdMs"] = serde_json::Value::from(t);
        }
        Ok(serde_json::json!({
            "kind": "shift",
            "params": params,
        }))
    }
}

impl EmbedArgs {
    pub(crate) fn to_chain_step(&self) -> serde_json::Value {
        // Path → string conversion uses `to_string_lossy` so non-UTF-8
        // path bytes (Windows wide chars converted via WTF-8, or the
        // rare UNIX path with invalid UTF-8) survive the JSON round-
        // trip. The TS side treats the strings opaquely until
        // resolution time, where they're handed back to Rust ops and
        // converted back to PathBuf — at which point lossy-encoded
        // bytes become a different lookup but produce a clear "font
        // not found" error rather than silent corruption.
        let font_dirs: Vec<String> = self
            .font_dirs
            .iter()
            .map(|p| p.to_string_lossy().into_owned())
            .collect();
        let font_files: Vec<String> = self
            .font_files
            .iter()
            .map(|p| p.to_string_lossy().into_owned())
            .collect();
        serde_json::json!({
            "kind": "embed",
            "params": {
                "fontDirs": font_dirs,
                "fontFiles": font_files,
                "noSystemFonts": self.no_system_fonts,
                "onMissing": match self.on_missing {
                    MissingFontAction::Warn => "warn",
                    MissingFontAction::Fail => "fail",
                },
            },
        })
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
        Command::Chain(args) => run_chain(&globals, args),
        Command::RefreshFonts(args) => run_refresh_fonts(&globals, args),
    }
}

fn run_refresh_fonts(globals: &GlobalOptions, args: RefreshFontsArgs) -> Result<ExitCode, String> {
    // refresh-fonts's whole purpose is to write to the cache. Running
    // with --no-cache is contradictory; surface as a clear error
    // rather than silently doing nothing (per no-silent-action).
    if globals.no_cache {
        return Err("refresh-fonts requires the cache; --no-cache contradicts \
             this subcommand's purpose. Remove --no-cache or use a \
             different subcommand."
            .to_string());
    }

    // Resolve cache file path: user override (--cache-file) or default
    // Windows path. Both come from globals now (they're global flags).
    let cache_path = match &globals.cache_file {
        Some(p) => p.clone(),
        None => app_lib::font_cache::default_cli_cache_path()?,
    };

    if !globals.quiet {
        eprintln!(
            "ℹ Refreshing cache. Scanning {} source root{}:",
            args.font_dirs.len(),
            if args.font_dirs.len() == 1 { "" } else { "s" }
        );
        for dir in &args.font_dirs {
            eprintln!("    {}", dir.display());
        }
    }

    // Open or create cache. Schema version mismatch on existing file
    // is treated as drift-equivalent: tell user, suggest rebuild via
    // wiping the file. Per the no-silent-action principle, we don't
    // auto-delete.
    let mut cache = match app_lib::font_cache::FontCache::open_or_create(&cache_path) {
        Ok(c) => c,
        Err(app_lib::font_cache::CacheError::SchemaVersionMismatch { found, expected }) => {
            return Err(format!(
                "Cache at {} has schema version {found} but this CLI uses version {expected}.\n\
                 The cache is from a different release and must be rebuilt.\n\
                 Delete the file manually and re-run refresh-fonts:\n  \
                 (file: {})",
                cache_path.display(),
                cache_path.display(),
            ));
        }
        Err(e) => return Err(format!("opening cache: {e}")),
    };

    let mut total_fonts: usize = 0;
    let mut total_folders: usize = 0;

    for dir in &args.font_dirs {
        // Resolve to absolute path (mirrors embed's behavior so cache
        // entries are stable across invocations from different cwd's).
        let abs_dir = absolute_path(dir)?;
        let canonical = abs_dir
            .canonicalize()
            .map_err(|e| format!("cannot canonicalize {}: {e}", abs_dir.display()))?;
        let folder_path_str = display_path(&canonical);

        // Stat the folder for mtime — drift detection on next run
        // compares this against live stat.
        let folder_mtime = std::fs::metadata(&canonical)
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);

        // Scan the folder. Non-recursive — matches `embed`'s
        // --font-dir semantics exactly. Per-source error (e.g. cache-
        // populate cap exceeded for malicious / oversized packs) is
        // logged and skipped so one bad source doesn't abort the whole
        // refresh run — refresh-fonts is multi-dir by design.
        let entries = match app_lib::fonts::scan_directory_collecting(&canonical) {
            Ok(e) => e,
            Err(err) => {
                if !globals.quiet {
                    eprintln!("  ⚠ {}: skipped — {err}", folder_path_str);
                }
                continue;
            }
        };

        // Shared GUI/CLI helper: per-file mtime dedup (TTC files
        // contribute multiple entries with one path) + saturating cast
        // discipline + family-key flattening.
        let metadata: Vec<app_lib::font_cache::FontMetadata> =
            app_lib::fonts::entries_to_cache_metadata(&entries);

        let font_count = metadata.len();
        cache
            .replace_folder(&folder_path_str, folder_mtime, &metadata)
            .map_err(|e| format!("writing cache for {}: {e}", folder_path_str))?;

        if !globals.quiet {
            eprintln!(
                "  ✓ {}: indexed {} font face{}",
                folder_path_str,
                font_count,
                if font_count == 1 { "" } else { "s" }
            );
        }
        total_fonts += font_count;
        total_folders += 1;
    }

    if !globals.quiet {
        eprintln!(
            "✓ Cache updated: {total_fonts} font face{} indexed across {total_folders} folder{}.",
            if total_fonts == 1 { "" } else { "s" },
            if total_folders == 1 { "" } else { "s" }
        );
        eprintln!("  Cache file: {}", cache_path.display());
    }

    Ok(ExitCode::SUCCESS)
}

fn run_chain(globals: &GlobalOptions, args: ChainArgs) -> Result<ExitCode, String> {
    // Capture whether the user explicitly supplied --output-template
    // BEFORE moving args.output_template into parse_chain_argv —
    // needed for the β stderr info line below ("did the user pick
    // this template, or are we using the stacked default?").
    let user_supplied_template = args.output_template.is_some();
    let plan = chain::parse_chain_argv(&args.raw_argv, args.output_template)?;

    // Suspicious-pattern warnings are non-blocking per the locked
    // decision (catalog: HDR×2, shift-after-embed). Emit to stderr
    // and proceed. Honors --quiet to match prepare_embed_cache's
    // posture: --quiet suppresses informational diagnostics, errors
    // still surface elsewhere.
    if !globals.quiet {
        for warning in &plan.warnings {
            eprintln!("{warning}");
        }
    }

    // β behavior for default-stacked output: stderr info line +
    // dry-run hint, NO interactive prompt (preserves the no-prompt
    // principle in `命令设计 § Cross-cutting 行为`). Users wanting
    // safety run with --dry-run; users wanting a different name
    // pass --output-template. --quiet suppresses (consistent with
    // every other informational stderr line in the CLI).
    if !user_supplied_template && !globals.quiet {
        eprintln!(
            "ℹ Output template defaulted to '{}' (stacked from chain steps).",
            plan.output_template
        );
        eprintln!("  Pass --output-template <T> to override, or --dry-run to preview.");
    }

    let embed_step_index = find_embed_step_index(&plan);
    // Inform user when --no-cache is meaningless in chain — chain v1
    // doesn't consult the persistent cache (`resolve_chain_embed_subsets`
    // always passes None per the locked design). Without this, --no-cache
    // looks like it's silently ignored. Mirror prepare_embed_cache's
    // posture: stderr informational line, gated on --quiet. Hoisted
    // ABOVE the dry-run early-return so `--dry-run --no-cache` users
    // also see the diagnostic (otherwise the early-return swallowed it).
    if globals.no_cache && !globals.quiet && embed_step_index.is_some() {
        eprintln!(
            "ℹ --no-cache has no effect in chain mode; chain v1 doesn't use the persistent cache."
        );
    }

    if globals.dry_run {
        emit_chain_dry_run(&plan, globals);
        return Ok(ExitCode::SUCCESS);
    }

    // HDR / Shift / Embed all work in chain. Embed steps get their
    // fonts pre-resolved against the original input content (HDR/Shift
    // don't change font references, so pre-resolution is safe) and the
    // subsets injected into params before runChain.
    app_lib::fonts::init_system_dirs();
    // Hold the font-DB session for the duration of the chain batch.
    // Mirrors run_embed's pattern — guard lives across all input
    // files, dropped at end. Skipped if the embed step has no user
    // fonts (saves the SQLite init + scan). dry-run short-circuits
    // via the `emit_chain_dry_run` early return above before this code
    // runs, so the standalone-embed guard pattern is already satisfied
    // structurally.
    let _font_db_guard = match embed_step_index {
        Some(idx) => {
            let chain::ParsedStep::Embed(embed_args) = &plan.steps[idx] else {
                // find_embed_step_index returns Some only for Embed
                // variants — invariant holds by construction.
                unreachable!("find_embed_step_index returned a non-Embed index");
            };
            let use_user_fonts =
                !embed_args.font_dirs.is_empty() || !embed_args.font_files.is_empty();
            if use_user_fonts {
                Some(init_cli_font_sources(globals, embed_args)?)
            } else {
                None
            }
        }
        None => None,
    };

    let mut engine = engine::CliEngine::new()?;
    let mut written = 0usize;
    let mut failed = 0usize;
    let mut skipped = 0usize;
    for input in &plan.input_files {
        match process_one_chain_input(&mut engine, &plan, embed_step_index, input, globals) {
            ChainFileOutcome::Written(out, warnings) => {
                if !globals.quiet {
                    println!("✓ {} → {}", input.display(), out.display());
                    // Surface embed pre-resolution warnings (missing
                    // fonts under --on-missing warn, subset failures)
                    // — without this chain mode silently drops the
                    // diagnostics that standalone embed surfaces
                    // through FileReport.warnings.
                    for warning in &warnings {
                        eprintln!("  ⚠ {warning}");
                    }
                }
                written += 1;
            }
            ChainFileOutcome::Skipped(reason) => {
                if !globals.quiet {
                    println!("⊘ {}: {}", input.display(), reason);
                }
                skipped += 1;
            }
            ChainFileOutcome::Failed(err) => {
                eprintln!("✗ {}: {}", input.display(), err);
                failed += 1;
            }
        }
    }
    if !globals.quiet {
        println!("Summary: {written} written, {skipped} skipped, {failed} failed");
    }
    Ok(if failed > 0 {
        ExitCode::from(1)
    } else {
        ExitCode::SUCCESS
    })
}

enum ChainFileOutcome {
    /// Written(output_path, warnings) — warnings are non-fatal
    /// diagnostics propagated from the embed pre-resolution path
    /// (missing fonts, subset failures) so chain output matches
    /// standalone embed's `FileReport.warnings` semantics.
    Written(PathBuf, Vec<String>),
    Skipped(String),
    Failed(String),
}

fn find_embed_step_index(plan: &chain::ChainPlan) -> Option<usize> {
    plan.steps
        .iter()
        .position(|s| matches!(s, chain::ParsedStep::Embed(_)))
}

/// Port of TS `substituteTemplate` (`src/lib/path-validation.ts`).
/// Segment-based: tokens substitute literally, `..` runs INSIDE
/// template literals collapse to `.`, and at literal/value boundaries
/// at most one dot is dropped — so user-content `..` in stems
/// (`Show..special`) survives intact. The pre-Round-1.5 implementation
/// here used a blanket `replace("..", ".")` post-pass that mangled
/// such filenames, diverging from the TS resolver and causing the
/// cheap-first existence check to short-circuit to "Skipped" against
/// a path V8 would actually produce differently (Codex bd782f90).
///
/// Token shape `[a-z_][a-z0-9_]*` matches the TS regex. Unknown tokens
/// substitute to "". Caller supplies a slice of `(name, value)` pairs;
/// linear scan is fine — chain templates have 2-3 tokens at most.
fn substitute_template(template: &str, vars: &[(&str, &str)]) -> String {
    enum Seg {
        Literal(String),
        Value(String),
    }
    let bytes = template.as_bytes();
    let mut segments: Vec<Seg> = Vec::new();
    let mut i = 0usize;
    let mut last_lit_start = 0usize;
    while i < bytes.len() {
        if bytes[i] == b'{' {
            let name_start = i + 1;
            if name_start < bytes.len()
                && (bytes[name_start].is_ascii_lowercase() || bytes[name_start] == b'_')
            {
                let mut j = name_start;
                while j < bytes.len()
                    && (bytes[j].is_ascii_lowercase()
                        || bytes[j].is_ascii_digit()
                        || bytes[j] == b'_')
                {
                    j += 1;
                }
                if j < bytes.len() && bytes[j] == b'}' && j > name_start {
                    let name = &template[name_start..j];
                    if i > last_lit_start {
                        let lit_text = &template[last_lit_start..i];
                        segments.push(Seg::Literal(collapse_internal_double_dots(lit_text)));
                    }
                    let value = vars
                        .iter()
                        .find(|(k, _)| *k == name)
                        .map(|(_, v)| *v)
                        .unwrap_or("");
                    segments.push(Seg::Value(value.to_string()));
                    i = j + 1;
                    last_lit_start = i;
                    continue;
                }
            }
        }
        i += 1;
    }
    if last_lit_start < bytes.len() {
        let lit_text = &template[last_lit_start..];
        segments.push(Seg::Literal(collapse_internal_double_dots(lit_text)));
    }

    let mut out = String::with_capacity(template.len());
    for seg in &segments {
        let chunk: &str = match seg {
            Seg::Literal(s) | Seg::Value(s) => s.as_str(),
        };
        if chunk.starts_with('.') && out.ends_with('.') {
            out.push_str(&chunk[1..]);
        } else {
            out.push_str(chunk);
        }
    }
    out
}

/// Collapse any run of 2+ dots to a single dot. ASCII-only fast path —
/// `.` is U+002E, single byte in UTF-8, so byte iteration is safe.
fn collapse_internal_double_dots(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut prev_was_dot = false;
    for c in s.chars() {
        if c == '.' {
            if !prev_was_dot {
                out.push(c);
            }
            prev_was_dot = true;
        } else {
            out.push(c);
            prev_was_dot = false;
        }
    }
    out
}

/// Best-effort prediction of the chain output path for the cheap-first
/// skip-on-exists check. Mirrors `resolveChainOutputPath` in
/// chain-runtime.ts for the common template tokens (`{name}`, `{ext}`)
/// so the Rust shell can short-circuit BEFORE invoking V8 when the
/// destination already exists.
///
/// Permissive prediction is the danger direction: if Rust predicts a
/// path TS would reject (traversal `..`, path separators in the
/// template, reserved Windows names), and that predicted path
/// coincidentally exists, the cheap-first check would short-circuit
/// to "Skipped: already exists" — a misleading false-skip instead of
/// the precise rejection error TS would produce. Reject those shapes
/// here by returning None so V8 sees the real input + TS reports the
/// authoritative error.
fn predict_chain_output_path(
    input_abs: &Path,
    output_template: &str,
    output_dir: Option<&Path>,
) -> Option<PathBuf> {
    let parent = input_abs.parent()?;
    let stem = input_abs.file_stem()?.to_str()?;
    let ext = input_abs
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| format!(".{e}"))
        .unwrap_or_default();
    let output_name = substitute_template(output_template, &[("name", stem), ("ext", &ext)]);
    // Reject shapes TS-side `assertSafeOutputFilename` would reject:
    // path separators (chain output is a single filename in input's
    // dir, never a relative or absolute path), drive-letter prefixes,
    // empty after substitution, OR a Windows reserved device name
    // (CON, PRN, AUX, NUL, COM[0-9], LPT[0-9]) — Win32 treats these
    // as device paths regardless of extension, and a template like
    // `CON.{ext}` would predict a path that creates a console handle
    // not a file. Any of these means "Rust prediction and TS
    // resolution will diverge" → defer to V8 + TS for the precise
    // rejection error.
    //
    // Reserved-name coverage scope: ASCII digit variants only
    // (COM0-COM9 / LPT0-LPT9), and the bare-stem form (no trailing
    // whitespace stripping). TS-side `assertSafeOutputFilename`
    // additionally rejects Unicode superscript variants (COM¹/²/³,
    // LPT¹/²/³) AND strips trailing whitespace / dots before the
    // reserved-name check (so `CON ` and `CON.` resolve to the device
    // too). The Rust pre-check intentionally omits both — Windows
    // refuses to create files with any of these names, so the
    // predicted path can never exist on disk → `predicted.exists()`
    // returns false → prediction returns Some → V8 runs → TS rejects
    // authoritatively. The harmless-slip set is closed-form because
    // the Win32 device-namespace gate at the OS layer is the final
    // arbiter (Round 2 A-R2-6 / N-R2-10). The comment is the
    // contract, not the regex.
    if output_name.is_empty()
        || output_name.contains('/')
        || output_name.contains('\\')
        || output_name.contains('\0')
        || output_name.starts_with('.')
        || (output_name.len() >= 2 && output_name.as_bytes()[1] == b':')
    {
        return None;
    }
    let stem_upper = output_name
        .split('.')
        .next()
        .unwrap_or("")
        .to_ascii_uppercase();
    let is_reserved = matches!(stem_upper.as_str(), "CON" | "PRN" | "AUX" | "NUL")
        || (stem_upper.len() == 4
            && (stem_upper.starts_with("COM") || stem_upper.starts_with("LPT"))
            && stem_upper.as_bytes()[3].is_ascii_digit());
    if is_reserved {
        return None;
    }
    let predicted = parent.join(&output_name);
    relocate_output_path(&predicted.to_string_lossy(), output_dir).ok()
}

fn process_one_chain_input(
    engine: &mut engine::CliEngine,
    plan: &chain::ChainPlan,
    embed_step_index: Option<usize>,
    input: &Path,
    globals: &GlobalOptions,
) -> ChainFileOutcome {
    let input_abs = match absolute_path(input) {
        Ok(p) => p,
        Err(err) => return ChainFileOutcome::Failed(err),
    };
    let input_str = display_path(&input_abs);

    // Cheap-first skip: if the predicted output path already exists
    // and --overwrite is off, return Skipped before any I/O or V8
    // work. Mirrors the per-feature process_* paths' ordering. Path
    // prediction is best-effort and may differ from TS-side
    // resolution in template corner cases — the post-V8 existence
    // check below catches anything prediction misses.
    if !globals.overwrite {
        if let Some(predicted) = predict_chain_output_path(
            &input_abs,
            &plan.output_template,
            globals.output_dir.as_deref(),
        ) {
            if predicted.exists() {
                return ChainFileOutcome::Skipped(format!(
                    "{} already exists (use --overwrite to replace)",
                    predicted.display()
                ));
            }
        }
    }

    // Read input via existing encoding-aware path. Honors the same
    // size cap, BOM detection, and fallback-on-canonicalize-failure
    // semantics every other CLI subcommand uses.
    let read_result = match app_lib::encoding::read_text_detect_encoding_inner(&input_str, |_| true)
    {
        Ok(r) => r,
        Err(err) => return ChainFileOutcome::Failed(err),
    };

    // Build the JSON payload matching the TS-side ChainRunRequest.
    let mut payload = match plan.to_runtime_payload(&input_str, &read_result.text) {
        Ok(p) => p,
        Err(err) => return ChainFileOutcome::Failed(err),
    };

    // Pre-resolve fonts for the embed step (if present) and inject
    // the subset bytes into its params. Done per-file because
    // planFontEmbed needs the file's content; the user-font DB
    // session itself is shared across files (set up once before the
    // loop in run_chain).
    let mut warnings: Vec<String> = Vec::new();
    if let Some(idx) = embed_step_index {
        let chain::ParsedStep::Embed(embed_args) = &plan.steps[idx] else {
            unreachable!("find_embed_step_index returned a non-Embed index");
        };
        let (subsets, embed_warnings) = match resolve_chain_embed_subsets(
            engine,
            globals,
            embed_args,
            &input_str,
            &read_result.text,
        ) {
            Ok(s) => s,
            Err(err) => return ChainFileOutcome::Failed(err),
        };
        warnings = embed_warnings;
        // Encode subset bytes as base64 strings. The previous form
        // (`{ "data": [byte, byte, ...] }`) expanded ~4-5× per byte
        // when serde_json wrote bytes as decimal+comma JSON-in-JS-source,
        // which compounds against CUMULATIVE_FALLBACK_BYTES (50 MB)
        // into ~200 MB of V8 heap pressure on the worst-case fallback
        // path. Base64 is ~1.33× and decoded in TS via atob().
        let subsets_json: Vec<serde_json::Value> = subsets
            .into_iter()
            .map(|s| {
                use base64::Engine as _;
                let data_b64 = base64::engine::general_purpose::STANDARD.encode(&s.data);
                serde_json::json!({ "fontName": s.font_name, "dataB64": data_b64 })
            })
            .collect();
        payload["plan"]["steps"][idx]["params"]["subsets"] = serde_json::Value::Array(subsets_json);
    }

    let request = engine::ChainRunRequest { payload };

    let result = match engine.run_chain(&request) {
        Ok(r) => r,
        Err(err) => return ChainFileOutcome::Failed(err),
    };

    // Apply --output-dir relocation (chain-global, terminal step
    // only) using the existing helper. The runtime returned the
    // path resolved against the input's directory; relocation
    // re-roots that into --output-dir if set.
    let output_path = match relocate_output_path(&result.output_path, globals.output_dir.as_deref())
    {
        Ok(p) => p,
        Err(err) => return ChainFileOutcome::Failed(err),
    };

    // Skip-or-overwrite check matching existing per-feature behavior.
    if !globals.overwrite && output_path.exists() {
        return ChainFileOutcome::Skipped(format!(
            "{} already exists (use --overwrite to replace)",
            output_path.display()
        ));
    }

    // Route through the safe writer used by every other CLI subcommand
    // (write_output uses OpenOptions::create_new(true), which refuses to
    // create through a pre-planted symlink/junction at the output path
    // — fs::write would follow it and clobber an attacker-chosen target
    // outside the intended output directory).
    if let Err(err) = write_output(globals, &output_path, &result.content, globals.overwrite) {
        return ChainFileOutcome::Failed(err);
    }

    if globals.verbose {
        for note in &result.notes {
            println!("  {note}");
        }
    }

    ChainFileOutcome::Written(output_path, warnings)
}

fn emit_chain_dry_run(plan: &chain::ChainPlan, globals: &GlobalOptions) {
    println!("Plan (no files written):");
    println!();
    println!("Output template: {}", plan.output_template);
    println!();
    for input in &plan.input_files {
        println!("  {}", input.display());
        // Show the resolved output path for parity with per-feature
        // dry-run output, so users can verify the template + output_dir
        // combination produces what they expect before they remove
        // --dry-run.
        let resolved = absolute_path(input)
            .ok()
            .and_then(|abs| {
                predict_chain_output_path(
                    &abs,
                    &plan.output_template,
                    globals.output_dir.as_deref(),
                )
            })
            .map(|p| p.display().to_string());
        if let Some(out) = resolved {
            println!("    → {out}");
        }
        for (i, step) in plan.steps.iter().enumerate() {
            println!("    {}. {}", i + 1, step.kind_name());
        }
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

    let read_result = match app_lib::encoding::read_text_detect_encoding_inner(&input, |_| true) {
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

    let read_result = match app_lib::encoding::read_text_detect_encoding_inner(&input, |_| true) {
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

    let read_result = match app_lib::encoding::read_text_detect_encoding_inner(&input, |_| true) {
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
    let cache = if globals.no_cache || globals.dry_run {
        if globals.no_cache && !globals.quiet {
            eprintln!("ℹ Cache disabled (--no-cache). Using --font-dir / system fonts only.");
        }
        None
    } else {
        prepare_embed_cache(globals, &args)
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
            cache.as_ref(),
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

/// Resolve cache path, open the cache, detect drift, announce status
/// to stderr per the locked transparency design, and return
/// `Some(cache)` if usable for this run, or `None` to fall back to
/// no-cache mode.
///
/// Never writes to the cache file — read-only operation. Refresh is
/// the user's explicit `refresh-fonts` invocation.
fn prepare_embed_cache(
    globals: &GlobalOptions,
    args: &EmbedArgs,
) -> Option<app_lib::font_cache::FontCache> {
    // Resolve path: --cache-file override or default Windows path.
    let cache_path = match &globals.cache_file {
        Some(p) => p.clone(),
        None => match app_lib::font_cache::default_cli_cache_path() {
            Ok(p) => p,
            Err(e) => {
                if !globals.quiet {
                    eprintln!("⚠ Cannot resolve cache path: {e}");
                    eprintln!("  Skipping cache for this run.");
                }
                return None;
            }
        },
    };

    if !cache_path.exists() {
        // No cache yet (first-ever invocation, or user wiped it).
        // Per locked design: distinct messaging from drift, same
        // behavior (skip cache + suggest refresh-fonts).
        if !globals.quiet {
            eprintln!("ℹ No font cache exists yet at {}.", cache_path.display());
            eprintln!(
                "  Run `ssahdrify-cli refresh-fonts --font-dir <DIR>...` to build one (--font-dir is repeatable)."
            );
        }
        return None;
    }

    let cache = match app_lib::font_cache::FontCache::open_or_create(&cache_path) {
        Ok(c) => c,
        Err(app_lib::font_cache::CacheError::SchemaVersionMismatch { found, expected }) => {
            if !globals.quiet {
                eprintln!("⚠ Font cache schema mismatch (found {found}, expected {expected}).");
                eprintln!("  Cache is from a different release; skipping for this run.");
                eprintln!(
                    "  Delete {} and run `refresh-fonts` to rebuild.",
                    cache_path.display()
                );
            }
            return None;
        }
        Err(e) => {
            if !globals.quiet {
                eprintln!("⚠ Cannot open font cache: {e}");
                eprintln!("  Skipping cache for this run.");
            }
            return None;
        }
    };

    // Drift check: walk cached folders' stat()s and compare against
    // recorded mtimes. "Added" folders aren't detectable here — we'd
    // need to walk source roots, which embed doesn't have. So the
    // report covers modified + removed; added is empty by design.
    let drift = match check_cache_drift(&cache) {
        Ok(report) => report,
        Err(e) => {
            if !globals.quiet {
                eprintln!("⚠ Cannot validate cache: {e}");
                eprintln!("  Skipping cache for this run.");
            }
            return None;
        }
    };

    if !drift.is_empty() {
        if !globals.quiet {
            eprintln!(
                "⚠ Cache drift detected — {} folder(s) changed since last refresh:",
                drift.modified.len() + drift.removed.len()
            );
            for f in &drift.modified {
                eprintln!("    ~ {f}  (modified)");
            }
            for f in &drift.removed {
                eprintln!("    - {f}  (removed)");
            }
            eprintln!("  Skipping cache for this run; using --font-dir / system fonts only.");
            eprintln!("  Run `refresh-fonts` to update the cache.");
        }
        return None;
    }

    // Cache is valid. Announce per locked transparency design:
    // Situation A (--font-dir provided) → "cache + dirs" merge
    // announcement; Situation B (no --font-dir) → implicit cache
    // use announcement.
    let user_supplied_dirs = !args.font_dirs.is_empty() || !args.font_files.is_empty();
    if !globals.quiet {
        if user_supplied_dirs {
            eprintln!(
                "ℹ Using font cache (at {}) plus the --font-dir / --font-file paths you supplied.",
                cache_path.display()
            );
        } else {
            eprintln!("ℹ Using font cache (at {}).", cache_path.display());
            eprintln!("  Pass --no-cache to use system fonts only.");
        }
    }
    Some(cache)
}

/// Walk every folder the cache has indexed, stat() each one, and
/// build a snapshot for drift detection. Folders that no longer
/// exist (or that we can't stat) get omitted from the snapshot,
/// which `diff_against` then reports as `removed`.
///
/// `added` is intentionally not detectable here: embed doesn't walk
/// source roots, so we can't see folders the user has on disk but
/// hasn't yet cached. Those land in the cache via `refresh-fonts`,
/// not via embed-time drift detection.
fn check_cache_drift(
    cache: &app_lib::font_cache::FontCache,
) -> Result<app_lib::font_cache::DriftReport, String> {
    let cached_folders = cache
        .list_folders()
        .map_err(|e| format!("list cached folders: {e}"))?;
    let mut snapshot: Vec<(String, i64)> = Vec::with_capacity(cached_folders.len());
    for folder in &cached_folders {
        let folder_path_buf = std::path::Path::new(&folder.folder_path);
        // Both metadata() and modified() can fail (folder gone,
        // permission denied, etc.). We treat "can't stat" the same
        // as "doesn't exist" — the folder won't appear in the
        // snapshot and `diff_against` flags it as removed. For
        // permission errors specifically, this is a slight false-
        // positive (folder exists but we can't see it), but the
        // user likely wants to know either way.
        if let Ok(metadata) = std::fs::metadata(folder_path_buf) {
            if let Ok(modified) = metadata.modified() {
                let mtime = modified
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_secs() as i64)
                    .unwrap_or(0);
                snapshot.push((folder.folder_path.clone(), mtime));
            }
        }
    }
    cache
        .diff_against(&snapshot)
        .map_err(|e| format!("compute drift: {e}"))
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

    // IIFE so a single `?` in any import step routes to the cleanup
    // path below — the static `USER_FONT_DB_PATH` set by
    // `init_user_font_db` must be cleared alongside the temp dir wipe,
    // otherwise the slot points at a deleted file after the guard's
    // Drop runs (Round 2 N-R2-8 — latent bug, no current caller
    // retries, but the helper makes the cleanup explicit).
    let import_result: Result<(), String> = (|| -> Result<(), String> {
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
        Ok(())
    })();

    match import_result {
        Ok(()) => Ok(guard),
        Err(e) => {
            app_lib::fonts::clear_user_font_db_path();
            // `guard` drops here, wiping the temp dir alongside the
            // path-slot reset above.
            Err(e)
        }
    }
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

// 8 args: globals + args + use_user_fonts + cache + output_dir +
// engine + file + seen_outputs. The cache and use_user_fonts could
// be folded into a per-run state struct, but the existing run_embed
// already passes them as parallel locals; bundling here would just
// shift the boilerplate. Allowing this one lint locally.
#[allow(clippy::too_many_arguments)]
fn process_embed_file(
    globals: &GlobalOptions,
    args: &EmbedArgs,
    use_user_fonts: bool,
    cache: Option<&app_lib::font_cache::FontCache>,
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

    let read_result = match app_lib::encoding::read_text_detect_encoding_inner(&input, |_| true) {
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

    let resolved_fonts =
        match resolve_embed_fonts(globals, args, use_user_fonts, cache, &plan.fonts) {
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
fn resolve_chain_embed_subsets(
    engine: &mut engine::CliEngine,
    globals: &GlobalOptions,
    embed_args: &EmbedArgs,
    input_path: &str,
    content: &str,
) -> Result<(Vec<engine::FontSubsetPayload>, Vec<String>), String> {
    let use_user_fonts = !embed_args.font_dirs.is_empty() || !embed_args.font_files.is_empty();

    // output_template is unused at the chain level (the chain-global
    // template wins) but plan_font_embed expects one. The default
    // satisfies the schema; the returned outputPath gets ignored.
    let plan_request = engine::FontEmbedPlanRequest {
        input_path: input_path.to_string(),
        content: content.to_string(),
        output_template: "{name}.embed.ass".to_string(),
    };
    let plan_result = engine.plan_font_embed(&plan_request)?;

    // Chain's embed step doesn't use the persistent cache (yet) — chain
    // pre-resolution runs against the input content with whatever
    // --font-dir the embed step itself was given. Cache integration
    // for chain is a future expansion; for now, pass None.
    //
    // Propagate both warning lists to the caller so chain mode and
    // standalone embed produce equivalent diagnostics. Standalone embed
    // surfaces these as FileReport.warnings; chain wraps them into
    // ChainFileOutcome::Written(_, warnings).
    let (resolved, missing_warnings) = resolve_embed_fonts(
        globals,
        embed_args,
        use_user_fonts,
        None,
        &plan_result.fonts,
    )?;
    let (subsets, skipped_warnings) = subset_resolved_fonts(globals, embed_args, &resolved)?;
    let mut warnings = missing_warnings;
    warnings.extend(skipped_warnings);
    Ok((subsets, warnings))
}

/// Resolve fonts; under `--on-missing warn`, returns the resolved
/// list AND the missing-font diagnostics so the caller can surface
/// them in `FileReport.warnings` (not just on stderr). Under
/// `--on-missing fail`, returns Err on any missing font.
fn resolve_embed_fonts(
    globals: &GlobalOptions,
    args: &EmbedArgs,
    use_user_fonts: bool,
    cache: Option<&app_lib::font_cache::FontCache>,
    fonts: &[engine::FontEmbedUsage],
) -> Result<(Vec<ResolvedEmbedFont>, Vec<String>), String> {
    let mut resolved = Vec::new();
    let mut missing = Vec::new();

    for font in fonts {
        let lookup = resolve_embed_font(args, use_user_fonts, cache, font);
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
    cache: Option<&app_lib::font_cache::FontCache>,
    font: &engine::FontEmbedUsage,
) -> Result<Option<(String, u32)>, String> {
    // Lookup tier 1: session DB populated by --font-dir for THIS run
    // (Situation A's explicit "merge in these dirs" inputs).
    if use_user_fonts {
        if let Some(found) =
            app_lib::fonts::resolve_user_font(font.family.clone(), font.bold, font.italic)?
        {
            return Ok(Some((found.path, found.index)));
        }
    }

    // Lookup tier 2: persistent cache. Implements Situation A's
    // "merge with cache" semantic (when --font-dir is also provided,
    // cache fills in fonts the user didn't explicitly hand) and
    // Situation B's "implicit cache use" (when no --font-dir, cache
    // is the primary source). Cache is None when --no-cache is set,
    // when the cache file doesn't exist, or when drift detection
    // fell us back to no-cache for this run.
    if let Some(c) = cache {
        match c.lookup_family(&font.family, font.bold, font.italic) {
            Ok(Some(result)) => {
                return Ok(Some((result.font_path, result.face_index as u32)));
            }
            Ok(None) => {
                // Cache miss; fall through to system fonts.
            }
            Err(e) => {
                // Cache read error; log but don't fail the whole
                // embed — fall through to system fonts.
                log::warn!("font cache lookup failed for {}: {e}", font.family);
            }
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
            // a misleading "skipped: output exists" diagnostic. Honors
            // --quiet (Round 1 A3.N-R1-9): the user opted into a
            // diagnostics-free run and this warning fired even then,
            // breaking the "no stderr noise when --quiet" contract.
            if !globals.quiet {
                // Scrub the path and error through `strip_visual_line_breaks`
                // before printing — a Windows filename containing CR/LF,
                // NEL, or U+2028/U+2029 would otherwise wrap the warning
                // across multiple lines (Round 2 N-R2-11). Same defense
                // the rfd startup dialog already applies; mirroring for
                // CLI stderr keeps the posture symmetric.
                let display = app_lib::util::strip_visual_line_breaks(&path.display().to_string());
                let err_one_line = app_lib::util::strip_visual_line_breaks(&err.to_string());
                eprintln!(
                    "{}",
                    localize(
                        globals,
                        format!(
                            "warning: stat({display}) failed: {err_one_line}; treating as 'output exists'"
                        ),
                        format!("警告：stat({display}) 失败：{err_one_line}；按「输出存在」处理"),
                    )
                );
            }
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
//
// Windows junction caveat: a junction whose target points at a
// non-existent location is NOT caught by `create_new(true)` — the
// file is created at the resolved location, not at the junction.
// `output_path_exists` uses `fs::metadata` which DOES follow
// junctions, so the cheap-first existence check would have caught
// any such junction's target if it existed at check time. The
// race-window junction-swap is bounded by single-user scope.
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
    let raw = path.to_string_lossy();
    // Strip the Win32 extended-length prefix BEFORE slash folding so a
    // future caller passing canonicalize() output keys identically to a
    // sibling caller passing the user-shape `C:\…`. Without this strip,
    // `\\?\C:\foo.ass` and `C:\foo.ass` produce different keys (the
    // former becomes `//?/c:/foo.ass`, the latter `c:/foo.ass`) and
    // within-batch dedup misses. Mirrors fonts::normalize_canonical_path.
    let stripped: &str = if let Some(rest) = raw.strip_prefix("\\\\?\\UNC\\") {
        // \\?\UNC\server\share\... → //server/share/...
        // (handled inside the slash fold below; reattach the leading
        // backslashes that map to // after folding)
        return normalize_output_key_after_strip(&format!("\\\\{rest}"));
    } else if let Some(rest) = raw.strip_prefix("\\\\?\\") {
        rest
    } else {
        raw.as_ref()
    };
    normalize_output_key_after_strip(stripped)
}

fn normalize_output_key_after_strip(s: &str) -> String {
    let normalized = s.replace('\\', "/").nfc().collect::<String>();
    // Lowercase on case-insensitive filesystems (Codex dd2d9554): Windows
    // NTFS and macOS APFS / HFS+ default to case-insensitive, so
    // `Episode.ass` and `episode.ass` collide on disk and must collapse
    // to one dedup key. Linux ext4 / btrfs / xfs are case-sensitive and
    // keep distinct names distinct. macOS users who opt into the
    // case-sensitive APFS variant are <1% and have to live with the
    // over-merge (better to over-merge than to under-merge and silently
    // overwrite outputs).
    if cfg!(windows) || cfg!(target_os = "macos") {
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
    // Enforce strictly-descending unit order so each unit appears at
    // most once and only in canonical h→m→s→ms sequence. Without this,
    // `+1s2s` parses as 3000 ms (silent sum) and `+30s1m` flips the
    // documented `+1m30s` form. Per the locked help docs, only the
    // canonical descending form is contract.
    let mut last_rank: Option<u8> = None;

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

        let unit_lower = rest[unit_start..index].to_ascii_lowercase();
        let (factor, rank) = match &unit_lower[..] {
            "ms" => (1.0, 1u8),
            "s" => (1000.0, 2u8),
            "m" => (60_000.0, 3u8),
            "h" => (3_600_000.0, 4u8),
            unit => return Err(format!("unsupported duration unit '{unit}'")),
        };
        if let Some(prev) = last_rank {
            if rank >= prev {
                return Err(format!(
                    "duration units must appear at most once and in descending order \
                     (h, m, s, ms); '{unit_lower}' followed a same-or-larger unit"
                ));
            }
        }
        last_rank = Some(rank);
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
        engine, normalize_output_key, parse_duration_ms, parse_timestamp_ms,
        predict_chain_output_path, relocate_output_path, substitute_template, write_output,
        GlobalOptions, OutputLang, TempFontDbDir, CLI_FONT_DB_FILENAME,
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
            no_cache: false,
            cache_file: None,
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
    fn rejects_repeated_or_out_of_order_units() {
        // Same-unit repetition: silent-sum bug from Round 1 review
        // (A-R1-14 / N-R2-1 / A-R2-1).
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
        // Lowercase expected on case-insensitive filesystems (Windows + macOS);
        // matches the production normalize_output_key_after_strip logic so
        // Linux CI sees a case-distinct key and macOS/Windows see the folded form.
        let expected_key = if cfg!(windows) || cfg!(target_os = "macos") {
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
        let expected_key = if cfg!(windows) || cfg!(target_os = "macos") {
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

        if cfg!(windows) || cfg!(target_os = "macos") {
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

    // ── substitute_template — Codex bd782f90 regression coverage ──

    #[test]
    fn substitute_template_preserves_double_dots_inside_user_content() {
        // Old blanket `replace("..", ".")` mangled this — Codex bd782f90.
        let got = substitute_template(
            "{name}.shifted{ext}",
            &[("name", "Show..special"), ("ext", ".ass")],
        );
        assert_eq!(got, "Show..special.shifted.ass");
    }

    #[test]
    fn substitute_template_collapses_boundary_double_dots() {
        // Template-side dot + ext-leading dot at the seam → drop one.
        let got = substitute_template("{name}.{ext}", &[("name", "Show"), ("ext", ".ass")]);
        assert_eq!(got, "Show.ass");
    }

    #[test]
    fn substitute_template_collapses_template_literal_dot_runs() {
        // `..` inside the user-typed template (typo) collapses, but
        // user-content `..` would not (covered by the preserve test).
        let got = substitute_template("{name}..shifted{ext}", &[("name", "Show"), ("ext", ".ass")]);
        assert_eq!(got, "Show.shifted.ass");
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
        assert_eq!(got, "Show$1$&.shifted.ass");
    }

    #[test]
    fn substitute_template_missing_token_becomes_empty() {
        let got = substitute_template("{name}.{lang}{ext}", &[("name", "Show"), ("ext", ".ass")]);
        // Empty {lang} between two dots → boundary collapse leaves "Show.ass".
        assert_eq!(got, "Show.ass");
    }

    #[test]
    fn substitute_template_leaves_unknown_braces_intact() {
        // Token shape doesn't match (uppercase) → kept as literal text.
        let got = substitute_template("{NAME}.{ext}", &[("name", "Show"), ("ext", ".ass")]);
        assert_eq!(got, "{NAME}.ass");
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
}
