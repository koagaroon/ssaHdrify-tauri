//! CLI clap definitions, step-plan serialization, locale detection, and
//! duration/timestamp argument parsers.

use super::*;
use clap::{Args, Parser, Subcommand, ValueEnum};
use serde::Serialize;
use std::path::PathBuf;
use std::sync::OnceLock;

/// Command-line interface for SSA HDRify subtitle workflows.
///
/// SSA HDRify 字幕工作流命令行工具。
#[derive(Debug, Parser)]
#[command(name = "ssahdrify-cli", version, arg_required_else_help = true)]
pub(super) struct Cli {
    #[command(flatten)]
    pub(super) globals: GlobalOptions,

    #[command(subcommand)]
    pub(super) command: Command,
}

#[derive(Args, Debug)]
pub(super) struct GlobalOptions {
    /// Output directory. Defaults to each input file's directory. 输出目录；不指定时为每个输入文件所在目录。
    #[arg(long, global = true, value_name = "DIR")]
    pub(super) output_dir: Option<PathBuf>,

    /// Replace existing output files instead of skipping them. 覆盖已存在的输出文件而非跳过。
    #[arg(long, global = true)]
    pub(super) overwrite: bool,

    /// Show planned work without writing files. 预演计划工作但不写入文件。
    #[arg(long, global = true)]
    pub(super) dry_run: bool,

    /// Suppress normal progress output. 抑制常规进度输出。
    #[arg(long, global = true, conflicts_with = "verbose")]
    pub(super) quiet: bool,

    /// Show more progress detail. 显示更多进度细节。
    #[arg(long, global = true)]
    pub(super) verbose: bool,

    /// Emit machine-readable JSON. 输出机器可读的 JSON。
    #[arg(long, global = true)]
    pub(super) json: bool,

    /// Output language. Defaults to OS locale (zh* → zh, otherwise en). 输出语言；不指定时按系统区域设置自动检测。
    #[arg(long, global = true, value_enum, value_name = "LANG")]
    pub(super) lang: Option<OutputLang>,

    /// Skip the persistent font cache for this run. Cache file is left
    /// untouched. Use when you want a fresh scan without affecting
    /// the cached state. 本次运行跳过持久化字体缓存；缓存文件保持不变。
    // Declared globally so cache-aware subcommands share one parser definition.
    #[arg(long, global = true)]
    pub(super) no_cache: bool,

    /// Override the default font cache file path. Default location
    /// is `%APPDATA%/ssahdrify/cli_font_cache.sqlite3`. Useful for testing or
    /// non-default layouts. 覆盖字体缓存文件路径。
    // Global for the same shared-parser reason as no_cache. Cache-aware
    // commands validate the path before opening it; chain reports it as inert.
    #[arg(long, global = true, value_name = "PATH")]
    pub(super) cache_file: Option<PathBuf>,

    /// Abort the batch on the first failed file. Useful when running
    /// many inputs unattended — surfaces the first failure right away
    /// instead of finishing the whole batch and forcing a log scroll.
    /// Files processed before the failure are kept; remaining files
    /// stay untouched.
    /// 首个失败文件即终止批处理。在无人值守批量运行时有用：第一次失败
    /// 立刻显现，而非跑完整批后再翻日志。失败前已处理的文件保留，
    /// 剩余文件不动。
    ///
    /// Pairs naturally with `embed --on-missing fail` (which marks a
    /// file Failed when any font is missing OR fails subsetting): set
    /// both flags to get "stop the batch the moment any font can't be
    /// embedded." `--fail-fast` itself is policy-neutral — it triggers
    /// on whatever the per-subcommand failure semantics produce.
    /// 与 `embed --on-missing fail`（缺失字体或子集化失败时标记
    /// 文件为 Failed）天然搭配：两者同开即"任何字体无法嵌入立即停"。
    /// `--fail-fast` 本身不规定何为失败，只在各子命令现有失败语义
    /// 触发时生效。
    #[arg(long, global = true)]
    pub(super) fail_fast: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub(super) enum OutputLang {
    En,
    Zh,
}

#[derive(Args, Debug, Clone, Default)]
pub(super) struct DiagnoseOptions {
    /// Attach diagnostics to this command. `--diagnose` is the same as
    /// `--diagnose=summary`; use `--diagnose=full` for per-file and
    /// per-font tier detail. 附加诊断输出；`--diagnose` 等同 summary。
    #[arg(
        long,
        value_enum,
        value_name = "MODE",
        num_args = 0..=1,
        default_missing_value = "summary",
        require_equals = true
    )]
    pub(super) diagnose: Option<DiagnoseMode>,
}

impl DiagnoseOptions {
    pub(super) fn mode(&self) -> Option<DiagnoseMode> {
        self.diagnose
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum, Serialize)]
#[serde(rename_all = "kebab-case")]
pub(super) enum DiagnoseMode {
    Summary,
    Full,
}

#[derive(Debug, Subcommand)]
pub(super) enum Command {
    /// Print bundled JavaScript and GUI asset license notices as plain text.
    /// 输出内嵌的 JavaScript 与 GUI 资源许可证声明（纯文本）。
    Licenses,
    /// Convert SDR subtitle colors to HDR. 将 SDR 字幕颜色转换为 HDR。
    Hdr(HdrCommandArgs),
    /// Shift subtitle timings by an offset. 按偏移量平移字幕时间轴。
    Shift(ShiftCommandArgs),
    /// Embed fonts into ASS subtitle files. 将字体嵌入 ASS 字幕文件。
    ///
    /// Tips / 提示:
    ///   --font-dir, --recursive-font-dir, and --font-file are repeatable.
    ///   --font-dir、--recursive-font-dir 与 --font-file 均可重复传入。
    ///
    ///   Font cache (when present): used automatically. Explicit font
    ///   sources merge with it; without them, the cache is the primary
    ///   source. Pass --no-cache to skip it for one run, or run
    ///   `refresh-fonts` to rebuild it.
    ///   字体缓存（如已存在）会自动使用；显式字体源会与缓存合并。
    ///   未提供显式字体源时缓存为主源。--no-cache 跳过本次；
    ///   refresh-fonts 重建。
    Embed(EmbedCommandArgs),
    /// Pair subtitles with videos and rename subtitles to match. 配对视频和字幕，按视频名重命名字幕。
    Rename(RenameCommandArgs),

    /// Diagnose ASS/SSA font resolution without writing subtitle files.
    /// 诊断 ASS/SSA 字体解析，不写出字幕文件。
    DiagnoseFonts(DiagnoseFontsArgs),

    /// Build or refresh the persistent font cache. Always requires at
    /// least one --font-dir or --recursive-font-dir (cache-recorded
    /// source roots are not auto-rescanned; user must specify).
    /// 构建或刷新持久化字体缓存。始终必须传至少一个 --font-dir 或
    /// --recursive-font-dir（缓存记录的 source roots 不会自动 rescan，
    /// 用户必须显式指定）。
    ///
    /// --font-dir scans one level; --recursive-font-dir scans the root
    /// and its ordinary subfolders without following links or junctions.
    ///
    /// Example / 示例:
    ///   ssahdrify-cli refresh-fonts --recursive-font-dir ./FontLibrary
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
    pub(super) eotf: EotfArg,

    /// Target subtitle brightness in nits. 字幕目标亮度（nits）。
    #[arg(
        long,
        default_value_t = 203,
        value_parser = clap::value_parser!(u16).range(1..=10_000)
    )]
    pub(super) nits: u16,

    /// Output filename template. 输出文件名模板。
    #[arg(long, default_value = "{name}.hdr.ass")]
    pub(super) output_template: String,

    /// Subtitle files to convert. 要转换的字幕文件。
    // pub(crate) so the chain module's parser can take/clear this
    // field after parsing each step segment. Other fields stay
    // private until a callsite needs them.
    #[arg(required = true)]
    pub(crate) files: Vec<PathBuf>,
}

#[derive(Args, Debug)]
pub(super) struct HdrCommandArgs {
    #[command(flatten)]
    pub(super) args: HdrArgs,
    #[command(flatten)]
    pub(super) diagnose: DiagnoseOptions,
}

#[derive(Clone, Copy, Debug, ValueEnum)]
pub(super) enum EotfArg {
    Pq,
    Hlg,
}

impl EotfArg {
    pub(super) fn as_engine_value(self) -> &'static str {
        match self {
            EotfArg::Pq => "PQ",
            EotfArg::Hlg => "HLG",
        }
    }
}

#[derive(Args, Debug)]
pub(crate) struct ShiftArgs {
    /// Signed duration, for example "+2.5s", "-500ms", or "+1m30s". 带符号的偏移量，如 "+2.5s"、"-500ms" 或 "+1m30s"。
    #[arg(
        long,
        allow_hyphen_values = true,
        required_unless_present = "map",
        conflicts_with = "map"
    )]
    pub(super) offset: Option<String>,

    /// Shift only entries after this timestamp.
    /// Format: HH:MM:SS, HH:MM:SS.mmm, or HH:MM:SS,mmm (ISO 8601 comma form).
    /// 仅平移此时间戳之后的字幕条目。格式：HH:MM:SS、HH:MM:SS.mmm 或 HH:MM:SS,mmm。
    #[arg(long, conflicts_with = "map")]
    pub(super) after: Option<String>,

    /// Apply a timing-map file instead of one global offset. Supports JSON rules or CSV lines: start,end,offset[,label[,enabled]].
    /// 使用时间轴映射文件，而不是单一全局偏移。支持 JSON 规则或 CSV 行：start,end,offset[,label[,enabled]]。
    #[arg(long = "map", value_name = "FILE", conflicts_with_all = ["offset", "after"])]
    pub(super) map: Option<PathBuf>,

    /// Output filename template. 输出文件名模板。
    #[arg(long, default_value = "{name}.shifted{ext}")]
    pub(super) output_template: String,

    /// Subtitle files to shift. 要平移的字幕文件。
    // See note on HdrArgs.files.
    #[arg(required = true)]
    pub(crate) files: Vec<PathBuf>,
}

#[derive(Args, Debug)]
pub(super) struct ShiftCommandArgs {
    #[command(flatten)]
    pub(super) args: ShiftArgs,
    #[command(flatten)]
    pub(super) diagnose: DiagnoseOptions,
}

#[derive(Args, Debug)]
pub(crate) struct EmbedArgs {
    /// Add a shallow font folder (repeatable). 添加浅层字体目录（可重复传入）。
    ///
    /// Pass once per folder; only font files directly inside each folder
    /// are scanned. Use --recursive-font-dir for a whole folder tree.
    /// 每个目录传一次；只扫描目录直接包含的字体文件。完整目录树请使用
    /// --recursive-font-dir。
    ///
    /// Example / 示例:
    ///   ssahdrify-cli embed --font-dir ./fonts --font-dir C:/MyFonts subs.ass
    #[arg(long = "font-dir", value_name = "DIR")]
    pub(super) font_dirs: Vec<PathBuf>,

    /// Add a recursive font library root (repeatable). 添加递归字体库根目录（可重复传入）。
    ///
    /// Scans the root and all ordinary subfolders. Symbolic links,
    /// junctions, and other reparse points are not followed.
    /// 扫描根目录及所有普通子目录；不会跟随符号链接、目录联接或其他
    /// 重解析点（reparse point）。
    ///
    /// Example / 示例:
    ///   ssahdrify-cli embed --recursive-font-dir ./FontLibrary subs.ass
    #[arg(long = "recursive-font-dir", value_name = "DIR")]
    pub(super) recursive_font_dirs: Vec<PathBuf>,

    /// Add a specific font file (repeatable). 添加具体字体文件（可重复传入）。
    ///
    /// Pass once per file; useful for embedding a single TTF/OTF without
    /// scanning a whole directory.
    /// 每个文件传一次；适合只嵌入单个 TTF/OTF 而不扫描整个目录。
    ///
    /// Example / 示例:
    ///   ssahdrify-cli embed --font-file ./SmileySans.ttf --font-file ./MyFont.otf subs.ass
    #[arg(long = "font-file", value_name = "FILE")]
    pub(super) font_files: Vec<PathBuf>,

    /// Do not use system-installed fonts. 不使用系统已安装的字体。
    #[arg(long)]
    pub(super) no_system_fonts: bool,

    /// Behavior when a referenced font cannot be embedded — either
    /// because the family was not found in any font source, OR because
    /// the font was found but failed to subset (corrupted file,
    /// unsupported table layout, etc.). `warn` (default) emits a
    /// warning and embeds whatever did work; `fail` marks the file
    /// Failed in the batch summary and surfaces a non-zero exit code.
    /// Pair with `--fail-fast` to also abort the rest of the batch.
    /// 引用字体无法嵌入时的行为——无论是任何字体源都找不到该家族，
    /// 还是找到了但子集化失败（文件损坏、字体表布局不支持等）。
    /// `warn`（默认）发警告并嵌入已成功的字体；`fail` 在批量汇总中
    /// 标记该文件失败并触发非零退出码。与 `--fail-fast` 搭配可同时
    /// 终止剩余批次。
    #[arg(long, value_enum, default_value_t = MissingFontAction::Warn)]
    pub(super) on_missing: MissingFontAction,

    /// Output filename template. 输出文件名模板。
    #[arg(long, default_value = "{name}.embed.ass")]
    pub(super) output_template: String,

    /// ASS/SSA files to process. 要处理的 ASS/SSA 文件。
    // See note on HdrArgs.files.
    #[arg(required = true)]
    pub(crate) files: Vec<PathBuf>,
}

#[derive(Args, Debug)]
pub(super) struct EmbedCommandArgs {
    #[command(flatten)]
    pub(super) args: EmbedArgs,
    #[command(flatten)]
    pub(super) diagnose: DiagnoseOptions,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub(super) enum MissingFontAction {
    Warn,
    Fail,
}

#[derive(Args, Debug)]
pub(super) struct RenameArgs {
    /// Output mode. 输出模式。
    #[arg(long, value_enum, default_value_t = RenameMode::CopyToVideo)]
    pub(super) mode: RenameMode,

    /// Language selection: auto chooses one subtitle per video; all/list keep language suffixes. 语言选择：auto 每集选一个字幕；all/列表会保留语言后缀。
    #[arg(long, default_value = "auto")]
    pub(super) langs: String,

    /// Video/subtitle files or folders to pair. 要配对的视频/字幕文件或文件夹。
    #[arg(required = true)]
    pub(super) paths: Vec<PathBuf>,
}

#[derive(Args, Debug)]
pub(super) struct RenameCommandArgs {
    #[command(flatten)]
    pub(super) args: RenameArgs,
    #[command(flatten)]
    pub(super) diagnose: DiagnoseOptions,
}

#[derive(Args, Debug)]
pub(super) struct DiagnoseFontsArgs {
    /// Add a shallow font folder for this diagnostic run (repeatable).
    /// 添加本次诊断使用的浅层字体目录（可重复传入）。
    #[arg(long = "font-dir", value_name = "DIR")]
    pub(super) font_dirs: Vec<PathBuf>,

    /// Add a recursive font library root for this diagnostic run (repeatable).
    /// 添加本次诊断使用的递归字体库根目录（可重复传入）。
    #[arg(long = "recursive-font-dir", value_name = "DIR")]
    pub(super) recursive_font_dirs: Vec<PathBuf>,

    /// Add a specific font file for this diagnostic run (repeatable). 添加本次诊断使用的字体文件（可重复传入）。
    #[arg(long = "font-file", value_name = "FILE")]
    pub(super) font_files: Vec<PathBuf>,

    /// Do not use system-installed fonts. 不使用系统已安装的字体。
    #[arg(long)]
    pub(super) no_system_fonts: bool,

    /// Also try in-memory subsetting for resolved fonts. 额外对已解析字体执行内存中的子集化检查。
    #[arg(long = "subset-check")]
    pub(super) subset_check: bool,

    /// ASS/SSA files to diagnose. 要诊断的 ASS/SSA 文件。
    #[arg(required = true)]
    pub(super) files: Vec<PathBuf>,
}

impl DiagnoseFontsArgs {
    pub(super) fn to_embed_args(&self) -> EmbedArgs {
        EmbedArgs {
            font_dirs: self.font_dirs.clone(),
            recursive_font_dirs: self.recursive_font_dirs.clone(),
            font_files: self.font_files.clone(),
            no_system_fonts: self.no_system_fonts,
            on_missing: MissingFontAction::Warn,
            output_template: "{name}.embed.ass".to_string(),
            files: self.files.clone(),
        }
    }
}

#[derive(Clone, Copy, Debug, ValueEnum)]
pub(super) enum RenameMode {
    Rename,
    CopyToVideo,
    CopyToChosen,
}

impl RenameMode {
    pub(super) fn as_engine_value(self) -> &'static str {
        match self {
            RenameMode::Rename => "rename",
            RenameMode::CopyToVideo => "copy_to_video",
            RenameMode::CopyToChosen => "copy_to_chosen",
        }
    }

    pub(super) fn is_copy(self) -> bool {
        matches!(self, RenameMode::CopyToVideo | RenameMode::CopyToChosen)
    }
}

#[derive(Args, Debug)]
pub(super) struct RefreshFontsArgs {
    /// Add a shallow font folder to scan (repeatable). Pass this or
    /// --recursive-font-dir at least once. 添加要浅层扫描的字体目录
    /// （可重复传入）；必须至少传入此参数或 --recursive-font-dir 之一。
    ///
    /// Each folder is scanned one level deep (non-recursive); same
    /// semantics as `embed --font-dir`. Use --recursive-font-dir to
    /// index a tree from one root.
    /// 每个目录扫描一层（不递归）；与 `embed --font-dir` 语义一致。
    /// 树状字体目录请使用 --recursive-font-dir。
    #[arg(
        long = "font-dir",
        value_name = "DIR",
        required_unless_present = "recursive_font_dirs"
    )]
    pub(super) font_dirs: Vec<PathBuf>,

    /// Add a recursive font library root to scan (repeatable). Pass this
    /// or --font-dir at least once. 添加要递归扫描的字体库根目录（可重复传入）；
    /// 必须至少传入此参数或 --font-dir 之一。
    #[arg(
        long = "recursive-font-dir",
        value_name = "DIR",
        required_unless_present = "font_dirs"
    )]
    pub(super) recursive_font_dirs: Vec<PathBuf>,
}

// `ChainArgs` wraps the top-level `chain` subcommand rather than an individual
// `ParsedStep` variant; non-CLI callers constructing a `ChainPlan` should build
// `ChainPlan` directly rather than synthesizing `ChainArgs`.
#[derive(Args, Debug)]
pub(super) struct ChainArgs {
    /// Chain-global output filename template applied at the terminal step.
    /// Defaults to a stacked-suffix form (`{name}.<step1>.<step2>...<stepN>.ass`).
    /// 链全局输出文件名模板，仅终端步骤应用；缺省按各步后缀堆叠。
    #[arg(long)]
    pub(super) output_template: Option<String>,

    /// Steps and input files: `<step1> + <step2> + ... <stepN> file...`.
    /// 步骤与输入文件：`<step1> + <step2> + ... <stepN> file...`。
    ///
    // `trailing_var_arg` captures everything after the first positional
    // (which is also the first step's keyword). `allow_hyphen_values`
    // is required because step segments contain `--eotf`-style flags
    // that would otherwise be interpreted as ChainArgs's own flags.
    #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
    pub(super) raw_argv: Vec<String>,
}

// ── Chain serialization ─────────────────────────────────────
//
// Each per-feature Args struct provides a `to_chain_step` method
// that produces a JSON shape matching the TS-side `ChainStep`
// discriminated union (see src/features/chain/chain-types.ts).
// Living in main.rs keeps the field-access privacy minimal — chain
// step variants need to read `eotf`, `nits`, `offset`, `after`,
// `font_dirs`, `recursive_font_dirs`, etc., which are private to main.rs.
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
    /// Serialize this step into the chain runtime's `{ kind, params }`
    /// JSON shape. Infallible: the `--offset` /
    /// `--after` strings are validated by `chain::parse_chain_argv`
    /// right after `parse_one_step` succeeds — any parse error
    /// surfaces at chain-parse time. The `.expect()` calls below
    /// reference that validation site; if either fires, the
    /// chain-parse gate regressed and the panic is the correct fail-
    /// fast (better than a silent semantic divergence).
    pub(crate) fn to_chain_step(&self) -> serde_json::Value {
        let offset = self
            .offset
            .as_deref()
            .expect("ShiftArgs.offset validated in chain::parse_chain_argv");
        let offset_ms = parse_duration_ms(offset)
            .expect("ShiftArgs.offset validated in chain::parse_chain_argv");
        let threshold_ms = self.after.as_deref().map(|text| {
            parse_timestamp_ms(text).expect("ShiftArgs.after validated in chain::parse_chain_argv")
        });
        let mut params = serde_json::json!({ "offsetMs": offset_ms });
        if let Some(t) = threshold_ms {
            params["thresholdMs"] = serde_json::Value::from(t);
        }
        serde_json::json!({
            "kind": "shift",
            "params": params,
        })
    }
}

impl EmbedArgs {
    pub(super) fn has_user_font_sources(&self) -> bool {
        !self.recursive_font_dirs.is_empty()
            || !self.font_dirs.is_empty()
            || !self.font_files.is_empty()
    }

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
        let recursive_font_dirs: Vec<String> = self
            .recursive_font_dirs
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
                "recursiveFontDirs": recursive_font_dirs,
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

pub(super) fn localize(globals: &GlobalOptions, en: String, zh: String) -> String {
    match globals.lang.unwrap_or_else(detect_os_locale) {
        OutputLang::En => en,
        OutputLang::Zh => zh,
    }
}

pub(super) fn emit_verbose(globals: &GlobalOptions, en: String, zh: String) {
    if globals.verbose && !globals.json && !globals.quiet {
        println!("{}", localize(globals, en, zh));
    }
}

pub(super) fn emit_verbose_err(globals: &GlobalOptions, en: String, zh: String) {
    if globals.verbose && !globals.json && !globals.quiet {
        let message = localize(globals, en, zh);
        eprintln!("{}", sanitize_for_display(&message));
    }
}

// Detect OS UI locale once per process and cache it. sys-locale reads env
// vars (LC_ALL / LC_MESSAGES / LANG) on Unix and calls
// GetUserDefaultLocaleName on Windows — the same surface every other CLI
// tool uses. Empty / malformed locales fall through to En, matching the
// behavior users got before this detection landed.
pub(super) fn detect_os_locale() -> OutputLang {
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
pub(super) fn classify_locale(raw: &str) -> OutputLang {
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

pub(crate) fn parse_duration_ms(input: &str) -> Result<i64, String> {
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

pub(crate) fn parse_timestamp_ms(input: &str) -> Result<i64, String> {
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

pub(super) fn parse_timestamp_part(part: &str, label: &str) -> Result<i64, String> {
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
