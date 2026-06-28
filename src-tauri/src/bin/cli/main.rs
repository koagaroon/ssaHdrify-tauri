use std::collections::HashSet;
use std::fs;
use std::io::ErrorKind;
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
const MAX_TIMING_MAP_BYTES: u64 = 1024 * 1024;

/// Cumulative cap on aggregate raw font-subset bytes before they are
/// base64-encoded and handed to V8. Per-font cap is
/// `MAX_FONT_DATA_SIZE` (64 MB, in `app_lib::fonts`); the gap is the
/// cumulative case — a pathological subtitle referencing many heavy
/// fonts can produce hundreds of MB of raw subset bytes, then expand
/// to ~1.33× as base64 and again while the TS side builds uuencoded
/// ASS font entries. Reject before payload assembly with a focused
/// per-input error instead of letting deno_core panic with
/// FastStringV8AllocationError.
const MAX_V8_SUBSET_TOTAL_BYTES: usize = 100 * 1024 * 1024;
const MAX_CHAIN_SUBSET_TOTAL_BYTES: usize = MAX_V8_SUBSET_TOTAL_BYTES;
const MAX_EMBED_SUBSET_TOTAL_BYTES: usize = MAX_V8_SUBSET_TOTAL_BYTES;
const MAX_DIAGNOSTIC_SUBSET_CALLS: usize = 128;
const MAX_DIAGNOSTIC_SUBSET_TOTAL_BYTES: usize = MAX_V8_SUBSET_TOTAL_BYTES;

/// cap font.codepoints.len() at the CLI's
/// resolve_embed_fonts boundary before cloning into
/// ResolvedEmbedFont. `subset_font`'s downstream
/// `MAX_SUBSET_CODEPOINTS` (200_000, in `app_lib::fonts`) refuses
/// the actual subset call, but the clone happens earlier and a
/// V8/TS-supplied codepoint vec at full size (e.g., 1M elements ×
/// 4 bytes = 4 MB per font, multiplied across many fonts) sits in
/// the resolved-vec until subset_font runs. Half of the downstream
/// cap gives defense-in-depth headroom without rejecting cases that
/// would otherwise subset successfully.
const MAX_RESOLVED_FONT_CODEPOINTS: usize = 100_000;
// Single source of truth in `app_lib::fonts::USER_FONT_DB_FILENAME`
// : if the CLI ever needs to reference the literal
// again (the post-5.3a TempFontDbDir::drop uses remove_dir_all so it
// doesn't), import it from app_lib::fonts directly — do NOT re-declare
// the literal locally.

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
    ///
    /// `global = true` is intentional even though only `embed` /
    /// `diagnose-fonts` / `refresh-fonts` / `chain` consume the flag.
    /// The clap idiom for cross-subcommand flags is one declaration
    /// here; per-subcommand declaration would mean N duplicates +
    /// drift surface. Subcommands that don't read the flag (hdr /
    /// shift / rename) silently ignore it — standard clap behavior,
    /// not a no-silent-action violation: the flag has no observable
    /// effect anywhere it isn't read, so there's nothing to surface.
    #[arg(long, global = true)]
    no_cache: bool,

    /// Override the default font cache file path. Default location
    /// follows each OS's user-data convention: `%APPDATA%/ssahdrify/`
    /// on Windows, `$XDG_DATA_HOME/ssahdrify/` or `~/.local/share/ssahdrify/`
    /// on Linux, `~/Library/Application Support/ssahdrify/` on macOS,
    /// always named `cli_font_cache.sqlite3`. Useful for testing or
    /// non-default layouts. 覆盖字体缓存文件路径。
    ///
    /// Same `global = true` rationale as `no_cache` above: one
    /// declaration, cross-subcommand visible. Commands that read the
    /// cache validate the path before opening it; chain reports that
    /// the flag has no effect because chain v1 never uses the cache.
    #[arg(long, global = true, value_name = "PATH")]
    cache_file: Option<PathBuf>,

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
    fail_fast: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
enum OutputLang {
    En,
    Zh,
}

#[derive(Args, Debug, Clone, Default)]
struct DiagnoseOptions {
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
    diagnose: Option<DiagnoseMode>,
}

impl DiagnoseOptions {
    fn mode(&self) -> Option<DiagnoseMode> {
        self.diagnose
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum, Serialize)]
#[serde(rename_all = "kebab-case")]
enum DiagnoseMode {
    Summary,
    Full,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Convert SDR subtitle colors to HDR. 将 SDR 字幕颜色转换为 HDR。
    Hdr(HdrCommandArgs),
    /// Shift subtitle timings by an offset. 按偏移量平移字幕时间轴。
    Shift(ShiftCommandArgs),
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
    Embed(EmbedCommandArgs),
    /// Pair subtitles with videos and rename subtitles to match. 配对视频和字幕，按视频名重命名字幕。
    Rename(RenameCommandArgs),

    /// Diagnose ASS/SSA font resolution without writing subtitle files.
    /// 诊断 ASS/SSA 字体解析，不写出字幕文件。
    DiagnoseFonts(DiagnoseFontsArgs),

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

#[derive(Args, Debug)]
struct HdrCommandArgs {
    #[command(flatten)]
    args: HdrArgs,
    #[command(flatten)]
    diagnose: DiagnoseOptions,
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
    #[arg(
        long,
        allow_hyphen_values = true,
        required_unless_present = "map",
        conflicts_with = "map"
    )]
    offset: Option<String>,

    /// Shift only entries after this timestamp.
    /// Format: HH:MM:SS, HH:MM:SS.mmm, or HH:MM:SS,mmm (ISO 8601 comma form).
    /// 仅平移此时间戳之后的字幕条目。格式：HH:MM:SS、HH:MM:SS.mmm 或 HH:MM:SS,mmm。
    #[arg(long, conflicts_with = "map")]
    after: Option<String>,

    /// Apply a timing-map file instead of one global offset. Supports JSON rules or CSV lines: start,end,offset[,label[,enabled]].
    /// 使用时间轴映射文件，而不是单一全局偏移。支持 JSON 规则或 CSV 行：start,end,offset[,label[,enabled]]。
    #[arg(long = "map", value_name = "FILE", conflicts_with_all = ["offset", "after"])]
    map: Option<PathBuf>,

    /// Output filename template. 输出文件名模板。
    #[arg(long, default_value = "{name}.shifted{ext}")]
    output_template: String,

    /// Subtitle files to shift. 要平移的字幕文件。
    // See note on HdrArgs.files.
    #[arg(required = true)]
    pub(crate) files: Vec<PathBuf>,
}

#[derive(Args, Debug)]
struct ShiftCommandArgs {
    #[command(flatten)]
    args: ShiftArgs,
    #[command(flatten)]
    diagnose: DiagnoseOptions,
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
    on_missing: MissingFontAction,

    /// Output filename template. 输出文件名模板。
    #[arg(long, default_value = "{name}.embed.ass")]
    output_template: String,

    /// ASS/SSA files to process. 要处理的 ASS/SSA 文件。
    // See note on HdrArgs.files.
    #[arg(required = true)]
    pub(crate) files: Vec<PathBuf>,
}

#[derive(Args, Debug)]
struct EmbedCommandArgs {
    #[command(flatten)]
    args: EmbedArgs,
    #[command(flatten)]
    diagnose: DiagnoseOptions,
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

    /// Language selection: auto chooses one subtitle per video; all/list keep language suffixes. 语言选择：auto 每集选一个字幕；all/列表会保留语言后缀。
    #[arg(long, default_value = "auto")]
    langs: String,

    /// Video/subtitle files or folders to pair. 要配对的视频/字幕文件或文件夹。
    #[arg(required = true)]
    paths: Vec<PathBuf>,
}

#[derive(Args, Debug)]
struct RenameCommandArgs {
    #[command(flatten)]
    args: RenameArgs,
    #[command(flatten)]
    diagnose: DiagnoseOptions,
}

#[derive(Args, Debug)]
struct DiagnoseFontsArgs {
    /// Add a font folder for this diagnostic run (repeatable). 添加本次诊断使用的字体目录（可重复传入）。
    #[arg(long = "font-dir", value_name = "DIR")]
    font_dirs: Vec<PathBuf>,

    /// Add a specific font file for this diagnostic run (repeatable). 添加本次诊断使用的字体文件（可重复传入）。
    #[arg(long = "font-file", value_name = "FILE")]
    font_files: Vec<PathBuf>,

    /// Do not use system-installed fonts. 不使用系统已安装的字体。
    #[arg(long)]
    no_system_fonts: bool,

    /// Also try in-memory subsetting for resolved fonts. 额外对已解析字体执行内存中的子集化检查。
    #[arg(long = "subset-check")]
    subset_check: bool,

    /// ASS/SSA files to diagnose. 要诊断的 ASS/SSA 文件。
    #[arg(required = true)]
    files: Vec<PathBuf>,
}

impl DiagnoseFontsArgs {
    fn to_embed_args(&self) -> EmbedArgs {
        EmbedArgs {
            font_dirs: self.font_dirs.clone(),
            font_files: self.font_files.clone(),
            no_system_fonts: self.no_system_fonts,
            on_missing: MissingFontAction::Warn,
            output_template: "{name}.embed.ass".to_string(),
            files: self.files.clone(),
        }
    }
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

// (visibility asymmetry WHY): `output_template`
// and `raw_argv` stay private to main.rs. Sibling Args structs
// (HdrArgs / ShiftArgs / EmbedArgs) use `pub(crate)` on their `files`
// field because `chain::take_step_files` reaches in via the
// ParsedStep variant to extract them — see `chain.rs` line 371-377.
// ChainArgs itself doesn't flow into ParsedStep (it's the wrapper
// for the `chain` subcommand, not a chain step), so its fields have
// no cross-module consumer. Keep private as the explicit signal —
// if a future feature parses a chain plan from JSON / config-file
// instead of clap argv, that path should construct ChainPlan
// directly rather than reach in here.
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
    #[serde(skip_serializing_if = "Option::is_none")]
    diagnostics: Option<CommandDiagnostics>,
    /// True when `--fail-fast` short-circuited the per-file loop after
    /// a failure. JSON consumers use this to distinguish "all remaining
    /// inputs tried and failed" from "first failure aborted the rest";
    /// `results.len()` together with `aborted_by_fail_fast` tells the
    /// caller how many inputs were actually processed.
    ///
    /// Field is skipped from JSON output when false to keep the
    /// pre-existing JSON shape backward-compatible (consumers reading
    /// pre-`--fail-fast` output keep working).
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    aborted_by_fail_fast: bool,
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

#[derive(Debug, Serialize, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
enum FileStatus {
    Written,
    Planned,
    Diagnosed,
    Skipped,
    Failed,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct CommandDiagnostics {
    mode: DiagnoseMode,
    files_with_warnings: usize,
    warning_count: usize,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    notes: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    files: Vec<FileDiagnostic>,
    #[serde(skip_serializing_if = "Option::is_none")]
    cache: Option<CacheDiagnostic>,
    #[serde(skip_serializing_if = "Option::is_none")]
    qa: Option<FontQaSummary>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    fonts: Vec<FontDiagnostic>,
}

#[derive(Debug, Serialize, Clone)]
#[serde(rename_all = "camelCase")]
struct FontQaSummary {
    status: FontQaStatus,
    file_count: usize,
    failed_file_count: usize,
    font_reference_count: usize,
    resolved_count: usize,
    missing_count: usize,
    error_count: usize,
    subset_checked_count: usize,
    subset_ok_count: usize,
    subset_failed_count: usize,
    subset_skipped_count: usize,
}

#[derive(Debug, Serialize, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
enum FontQaStatus {
    Complete,
    Incomplete,
    Blocked,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct FileDiagnostic {
    input: String,
    output: Option<String>,
    encoding: Option<String>,
    status: FileStatus,
    error: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    warnings: Vec<String>,
}

#[derive(Debug, Serialize, Clone)]
#[serde(rename_all = "camelCase")]
struct CacheDiagnostic {
    path: Option<String>,
    status: CacheDiagnosticStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    message: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    found_schema: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    expected_schema: Option<i64>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    modified_folders: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    removed_folders: Vec<String>,
}

#[derive(Debug, Serialize, Clone, Copy)]
#[serde(rename_all = "kebab-case")]
enum CacheDiagnosticStatus {
    Disabled,
    DryRun,
    Missing,
    Usable,
    SchemaMismatch,
    OpenError,
    ValidationError,
    Drift,
    PathError,
}

impl CacheDiagnostic {
    fn new(path: Option<String>, status: CacheDiagnosticStatus, message: Option<String>) -> Self {
        Self {
            path,
            status,
            message,
            found_schema: None,
            expected_schema: None,
            modified_folders: Vec::new(),
            removed_folders: Vec::new(),
        }
    }
}

struct PreparedFontCache {
    cache: Option<app_lib::font_cache::FontCache>,
    diagnostic: CacheDiagnostic,
}

#[derive(Debug, Serialize, Clone)]
#[serde(rename_all = "camelCase")]
struct FontDiagnostic {
    #[serde(skip_serializing_if = "Option::is_none")]
    file: Option<String>,
    label: String,
    family: String,
    embedded_font_name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    requested_embedded_font_name: Option<String>,
    bold: bool,
    italic: bool,
    glyph_count: usize,
    result: FontResolutionResult,
    #[serde(skip_serializing_if = "Option::is_none")]
    path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    index: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    subset_check: Option<FontSubsetCheckDiagnostic>,
    tiers: Vec<FontTierDiagnostic>,
}

#[derive(Debug, Serialize, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
enum FontResolutionResult {
    Resolved,
    Missing,
    Error,
}

#[derive(Debug, Serialize, Clone)]
#[serde(rename_all = "camelCase")]
struct FontSubsetCheckDiagnostic {
    status: FontSubsetCheckStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    bytes: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}

#[derive(Debug, Serialize, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
enum FontSubsetCheckStatus {
    Ok,
    Failed,
    Skipped,
}

#[derive(Debug, Default)]
struct DiagnosticSubsetBudget {
    calls: usize,
    bytes: usize,
    exhausted_reason: Option<String>,
}

impl DiagnosticSubsetBudget {
    fn skipped(reason: String) -> FontSubsetCheckDiagnostic {
        FontSubsetCheckDiagnostic {
            status: FontSubsetCheckStatus::Skipped,
            bytes: None,
            error: Some(reason),
        }
    }

    fn exhausted_check(&self) -> Option<FontSubsetCheckDiagnostic> {
        self.exhausted_reason
            .as_ref()
            .map(|reason| Self::skipped(reason.clone()))
    }

    fn begin_call(&mut self) -> Option<FontSubsetCheckDiagnostic> {
        if let Some(check) = self.exhausted_check() {
            return Some(check);
        }
        if self.calls >= MAX_DIAGNOSTIC_SUBSET_CALLS {
            let reason =
                format!("subset-check call budget exceeded ({MAX_DIAGNOSTIC_SUBSET_CALLS} calls)");
            self.exhausted_reason = Some(reason.clone());
            return Some(Self::skipped(reason));
        }
        self.calls += 1;
        None
    }

    fn finish_bytes(&mut self, len: usize) {
        let Some(total) = self
            .bytes
            .checked_add(len)
            .filter(|total| *total <= MAX_DIAGNOSTIC_SUBSET_TOTAL_BYTES)
        else {
            let reason = format!(
                "subset-check byte budget exceeded ({len} bytes would exceed max {MAX_DIAGNOSTIC_SUBSET_TOTAL_BYTES})"
            );
            self.exhausted_reason = Some(reason.clone());
            return;
        };
        self.bytes = total;
    }
}

#[derive(Debug, Serialize, Clone)]
#[serde(rename_all = "camelCase")]
struct FontTierDiagnostic {
    tier: FontResolveTier,
    status: FontTierStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    index: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    reason: Option<String>,
}

#[derive(Debug, Serialize, Clone, Copy)]
#[serde(rename_all = "kebab-case")]
enum FontResolveTier {
    Local,
    Cache,
    System,
}

#[derive(Debug, Serialize, Clone, Copy)]
#[serde(rename_all = "kebab-case")]
enum FontTierStatus {
    Hit,
    Miss,
    Disabled,
    Unavailable,
    Error,
}

impl FontDiagnostic {
    fn new(font: &engine::FontEmbedUsage) -> Self {
        Self {
            file: None,
            label: font.label.clone(),
            family: font.family.clone(),
            embedded_font_name: font.font_name.clone(),
            requested_embedded_font_name: None,
            bold: font.bold,
            italic: font.italic,
            glyph_count: font.glyph_count,
            result: FontResolutionResult::Missing,
            path: None,
            index: None,
            error: None,
            subset_check: None,
            tiers: Vec::new(),
        }
    }

    fn add_tier(
        &mut self,
        tier: FontResolveTier,
        status: FontTierStatus,
        path: Option<String>,
        index: Option<u32>,
        reason: Option<String>,
    ) {
        self.tiers.push(FontTierDiagnostic {
            tier,
            status,
            path,
            index,
            reason,
        });
    }

    fn mark_resolved(&mut self, path: String, index: u32) {
        self.result = FontResolutionResult::Resolved;
        self.path = Some(path);
        self.index = Some(index);
        self.error = None;
    }

    fn mark_error(&mut self, error: String) {
        self.result = FontResolutionResult::Error;
        self.error = Some(error);
    }

    fn mark_effective_embedded_font_name(&mut self, effective_name: &str) {
        if self.embedded_font_name != effective_name {
            self.requested_embedded_font_name = Some(self.embedded_font_name.clone());
            self.embedded_font_name = effective_name.to_string();
        }
    }
}

struct FontLookupOutcome {
    found: Option<(String, u32)>,
    error: Option<String>,
    diagnostic: FontDiagnostic,
}

struct ResolveEmbedFontsOutcome {
    resolved: Vec<ResolvedEmbedFont>,
    warnings: Vec<String>,
    diagnostics: Vec<FontDiagnostic>,
}

#[derive(Debug)]
struct ResolveEmbedFontsError {
    error: String,
    diagnostics: Vec<FontDiagnostic>,
}

type EmbedFileOutcome = (FileReport, Vec<FontDiagnostic>);

struct ResolvedEmbedFont {
    label: String,
    font_name: String,
    path: String,
    index: u32,
    bold: bool,
    italic: bool,
    codepoints: Vec<u32>,
}

struct ShiftProcessContext<'a> {
    offset_ms: i64,
    threshold_ms: Option<i64>,
    timing_map_rules: Option<Vec<engine::TimingMapRule>>,
    output_dir: Option<&'a Path>,
}

struct TempFontDbDir(PathBuf);

impl Drop for TempFontDbDir {
    fn drop(&mut self) {
        app_lib::fonts::clear_user_font_db_path();
        // Pattern-over-enumeration : a future SQLite
        // version adding a new sidecar suffix would leak files past
        // the suffix list. The dir is owned exclusively by this temp
        // (CLI_FONT_DB_DIR_PREFIX + random suffix), so `remove_dir_all`
        // is strictly more correct than the per-suffix enumeration and
        // cleans up any unexpected sidecar in one go.
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
            diagnostics: None,
            aborted_by_fail_fast: false,
        }
    }

    fn push(&mut self, result: FileReport) {
        match result.status {
            FileStatus::Written => self.written += 1,
            FileStatus::Planned => self.planned += 1,
            FileStatus::Diagnosed => self.planned += 1,
            FileStatus::Skipped => self.skipped += 1,
            FileStatus::Failed => self.failed += 1,
        }
        self.results.push(result);
    }

    fn mark_fail_fast_abort(&mut self) {
        self.aborted_by_fail_fast = true;
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

/// Emit the visible stderr notice that `--fail-fast` short-circuited
/// the batch. Suppressed under `--quiet` (by convention; all
/// non-error chatter is suppressed) and under `--json` (the
/// `abortedByFailFast` field in the JSON report carries the signal
/// programmatically — printing a text notice on top would be noise
/// to script consumers). `remaining` is the count of inputs that
/// were declared on argv but never processed.
fn emit_fail_fast_abort_notice(globals: &GlobalOptions, remaining: usize) {
    if globals.quiet || globals.json || remaining == 0 {
        return;
    }
    eprintln!(
        "{}",
        localize(
            globals,
            format!(
                "⚠ --fail-fast: aborting batch after the first failure; {remaining} remaining input(s) untouched."
            ),
            format!(
                "⚠ --fail-fast：首个失败触发批量终止，剩余 {remaining} 个输入未处理。"
            ),
        )
    );
}

fn main() -> ExitCode {
    init_logger();
    match run() {
        Ok(code) => code,
        Err(err) => {
            // sanitize the bubbled-up Err string at
            // the print boundary. Inner returns interpolate raw
            // operational paths (e.g., `cache_path.display()` in
            // run_refresh_fonts's schema-mismatch return) into the Err
            // body; a crafted --cache-file carrying BiDi / control
            // chars would otherwise corrupt the terminal here. All other
            // print sites that interpolate paths sanitize at their own
            // site too; this is the catch-all for everything that
            // propagates as Err through `run()`.
            let err_disp = sanitize_for_display(&err);
            eprintln!("ssahdrify-cli: {err_disp}");
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
        Command::Hdr(args) => run_hdr(&globals, args.args, args.diagnose.mode()),
        Command::Shift(args) => run_shift(&globals, args.args, args.diagnose.mode()),
        Command::Embed(args) => run_embed(&globals, args.args, args.diagnose.mode()),
        Command::Rename(args) => run_rename(&globals, args.args, args.diagnose.mode()),
        Command::DiagnoseFonts(args) => run_diagnose_fonts(&globals, args),
        Command::Chain(args) => run_chain(&globals, args),
        Command::RefreshFonts(args) => run_refresh_fonts(&globals, args),
    }
}

fn validate_cache_file_arg(globals: &GlobalOptions) -> Result<(), String> {
    if let Some(ref cache_file) = globals.cache_file {
        let cache_str = cache_file.to_str().ok_or_else(|| {
            "--cache-file: path contains non-UTF-8 bytes; refuse upfront so the IPC \
             validator and the subsequent SQLite open agree on the same byte sequence"
                .to_string()
        })?;
        app_lib::util::validate_ipc_path(cache_str, "--cache-file")?;
    }
    Ok(())
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

    // --output-dir / --overwrite / --fail-fast affect OUTPUT-file writing;
    // refresh-fonts writes only to the cache, so they're inert here. Surface
    // a notice rather than ignoring them silently (no-silent-action). Unlike
    // --no-cache (contradictory → hard error) these are harmless, so a
    // non-fatal stderr line is the right level. --cache-file / --no-cache are
    // the documented handled/bounded-ignore flags and stay excluded.
    if !globals.quiet {
        let mut inert: Vec<&str> = Vec::new();
        if globals.output_dir.is_some() {
            inert.push("--output-dir");
        }
        if globals.overwrite {
            inert.push("--overwrite");
        }
        if globals.fail_fast {
            inert.push("--fail-fast");
        }
        if !inert.is_empty() {
            let flags = inert.join(", ");
            eprintln!(
                "{}",
                localize(
                    globals,
                    format!(
                        "ℹ refresh-fonts writes only to the cache; these flags have no effect here: {flags}"
                    ),
                    format!("ℹ refresh-fonts 仅写入缓存；以下参数在此无效：{flags}"),
                )
            );
        }
    }

    validate_cache_file_arg(globals)?;

    // validate each --font-dir argv early —
    // fail-fast before opening the cache or starting any scan. The
    // downstream scan_directory_collecting now validates too (defense
    // in depth), but the early check produces a cleaner per-arg
    // error message at the right level. `--cache-file` (also argv untrusted-input)
    // is already validated at the top of run() before any subcommand.
    for dir in &args.font_dirs {
        // Refuse non-UTF-8 paths upfront via `to_str()` rather than
        // routing `to_string_lossy()` through the validator. With
        // lossy substitution, WTF-16-surrogate / non-UTF-8 bytes
        // become U+FFFD for the validate call while downstream scan
        // / cache writes consume the ORIGINAL PathBuf — different
        // bytes than what validate_ipc_path checked. Sibling pattern
        // to the `--cache-file` refusal at the top of run().
        let dir_str = dir.to_str().ok_or_else(|| {
            "--font-dir: path contains non-UTF-8 bytes; refuse upfront so the IPC \
             validator and the subsequent scan / cache write agree on the same \
             byte sequence"
                .to_string()
        })?;
        app_lib::util::validate_ipc_path(dir_str, "--font-dir")?;
    }

    // Resolve cache file path: user override (--cache-file) or default
    // Windows path. Both come from globals now (they're global flags).
    let cache_path = match &globals.cache_file {
        Some(p) => p.clone(),
        None => app_lib::font_cache::default_cli_cache_path()?,
    };

    let mut canonical_font_dirs: Vec<(PathBuf, String)> = Vec::with_capacity(args.font_dirs.len());
    let mut final_folder_paths: HashSet<String> = HashSet::new();
    for dir in &args.font_dirs {
        let abs_dir = absolute_path(dir)?;
        let canonical = abs_dir
            .canonicalize()
            .map_err(|e| format!("cannot canonicalize {}: {e}", abs_dir.display()))?;
        let folder_path_str = display_path(&canonical);
        final_folder_paths.insert(folder_path_str.clone());
        canonical_font_dirs.push((canonical, folder_path_str));
    }
    if final_folder_paths.len() > app_lib::font_cache::MAX_CACHED_FOLDERS {
        return Err(format!(
            "refresh-fonts would track {} unique source folders, exceeding the {}-folder cache sanity cap. Reduce --font-dir inputs and rebuild a smaller cache.",
            final_folder_paths.len(),
            app_lib::font_cache::MAX_CACHED_FOLDERS
        ));
    }

    // --dry-run previews planned work without touching persisted state,
    // matching the cheap-first dry-run contract of the other subcommands:
    // gate BEFORE the expensive scan and mutate nothing. The existing cache
    // is read READ-ONLY (no file is created when absent), so the folder-cap
    // preview reflects reality without the open_or_create side effect.
    if globals.dry_run {
        if cache_path.exists() {
            match app_lib::font_cache::FontCache::open_existing_read_only(&cache_path) {
                Ok(existing) => {
                    for folder in existing
                        .list_folders()
                        .map_err(|e| format!("reading existing cache folders: {e}"))?
                    {
                        final_folder_paths.insert(folder.folder_path);
                    }
                }
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
            }
        }
        if final_folder_paths.len() > app_lib::font_cache::MAX_CACHED_FOLDERS {
            return Err(format!(
                "refresh-fonts would track {} total cached folders, exceeding the {}-folder cache sanity cap. Delete the cache file or rebuild it with fewer --font-dir sources.",
                final_folder_paths.len(),
                app_lib::font_cache::MAX_CACHED_FOLDERS
            ));
        }
        if !globals.quiet {
            let n = args.font_dirs.len();
            eprintln!(
                "{}",
                localize(
                    globals,
                    format!(
                        "ℹ Dry run: would scan {n} source root{} and update the cache. No changes written:",
                        s_if(n)
                    ),
                    format!("ℹ 预演：将扫描 {n} 个源根目录并更新缓存。未写入任何更改："),
                )
            );
            for dir in &args.font_dirs {
                let dir_disp = sanitize_for_display(&dir.to_string_lossy());
                eprintln!("    {dir_disp}");
            }
            let cache_disp = sanitize_for_display(&cache_path.display().to_string());
            eprintln!(
                "{}",
                localize(
                    globals,
                    format!("  Cache file: {cache_disp}"),
                    format!("  缓存文件：{cache_disp}"),
                )
            );
        }
        return Ok(ExitCode::SUCCESS);
    }

    if !globals.quiet {
        // refresh-fonts stderr now flows through
        // `localize()` for the en/zh switch, matching every other CLI
        // subcommand. Previously refresh-fonts was English-only — a
        // Chinese-locale user saw localized status lines from
        // hdr / shift / embed / rename / chain but English from this
        // subcommand. Cache file path interpolation stays sanitized via
        // sanitize_for_display per the existing print-boundary contract.
        let n = args.font_dirs.len();
        eprintln!(
            "{}",
            localize(
                globals,
                format!("ℹ Refreshing cache. Scanning {n} source root{}:", s_if(n)),
                format!("ℹ 正在刷新缓存。扫描 {n} 个源根目录："),
            )
        );
        for dir in &args.font_dirs {
            // Sanitize the per-source-root header print (argv is now
            // validate_ipc_path-clean above, but double-sanitize on
            // already-clean strings is a no-op and matches every other
            // refresh-fonts print site).
            let dir_disp = sanitize_for_display(&dir.to_string_lossy());
            eprintln!("    {dir_disp}");
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

    for folder in cache
        .list_folders()
        .map_err(|e| format!("reading existing cache folders: {e}"))?
    {
        final_folder_paths.insert(folder.folder_path);
    }
    if final_folder_paths.len() > app_lib::font_cache::MAX_CACHED_FOLDERS {
        return Err(format!(
            "refresh-fonts would track {} total cached folders, exceeding the {}-folder cache sanity cap. Delete the cache file or rebuild it with fewer --font-dir sources.",
            final_folder_paths.len(),
            app_lib::font_cache::MAX_CACHED_FOLDERS
        ));
    }

    let mut total_fonts: usize = 0;
    let mut total_folders: usize = 0;

    for (canonical, folder_path_str) in &canonical_font_dirs {
        // Stat the folder for mtime — drift detection on next run
        // compares this against live stat. None → skip the folder
        // (matches the GUI `stat_mtime` behavior): a transient stat
        // failure would otherwise write an epoch-zero row that the
        // next drift-detect re-flags as `modified`, prompting an
        // endless refresh loop . Surface to stderr
        // with the user-visible consequence (folder skipped, not "stat
        // failed at line N").
        //
        // Routed through `font_cache::try_modified_at` — the helper is
        // pub fn for exactly this kind of cross-binary reuse, and an
        // inline duplicate would drift over time.
        let folder_mtime = match app_lib::font_cache::try_modified_at(canonical) {
            Some(m) => m,
            None => {
                if !globals.quiet {
                    // Sanitize before stderr interpolation, then localize.
                    let folder_disp = sanitize_for_display(folder_path_str);
                    eprintln!(
                        "{}",
                        localize(
                            globals,
                            format!(
                                "  ⚠ {folder_disp}: skipped — folder mtime unreadable (would cache as epoch-zero and re-trigger refresh next run)"
                            ),
                            format!(
                                "  ⚠ {folder_disp}：已跳过——文件夹 mtime 不可读（否则会以 epoch-zero 缓存并导致下次再触发刷新）"
                            ),
                        )
                    );
                }
                continue;
            }
        };

        // Scan the folder. Non-recursive — matches `embed`'s
        // --font-dir semantics exactly. Per-source error (e.g. cache-
        // populate cap exceeded for malicious / oversized packs) is
        // logged and skipped so one bad source doesn't abort the whole
        // refresh run — refresh-fonts is multi-dir by design.
        let entries = match app_lib::fonts::scan_directory_collecting(canonical) {
            Ok(e) => e,
            Err(err) => {
                if !globals.quiet {
                    // Refresh-fonts print sites also need sanitize at
                    // the boundary. `folder_path_str` is display_path
                    // output (operational); print sites wrap with
                    // sanitize_for_display, then localize.
                    let folder_disp = sanitize_for_display(folder_path_str);
                    let err_disp = sanitize_for_display(&err);
                    eprintln!(
                        "{}",
                        localize(
                            globals,
                            format!("  ⚠ {folder_disp}: skipped — {err_disp}"),
                            format!("  ⚠ {folder_disp}：已跳过——{err_disp}"),
                        )
                    );
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
            .replace_folder(folder_path_str, folder_mtime, &metadata)
            .map_err(|e| format!("writing cache for {}: {e}", folder_path_str))?;

        if !globals.quiet {
            // sanitize_for_display on the success line too — same
            // callsite shape as the error path above, then localize.
            let folder_disp = sanitize_for_display(folder_path_str);
            eprintln!(
                "{}",
                localize(
                    globals,
                    format!(
                        "  ✓ {folder_disp}: indexed {font_count} font face{}",
                        s_if(font_count)
                    ),
                    format!("  ✓ {folder_disp}：已索引 {font_count} 个字体面"),
                )
            );
        }
        total_fonts += font_count;
        total_folders += 1;
    }

    if total_folders == 0 {
        let source_count = args.font_dirs.len();
        return Err(format!(
            "refresh-fonts could not index any source folders; all {source_count} --font-dir argument{} were skipped. Pass at least one readable font directory.",
            s_if(source_count),
        ));
    }

    if !globals.quiet {
        // localize.
        eprintln!(
            "{}",
            localize(
                globals,
                format!(
                    "✓ Cache updated: {total_fonts} font face{} indexed across {total_folders} folder{}.",
                    s_if(total_fonts),
                    s_if(total_folders)
                ),
                format!(
                    "✓ 缓存已更新：在 {total_folders} 个文件夹中索引了 {total_fonts} 个字体面。"
                ),
            )
        );
        // cache_path.display() is also sanitized — `cache_path` comes
        // from globals.cache_file (argv untrusted-input) or default_cli_cache_path
        // (env-var-resolved).
        let cache_disp = sanitize_for_display(&cache_path.display().to_string());
        eprintln!(
            "{}",
            localize(
                globals,
                format!("  Cache file: {cache_disp}"),
                format!("  缓存文件：{cache_disp}"),
            )
        );
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
    //
    // Route through `emit_chain_warnings` for parity with per-input
    // chain warnings (`⚠` glyph + localize English/Chinese). Bypassing
    // the helper here would leave plan-level warnings in raw English
    // while the surrounding status / file lines stay localized.
    // `collect_suspicious_orderings`'s constructed strings drop their
    // `warning: ` prefix in tandem so the helper's localized prefix
    // doesn't double up.
    if !globals.quiet {
        emit_chain_warnings(globals, &plan.warnings);
    }

    // β behavior for default-stacked output: stderr info line +
    // dry-run hint, NO interactive prompt (preserves the no-prompt
    // principle in `命令设计 § Cross-cutting 行为`). Users wanting
    // safety run with --dry-run; users wanting a different name
    // pass --output-template. --quiet suppresses (consistent with
    // every other informational stderr line in the CLI).
    if !user_supplied_template && !globals.quiet {
        let tmpl = &plan.output_template;
        eprintln!(
            "{}",
            localize(
                globals,
                format!("ℹ Output template defaulted to '{tmpl}' (stacked from chain steps)."),
                format!("ℹ 输出模板使用默认 '{tmpl}'（按 chain 步骤栈叠生成）。"),
            )
        );
        eprintln!(
            "{}",
            localize(
                globals,
                "  Pass --output-template <T> to override, or --dry-run to preview.".to_string(),
                "  传 --output-template <T> 自定义，或 --dry-run 预览。".to_string(),
            )
        );
    }

    let embed_step_index = find_embed_step_index(&plan);
    // Inform user when --no-cache is meaningless in chain — chain v1
    // doesn't consult the persistent cache regardless of step
    // composition (`resolve_chain_embed_subsets` always passes None per
    // the locked design; non-embed chains never reach a cache-aware
    // path either). Without this, --no-cache looks silently ignored.
    // Mirror prepare_embed_cache's posture: stderr informational line,
    // gated on --quiet. Hoisted ABOVE the dry-run early-return so
    // `--dry-run --no-cache` users also see the diagnostic.
    //
    // dropped the `embed_step_index.is_some()`
    // gate. Chain v1 never consults the cache at ALL — gating only on
    // embed presence meant `chain hdr ... --no-cache cat.ass` silently
    // ignored the flag, violating no-silent-action. Surfacing the info
    // line whenever --no-cache is set in chain keeps the signal
    // consistent across step compositions.
    if globals.no_cache && !globals.quiet {
        eprintln!(
            "{}",
            localize(
                globals,
                "ℹ --no-cache has no effect in chain mode; chain v1 doesn't use the persistent cache.".to_string(),
                "ℹ chain 模式下 --no-cache 不生效；chain v1 不读取持久缓存。".to_string(),
            )
        );
    }
    if globals.cache_file.is_some() && !globals.quiet {
        eprintln!(
            "{}",
            localize(
                globals,
                "ℹ --cache-file has no effect in chain mode; chain v1 doesn't use the persistent cache.".to_string(),
                "ℹ chain 模式下 --cache-file 不生效；chain v1 不读取持久缓存。".to_string(),
            )
        );
    }

    // chain v1 doesn't implement the --json output shape
    // that hdr / shift / embed / rename support; reporting loop below
    // emits the plain-text ✓ / ⊘ / ✗ summary regardless of globals.json.
    // Surface this explicitly per the no-silent-action principle —
    // a user piping `chain --json | jq` would otherwise see plain
    // text and silently get parse errors. Same shape as the
    // --no-cache informational line above.
    if globals.json && !globals.quiet {
        eprintln!(
            "{}",
            localize(
                globals,
                "ℹ chain v1 does not implement --json output; reporting in plain text.".to_string(),
                "ℹ chain v1 不支持 --json 输出，按纯文本报告。".to_string(),
            )
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
    // First-input-wins dedup, mirroring run_hdr / run_shift / run_embed
    // . Without this, `chain hdr ... cat.ass cat.ass`
    // runs the chain twice and `--overwrite=true` silently clobbers
    // the first output. Chain prediction is best-effort (None on
    // path shapes V8 will reject), but the common case of repeated
    // input → repeated predicted output catches the user-error class
    // the other subcommands surface as "duplicate output path in
    // planned batch".
    //
    // set size is transitively bounded by OS argv (~32 KB
    // on Windows, ~2 MB on Linux). One entry per `plan.input_files`
    // element, each a path string from clap argv parsing — same
    // transitive bound that run_hdr / run_shift / run_embed rely on,
    // so no chain-local cap is added.
    let mut seen_outputs: HashSet<String> = HashSet::new();
    // Named `chain_aborted` instead of the CommandReport field name
    // `aborted_by_fail_fast` so the chain-local flag and the per-
    // CommandReport struct field don't collide on grep.
    // Chain has no CommandReport (its summary path is bespoke); the two
    // never appear in the same scope but reusing the identifier made
    // "where does this flag flow" harder to answer at a glance.
    let mut chain_aborted = false;
    for (idx, input) in plan.input_files.iter().enumerate() {
        let mut failed_this_input = false;
        match process_one_chain_input(
            &mut engine,
            &plan,
            embed_step_index,
            input,
            globals,
            &mut seen_outputs,
        ) {
            ChainFileOutcome::Written(out, warnings) => {
                if !globals.quiet {
                    // `input` / `out` are raw PathBufs from clap
                    // argv + Rust shell output resolution. Sanitize
                    // before terminal interpolation.
                    let input_disp = sanitize_for_display(&input.to_string_lossy());
                    let out_disp = sanitize_for_display(&out.to_string_lossy());
                    println!("✓ {input_disp} → {out_disp}");
                    emit_chain_warnings(globals, &warnings);
                }
                written += 1;
            }
            ChainFileOutcome::Skipped(reason, warnings) => {
                if !globals.quiet {
                    let input_disp = sanitize_for_display(&input.to_string_lossy());
                    let reason_disp = sanitize_for_display(&reason);
                    println!("⊘ {input_disp}: {reason_disp}");
                    emit_chain_warnings(globals, &warnings);
                }
                skipped += 1;
            }
            ChainFileOutcome::Failed(err, warnings) => {
                let input_disp = sanitize_for_display(&input.to_string_lossy());
                let err_disp = sanitize_for_display(&err);
                eprintln!("✗ {input_disp}: {err_disp}");
                // Error line surfaces unconditionally (matches
                // emit_file_report); warnings respect --quiet because
                // they're informational and the user explicitly silenced
                // output (errors are the only exception to that rule).
                if !globals.quiet {
                    emit_chain_warnings(globals, &warnings);
                }
                failed += 1;
                failed_this_input = true;
            }
        }
        if globals.fail_fast && failed_this_input {
            let remaining = plan.input_files.len().saturating_sub(idx + 1);
            emit_fail_fast_abort_notice(globals, remaining);
            chain_aborted = true;
            break;
        }
    }
    if !globals.quiet {
        // chain Summary line + fail-fast suffix go through `localize`,
        // for sibling-parity with the refresh-fonts sweep and the
        // standalone subcommands' `emit_report_summary`.
        let summary = if chain_aborted {
            localize(
                globals,
                format!(
                    "Summary: {written} written, {skipped} skipped, {failed} failed (aborted by --fail-fast)"
                ),
                format!(
                    "汇总：{written} 个已写入，{skipped} 个已跳过，{failed} 个失败（已被 --fail-fast 中止）"
                ),
            )
        } else {
            localize(
                globals,
                format!("Summary: {written} written, {skipped} skipped, {failed} failed"),
                format!("汇总：{written} 个已写入，{skipped} 个已跳过，{failed} 个失败"),
            )
        };
        println!("{summary}");
    }
    // partial-failure (1) vs complete-failure
    // (2) exit-code split, matching `CommandReport::exit_code`'s
    // semantics for HDR / Shift / Embed / Rename per the design doc
    // § Cross-cutting 行为 § Exit codes. A flat exit code 1 for any
    // failure would prevent CI / pipeline scripts from distinguishing
    // "everything failed" (likely config / argv mistake) from "some
    // files failed" (likely per-file content issue).
    let exit_code = if failed == 0 {
        ExitCode::SUCCESS
    } else if (written + skipped) > 0 {
        ExitCode::from(1)
    } else {
        ExitCode::from(2)
    };
    Ok(exit_code)
}

enum ChainFileOutcome {
    /// Written(output_path, warnings) — warnings are non-fatal
    /// diagnostics propagated from the embed pre-resolution path
    /// (missing fonts, subset failures) so chain output matches
    /// standalone embed's `FileReport.warnings` semantics.
    Written(PathBuf, Vec<String>),
    /// Skipped / Failed also carry warnings so post-V8 early-return
    /// paths don't silently lose embed pre-resolution + oversized-
    /// skipped diagnostics. Without the warnings field, a chain whose
    /// V8 step skipped oversized captions AND then took the post-V8
    /// `output_path.exists()` Skipped branch (or any post-V8 Failed
    /// branch) would emit only the status line, the `⚠ ...` warnings
    /// dropped. Empty vec at sites that fire before warnings accumulate
    /// is intentional — uniform call shape, costless move.
    Skipped(String, Vec<String>),
    Failed(String, Vec<String>),
}

/// single funnel for `ChainFileOutcome` construction within
/// `process_one_chain_input`. Owns the `warnings: Vec<String>` vec
/// accumulated across the 11 early-return sites; each `into_*` method
/// consumes the builder by move, so the compiler enforces single-use
/// and a future early-return site cannot accidentally substitute
/// `Vec::new()` in the warnings slot. Eviction of stale
/// `seen_outputs` keys stays at the call site — this builder owns
/// warnings, not the dedup-set membership.
struct ChainOutcomeBuilder {
    warnings: Vec<String>,
}

impl ChainOutcomeBuilder {
    fn new() -> Self {
        Self {
            warnings: Vec::new(),
        }
    }

    fn replace_warnings(&mut self, new: Vec<String>) {
        self.warnings = new;
    }

    fn extend_warnings<I: IntoIterator<Item = String>>(&mut self, iter: I) {
        self.warnings.extend(iter);
    }

    fn into_failed(self, err: String) -> ChainFileOutcome {
        ChainFileOutcome::Failed(err, self.warnings)
    }

    fn into_skipped(self, reason: String) -> ChainFileOutcome {
        ChainFileOutcome::Skipped(reason, self.warnings)
    }

    fn into_written(self, output_path: PathBuf) -> ChainFileOutcome {
        ChainFileOutcome::Written(output_path, self.warnings)
    }
}

fn find_embed_step_index(plan: &chain::ChainPlan) -> Option<usize> {
    plan.steps
        .iter()
        .position(|s| matches!(s, chain::ParsedStep::Embed(_)))
}

/// chain warning emission consolidated through
/// one helper that mirrors `emit_file_report`'s warnings format
/// (`warning: <msg>` / `警告：<msg>` localized) while keeping the
/// chain-style `⚠` glyph prefix that distinguishes warnings from the
/// `✓` / `⊘` / `✗` status lines. Without this helper, the three
/// Written / Skipped / Failed arms would each emit `  ⚠ {warning}`
/// directly, bypassing localization — a Chinese-locale user would see
/// the status text in Chinese but the warning text untranslated. The
/// helper also keeps the sanitize_for_display call in one place.
fn emit_chain_warnings(globals: &GlobalOptions, warnings: &[String]) {
    for warning in warnings {
        let w_disp = sanitize_for_display(warning);
        eprintln!(
            "  ⚠ {}",
            localize(
                globals,
                format!("warning: {w_disp}"),
                format!("警告：{w_disp}"),
            )
        );
    }
}

/// Port of TS `substituteTemplate` (`src/lib/path-validation.ts`).
/// Segment-based: tokens substitute literally, `..` runs INSIDE
/// template literals collapse to `.`, and at literal/value boundaries
/// at most one dot is dropped — so user-content `..` in stems
/// (`Show..special`) survives intact. A blanket `replace("..", ".")`
/// post-pass would mangle such filenames, diverging from the TS
/// resolver and causing the cheap-first existence check to short-
/// circuit to "Skipped" against a path V8 would actually produce
/// differently.
///
/// Token shape `[a-z_][a-z0-9_]{0,31}` mirrors the TS regex
/// (32-char identifier cap). Returns `None` when a token's name is not
/// present in `vars`. Current chain parsing rejects unsupported
/// chain-level template tokens before V8 starts; this helper stays
/// fail-closed in case a future caller bypasses that validator or adds
/// a token without updating this prediction layer.
///
/// Case asymmetry note : this lexer is lowercase-only
/// (matches the TS substituteTemplate lexer). The TS chain validator
/// at `chain-runtime.ts::resolveChainOutputPath` is case-insensitive
/// (`[a-zA-Z_]...`) by design — it widens to catch capitalized typos
/// like `{Eotf}` / `{NAME}` at the chain-level error path with a
/// clean message. Uppercase fall-through from this lexer (`{NAME}`
/// stays as literal `{NAME}` text) is then caught downstream by
/// predict_chain_output_path's per-char brace reject (and on the TS
/// side by `assertSafeOutputFilename`'s default-strict brace gate).
/// Both layers fail loud; widening this lexer to mixed-case would
/// duplicate the chain validator's check without changing the user-
/// visible outcome.
///
/// Non-recursive by design : substitution scans the
/// TEMPLATE for `{...}` placeholders, not the substituted VALUES, so
/// a malicious filename like `{name}.ass` substituted into a `{name}`
/// template lands as the literal string `{name}.ass`, not as an
/// infinite-expansion or second-pass substitution. Keep it that way —
/// a recursive form would expose template-injection via filenames.
fn substitute_template(template: &str, vars: &[(&str, &str)]) -> Option<String> {
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
                // 32-char identifier cap (j - name_start <= 32). A
                // longer run of lowercase/digit/underscore chars is
                // NOT a token — the loop stops at the cap and the
                // outer `bytes[j] == b'}'` check fails (since bytes[j]
                // is still a token char, not `}`), so the segment is
                // left as literal text. Same behavior as the TS
                // lexer's `{0,31}` quantifier.
                while j < bytes.len()
                    && (j - name_start) < 32
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
                    // Unknown token → return None. The caller falls
                    // back to V8 + TS for the authoritative error;
                    // see fn-level doc comment.
                    let value = vars.iter().find(|(k, _)| *k == name).map(|(_, v)| *v)?;
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
    Some(out)
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
    // Unsupported tokens are rejected at chain-parse time before this
    // function runs. If vars ever drift from that validator, returning
    // None here keeps the cheap-first predictor fail-closed instead of
    // silently producing a different output path.
    let output_name = substitute_template(output_template, &[("name", stem), ("ext", &ext)])?;
    // Reject shapes TS-side `assertSafeOutputFilename` would reject:
    // path separators (chain output is a single filename in input's
    // dir, never a relative or absolute path), drive-letter prefixes,
    // empty after substitution, OR a Windows reserved device name
    // (CON, PRN, AUX, NUL, COM[0-9], LPT[0-9]) — Win32 treats these
    // as device paths regardless of extension, and a template like
    // `CON.{ext}` would predict a path that creates a console handle
    // not a file.
    //
    // Microsoft's reserved-name docs spec COM1-COM9 / LPT1-LPT9 only,
    // but we deliberately over-reject COM0 + LPT0 too :
    // some Win32 reparse layers treat the 0 variants as device aliases
    // depending on driver state, and the cost of one extra rejection
    // (template authors don't use device names by accident) is much
    // smaller than the cost of a silent device-handle-write surprise.
    // The TS-side `assertSafeOutputFilename` mirrors this widened set.
    //
    // Any of these means "Rust prediction and TS resolution will
    // diverge" → defer to V8 + TS for the precise
    // rejection error.
    //
    // Reserved-name coverage scope: the matches! arm covers CON, PRN,
    // AUX, NUL, CONIN$, CONOUT$, ASCII digit variants COM0-COM9 /
    // LPT0-LPT9, AND Unicode superscript variants COM¹/²/³ + LPT¹/²/³
    // — parity with TS `assertSafeOutputFilename` +
    // `util.rs::validate_ipc_path`. The
    // remaining asymmetry vs TS is the trailing-whitespace / dot strip
    // before the reserved-name check (`CON ` and `CON.` resolve to the
    // device on Windows). The Rust pre-check intentionally omits the
    // strip — Windows refuses to create files with those names, so
    // the predicted path can never exist on disk →
    // `predicted.exists()` returns false → prediction returns Some →
    // V8 runs → TS rejects authoritatively. The harmless-slip set is
    // closed-form because the Win32 device-namespace gate at the OS
    // layer is the final arbiter.
    //
    // Cross-platform asymmetry note : on Linux/macOS
    // the TS-side reserved-name check still rejects `COM¹.ass` etc.,
    // even though those names are perfectly valid on POSIX. This is
    // a Windows-first project; the asymmetry doesn't bite today.
    // Revisit if a future build targets POSIX as a first-class
    // platform (gate `WINDOWS_RESERVED_NAMES` in TS on
    // `isWindowsRuntime`).
    // rejection set tightened to match
    // TS-side `assertSafeOutputFilename`. Previously Rust rejected
    // /\\\0 + starts_with('.') + drive-letter prefix only; TS rejected
    // a superset including NTFS punctuation (<>:"|?*{}) + control chars
    // + BiDi/zero-width. The asymmetry meant Rust's prediction said
    // "OK, this path will round-trip" for filenames that TS would then
    // reject inside V8 — letting V8 work on doomed paths wastes work
    // AND surfaces a worse error message (the V8-side rejection text
    // arrives buried in a chain step error rather than at the
    // predictor's clearer "this template will not work" gate).
    //
    // Dropped `starts_with('.')` reject — TS doesn't reject leading
    // dots (".hidden.ass" is a legitimate POSIX dotfile shape).
    //
    // Per-char loop covers control chars + NTFS punctuation + BiDi/zw
    // in one pass; mirrors `util::validate_ipc_path` but for the
    // narrower "single filename, no separators" shape.
    // Drive-letter check via `chars().nth(1)` — `as_bytes()[1]` would
    // read into the middle of a multi-byte UTF-8 sequence if a future
    // template / substitution shape lands a non-ASCII first char at
    // byte 0. Safe today (the lexer's accepted token set is ASCII)
    // but the byte-indexed form is brittle to the next input-shape
    // change; sibling code elsewhere in this file uses `chars().nth`.
    if output_name.is_empty() || output_name.chars().nth(1) == Some(':') {
        return None;
    }
    // whitespace-only output_name is rejected for
    // parity with TS `assertSafeOutputFilename`'s `!filename.trim()`
    // gate (path-validation.ts). Without this check the predictor
    // would accept `"   "` and `"\t\t"` while V8/TS refuses them inside
    // the chain step, surfacing as a buried chain-step error rather
    // than the clearer predictor-layer rejection. Same shape as the
    // `.` / `..` reject below.
    if output_name.trim().is_empty() {
        return None;
    }
    // `.` and `..` as the WHOLE output_name resolve
    // to the input's parent dir itself, which always exists. Without
    // this reject, the cheap-first short-circuit below would either
    // dedup-block every file in the batch (seen_outputs collision on
    // the same predicted key) or emit `Skipped: '…/..' already exists`
    // for each input, never reaching V8 / TS for the proper rejection
    // message. TS-side `assertSafeOutputFilename` rejects them via the
    // empty-stem and traversal gates; mirror at the prediction layer.
    if output_name == "." || output_name == ".." {
        return None;
    }
    let illegal_in_filename = output_name.chars().any(|c| {
        // NTFS-illegal punctuation + path separators + null.
        matches!(
            c,
            '<' | '>' | ':' | '"' | '|' | '?' | '*' | '{' | '}' | '/' | '\\' | '\0'
        )
        // Control characters (C0 + DEL + C1) — `is_control()` covers
        // both Cc ranges. Aligns with assertSafeOutputFilename's
        // ILLEGAL_FILENAME_CHARS regex.
        || c.is_control()
        // BiDi format chars + zero-width — same codepoint set as
        // validate_ipc_path's unicode-controls gate. A U+202E-bearing
        // filename would otherwise predict OK here and trip on TS-side
        // hasUnicodeControls inside V8.
        || matches!(
            c,
            '\u{200E}' | '\u{200F}'
            | '\u{202A}'..='\u{202E}'
            | '\u{2066}'..='\u{2069}'
            | '\u{200B}'..='\u{200D}'
            | '\u{2060}'
            | '\u{180E}'
            | '\u{FEFF}'
            | '\u{2028}' | '\u{2029}'
            | '\u{061C}'
        )
    });
    if illegal_in_filename {
        return None;
    }
    let stem_upper = output_name
        .split('.')
        .next()
        .unwrap_or("")
        .to_ascii_uppercase();
    // Unicode superscript COM/LPT variants
    // (COM¹/²/³, LPT¹/²/³). TS-side WINDOWS_RESERVED_NAMES has had
    // these since extraction; multi-byte UTF-8 superscripts don't
    // satisfy `is_ascii_digit()` so the ASCII-digit branch below
    // misses them. Parity with the TS check + util.rs's
    // validate_ipc_path.
    let is_reserved = matches!(
        stem_upper.as_str(),
        "CON"
            | "PRN"
            | "AUX"
            | "NUL"
            | "CONIN$"
            | "CONOUT$"
            | "COM\u{00B9}"
            | "COM\u{00B2}"
            | "COM\u{00B3}"
            | "LPT\u{00B9}"
            | "LPT\u{00B2}"
            | "LPT\u{00B3}"
    ) || (stem_upper.len() == 4
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
    seen_outputs: &mut HashSet<String>,
) -> ChainFileOutcome {
    // Outcome funnel owns the warnings vec; each early-return consumes
    // the builder via `.into_failed(…)` /
    // `.into_skipped(…)` / `.into_written(…)`. The compiler enforces
    // single-use (any second consumption is move-of-moved-value), so a
    // future return site can't accidentally pass `Vec::new()` in the
    // warnings slot. Eviction of stale `seen_outputs` entries stays at
    // the call site (separate concern, see `evict_predicted` below).
    let mut builder = ChainOutcomeBuilder::new();

    let input_abs = match absolute_path(input) {
        Ok(p) => p,
        Err(err) => return builder.into_failed(err),
    };
    let input_str = display_path(&input_abs);

    // Cheap-first checks via the predicted output path. Two early
    // returns share the prediction:
    //   1. Duplicate-output-in-batch — if a prior input in this run
    //      produced the same predicted output, fail before any I/O
    //      or V8 work. Mirrors `dedup_and_exists_check` in the
    //      per-feature dispatchers.
    //   2. Already-exists — if --overwrite is off and the predicted
    //      path exists, skip before V8.
    // Prediction is best-effort (None on path shapes V8 will reject);
    // when None, dedup falls back to OS-level create_new(true) at
    // write time which fails with AlreadyExists. Less friendly
    // message but no data loss.
    //
    // also remember the predicted key so the post-V8
    // path can reconcile against the ACTUAL output path. The Rust
    // predictor only models `{name}` / `{ext}`; if a future template
    // introduces a token the Rust side doesn't know about, prediction
    // produces a different path than V8's resolver. Predict-time dedup
    // would then miss "two inputs whose predictions differ but whose
    // V8-resolutions coincide" — the second write would only fail at
    // `create_new(true)` time, with a less friendly error.
    let predicted_key: Option<String> = if let Some(predicted) = predict_chain_output_path(
        &input_abs,
        &plan.output_template,
        globals.output_dir.as_deref(),
    ) {
        let key = normalize_output_key(&predicted);
        if !seen_outputs.insert(key.clone()) {
            // `predicted.display()` is interpolated
            // raw here; sanitization happens at the chain print boundary
            // in `run_chain`'s match arm via `sanitize_for_display(&err)`.
            // Sanitize-at-print is the design — sanitizing
            // here would corrupt operational consumers if a future
            // change consumed this Err string programmatically.
            return builder.into_failed(format!(
                "{} duplicate output path in planned batch",
                predicted.display()
            ));
        }
        // Route the cheap-first existence check through
        // `output_path_exists` (`fs::metadata` + stat-fail-treated-as-
        // exists fail-safe + `--quiet`-respecting stderr WARN) instead
        // of raw `Path::exists()`. Otherwise a restrictive-ACL /
        // network-share stat failure would silently resolve as
        // "doesn't exist" and proceed to V8, eventually failing at
        // `create_new(true)` with a generic AlreadyExists. The helper
        // matches every other CLI subcommand's skip-check and
        // preserves the stat-fail fail-safe.
        if !globals.overwrite && output_path_exists(globals, &predicted) {
            // (sibling): evict the predicted_key
            // from `seen_outputs` before returning Skipped. The file
            // at `predicted` exists from a prior session — no write
            // happens this run. A later input whose prediction
            // resolves to the same path should reach this exists
            // check and also Skip; keeping the key would surface as
            // a misleading "duplicate output path" Failed instead.
            // Sibling of the Failed-path eviction and the post-V8
            // Skipped path further below.
            seen_outputs.remove(&key);
            return builder.into_skipped(format!(
                "{} already exists (use --overwrite to replace)",
                predicted.display()
            ));
        }
        Some(key)
    } else {
        None
    };

    // Failed early-return paths between this
    // point and the post-V8 dedup reconcile (below) must REMOVE the
    // already-inserted predicted_key from seen_outputs. Otherwise the
    // stale key lingers — a later input whose predicted_key
    // legitimately equals this one would falsely collide and surface
    // as `duplicate output path` despite no file having been written.
    // Helper closure (captures `&mut seen_outputs`
    // via the function-local `seen_outputs` borrow) keeps the 5
    // failure sites tidy. Successful path (post-V8 reconcile +
    // write) keeps the key inserted, since a real output file
    // landed under that name.
    let evict_predicted = |seen: &mut std::collections::HashSet<String>| {
        if let Some(ref k) = predicted_key {
            seen.remove(k);
        }
    };

    // Read input via existing encoding-aware path. Honors the same
    // size cap, BOM detection, and fallback-on-canonicalize-failure
    // semantics every other CLI subcommand uses.
    let read_result = match app_lib::encoding::read_text_detect_encoding_inner(&input_str, |_| true)
    {
        Ok(r) => r,
        Err(err) => {
            evict_predicted(seen_outputs);
            return builder.into_failed(err);
        }
    };

    // Build the JSON payload matching the TS-side ChainRunRequest.
    // `to_runtime_payload` is now infallible —
    // Shift step argument strings are validated upstream in
    // `chain::parse_chain_argv`.
    let mut payload = plan.to_runtime_payload(&input_str, &read_result.text);

    // Pre-resolve fonts for the embed step (if present) and inject
    // the subset bytes into its params. Done per-file because
    // planFontEmbed needs the file's content; the user-font DB
    // session itself is shared across files (set up once before the
    // loop in run_chain).
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
            // partial_warnings carries
            // missing_warnings collected before the inner Err — surface
            // them through ChainFileOutcome so chain mode matches
            // standalone embed's diagnostic surface even on the failure
            // path. `warnings` at this point is empty (not yet
            // overwritten by embed_warnings), so the partial vec is
            // the whole story.
            Err((err, partial_warnings)) => {
                evict_predicted(seen_outputs);
                // Replace into builder so the failed-shape funnel still
                // owns the partial warnings collected pre-Err.
                builder.replace_warnings(partial_warnings);
                return builder.into_failed(err);
            }
        };
        builder.replace_warnings(embed_warnings);
        // Cumulative cap on the aggregate raw font-subset bytes
        // BEFORE the base64 +
        // serde_json marshal below. Per-font cap (MAX_FONT_DATA_SIZE,
        // 64 MB) holds; the gap was the cumulative case — N×64 MB
        // raw bytes ride through `format!()` / serde_json on Rust
        // heap before V8 sees the payload. Sum up to the
        // MAX_CHAIN_SUBSET_TOTAL_BYTES ceiling and Fail with a
        // focused message if exceeded.
        let total_subset_bytes: usize = subsets.iter().map(|s| s.data.len()).sum();
        if total_subset_bytes > MAX_CHAIN_SUBSET_TOTAL_BYTES {
            evict_predicted(seen_outputs);
            return builder.into_failed(format!(
                "chain embed subsets total {total_subset_bytes} bytes exceeds the \
                 {MAX_CHAIN_SUBSET_TOTAL_BYTES}-byte cap; reduce per-input font count \
                 or split the subtitle before embedding"
            ));
        }
        // Encode subset bytes as base64 strings. The previous form
        // (`{ "data": [byte, byte, ...] }`) expanded ~4-5× per byte
        // when serde_json wrote bytes as decimal+comma JSON-in-JS-source,
        // which compounded against the per-font MAX_FONT_DATA_SIZE
        // budget (64 MB, defined in fonts.rs) into heavy V8 heap
        // pressure on the worst-case path. Base64 is ~1.33× and
        // decoded in TS via the local base64 byte decoder because bare
        // deno_core has no Web API globals like atob().
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
        Err(err) => {
            evict_predicted(seen_outputs);
            return builder.into_failed(err);
        }
    };

    // Surface chain's aggregated skipped-caption count through the
    // same path the standalone HDR / Shift CLIs use — stderr "⚠ ..."
    // line + append to FileReport.warnings (for --json output). An
    // older shape rode along inside an opaque chain note string that
    // printed to stdout under --verbose only, missing both the
    // stderr-routing and the json wire. Embed pre-resolution warnings
    // (collected above) sit in the same vec; both get surfaced via
    // the Written outcome.
    if let Some(msgs) = format_oversized_skipped_warning(globals, result.skipped_count, &input_str)
    {
        builder.extend_warnings(msgs);
    }

    // Apply --output-dir relocation (chain-global, terminal step
    // only) using the existing helper. The runtime returned the
    // path resolved against the input's directory; relocation
    // re-roots that into --output-dir if set.
    let output_path = match relocate_output_path(&result.output_path, globals.output_dir.as_deref())
    {
        Ok(p) => p,
        Err(err) => {
            evict_predicted(seen_outputs);
            return builder.into_failed(err);
        }
    };

    // Post-V8 dedup reconcile. If the actual output path differs from
    // the prediction, REMOVE the stale predicted_key from seen_outputs
    // before inserting the actual one. Without the removal, the stale
    // key would linger — harmless when the same template ran on every
    // input (next input's predicted_key matches and naturally re-
    // collides), but pathological when one input's predictor ran but a
    // later input collided with that stale key while its OWN
    // predicted_key was None or different. Removing the stale entry
    // makes seen_outputs always reflect the actual set of files this
    // run will write.
    //
    // When predicted_key was Some AND matches the actual output_key,
    // skip the re-insert (already inserted upstream; would self-
    // collide). When predicted_key was None we DO insert — the
    // predictor abstained (template too dynamic), so this is the
    // first insertion of the actual key.
    let output_key = normalize_output_key(&output_path);
    if predicted_key.as_deref() != Some(output_key.as_str()) {
        if let Some(stale) = predicted_key.as_deref() {
            seen_outputs.remove(stale);
        }
        // clone so the write-failure cleanup
        // below can also reference output_key for eviction.
        if !seen_outputs.insert(output_key.clone()) {
            return builder.into_failed(format!(
                "{} duplicate output path in planned batch (predictor / V8 resolver disagreed; reconciled post-V8)",
                output_path.display()
            ));
        }
    }

    // Skip-or-overwrite check matching existing per-feature behavior.
    //
    // this is the POST-V8 existence check,
    // distinct from the cheap-first pre-V8 Skipped at lines 1285+.
    // The cheap-first check fires when the Rust predictor's output
    // path already exists (common case, no V8 work wasted). This
    // post-V8 check fires when V8's TS substituteTemplate resolves
    // to a different path than the Rust predictor produced — e.g.,
    // a template with a token the Rust port doesn't model — AND that
    // path also exists. Rare, but a fixture exercises a related
    // post-V8 path (write_output Failed) to pin the warnings-on-non-
    // Written contract; this Skipped branch shares the same warnings-
    // attach semantics.
    //
    // Two coordinated behaviors here:
    //   - Route the existence check through `output_path_exists`:
    //     preserves the stat-fail-treated-as-exists fail-safe +
    //     `--quiet`-respecting WARN that the cheap-first check already
    //     uses. A raw `Path::exists()` would silently treat
    //     restrictive-ACL stat failure as "doesn't exist" and let
    //     write_output below trip its own check with a less specific
    //     error.
    //   - Evict the post-reconcile `output_key` from `seen_outputs`
    //     before returning Skipped — same eviction the write-failure
    //     cleanup path does. A later input whose V8-resolved
    //     output_key legitimately equals this one would otherwise
    //     falsely collide and surface as "duplicate output path".
    if !globals.overwrite && output_path_exists(globals, &output_path) {
        seen_outputs.remove(&output_key);
        return builder.into_skipped(format!(
            "{} already exists (use --overwrite to replace)",
            output_path.display()
        ));
    }

    // Route through the safe writer used by every other CLI subcommand
    // (write_output uses OpenOptions::create_new(true), which refuses to
    // create through a pre-planted symlink/junction at the output path
    // — fs::write would follow it and clobber an attacker-chosen target
    // outside the intended output directory).
    //
    // write-failure cleanup removes `output_key`
    // (the post-reconcile actual key) from `seen_outputs`. Without
    // this a later input whose output legitimately resolves to the
    // same path would falsely collide despite no file having
    // landed.
    if let Err(err) = write_output(globals, &output_path, &result.content, globals.overwrite) {
        seen_outputs.remove(&output_key);
        return builder.into_failed(err);
    }

    if globals.verbose {
        for note in &result.notes {
            // Today's chain-runtime.ts produces notes from static
            // strings + numeric counts (safe). Sanitizing here defends
            // a future note-source addition that might include parsed
            // ASS content from bypassing laundering invisibly. The
            // sibling Written outcome's ✓-line already sanitizes
            // input/out, so this completes the print-site coverage.
            let n_disp = sanitize_for_display(note);
            println!("  {n_disp}");
        }
    }

    builder.into_written(output_path)
}

fn emit_chain_dry_run(plan: &chain::ChainPlan, globals: &GlobalOptions) {
    println!("Plan (no files written):");
    println!();
    // Every emit_chain_dry_run print site sanitizes interpolated
    // strings. `plan.output_template` is user-supplied argv (untrusted-input);
    // `input` is
    // also argv; `out_str` derives from input + template via
    // predict_chain_output_path, so any input control char leaks into
    // the predicted path string. dry-run runs in a context where the
    // user pipes / scripts the output, so terminal corruption is the
    // same risk class as the per-file run-time sites.
    let template_disp = sanitize_for_display(&plan.output_template);
    println!("Output template: {template_disp}");
    println!();
    // track predicted outputs across inputs to surface
    // duplicate-output collisions in dry-run output. The real run
    // catches these via `seen_outputs.insert` returning false (HDR /
    // Shift / Embed in chain). Without this tracking, dry-run would
    // silently print both rows pointing at the same output, hiding the
    // future failure from the user.
    //
    // Fidelity caveat: chain parsing currently limits templates to
    // {name} and {ext}, and this predictor mirrors those tokens byte
    // for byte. If a future template feature adds a token, that work
    // must update both the parser and this predictor; otherwise dry-run
    // would omit the `→ outpath` line and miss duplicate-output
    // detection for that input.
    let mut seen_outputs: HashSet<String> = HashSet::new();
    for input in &plan.input_files {
        let input_disp = sanitize_for_display(&input.display().to_string());
        println!("  {input_disp}");
        // Show the resolved output path for parity with per-feature
        // dry-run output, so users can verify the template + output_dir
        // combination produces what they expect before they remove
        // --dry-run.
        //
        // Make `absolute_path` failure explicit rather than collapsing
        // into `None` via `.ok().and_then(...)`. Otherwise an
        // unresolvable input (e.g., current-dir lookup fails under
        // restrictive ACLs) would silently drop the `→ <out>` line
        // with no diagnostic — the user couldn't tell whether
        // prediction abstained (template uses tokens the Rust port
        // doesn't model) or whether `absolute_path` outright failed.
        // Per no-silent-action, surface the failure with a stderr
        // line; per-feature real-run paths surface the same failure
        // via Failed outcome.
        match absolute_path(input) {
            Ok(abs) => {
                let resolved_path = predict_chain_output_path(
                    &abs,
                    &plan.output_template,
                    globals.output_dir.as_deref(),
                );
                if let Some(out_path) = resolved_path.as_ref() {
                    let out_str = sanitize_for_display(&out_path.display().to_string());
                    // Same dedup key as real-run seen_outputs (case-
                    // folded / separator-normalized via
                    // normalize_output_key) so the dry-run preview
                    // matches what the real run will skip.
                    let key = normalize_output_key(out_path);
                    if !seen_outputs.insert(key) {
                        println!("    → {out_str}  ⚠ duplicate output (real run will fail)");
                    } else {
                        println!("    → {out_str}");
                    }
                }
            }
            Err(err) => {
                let err_disp = sanitize_for_display(&err);
                println!("    → (output unresolved: {err_disp})");
            }
        }
        for (i, step) in plan.steps.iter().enumerate() {
            println!("    {}. {}", i + 1, step.kind_name());
        }
    }
}

fn run_hdr(
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

    let warnings = format_oversized_skipped_warning(globals, conversion.skipped_count, &input);

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

    FileReport {
        input,
        output: Some(output),
        encoding: Some(read_result.encoding),
        status: FileStatus::Written,
        error: None,
        warnings,
    }
}

/// stderr-surface the count of skipped oversized captions
/// (>64 KB text) so CLI / chain users get the same signal the GUI shows
/// via msg_oversized_skipped. Returns the warning string for inclusion
/// in `FileReport.warnings` (used by --json output) so machine readers
/// see it too. English-only per the existing convention for
/// unconditional warnings (verbose-gated paths use `emit_verbose` /
/// `localize` for bilingual output).
/// Pure format helper: builds the oversized-caption warning message
/// and returns it; does NOT `eprintln!`. Callers attach the returned
/// `Vec<String>` to `FileReport.warnings`; the actual stderr emission
/// happens at the existing print loops — `emit_file_report` for
/// standalone HDR / Shift / Embed, and the `ChainFileOutcome::Written`
/// arm for chain. A combined eprintln+return helper would cause double
/// emission AND bypass `--quiet` at the helper's eprintln (the print
/// loops are `!globals.quiet`-gated; a helper wouldn't be). The
/// `format_*` name reinforces the contract.
fn format_oversized_skipped_warning(
    globals: &GlobalOptions,
    skipped_count: usize,
    input: &str,
) -> Option<Vec<String>> {
    if skipped_count == 0 {
        return None;
    }
    // `input` is a raw operational path; sanitize before embedding
    // in the message body so downstream stderr / println emission
    // can't be corrupted by control / BiDi chars from a crafted
    // argv.
    let input_disp = sanitize_for_display(input);
    Some(vec![localize(
        globals,
        format!(
            "Dropped {skipped_count} oversized caption(s) from {input_disp}: \
             text exceeded 64 KB per-caption cap"
        ),
        format!("已丢弃 {skipped_count} 条超大字幕（来自 {input_disp}）：单条文本超过 64 KB 上限"),
    )])
}

fn load_timing_map_rules(
    engine: &mut engine::CliEngine,
    map_path: &Path,
) -> Result<Vec<engine::TimingMapRule>, String> {
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
    let content = fs::read_to_string(&absolute)
        .map_err(|err| format!("failed to read timing map as UTF-8 text from {display}: {err}"))?;
    let parsed = engine.parse_timing_map(&engine::TimingMapParseRequest { content })?;
    if parsed.rules.is_empty() {
        return Err("timing map contains no rules".to_string());
    }
    if !parsed.rules.iter().any(|rule| rule.enabled.unwrap_or(true)) {
        return Err("timing map contains no enabled rules".to_string());
    }
    Ok(parsed.rules)
}

fn run_shift(
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
        timing_map_rules: context.timing_map_rules.clone(),
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

    let warnings = format_oversized_skipped_warning(globals, conversion.skipped_count, &input);

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

    FileReport {
        input,
        output: Some(output),
        encoding: Some(read_result.encoding),
        status: FileStatus::Written,
        error: None,
        warnings,
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
    let warnings = format_oversized_skipped_warning(globals, conversion.skipped_count, &input);

    let output_path = match relocate_output_path(&conversion.output_path, context.output_dir) {
        Ok(path) => path,
        Err(error) => {
            let mut report = failed_report(&input_path, None, Some(read_result.encoding), error);
            report.warnings = warnings;
            return report;
        }
    };
    let output = display_path(&output_path);

    if let Some(mut early) = dedup_and_exists_check(
        globals,
        &input_path,
        &output_path,
        &output,
        Some(&read_result.encoding),
        seen_outputs,
    ) {
        early.warnings = warnings.clone();
        return early;
    }

    // Dry-run gates BEFORE the verbose progress print: a
    // `--dry-run --verbose` invocation should NOT emit the "shift: N
    // captions, M shifted" line because no shift was actually
    // committed. Matches the cheap-first path's ordering.
    if globals.dry_run {
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

    FileReport {
        input,
        output: Some(output),
        encoding: Some(read_result.encoding),
        status: FileStatus::Written,
        error: None,
        warnings,
    }
}

fn run_embed(
    globals: &GlobalOptions,
    args: EmbedArgs,
    diagnose: Option<DiagnoseMode>,
) -> Result<ExitCode, String> {
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
    let prepared_cache = prepare_font_cache_for_resolution(globals, &args, true, false)?;
    let cache = prepared_cache.cache.as_ref();

    let mut engine = engine::CliEngine::new()?;
    let output_dir = globals
        .output_dir
        .as_deref()
        .map(absolute_path)
        .transpose()?;
    let mut report = CommandReport::new("embed");
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

fn prepare_font_cache_for_resolution(
    globals: &GlobalOptions,
    args: &EmbedArgs,
    skip_for_dry_run: bool,
    read_only: bool,
) -> Result<PreparedFontCache, String> {
    if globals.no_cache {
        if !globals.quiet {
            // Route through localize() like every sibling cache line — a bare
            // eprintln! rendered English under --lang zh while the rest of the
            // cache output was Chinese.
            eprintln!(
                "{}",
                localize(
                    globals,
                    "ℹ Cache disabled (--no-cache). Using --font-dir / system fonts only."
                        .to_string(),
                    "ℹ 已禁用缓存（--no-cache）。仅使用 --font-dir / 系统字体。".to_string(),
                )
            );
        }
        return Ok(PreparedFontCache {
            cache: None,
            diagnostic: CacheDiagnostic::new(
                globals.cache_file.as_ref().map(|path| display_path(path)),
                CacheDiagnosticStatus::Disabled,
                Some("disabled by --no-cache".to_string()),
            ),
        });
    }

    if skip_for_dry_run && globals.dry_run {
        return Ok(PreparedFontCache {
            cache: None,
            diagnostic: CacheDiagnostic::new(
                globals.cache_file.as_ref().map(|path| display_path(path)),
                CacheDiagnosticStatus::DryRun,
                Some("skipped because --dry-run does not resolve fonts".to_string()),
            ),
        });
    }

    validate_cache_file_arg(globals)?;
    Ok(prepare_embed_cache(globals, args, read_only))
}

fn run_diagnose_fonts(
    globals: &GlobalOptions,
    args: DiagnoseFontsArgs,
) -> Result<ExitCode, String> {
    validate_diagnose_fonts_globals(globals)?;
    app_lib::fonts::init_system_dirs();
    let embed_args = args.to_embed_args();
    let use_user_fonts = !embed_args.font_dirs.is_empty() || !embed_args.font_files.is_empty();
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

struct DiagnoseFontContext<'a> {
    globals: &'a GlobalOptions,
    args: &'a EmbedArgs,
    use_user_fonts: bool,
    cache: Option<&'a app_lib::font_cache::FontCache>,
    engine: &'a mut engine::CliEngine,
    subset_check: bool,
    subset_budget: &'a mut DiagnosticSubsetBudget,
    subset_budget_warning_emitted: &'a mut bool,
}

fn validate_diagnose_fonts_globals(globals: &GlobalOptions) -> Result<(), String> {
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

fn diagnose_font_file(ctx: &mut DiagnoseFontContext<'_>, file: &Path) -> EmbedFileOutcome {
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

    let plan_request = engine::FontDiagnosticsPlanRequest {
        content: read_result.text,
    };
    let plan = match ctx.engine.plan_font_diagnostics(&plan_request) {
        Ok(result) => result,
        Err(error) => {
            return (
                failed_report(&input_path, None, Some(read_result.encoding), error),
                Vec::new(),
            );
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
            let mut warnings = outcome.warnings;
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
        Err(error) => (
            failed_report(&input_path, None, Some(read_result.encoding), error.error),
            error.diagnostics,
        ),
    }
}

fn apply_subset_checks_to_diagnostics(
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

fn run_budgeted_subset_check(
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

fn run_one_subset_check(
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

fn push_subset_budget_warning_once(
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

fn set_subset_check_for_resolved_font(
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

fn format_subset_check_summary(check: &FontSubsetCheckDiagnostic) -> String {
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

fn emit_standalone_font_diagnostics(globals: &GlobalOptions, diagnostics: &CommandDiagnostics) {
    let unresolved = diagnostics
        .fonts
        .iter()
        .filter(|font| font.result != FontResolutionResult::Resolved)
        .count();
    println!(
        "{}",
        localize(
            globals,
            format!(
                "Font diagnostics: {} file(s), {} font reference(s), {} unresolved",
                diagnostics.files.len(),
                diagnostics.fonts.len(),
                unresolved
            ),
            format!(
                "字体诊断：{} 个文件，{} 个字体引用，{} 个未解析",
                diagnostics.files.len(),
                diagnostics.fonts.len(),
                unresolved
            ),
        )
    );

    if let Some(qa) = &diagnostics.qa {
        println!("{}", format_font_qa_summary(globals, qa));
    }

    if let Some(cache) = &diagnostics.cache {
        let path = cache
            .path
            .as_deref()
            .map(sanitize_for_display)
            .unwrap_or_else(|| "<default path unavailable>".to_string());
        println!("cache: {:?} ({path})", cache.status);
        if let Some(message) = &cache.message {
            println!("  {}", sanitize_for_display(message));
        }
    }

    let actions = diagnostic_next_actions(globals, diagnostics, false);
    if !actions.is_empty() {
        println!(
            "{}",
            localize(
                globals,
                "next actions:".to_string(),
                "下一步建议：".to_string()
            )
        );
        for action in actions {
            println!("  - {action}");
        }
    }

    for file in &diagnostics.files {
        println!(
            "file: {} [{:?}]",
            sanitize_for_display(&file.input),
            file.status
        );
        if let Some(error) = &file.error {
            println!("  error: {}", sanitize_for_display(error));
        }
        for warning in &file.warnings {
            println!("  warning: {}", sanitize_for_display(warning));
        }
    }

    for font in &diagnostics.fonts {
        println!(
            "font: {} [{:?}]",
            sanitize_for_display(&font.label),
            font.result
        );
        println!(
            "  embedded label: {}",
            sanitize_for_display(&font.embedded_font_name)
        );
        if let Some(requested) = &font.requested_embedded_font_name {
            println!("  requested label: {}", sanitize_for_display(requested));
        }
        if let Some(check) = &font.subset_check {
            println!("  subset check: {}", format_subset_check_summary(check));
        }
        if let Some(path) = &font.path {
            println!(
                "  resolved: {}#{}",
                sanitize_for_display(path),
                font.index.unwrap_or(0)
            );
        }
        if let Some(error) = &font.error {
            println!("  error: {}", sanitize_for_display(error));
        }
        for tier in &font.tiers {
            let mut line = format!("  {:?}: {:?}", tier.tier, tier.status);
            if let Some(path) = &tier.path {
                line.push_str(&format!(" {}", sanitize_for_display(path)));
                if let Some(index) = tier.index {
                    line.push_str(&format!("#{index}"));
                }
            }
            if let Some(reason) = &tier.reason {
                line.push_str(&format!(" ({})", sanitize_for_display(reason)));
            }
            println!("{line}");
        }
    }
}

/// Resolve cache path, open the cache, detect drift, announce status
/// to stderr per the locked transparency design, and return
/// `Some(cache)` if usable for this run, or `None` to fall back to
/// no-cache mode.
///
/// `read_only` is true for `diagnose-fonts`, which must inspect an
/// existing cache without creating schema or changing SQLite journal
/// mode. `embed` uses the normal opener, matching the historical cache
/// behavior. Cache content writes remain exclusive to `refresh-fonts`.
fn prepare_embed_cache(
    globals: &GlobalOptions,
    args: &EmbedArgs,
    read_only: bool,
) -> PreparedFontCache {
    // Resolve path: --cache-file override or default Windows path.
    let cache_path = match &globals.cache_file {
        Some(p) => p.clone(),
        None => match app_lib::font_cache::default_cli_cache_path() {
            Ok(p) => p,
            Err(e) => {
                if !globals.quiet {
                    // Every prepare_embed_cache eprintln that
                    // interpolates `cache_path.display()` or a drift
                    // folder string (sourced from the SQLite cache,
                    // which is untrusted-input under --cache-file argv override)
                    // sanitizes at the print boundary. The error `e`
                    // from default_cli_cache_path can carry env-var
                    // resolution failure text that includes path
                    // fragments.
                    //
                    // Every prepare_embed_cache stderr line also
                    // routes through `localize` so cache-related
                    // output respects `--lang zh` for parity with the
                    // refresh-fonts and per-file ✓/⊘/✗ paths.
                    let e_disp = sanitize_for_display(&e);
                    eprintln!(
                        "{}",
                        localize(
                            globals,
                            format!("⚠ Cannot resolve cache path: {e_disp}"),
                            format!("⚠ 无法解析字体缓存路径：{e_disp}"),
                        )
                    );
                    eprintln!(
                        "{}",
                        localize(
                            globals,
                            "  Skipping cache for this run.".to_string(),
                            "  本次运行将跳过字体缓存。".to_string(),
                        )
                    );
                }
                return PreparedFontCache {
                    cache: None,
                    diagnostic: CacheDiagnostic::new(
                        None,
                        CacheDiagnosticStatus::PathError,
                        Some(format!("cannot resolve cache path: {e}")),
                    ),
                };
            }
        },
    };
    let cache_path_json = display_path(&cache_path);

    // Pre-compute the sanitized cache-path display once; reused at
    // every stderr site below. cache_path itself stays operational.
    let cache_path_disp = sanitize_for_display(&cache_path.display().to_string());

    if !cache_path.exists() {
        // No cache yet (first-ever invocation, or user wiped it).
        // Per locked design: distinct messaging from drift, same
        // behavior (skip cache + suggest refresh-fonts).
        if !globals.quiet {
            eprintln!(
                "{}",
                localize(
                    globals,
                    format!("ℹ No font cache exists yet at {cache_path_disp}."),
                    format!("ℹ 尚未在 {cache_path_disp} 建立字体缓存。"),
                )
            );
            eprintln!(
                "{}",
                localize(
                    globals,
                    "  Run `ssahdrify-cli refresh-fonts --font-dir <DIR>...` to build one (--font-dir is repeatable).".to_string(),
                    "  运行 `ssahdrify-cli refresh-fonts --font-dir <DIR>...` 建立缓存（--font-dir 可重复）。".to_string(),
                )
            );
        }
        return PreparedFontCache {
            cache: None,
            diagnostic: CacheDiagnostic::new(
                Some(cache_path_json),
                CacheDiagnosticStatus::Missing,
                Some("no font cache exists yet".to_string()),
            ),
        };
    }

    let cache_result = if read_only {
        app_lib::font_cache::FontCache::open_existing_read_only(&cache_path)
    } else {
        app_lib::font_cache::FontCache::open_or_create(&cache_path)
    };
    let cache = match cache_result {
        Ok(c) => c,
        Err(app_lib::font_cache::CacheError::SchemaVersionMismatch { found, expected }) => {
            if !globals.quiet {
                eprintln!(
                    "{}",
                    localize(
                        globals,
                        format!(
                            "⚠ Font cache schema mismatch (found {found}, expected {expected})."
                        ),
                        format!("⚠ 字体缓存 schema 版本不匹配（发现 {found}，期望 {expected}）。"),
                    )
                );
                eprintln!(
                    "{}",
                    localize(
                        globals,
                        "  Cache is from a different release; skipping for this run.".to_string(),
                        "  缓存来自另一发行版本；本次运行将跳过。".to_string(),
                    )
                );
                eprintln!(
                    "{}",
                    localize(
                        globals,
                        format!("  Delete {cache_path_disp} and run `refresh-fonts` to rebuild."),
                        format!("  删除 {cache_path_disp} 后运行 `refresh-fonts` 重新建立缓存。"),
                    )
                );
            }
            let mut diagnostic = CacheDiagnostic::new(
                Some(cache_path_json),
                CacheDiagnosticStatus::SchemaMismatch,
                Some("font cache schema mismatch".to_string()),
            );
            diagnostic.found_schema = Some(i64::from(found));
            diagnostic.expected_schema = Some(i64::from(expected));
            return PreparedFontCache {
                cache: None,
                diagnostic,
            };
        }
        Err(e) => {
            if !globals.quiet {
                let e_disp = sanitize_for_display(&e.to_string());
                eprintln!(
                    "{}",
                    localize(
                        globals,
                        format!("⚠ Cannot open font cache: {e_disp}"),
                        format!("⚠ 无法打开字体缓存：{e_disp}"),
                    )
                );
                eprintln!(
                    "{}",
                    localize(
                        globals,
                        "  Skipping cache for this run.".to_string(),
                        "  本次运行将跳过字体缓存。".to_string(),
                    )
                );
            }
            return PreparedFontCache {
                cache: None,
                diagnostic: CacheDiagnostic::new(
                    Some(cache_path_json),
                    CacheDiagnosticStatus::OpenError,
                    Some(format!("cannot open font cache: {e}")),
                ),
            };
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
                let e_disp = sanitize_for_display(&e);
                eprintln!(
                    "{}",
                    localize(
                        globals,
                        format!("⚠ Cannot validate cache: {e_disp}"),
                        format!("⚠ 无法校验字体缓存：{e_disp}"),
                    )
                );
                eprintln!(
                    "{}",
                    localize(
                        globals,
                        "  Skipping cache for this run.".to_string(),
                        "  本次运行将跳过字体缓存。".to_string(),
                    )
                );
            }
            return PreparedFontCache {
                cache: None,
                diagnostic: CacheDiagnostic::new(
                    Some(cache_path_json),
                    CacheDiagnosticStatus::ValidationError,
                    Some(format!("cannot validate cache: {e}")),
                ),
            };
        }
    };

    if !drift.is_empty() {
        if !globals.quiet {
            let drift_count = drift.modified.len() + drift.removed.len();
            eprintln!(
                "{}",
                localize(
                    globals,
                    format!(
                        "⚠ Cache drift detected — {drift_count} folder(s) changed since last refresh:"
                    ),
                    format!("⚠ 检测到字体缓存漂移 —— 自上次刷新以来 {drift_count} 个目录已变化："),
                )
            );
            for f in &drift.modified {
                // Drift folder strings originate from the SQLite cache
                // (cached_folders.folder_path). Under --cache-file argv
                // override, those rows are untrusted-input (attacker may craft).
                let f_disp = sanitize_for_display(f);
                eprintln!(
                    "{}",
                    localize(
                        globals,
                        format!("    ~ {f_disp}  (modified)"),
                        format!("    ~ {f_disp}  （已修改）"),
                    )
                );
            }
            for f in &drift.removed {
                let f_disp = sanitize_for_display(f);
                eprintln!(
                    "{}",
                    localize(
                        globals,
                        format!("    - {f_disp}  (removed)"),
                        format!("    - {f_disp}  （已删除）"),
                    )
                );
            }
            eprintln!(
                "{}",
                localize(
                    globals,
                    "  Skipping cache for this run; using --font-dir / system fonts only."
                        .to_string(),
                    "  本次运行将跳过缓存，仅使用 --font-dir / 系统字体。".to_string(),
                )
            );
            eprintln!(
                "{}",
                localize(
                    globals,
                    "  Run `refresh-fonts` to update the cache.".to_string(),
                    "  运行 `refresh-fonts` 更新缓存。".to_string(),
                )
            );
        }
        let mut diagnostic = CacheDiagnostic::new(
            Some(cache_path_json),
            CacheDiagnosticStatus::Drift,
            Some("cached font folders changed since last refresh".to_string()),
        );
        diagnostic.modified_folders = drift.modified.clone();
        diagnostic.removed_folders = drift.removed.clone();
        return PreparedFontCache {
            cache: None,
            diagnostic,
        };
    }

    // Cache is valid. Announce per locked transparency design:
    // Situation A (--font-dir provided) → "cache + dirs" merge
    // announcement; Situation B (no --font-dir) → implicit cache
    // use announcement.
    let user_supplied_dirs = !args.font_dirs.is_empty() || !args.font_files.is_empty();
    if !globals.quiet {
        if user_supplied_dirs {
            eprintln!(
                "{}",
                localize(
                    globals,
                    format!(
                        "ℹ Using font cache (at {cache_path_disp}) plus the --font-dir / --font-file paths you supplied."
                    ),
                    format!(
                        "ℹ 正在使用字体缓存（位于 {cache_path_disp}）以及你通过 --font-dir / --font-file 指定的路径。"
                    ),
                )
            );
        } else {
            eprintln!(
                "{}",
                localize(
                    globals,
                    format!("ℹ Using font cache (at {cache_path_disp})."),
                    format!("ℹ 正在使用字体缓存（位于 {cache_path_disp}）。"),
                )
            );
            eprintln!(
                "{}",
                localize(
                    globals,
                    "  Pass --no-cache to use system fonts only.".to_string(),
                    "  传 --no-cache 可改为仅使用系统字体。".to_string(),
                )
            );
        }
    }
    PreparedFontCache {
        cache: Some(cache),
        diagnostic: CacheDiagnostic::new(
            Some(cache_path_json),
            CacheDiagnosticStatus::Usable,
            Some("font cache usable for this run".to_string()),
        ),
    }
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
        // route through the shared `try_modified_at`
        // helper so CLI and GUI drift detection use identical stat
        // semantics. A future fix to the metadata-or-modified
        // failure-mode handling automatically flows to both sides.
        // None → omit from snapshot → `diff_against` reports as
        // `removed` (slight false-positive for permission-denied
        // folders, but the user wants to know either way).
        if let Some(mtime) =
            app_lib::font_cache::try_modified_at(std::path::Path::new(&folder.folder_path))
        {
            snapshot.push((folder.folder_path.clone(), mtime));
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
    // Drop runs (latent bug, no current caller retries, but the
    // helper makes the cleanup explicit).
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
    //
    // sanitize_for_display is required here because this site is a
    // stdout/println interpolation and `display_path` is a pure
    // formatter. A crafted POSIX font-dir name containing ANSI escape
    // sequences or U+202E would otherwise reach the terminal verbatim
    // and corrupt the verbose summary.
    let (path_suffix_en, path_suffix_zh) = match path {
        Some(p) => {
            let display = sanitize_for_display(&display_path(p));
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

// 9 args: globals + args + use_user_fonts + cache + diagnostic flag +
// output_dir + engine + file + seen_outputs. The cache and use_user_fonts
// could be folded into a per-run state struct, but the existing run_embed
// already passes them as parallel locals; bundling here would just shift
// the boilerplate. Allowing this one lint locally.
#[allow(clippy::too_many_arguments)]
fn process_embed_file(
    globals: &GlobalOptions,
    args: &EmbedArgs,
    use_user_fonts: bool,
    cache: Option<&app_lib::font_cache::FontCache>,
    collect_font_diagnostics: bool,
    output_dir: Option<&Path>,
    engine: &mut engine::CliEngine,
    file: &Path,
    seen_outputs: &mut HashSet<String>,
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

    if let Some(early) = dedup_and_exists_check(
        globals,
        &input_path,
        &output_path,
        &output,
        None,
        seen_outputs,
    ) {
        return (early, Vec::new());
    }

    if globals.dry_run {
        // Dry-run for embed reports the planned output path without
        // doing font discovery or content parsing — matches HDR/Shift
        // dry-run behavior and avoids the surprise of "dry-run scanned
        // 17k fonts then planned no actual write."
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

    let plan_request = engine::FontEmbedPlanRequest {
        input_path: input.clone(),
        content: read_result.text.clone(),
        output_template: args.output_template.clone(),
    };
    let plan = match engine.plan_font_embed(&plan_request) {
        Ok(result) => result,
        Err(error) => {
            return (
                failed_report(&input_path, Some(output), Some(read_result.encoding), error),
                Vec::new(),
            );
        }
    };

    let mut warnings: Vec<String> = Vec::new();

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
            return (
                failed_report(
                    &input_path,
                    Some(output),
                    Some(read_result.encoding),
                    error.error,
                ),
                font_diagnostics,
            );
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
type ChainEmbedSubsetsResult =
    Result<(Vec<engine::FontSubsetPayload>, Vec<String>), (String, Vec<String>)>;

fn resolve_chain_embed_subsets(
    engine: &mut engine::CliEngine,
    globals: &GlobalOptions,
    embed_args: &EmbedArgs,
    input_path: &str,
    content: &str,
) -> ChainEmbedSubsetsResult {
    let use_user_fonts = !embed_args.font_dirs.is_empty() || !embed_args.font_files.is_empty();

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
    // --font-dir the embed step itself was given. Cache integration
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
fn resolve_embed_fonts(
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

fn apply_effective_embedded_font_names(
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
struct ResolvedFaceGroup<'a> {
    aliases: Vec<&'a ResolvedEmbedFont>,
    labels: Vec<String>,
}

impl<'a> ResolvedFaceGroup<'a> {
    /// First-seen alias — its `font_name` / `path` / `index` drive
    /// the dedup-happy-path subset call. Group is non-empty by
    /// construction of `group_resolved_fonts_by_face` (a group is
    /// only inserted when there's at least one alias to seed it).
    fn template(&self) -> &'a ResolvedEmbedFont {
        self.aliases[0]
    }

    /// Lazy union of every alias's codepoints. Returns `Err(len)` as
    /// soon as the merged set exceeds `cap`, so the fallback decision
    /// does not allocate/work through the rest of a hostile over-cap
    /// alias union.
    fn merged_codepoints_with_cap(
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
    fn merged_codepoints(&self) -> std::collections::BTreeSet<u32> {
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
fn group_resolved_fonts_by_face(
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
/// on the GUI / Tauri path where `subset_font_b64` wraps it.
///
/// Cross-language drift defense: `dedup_cap_matches_ipc_cap` in
/// `mod tests` pins the equality with
/// `app_lib::fonts::MAX_SUBSET_CODEPOINTS` so a unilateral bump of
/// the subset cap (the only realistic regression shape — TS↔Rust
/// drift surfaces at the user-facing dedup decision) fails the test
/// instead of shipping silently.
const MAX_SUBSET_CODEPOINTS_FOR_DEDUP: usize = 200_000;

fn subset_resolved_fonts(
    globals: &GlobalOptions,
    args: &EmbedArgs,
    fonts: &[ResolvedEmbedFont],
) -> Result<(Vec<engine::FontSubsetPayload>, Vec<String>), String> {
    let face_groups = group_resolved_fonts_by_face(fonts);

    let mut payloads = Vec::new();
    let mut skipped = Vec::new();

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
                        Ok(data) => payloads.push(engine::FontSubsetPayload {
                            font_name: alias.font_name.clone(),
                            data,
                        }),
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
            Ok(data) => payloads.push(engine::FontSubsetPayload {
                font_name: template.font_name.clone(),
                data,
            }),
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

fn resolve_embed_font(
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

    // Lookup tier 1: session DB populated by --font-dir for THIS run
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
            Some("no --font-dir or --font-file source was provided".to_string()),
        );
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

fn has_ass_extension(path: &Path) -> bool {
    path.extension()
        .and_then(|ext| ext.to_str())
        .map(|ext| matches!(ext.to_ascii_lowercase().as_str(), "ass" | "ssa"))
        .unwrap_or(false)
}

fn run_rename(
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

fn expand_rename_inputs(globals: &GlobalOptions, paths: &[PathBuf]) -> Result<Vec<String>, String> {
    let absolute_paths: Result<Vec<String>, String> = paths
        .iter()
        .map(|path| absolute_path(path).map(|path| display_path(&path)))
        .collect();
    let expanded = app_lib::dropzone::expand_dropped_paths(absolute_paths?)?;

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
    // flagged here and refuses to act in process_rename_pair.
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

fn finish_command_report(
    globals: &GlobalOptions,
    report: &mut CommandReport,
    diagnose: Option<DiagnoseMode>,
    fonts: Vec<FontDiagnostic>,
    cache: Option<CacheDiagnostic>,
) -> Result<(), String> {
    if let Some(mode) = diagnose {
        report.diagnostics = Some(build_command_diagnostics(report, mode, fonts, cache));
    }

    emit_report_summary(globals, report)?;

    if !globals.json {
        if let Some(mode) = diagnose {
            if let Some(diagnostics) = &report.diagnostics {
                emit_attached_diagnostics(globals, diagnostics, mode);
            }
        }
    }

    Ok(())
}

fn build_command_diagnostics(
    report: &CommandReport,
    mode: DiagnoseMode,
    fonts: Vec<FontDiagnostic>,
    cache: Option<CacheDiagnostic>,
) -> CommandDiagnostics {
    let files_with_warnings = report
        .results
        .iter()
        .filter(|result| {
            result
                .warnings
                .as_ref()
                .is_some_and(|warnings| !warnings.is_empty())
        })
        .count();
    let warning_count = report
        .results
        .iter()
        .filter_map(|result| result.warnings.as_ref())
        .map(Vec::len)
        .sum();

    let mut notes = Vec::new();
    if report.aborted_by_fail_fast {
        notes.push("--fail-fast aborted the batch after the first failed file".to_string());
    }
    if warning_count > 0 {
        notes.push(
            "one or more files completed with warnings; inspect results[].warnings".to_string(),
        );
    }
    let qa = if report.command == "diagnose-fonts" || !fonts.is_empty() {
        Some(build_font_qa_summary(report, &fonts, warning_count))
    } else {
        None
    };

    CommandDiagnostics {
        mode,
        files_with_warnings,
        warning_count,
        notes,
        files: report
            .results
            .iter()
            .map(|result| FileDiagnostic {
                input: result.input.clone(),
                output: result.output.clone(),
                encoding: result.encoding.clone(),
                status: result.status,
                error: result.error.clone(),
                warnings: result.warnings.clone().unwrap_or_default(),
            })
            .collect(),
        cache,
        qa,
        fonts,
    }
}

fn build_font_qa_summary(
    report: &CommandReport,
    fonts: &[FontDiagnostic],
    warning_count: usize,
) -> FontQaSummary {
    let failed_file_count = report
        .results
        .iter()
        .filter(|result| result.status == FileStatus::Failed)
        .count();
    let mut resolved_count = 0;
    let mut missing_count = 0;
    let mut error_count = 0;
    let mut subset_checked_count = 0;
    let mut subset_ok_count = 0;
    let mut subset_failed_count = 0;
    let mut subset_skipped_count = 0;

    for font in fonts {
        match font.result {
            FontResolutionResult::Resolved => resolved_count += 1,
            FontResolutionResult::Missing => missing_count += 1,
            FontResolutionResult::Error => error_count += 1,
        }
        if let Some(check) = &font.subset_check {
            subset_checked_count += 1;
            match check.status {
                FontSubsetCheckStatus::Ok => subset_ok_count += 1,
                FontSubsetCheckStatus::Failed => subset_failed_count += 1,
                FontSubsetCheckStatus::Skipped => subset_skipped_count += 1,
            }
        }
    }

    let status = if failed_file_count > 0 || error_count > 0 || subset_failed_count > 0 {
        FontQaStatus::Blocked
    } else if missing_count > 0 || warning_count > 0 || subset_skipped_count > 0 {
        FontQaStatus::Incomplete
    } else {
        FontQaStatus::Complete
    };

    FontQaSummary {
        status,
        file_count: report.results.len(),
        failed_file_count,
        font_reference_count: fonts.len(),
        resolved_count,
        missing_count,
        error_count,
        subset_checked_count,
        subset_ok_count,
        subset_failed_count,
        subset_skipped_count,
    }
}

fn warning_counts(report: &CommandReport) -> (usize, usize, usize) {
    let mut files = 0;
    let mut warnings = 0;
    let mut written_files = 0;

    for result in &report.results {
        let count = result.warnings.as_ref().map_or(0, Vec::len);
        if count == 0 {
            continue;
        }
        files += 1;
        warnings += count;
        if result.status == FileStatus::Written {
            written_files += 1;
        }
    }

    (files, warnings, written_files)
}

fn unresolved_font_count(diagnostics: &CommandDiagnostics) -> usize {
    diagnostics
        .fonts
        .iter()
        .filter(|font| font.result != FontResolutionResult::Resolved)
        .count()
}

fn format_font_qa_summary(globals: &GlobalOptions, qa: &FontQaSummary) -> String {
    let status_en = match qa.status {
        FontQaStatus::Complete => "complete",
        FontQaStatus::Incomplete => "incomplete",
        FontQaStatus::Blocked => "blocked",
    };
    let status_zh = match qa.status {
        FontQaStatus::Complete => "完整",
        FontQaStatus::Incomplete => "不完整",
        FontQaStatus::Blocked => "受阻",
    };
    let subset_en = if qa.subset_checked_count > 0 {
        format!(
            ", subset checks: {} ok, {} failed, {} skipped",
            qa.subset_ok_count, qa.subset_failed_count, qa.subset_skipped_count
        )
    } else {
        String::new()
    };
    let subset_zh = if qa.subset_checked_count > 0 {
        format!(
            "，子集化检查：{} 个通过，{} 个失败，{} 个跳过",
            qa.subset_ok_count, qa.subset_failed_count, qa.subset_skipped_count
        )
    } else {
        String::new()
    };
    localize(
        globals,
        format!(
            "Font QA: {status_en} (files: {}, failed: {}, fonts: {}/{}, missing: {}, errors: {}{subset_en})",
            qa.file_count,
            qa.failed_file_count,
            qa.resolved_count,
            qa.font_reference_count,
            qa.missing_count,
            qa.error_count
        ),
        format!(
            "字体 QA：{status_zh}（文件：{}，失败：{}，字体：{}/{}, 缺失：{}，错误：{}{subset_zh}）",
            qa.file_count,
            qa.failed_file_count,
            qa.resolved_count,
            qa.font_reference_count,
            qa.missing_count,
            qa.error_count
        ),
    )
}

fn diagnostic_next_actions(
    globals: &GlobalOptions,
    diagnostics: &CommandDiagnostics,
    include_full_hint: bool,
) -> Vec<String> {
    let unresolved = unresolved_font_count(diagnostics);
    let has_failed_files = diagnostics
        .files
        .iter()
        .any(|file| file.status == FileStatus::Failed);
    let has_unresolved = unresolved > 0;
    let has_font_errors = diagnostics
        .fonts
        .iter()
        .any(|font| font.result == FontResolutionResult::Error);
    let has_local_disabled = diagnostics.fonts.iter().any(|font| {
        font.result != FontResolutionResult::Resolved
            && font.tiers.iter().any(|tier| {
                matches!(tier.tier, FontResolveTier::Local)
                    && matches!(tier.status, FontTierStatus::Disabled)
            })
    });
    let has_system_disabled = diagnostics.fonts.iter().any(|font| {
        font.result != FontResolutionResult::Resolved
            && font.tiers.iter().any(|tier| {
                matches!(tier.tier, FontResolveTier::System)
                    && matches!(tier.status, FontTierStatus::Disabled)
            })
    });
    let has_cache_miss_or_unavailable = diagnostics.fonts.iter().any(|font| {
        font.result != FontResolutionResult::Resolved
            && font.tiers.iter().any(|tier| {
                matches!(tier.tier, FontResolveTier::Cache)
                    && matches!(
                        tier.status,
                        FontTierStatus::Miss | FontTierStatus::Unavailable
                    )
            })
    });

    let mut seen = HashSet::new();
    let mut actions = Vec::new();
    let mut push_action = |code: &'static str, en: &str, zh: &str| {
        if seen.insert(code) {
            actions.push(localize(globals, en.to_string(), zh.to_string()));
        }
    };

    if let Some(cache) = &diagnostics.cache {
        match cache.status {
            CacheDiagnosticStatus::Missing => {
                push_action(
                    "cache-refresh",
                    "Build a font cache with `ssahdrify-cli refresh-fonts --font-dir <DIR>...`.",
                    "用 `ssahdrify-cli refresh-fonts --font-dir <DIR>...` 建立字体缓存。",
                );
            }
            CacheDiagnosticStatus::Drift => {
                push_action(
                    "cache-refresh",
                    "Run `ssahdrify-cli refresh-fonts --font-dir <DIR>...` after changing a cached font folder.",
                    "字体目录变化后，运行 `ssahdrify-cli refresh-fonts --font-dir <DIR>...` 刷新缓存。",
                );
            }
            CacheDiagnosticStatus::SchemaMismatch => {
                push_action(
                    "cache-rebuild",
                    "Rebuild the font cache for this release: delete the old cache file, then run `refresh-fonts` again.",
                    "为当前版本重建字体缓存：删除旧缓存文件，然后重新运行 `refresh-fonts`。",
                );
            }
            CacheDiagnosticStatus::OpenError
            | CacheDiagnosticStatus::ValidationError
            | CacheDiagnosticStatus::PathError => {
                push_action(
                    "cache-check",
                    "Check `--cache-file` and file permissions, or use `--no-cache` for a one-off run.",
                    "检查 `--cache-file` 和文件权限；临时处理可改用 `--no-cache`。",
                );
            }
            CacheDiagnosticStatus::Disabled if has_unresolved => {
                push_action(
                    "cache-disabled",
                    "Remove `--no-cache` if you want the persistent font cache to help resolve missing fonts.",
                    "如果想让持久化字体缓存参与解析缺失字体，请移除 `--no-cache`。",
                );
            }
            CacheDiagnosticStatus::DryRun
            | CacheDiagnosticStatus::Usable
            | CacheDiagnosticStatus::Disabled => {}
        }
    }

    if has_unresolved && (has_local_disabled || has_cache_miss_or_unavailable) {
        push_action(
            "font-source",
            "If you expected these fonts to embed, pass `--font-dir <DIR>` or `--font-file <FILE>` for the font pack.",
            "如果你希望这些字体被嵌入，请为字体包传入 `--font-dir <DIR>` 或 `--font-file <FILE>`。",
        );
    }

    if has_system_disabled {
        push_action(
            "system-disabled",
            "Remove `--no-system-fonts` if installed system fonts are acceptable fallback candidates.",
            "如果可以使用已安装的系统字体作为兜底候选，请移除 `--no-system-fonts`。",
        );
    }

    if has_unresolved {
        push_action(
            "font-name",
            "If the font file is present but still unresolved, check the ASS Style `Fontname` against the font's internal family or full-face name.",
            "如果字体文件存在但仍未解析，请核对 ASS Style 的 `Fontname` 是否匹配字体内部 family 或 full-face 名称。",
        );
    }

    if has_font_errors {
        push_action(
            "font-error",
            "Inspect the per-font tier error details; cache/provenance errors usually need a cache rebuild, while parse/subset errors usually point to a bad font file.",
            "查看逐字体层级错误；缓存/来源错误通常需要重建缓存，解析/子集化错误通常指向损坏或不兼容的字体文件。",
        );
    }

    if include_full_hint && (diagnostics.warning_count > 0 || has_unresolved || has_failed_files) {
        push_action(
            "diagnose-full",
            "Rerun with `--diagnose=full` for per-file and per-font tier details.",
            "重新运行 `--diagnose=full` 查看逐文件和逐字体层级细节。",
        );
    }

    if has_unresolved || (diagnostics.warning_count > 0 && !diagnostics.fonts.is_empty()) {
        push_action(
            "strict-embed",
            "For packaging runs that must embed every font, use `embed --on-missing fail --fail-fast --diagnose=full`.",
            "如果打包流程要求所有字体都必须嵌入，请使用 `embed --on-missing fail --fail-fast --diagnose=full`。",
        );
    }

    actions
}

fn emit_attached_diagnostics(
    globals: &GlobalOptions,
    diagnostics: &CommandDiagnostics,
    mode: DiagnoseMode,
) {
    let unresolved = diagnostics
        .fonts
        .iter()
        .filter(|font| font.result != FontResolutionResult::Resolved)
        .count();
    eprintln!(
        "{}",
        localize(
            globals,
            format!(
                "Diagnostics: {} file(s), {} warning(s), {} unresolved font(s)",
                diagnostics.files.len(),
                diagnostics.warning_count,
                unresolved
            ),
            format!(
                "诊断：{} 个文件，{} 条警告，{} 个未解析字体",
                diagnostics.files.len(),
                diagnostics.warning_count,
                unresolved
            ),
        )
    );

    if let Some(qa) = &diagnostics.qa {
        eprintln!("  {}", format_font_qa_summary(globals, qa));
    }

    if let Some(cache) = &diagnostics.cache {
        let path = cache
            .path
            .as_deref()
            .map(sanitize_for_display)
            .unwrap_or_else(|| "<default path unavailable>".to_string());
        eprintln!(
            "{}",
            localize(
                globals,
                format!("  cache: {:?} ({path})", cache.status),
                format!("  缓存：{:?}（{path}）", cache.status),
            )
        );
    }

    let actions = diagnostic_next_actions(globals, diagnostics, mode == DiagnoseMode::Summary);
    if !actions.is_empty() {
        eprintln!(
            "{}",
            localize(
                globals,
                "  next actions:".to_string(),
                "  下一步建议：".to_string()
            )
        );
        for action in actions {
            eprintln!("    - {action}");
        }
    }

    if mode == DiagnoseMode::Summary {
        return;
    }

    for file in &diagnostics.files {
        let input = sanitize_for_display(&file.input);
        eprintln!("  file: {input} [{:?}]", file.status);
        if let Some(output) = &file.output {
            eprintln!("    output: {}", sanitize_for_display(output));
        }
        if let Some(error) = &file.error {
            eprintln!("    error: {}", sanitize_for_display(error));
        }
        for warning in &file.warnings {
            eprintln!("    warning: {}", sanitize_for_display(warning));
        }
    }

    for font in &diagnostics.fonts {
        let file_suffix = font
            .file
            .as_deref()
            .map(|file| format!(" in {}", sanitize_for_display(file)))
            .unwrap_or_default();
        eprintln!(
            "  font: {}{} [{:?}]",
            sanitize_for_display(&font.label),
            file_suffix,
            font.result
        );
        eprintln!(
            "    embedded label: {}",
            sanitize_for_display(&font.embedded_font_name)
        );
        if let Some(requested) = &font.requested_embedded_font_name {
            eprintln!("    requested label: {}", sanitize_for_display(requested));
        }
        if let Some(check) = &font.subset_check {
            eprintln!("    subset check: {}", format_subset_check_summary(check));
        }
        if let Some(path) = &font.path {
            eprintln!(
                "    resolved: {}#{}",
                sanitize_for_display(path),
                font.index.unwrap_or(0)
            );
        }
        if let Some(error) = &font.error {
            eprintln!("    error: {}", sanitize_for_display(error));
        }
        for tier in &font.tiers {
            let mut line = format!("    {:?}: {:?}", tier.tier, tier.status);
            if let Some(path) = &tier.path {
                line.push_str(&format!(" {}", sanitize_for_display(path)));
                if let Some(index) = tier.index {
                    line.push_str(&format!("#{index}"));
                }
            }
            if let Some(reason) = &tier.reason {
                line.push_str(&format!(" ({})", sanitize_for_display(reason)));
            }
            eprintln!("{line}");
        }
    }
}

fn emit_report_summary(globals: &GlobalOptions, report: &CommandReport) -> Result<(), String> {
    if globals.json {
        let json = serde_json::to_string_pretty(report)
            .map_err(|err| format!("failed to encode JSON report: {err}"))?;
        println!("{json}");
    } else if !globals.quiet {
        // when --fail-fast aborts the batch, append a
        // `(aborted by --fail-fast)` / `（已被 --fail-fast 中止）`
        // suffix to the human-text summary. Without this suffix, the
        // fail-fast signal would live only in the stderr `⚠` notice
        // (emit_fail_fast_abort_notice) and JSON output's
        // `abortedByFailFast` field — users piping stdout to a log
        // would lose the signal. Chain's sibling Summary does this
        // too; this keeps standalone HDR/Shift/Embed/Rename aligned.
        let (_, warning_count, written_warning_files) = warning_counts(report);
        let warning_suffix_en = if written_warning_files > 0 {
            format!(
                ", {written_warning_files} written with warnings / incomplete ({warning_count} warning(s))"
            )
        } else {
            String::new()
        };
        let warning_suffix_zh = if written_warning_files > 0 {
            format!("，{written_warning_files} 个已写入但带警告/不完整（{warning_count} 条警告）")
        } else {
            String::new()
        };
        let fail_fast_suffix_en = if report.aborted_by_fail_fast {
            " (aborted by --fail-fast)"
        } else {
            ""
        };
        let fail_fast_suffix_zh = if report.aborted_by_fail_fast {
            "（已被 --fail-fast 中止）"
        } else {
            ""
        };
        let message = localize(
            globals,
            format!(
                "Done: {} written, {} planned, {} skipped, {} failed{warning_suffix_en}{fail_fast_suffix_en}",
                report.written, report.planned, report.skipped, report.failed
            ),
            format!(
                "完成：{} 个已写入，{} 个计划写入，{} 个已跳过，{} 个失败{warning_suffix_zh}{fail_fast_suffix_zh}",
                report.written, report.planned, report.skipped, report.failed
            ),
        );
        println!("{message}");
    }
    Ok(())
}

fn emit_file_report(globals: &GlobalOptions, result: &FileReport) {
    if globals.json {
        // JSON output: serde_json escapes C0 control characters
        // (U+0000–U+001F) and the double-quote/backslash pair as
        // `\uXXXX`/`\"`/`\\` per RFC 8259 §7. Higher-plane format
        // characters — BiDi controls (U+200E/U+200F, U+202A–U+202E,
        // U+2066–U+2069, U+061C), zero-width (U+200B–U+200D,
        // U+2060, U+180E, U+FEFF), and line separators (U+2028,
        // U+2029) — are NOT escaped by serde_json (they are valid
        // JSON string characters). The wire form preserves operational
        // path fidelity for programmatic consumers (`jq '.results[]
        // | .input'` returns the raw bytes intact, matching how the
        // path was actually invoked). Downstream consumers piping to
        // a terminal via `jq -r` should pre-sanitize before
        // interpolating into stderr/stdout (see README "JSON output"
        // section). The human-format branch below applies
        // sanitize_for_display at print boundaries.
        return;
    }

    // paths interpolated into stderr/println output must
    // pass through `sanitize_for_display` to strip control / BiDi /
    // zero-width chars. The strings themselves (`result.input` /
    // `result.output`) are the raw operational path forms — JSON
    // serialization above handles them correctly via serde escaping,
    // but a raw control char or U+202E reaching `eprintln!` here
    // would corrupt the terminal (ANSI escapes, direction reversal).
    let input_disp = sanitize_for_display(&result.input);

    // Status line first. Failed always surfaces to stderr regardless
    // of --quiet (it's an error, not output); other statuses respect
    // --quiet.
    if matches!(result.status, FileStatus::Failed) {
        if let Some(error) = &result.error {
            // Chain-mode emit_chain_warnings sanitizes error/warning
            // strings; the standalone emit_file_report path does the
            // same here. Failure messages bubble up from
            // `resolve_embed_fonts` / `subset_resolved_fonts` with
            // `font.label` interpolated raw — font.label flows from V8-
            // parsed ASS `\fn` content (attacker-influenced), so a
            // crafted subtitle with U+202E / control / zero-width in
            // \fn reaches stderr unlaundered without this sanitize.
            // Also covers safe_io's reparse-refusal / scope-deny /
            // same-canonical-path Err strings, which interpolate
            // `path.display()` raw (see `app_lib::safe_io`), which bubble
            // through `failed_report` to this site. Sanitize-at-print
            // is the design — sanitizing operationally inside
            // safe_io would corrupt path-resolution for operational
            // consumers.
            let error_disp = sanitize_for_display(error);
            eprintln!(
                "{}",
                localize(
                    globals,
                    format!("failed: {input_disp} ({error_disp})"),
                    format!("失败：{input_disp}（{error_disp}）"),
                )
            );
        }
    } else if !globals.quiet {
        if let Some(output) = &result.output {
            let output_disp = sanitize_for_display(output);
            match result.status {
                FileStatus::Written => {
                    if globals.verbose {
                        let encoding = result.encoding.as_deref().unwrap_or("unknown");
                        println!(
                            "{}",
                            localize(
                                globals,
                                format!("written: {input_disp} -> {output_disp} ({encoding})"),
                                format!("已写入：{input_disp} -> {output_disp}（{encoding}）"),
                            )
                        );
                    } else {
                        println!(
                            "{}",
                            localize(
                                globals,
                                format!("written: {output_disp}"),
                                format!("已写入：{output_disp}"),
                            )
                        );
                    }
                }
                FileStatus::Planned => println!(
                    "{}",
                    localize(
                        globals,
                        format!("would write: {output_disp}"),
                        format!("将写入：{output_disp}"),
                    )
                ),
                FileStatus::Diagnosed => println!(
                    "{}",
                    localize(
                        globals,
                        format!("diagnosed: {input_disp}"),
                        format!("已诊断：{input_disp}"),
                    )
                ),
                FileStatus::Skipped => println!(
                    "{}",
                    localize(
                        globals,
                        format!("skipped: {output_disp}"),
                        format!("已跳过：{output_disp}"),
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
                // Standalone-path warning content needs the same
                // print-boundary sanitize as chain-mode
                // emit_chain_warnings. `font.label` flows from V8-
                // parsed ASS \fn (untrusted-input) and reaches here via
                // resolve_embed_fonts missing_warnings +
                // subset_resolved_fonts skipped_warnings.
                let w_disp = sanitize_for_display(warning);
                eprintln!(
                    "  {}",
                    localize(
                        globals,
                        format!("warning: {w_disp}"),
                        format!("警告：{w_disp}"),
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

// Cap on the relocated output path — Windows MAX_PATH minus one.
// Windows-only: POSIX has PATH_MAX 4096 (Linux) / 1024 (macOS), so
// applying 259 there over-restricts legitimate long paths
// . Long-local paths (`\\?\C:\...`) get the extended
// cap on Windows. UNC long paths keep the standard cap because the
// server side may not honor the extended namespace.
#[cfg(target_os = "windows")]
const RELOCATED_PATH_MAX_LEN: usize = 259;
#[cfg(target_os = "windows")]
const RELOCATED_LONG_PATH_MAX_LEN: usize = 32766;
// POSIX: use PATH_MAX 4096 (Linux's value). macOS PATH_MAX is 1024
// but most modern macOS filesystems tolerate paths past that —
// matching Linux's 4096 keeps the cap permissive enough for both.
#[cfg(not(target_os = "windows"))]
const RELOCATED_PATH_MAX_LEN: usize = 4096;
#[cfg(not(target_os = "windows"))]
const RELOCATED_LONG_PATH_MAX_LEN: usize = 4096;

fn relocate_output_path(path: &str, output_dir: Option<&Path>) -> Result<PathBuf, String> {
    let path = PathBuf::from(path);
    let Some(output_dir) = output_dir else {
        return Ok(path);
    };

    // `path.file_name()` strips ALL path components from the
    // engine-returned output path, keeping only the final segment.
    // Contract : the engine's TS-side
    // `assertSafeOutputFilename` guarantees the returned `path` is a
    // flat filename — no path separators, no `..`, no drive letters.
    // If a future engine change relaxes that invariant, the
    // file_name() flattening here would silently mask the violation
    // (we'd just drop the directory prefix instead of erroring). Keep
    // assertSafeOutputFilename strict.
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

// stat-failure-treated-as-exists is the
// fail-safe (never silently overwrite a file we couldn't stat under
// restrictive ACLs / network share metadata-read denied). The CLI
// surfaces a stderr WARN so the user sees the underlying cause
// rather than a "skipped: output exists" misdirection. See design
// doc § Cross-cutting 行为 (overwrite default) for why the fail-safe
// is the design-locked direction.
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
            // --quiet: the user opted into a diagnostics-free run, so
            // suppress this warning to respect the "no stderr noise
            // when --quiet" contract.
            if !globals.quiet {
                // Use `sanitize_for_display` here (covers all C0/C1
                // controls + BiDi format + zero-width) rather than the
                // narrower `strip_visual_line_breaks`
                // (CR/LF/NEL/U+2028/U+2029 only). A Windows filename
                // containing ESC / U+202E would corrupt the warning
                // line even though CR/LF wouldn't have. Sanitize at
                // print boundary (path itself stays operational for
                // the surrounding fs::metadata call).
                let display = sanitize_for_display(&path.display().to_string());
                let err_one_line = sanitize_for_display(&err.to_string());
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

// write_output / copy_file_output / rename_file_output route through
// `app_lib::safe_io::*_inner` with a permissive `|_| true` predicate.
// CLI argv IS the user (local-user authorship); there is no Tauri fs:scope
// to enforce, so the closure short-circuits the scope check while
// every other defense in the safe_io chain still applies:
//   - `validate_ipc_path` rejects extended-length / DOS-device /
//     BiDi / control-char paths at the write boundary even though
//     argv-time validation is partial on the CLI side.
//   - `check_subtitle_extension` confines the destination to the
//     subtitle-extension whitelist (defense-in-depth against argv
//     typo redirecting to `.desktop` / `.lnk` persistence paths).
//   - `clear_existing_destination` lstat's the destination before any
//     remove_file (refuses reparse points there).
//   - copy / rename additionally enforce `reject_reparse_source` +
//     `reject_same_canonical_path` (the case-only NTFS self-overwrite
//     trap closed for the GUI) + the late re-check before
//     `File::open` (copy) / `fs::rename` (rename) for the dst-side
//     reparse swap window.
// Routing through the single source of truth means future findings
// against safe_io auto-propagate here instead of needing parallel
// fixes.
//
// The `_globals` parameter is preserved on each function for caller-
// site convention (every other CLI fs-helper takes globals); safe_io
// owns all decisions internally. Higher-level callers still use
// `output_path_exists` (preserved as a standalone CLI helper above)
// for the cheap-first skip-when-exists check + `--quiet`-respecting
// stderr WARN on stat failure. safe_io's `clear_existing_destination`
// provides the second-layer fail-shut on the race between the higher-
// level check and the write. Error wording at the safe_io boundary
// bubbles through `failed_report` and is sanitized at the print
// boundary by `emit_file_report`'s `sanitize_for_display`.
fn write_output(
    _globals: &GlobalOptions,
    path: &Path,
    content: &str,
    overwrite: bool,
) -> Result<(), String> {
    let path_str = path.to_string_lossy();
    app_lib::safe_io::safe_write_text_file_inner(&path_str, content, overwrite, |_| true)
}

fn copy_file_output(
    _globals: &GlobalOptions,
    input: &Path,
    output: &Path,
    overwrite: bool,
) -> Result<(), String> {
    let src = input.to_string_lossy();
    let dst = output.to_string_lossy();
    app_lib::safe_io::safe_copy_file_inner(&src, &dst, overwrite, |_| true)
}

fn rename_file_output(
    _globals: &GlobalOptions,
    input: &Path,
    output: &Path,
    overwrite: bool,
) -> Result<(), String> {
    let src = input.to_string_lossy();
    let dst = output.to_string_lossy();
    app_lib::safe_io::safe_rename_file_inner(&src, &dst, overwrite, |_| true)
}

/// English pluralization helper. `s_if(n)` returns "" for n == 1, "s"
/// otherwise. Replaces 6+ inline `if n == 1 { "" } else { "s" }`
/// repeats across stderr formatting . Only handles
/// the simple s-suffix case; irregular plurals stay inline.
fn s_if(n: usize) -> &'static str {
    if n == 1 {
        ""
    } else {
        "s"
    }
}

fn display_path(path: &Path) -> String {
    // Pure formatter: PathBuf → String + Windows slash normalization.
    // The returned string is canonical for OS-level filesystem
    // operations — `read_text_detect_encoding_inner`, engine path
    // resolution, FileReport.input field. Character filtering MUST
    // NOT happen here: sanitizing destructively at this layer would
    // make `evil\u{202e}.ass` silently resolve to a sibling
    // `evil.ass`, picking the wrong file at read / write time.
    // Display-time sanitization belongs at the print sites
    // (`emit_file_report`, `format_oversized_skipped_warning`, etc.)
    // via `sanitize_for_display`.
    let raw = path.to_string_lossy().into_owned();
    if cfg!(windows) {
        raw.replace('/', "\\")
    } else {
        raw
    }
}

/// Strip control + BiDi + zero-width characters from a path string
/// for safe interpolation into human-readable output (stderr/println).
/// The same filter set as `validate_ipc_path` rejects, but here we
/// strip rather than refuse: argv-supplied filenames may
/// legitimately contain these characters on POSIX (Linux filenames
/// accept any byte except `/` and NUL), so the conservative choice is
/// to launder them at display time rather than fail the whole batch.
///
/// JSON output doesn't need this helper — `serde_json` escapes control
/// characters as `\uXXXX` automatically, so machine-parseable readers
/// see the original bytes intact. Apply this helper ONLY at the
/// `eprintln!` / `println!` formatting sites where a raw control char
/// would corrupt the terminal (ANSI escapes, U+202E direction reversal).
fn sanitize_for_display(s: &str) -> String {
    s.chars()
        .filter(|c| {
            !c.is_control()
                && !matches!(
                    c,
                    '\u{2028}'
                        | '\u{2029}'
                        | '\u{200B}'
                        | '\u{200C}'
                        | '\u{200D}'
                        | '\u{2060}'
                        | '\u{180E}'
                        | '\u{FEFF}'
                        | '\u{200E}'
                        | '\u{200F}'
                        | '\u{202A}'
                        | '\u{202B}'
                        | '\u{202C}'
                        | '\u{202D}'
                        | '\u{202E}'
                        | '\u{2066}'
                        | '\u{2067}'
                        | '\u{2068}'
                        | '\u{2069}'
                        | '\u{061C}'
                )
        })
        .collect()
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
    // Lowercase on case-insensitive filesystems : Windows
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
        apply_effective_embedded_font_names, apply_subset_checks_to_diagnostics,
        build_font_qa_summary, classify_locale, copy_file_output, create_cli_font_db_dir,
        diagnostic_next_actions, display_path, duplicate_rename_output_keys, engine,
        group_resolved_fonts_by_face, normalize_output_key, parse_duration_ms, parse_timestamp_ms,
        predict_chain_output_path, relocate_output_path, resolve_embed_fonts, sanitize_for_display,
        substitute_template, write_output, Cli, Command, CommandDiagnostics, CommandReport,
        DiagnoseMode, DiagnosticSubsetBudget, EmbedArgs, FileDiagnostic, FileReport, FileStatus,
        FontDiagnostic, FontQaStatus, FontSubsetCheckDiagnostic, FontSubsetCheckStatus,
        GlobalOptions, MissingFontAction, OutputLang, ResolvedEmbedFont, TempFontDbDir,
        MAX_DIAGNOSTIC_SUBSET_CALLS, MAX_DIAGNOSTIC_SUBSET_TOTAL_BYTES,
        MAX_RESOLVED_FONT_CODEPOINTS, MAX_SHIFT_OFFSET_MS, MAX_SUBSET_CODEPOINTS_FOR_DEDUP,
    };
    // Import the canonical filename literal directly from app_lib so the
    // test pins the same name `TempFontDbDir::drop`'s remove_dir_all
    // sees on disk. No `as USER_FONT_DB_FILENAME` alias here — the
    // test doesn't pretend to have its own filename literal;
    // `USER_FONT_DB_FILENAME` is the canonical name everywhere, and an
    // alias would only add indirection.
    use app_lib::fonts::USER_FONT_DB_FILENAME;
    use clap::error::ErrorKind;
    use clap::Parser;
    use std::fs;
    use std::path::{Path, PathBuf};

    /// pin the cross-module constant equality. The CLI's
    /// dedup cap MUST equal the IPC cap inside fonts.rs — the dedup
    /// decision (merge union vs per-alias fallback) is bounded by
    /// what subset_font itself accepts. A unilateral bump on either
    /// side would silently break the b8aa3fd fallback contract; this
    /// test fails the build instead. TS sibling
    /// (`MAX_SUBSET_CODEPOINTS_FOR_DEDUP` in font-embedder.ts) carries
    /// a WHY comment naming `app_lib::fonts::MAX_SUBSET_CODEPOINTS` as
    /// the source of truth.
    #[test]
    fn dedup_cap_matches_ipc_cap() {
        assert_eq!(
            MAX_SUBSET_CODEPOINTS_FOR_DEDUP,
            app_lib::fonts::MAX_SUBSET_CODEPOINTS,
            "CLI dedup cap must equal the fonts.rs IPC cap; the dedup \
             fallback path is bounded by what subset_font accepts. \
             If you change one, change both AND update the TS sibling \
             at src/features/font-embed/font-embedder.ts."
        );
    }

    /// Pin the per-alias resolved cap < dedup cap inequality the
    /// fallback path's safety claim depends on. The cap-busting
    /// fallback subsets each alias independently — each alias's
    /// codepoints is bounded by `MAX_RESOLVED_FONT_CODEPOINTS`
    /// upstream in `resolve_embed_fonts`, and that bound MUST be
    /// strictly less than the subset cap or the per-alias call could
    /// itself overflow `subset_font`. Same cross-language drift
    /// defense rationale that motivated `dedup_cap_matches_ipc_cap`:
    /// the relationship is load-bearing, written down in one prose
    /// comment, and there is no compiler check today.
    #[test]
    fn resolved_font_cap_fits_subset_cap() {
        assert!(
            MAX_RESOLVED_FONT_CODEPOINTS < MAX_SUBSET_CODEPOINTS_FOR_DEDUP,
            "MAX_RESOLVED_FONT_CODEPOINTS ({}) must be strictly less \
             than MAX_SUBSET_CODEPOINTS_FOR_DEDUP ({}) — the cap-busting \
             fallback subsets each alias independently and its safety \
             claim depends on per-alias codepoints fitting under the \
             subset cap.",
            MAX_RESOLVED_FONT_CODEPOINTS,
            MAX_SUBSET_CODEPOINTS_FOR_DEDUP,
        );
    }

    #[test]
    fn diagnose_without_value_parses_as_summary() {
        let cli = Cli::try_parse_from([
            "ssahdrify-cli",
            "hdr",
            "--eotf",
            "pq",
            "--diagnose",
            "input.ass",
        ])
        .expect("bare --diagnose should parse");

        match cli.command {
            Command::Hdr(args) => assert_eq!(args.diagnose.mode(), Some(DiagnoseMode::Summary)),
            other => panic!("expected hdr command, got {other:?}"),
        }
    }

    #[test]
    fn diagnose_full_parses() {
        let cli = Cli::try_parse_from([
            "ssahdrify-cli",
            "embed",
            "--diagnose=full",
            "--no-system-fonts",
            "input.ass",
        ])
        .expect("--diagnose=full should parse");

        match cli.command {
            Command::Embed(args) => assert_eq!(args.diagnose.mode(), Some(DiagnoseMode::Full)),
            other => panic!("expected embed command, got {other:?}"),
        }
    }

    #[test]
    fn diagnose_suffix_after_input_parses() {
        let cli = Cli::try_parse_from(["ssahdrify-cli", "embed", "input.ass", "--diagnose=full"])
            .expect("diagnose should parse after command inputs");

        match cli.command {
            Command::Embed(args) => assert_eq!(args.diagnose.mode(), Some(DiagnoseMode::Full)),
            other => panic!("expected embed command, got {other:?}"),
        }
    }

    #[test]
    fn invalid_diagnose_value_fails_at_parse_time() {
        let err = Cli::try_parse_from([
            "ssahdrify-cli",
            "shift",
            "--offset",
            "+1s",
            "--diagnose=short",
            "input.ass",
        ])
        .expect_err("invalid diagnose value should fail");

        assert_eq!(err.kind(), ErrorKind::InvalidValue);
    }

    #[test]
    fn refresh_fonts_rejects_diagnose_option() {
        let err = Cli::try_parse_from([
            "ssahdrify-cli",
            "refresh-fonts",
            "--diagnose",
            "--font-dir",
            "fonts",
        ])
        .expect_err("unsupported command should reject --diagnose");

        assert_eq!(err.kind(), ErrorKind::UnknownArgument);
    }

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
            fail_fast: false,
        }
    }

    #[test]
    fn non_font_warnings_do_not_suggest_strict_embed_policy() {
        let globals = test_globals();
        let diagnostics = CommandDiagnostics {
            mode: DiagnoseMode::Summary,
            files_with_warnings: 1,
            warning_count: 1,
            notes: Vec::new(),
            files: vec![FileDiagnostic {
                input: "input.ass".to_string(),
                output: None,
                encoding: None,
                status: FileStatus::Written,
                error: None,
                warnings: vec!["non-font warning".to_string()],
            }],
            cache: None,
            qa: None,
            fonts: Vec::new(),
        };

        let actions = diagnostic_next_actions(&globals, &diagnostics, true);

        assert!(
            actions
                .iter()
                .any(|action| action.contains("--diagnose=full")),
            "warning diagnostics should still point users to full detail: {actions:?}"
        );
        assert!(
            actions
                .iter()
                .all(|action| !action.contains("embed --on-missing")),
            "non-font warnings should not suggest strict font-embedding policy: {actions:?}"
        );
    }

    fn qa_usage(label: &str) -> engine::FontEmbedUsage {
        engine::FontEmbedUsage {
            family: label.to_string(),
            bold: false,
            italic: false,
            label: label.to_string(),
            font_name: format!("{label}.ttf"),
            glyph_count: 1,
            codepoints: vec![0x41],
        }
    }

    fn qa_report(status: FileStatus) -> CommandReport {
        let mut report = CommandReport::new("diagnose-fonts");
        report.push(FileReport {
            input: "input.ass".to_string(),
            output: None,
            encoding: Some("utf-8".to_string()),
            status,
            error: None,
            warnings: None,
        });
        report
    }

    #[test]
    fn font_qa_summary_marks_complete_when_all_fonts_resolve() {
        let mut resolved = FontDiagnostic::new(&qa_usage("Resolved"));
        resolved.mark_resolved("/fonts/resolved.ttf".to_string(), 0);

        let qa = build_font_qa_summary(&qa_report(FileStatus::Diagnosed), &[resolved], 0);

        assert_eq!(qa.status, FontQaStatus::Complete);
        assert_eq!(qa.resolved_count, 1);
        assert_eq!(qa.missing_count, 0);
    }

    #[test]
    fn font_qa_summary_marks_incomplete_for_missing_fonts() {
        let missing = FontDiagnostic::new(&qa_usage("Missing"));

        let qa = build_font_qa_summary(&qa_report(FileStatus::Diagnosed), &[missing], 0);

        assert_eq!(qa.status, FontQaStatus::Incomplete);
        assert_eq!(qa.missing_count, 1);
    }

    #[test]
    fn font_qa_summary_marks_blocked_for_subset_failures() {
        let mut resolved = FontDiagnostic::new(&qa_usage("BrokenSubset"));
        resolved.mark_resolved("/fonts/broken.ttf".to_string(), 0);
        resolved.subset_check = Some(FontSubsetCheckDiagnostic {
            status: FontSubsetCheckStatus::Failed,
            bytes: None,
            error: Some("subset failed".to_string()),
        });

        let qa = build_font_qa_summary(&qa_report(FileStatus::Diagnosed), &[resolved], 0);

        assert_eq!(qa.status, FontQaStatus::Blocked);
        assert_eq!(qa.subset_failed_count, 1);
    }

    #[test]
    fn shift_request_omits_absent_threshold_from_json_wire_shape() {
        let request = engine::ShiftConversionRequest {
            input_path: "C:\\subs\\episode.ass".to_string(),
            content: "[Script Info]\n".to_string(),
            offset_ms: 1000,
            threshold_ms: None,
            timing_map_rules: None,
            output_template: "{name}.shifted{ext}".to_string(),
        };

        let json = serde_json::to_value(request).expect("request should serialize");
        assert!(
            json.get("thresholdMs").is_none(),
            "None threshold must be omitted, not serialized as thresholdMs:null"
        );
    }

    #[test]
    fn resolve_embed_fonts_drops_diagnostics_when_not_requested() {
        let globals = test_globals();
        let args = EmbedArgs {
            font_dirs: Vec::new(),
            font_files: Vec::new(),
            no_system_fonts: true,
            on_missing: MissingFontAction::Warn,
            output_template: "{name}.embed.ass".to_string(),
            files: Vec::new(),
        };
        let usage = engine::FontEmbedUsage {
            family: "Definitely Missing".to_string(),
            bold: false,
            italic: false,
            label: "Definitely Missing".to_string(),
            font_name: "Definitely Missing".to_string(),
            glyph_count: 1,
            codepoints: vec![0x41],
        };

        let no_diagnostics = resolve_embed_fonts(
            &globals,
            &args,
            false,
            None,
            false,
            std::slice::from_ref(&usage),
        )
        .expect("warn mode should return a skipped-font warning");
        assert!(no_diagnostics.diagnostics.is_empty());
        assert_eq!(
            no_diagnostics.warnings,
            vec!["missing font: Definitely Missing".to_string()]
        );

        let with_diagnostics = resolve_embed_fonts(&globals, &args, false, None, true, &[usage])
            .expect("diagnostic mode should still return warn-mode outcome");
        assert_eq!(with_diagnostics.diagnostics.len(), 1);
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
        // Same-unit repetition: guards against a silent-sum bug
        // where `1s2s` would parse as 3 seconds.
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

    // at-limit / over-limit boundary pair for MAX_SHIFT_OFFSET_MS.
    // When a test name promises a cap, pair the at-limit test (must
    // accept) with an over-limit counter-test (must reject), so a
    // refactor that loosens the cap in either direction surfaces. The
    // existing `parse_duration_ms_caps_extreme_values` test above only
    // exercises far-over-cap inputs; the at-cap boundary was unpinned.
    #[test]
    fn parse_duration_ms_at_max_shift_offset_accepts() {
        // MAX_SHIFT_OFFSET_MS = 365 * 24 * 60 * 60 * 1000 ms (= 1 year
        // in ms). At the boundary, parse_duration_ms must accept.
        // 1 year = 8760 hours exactly.
        let at_cap_str = "+8760h";
        let got = parse_duration_ms(at_cap_str)
            .expect("at-cap +8760h (= MAX_SHIFT_OFFSET_MS) must parse");
        assert_eq!(got, MAX_SHIFT_OFFSET_MS);
    }

    #[test]
    fn parse_duration_ms_one_ms_over_max_shift_offset_rejects() {
        // 1 ms over the cap must reject (counter-test paired with
        // the at-cap acceptance test above). Using the smallest
        // ms-precision step above the cap so the boundary is
        // pinned at exactly ±1 ms.
        let over_cap_str = "+31536000001ms"; // MAX_SHIFT_OFFSET_MS + 1
        assert!(
            parse_duration_ms(over_cap_str).is_err(),
            "one ms over MAX_SHIFT_OFFSET_MS must reject"
        );
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
        // Concrete repro: row 0 is a no-op (subtitle
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
    fn rename_dedup_allows_distinct_language_suffix_outputs() {
        let rows = vec![
            engine::RenamePlanRow {
                input_path: "C:\\Subs\\episode.sc.ass".to_string(),
                output_path: "C:\\Subs\\Episode.sc.ass".to_string(),
                video_path: "C:\\Subs\\Episode.mkv".to_string(),
                no_op: false,
            },
            engine::RenamePlanRow {
                input_path: "C:\\Subs\\episode.tc.ass".to_string(),
                output_path: "C:\\Subs\\Episode.tc.ass".to_string(),
                video_path: "C:\\Subs\\Episode.mkv".to_string(),
                no_op: false,
            },
        ];

        let duplicates = duplicate_rename_output_keys(&rows);
        assert!(
            duplicates.is_empty(),
            "distinct canonical language outputs must not be blocked as duplicates"
        );
    }

    #[test]
    fn rename_dedup_flags_canonical_alias_duplicate_outputs() {
        let rows = vec![
            engine::RenamePlanRow {
                input_path: "C:\\Subs\\episode.sc.ass".to_string(),
                output_path: "C:\\Subs\\Episode.sc.ass".to_string(),
                video_path: "C:\\Subs\\Episode.mkv".to_string(),
                no_op: false,
            },
            engine::RenamePlanRow {
                input_path: "C:\\Subs\\episode.zh-CN.ass".to_string(),
                output_path: "C:\\Subs\\Episode.sc.ass".to_string(),
                video_path: "C:\\Subs\\Episode.mkv".to_string(),
                no_op: false,
            },
        ];

        let duplicates = duplicate_rename_output_keys(&rows);
        let expected_key = if cfg!(windows) || cfg!(target_os = "macos") {
            "c:/subs/episode.sc.ass"
        } else {
            "C:/Subs/Episode.sc.ass"
        };
        assert!(
            duplicates.contains(expected_key),
            "canonical aliases that resolve to the same output must be blocked"
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
    fn relocate_output_path_accepts_ramdisk_shaped_output_dirs() {
        let drive_root = PathBuf::from("R:\\");
        let root_result =
            relocate_output_path("C:\\subs\\episode.embed.ass", Some(&drive_root)).unwrap();
        assert_eq!(root_result, drive_root.join("episode.embed.ass"));

        let spaced_cjk_dir = PathBuf::from("R:\\ass out 中文");
        let spaced_result =
            relocate_output_path("C:\\subs\\episode.embed.ass", Some(&spaced_cjk_dir)).unwrap();
        assert_eq!(spaced_result, spaced_cjk_dir.join("episode.embed.ass"));
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
        // A CJK directory path is 200
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

        fs::write(dir.join(USER_FONT_DB_FILENAME), b"db").unwrap();
        fs::write(dir.join(format!("{USER_FONT_DB_FILENAME}-wal")), b"wal").unwrap();
        app_lib::fonts::init_user_font_db(&dir).unwrap();
        let guard = TempFontDbDir(dir.clone());
        drop(guard);

        assert!(!dir.exists());
        let result = app_lib::fonts::resolve_user_font("Arial".to_string(), false, false);
        let Err(error) = result else {
            panic!("dropped CLI temp DB guard should clear the global DB path");
        };
        assert!(
            error.contains("not initialized"),
            "dropped CLI temp DB guard should fail shut after cleanup: {error}"
        );
    }

    // ── substitute_template — regression coverage ──

    #[test]
    fn substitute_template_preserves_double_dots_inside_user_content() {
        // Old blanket `replace("..", ".")` mangled this; segment-based
        // substitution keeps user-content `..` intact.
        let got = substitute_template(
            "{name}.shifted{ext}",
            &[("name", "Show..special"), ("ext", ".ass")],
        );
        assert_eq!(got.as_deref(), Some("Show..special.shifted.ass"));
    }

    #[test]
    fn substitute_template_collapses_boundary_double_dots() {
        // Template-side dot + ext-leading dot at the seam → drop one.
        let got = substitute_template("{name}.{ext}", &[("name", "Show"), ("ext", ".ass")]);
        assert_eq!(got.as_deref(), Some("Show.ass"));
    }

    #[test]
    fn substitute_template_collapses_template_literal_dot_runs() {
        // `..` inside the user-typed template (typo) collapses, but
        // user-content `..` would not (covered by the preserve test).
        let got = substitute_template("{name}..shifted{ext}", &[("name", "Show"), ("ext", ".ass")]);
        assert_eq!(got.as_deref(), Some("Show.shifted.ass"));
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
        assert_eq!(got.as_deref(), Some("Show$1$&.shifted.ass"));
    }

    #[test]
    fn substitute_template_unknown_token_returns_none() {
        // Unknown token (vars has name + ext, template uses {lang})
        // returns None. Chain parsing now rejects this before runtime,
        // but the helper still fails closed if called directly.
        let got = substitute_template("{name}.{lang}{ext}", &[("name", "Show"), ("ext", ".ass")]);
        assert!(got.is_none());
    }

    #[test]
    fn substitute_template_leaves_unknown_braces_intact() {
        // Token shape doesn't match (uppercase) → kept as literal text.
        // Uppercase `{NAME}` falls through the lexer; downstream the
        // brace gate in `assertSafeOutputFilename` (TS) /
        // predict_chain_output_path's per-char reject (Rust) catches it.
        let got = substitute_template("{NAME}.{ext}", &[("name", "Show"), ("ext", ".ass")]);
        assert_eq!(got.as_deref(), Some("{NAME}.ass"));
    }

    #[test]
    fn substitute_template_token_at_32_char_cap_matches() {
        // Boundary-pin pair (a): 32-char identifier matches the lexer
        // (first char + 31 subsequent chars = 32 total = cap). Vars
        // contain the token → substitutes normally. Pre-fix the lexer
        // was unbounded; this pins the inclusive boundary.
        let long_name = "a".repeat(32);
        let template = format!("{{{long_name}}}.ass");
        let got = substitute_template(&template, &[(long_name.as_str(), "value")]);
        assert_eq!(got.as_deref(), Some("value.ass"));
    }

    #[test]
    fn substitute_template_token_at_32_char_cap_unknown_returns_none() {
        // Boundary-pin pair (b): 32-char identifier matches the lexer
        // and is NOT in vars → unknown-token rejection fires.
        let long_name = "a".repeat(32);
        let template = format!("{{{long_name}}}.ass");
        let got = substitute_template(&template, &[("name", "Show")]);
        assert!(got.is_none());
    }

    #[test]
    fn substitute_template_token_over_cap_falls_through_as_literal() {
        // Boundary-pin pair (c): 33-char identifier exceeds the lexer
        // bound → not matched as a token → stays as literal text.
        // An unbounded lexer would silently collapse this; with the
        // bounded lexer the literal `{aaa...}` survives so the
        // downstream brace-reject path surfaces the failure.
        let long_name = "a".repeat(33);
        let template = format!("{{{long_name}}}.ass");
        let expected = format!("{{{long_name}}}.ass");
        let got = substitute_template(&template, &[("name", "Show")]);
        assert_eq!(got.as_deref(), Some(expected.as_str()));
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

    // Each test below pins one rejection CODEPOINT-CLASS — the input
    // exercises a distinct character / structural class
    // (Windows-reserved, superscript-COM, drive-letter, NTFS-illegal,
    // control, BiDi, zero-width, U+2028, whitespace-only, `..`,
    // empty post-substitution). Several of these classes collapse
    // onto the same internal boolean gate inside `predict_chain_output_path`
    // (e.g. the matches!()-block control + BiDi + NTFS-illegal arms
    // all flip `illegal_in_filename`), so this suite does NOT pin
    // "every distinct gate branch" — that's a different invariant
    // and isn't covered here. The codepoint-class structure IS what
    // changes most often in practice: a refactor that drops a
    // superscript variant or relaxes a BiDi codepoint slips past a
    // single positive-acceptance test, which is exactly what these
    // pins guard against.

    #[test]
    fn predict_chain_output_path_rejects_windows_reserved_device_name() {
        // {name} = "CON" → predicted name "CON.ass" → Win32 device
        // path. Predictor rejects → V8/TS handles authoritative msg.
        let input = PathBuf::from("/subs/CON.ass");
        let predicted = predict_chain_output_path(&input, "{name}{ext}", None);
        assert!(
            predicted.is_none(),
            "CON.ass should be rejected as a Windows reserved device name; got {predicted:?}"
        );
    }

    #[test]
    fn predict_chain_output_path_rejects_superscript_com_variant() {
        // Unicode superscript COM¹ is a Win32 device-alias on some
        // reparse layers — TS-side `assertSafeOutputFilename`
        // rejects it explicitly; Rust predictor is documented to
        // omit this superset and let V8/TS handle. Pin the predictor
        // returning None (so the V8/TS path is reached).
        let input = PathBuf::from("/subs/COM\u{00B9}.ass");
        let predicted = predict_chain_output_path(&input, "{name}{ext}", None);
        assert!(
            predicted.is_none(),
            "COM¹.ass should be rejected; got {predicted:?}"
        );
    }

    #[test]
    fn predict_chain_output_path_rejects_drive_letter_prefix_in_template() {
        // Template-output that starts with a drive-letter prefix
        // (`C:foo.ass`) is rejected at the predictor — chain output
        // is a single filename in input's directory, never a path.
        let input = PathBuf::from("/subs/Show.ass");
        let predicted = predict_chain_output_path(&input, "C:{name}{ext}", None);
        assert!(
            predicted.is_none(),
            "drive-letter prefix in template should reject; got {predicted:?}"
        );
    }

    #[test]
    fn predict_chain_output_path_rejects_ntfs_illegal_punctuation() {
        // `<`, `>`, `:`, `"`, `|`, `?`, `*` are NTFS-illegal. TS
        // `assertSafeOutputFilename` rejects them; predictor mirrors.
        let input = PathBuf::from("/subs/Show?wild.ass");
        let predicted = predict_chain_output_path(&input, "{name}{ext}", None);
        assert!(
            predicted.is_none(),
            "NTFS-illegal `?` should reject; got {predicted:?}"
        );
    }

    #[test]
    fn predict_chain_output_path_rejects_control_char_in_name() {
        // `\u{001B}` ESC is a C0 control. Predictor must reject;
        // V8 stringification round-trip is unsafe.
        let input = PathBuf::from("/subs/Show\u{001B}.ass");
        let predicted = predict_chain_output_path(&input, "{name}{ext}", None);
        assert!(
            predicted.is_none(),
            "ESC control char should reject; got {predicted:?}"
        );
    }

    #[test]
    fn predict_chain_output_path_rejects_bidi_override_in_name() {
        // U+202E RIGHT-TO-LEFT OVERRIDE — well-known display-flip
        // exploit primitive. TS-side rejects via `unicode-controls`;
        // predictor mirrors.
        let input = PathBuf::from("/subs/evil\u{202E}.ass");
        let predicted = predict_chain_output_path(&input, "{name}{ext}", None);
        assert!(
            predicted.is_none(),
            "U+202E should reject; got {predicted:?}"
        );
    }

    #[test]
    fn predict_chain_output_path_rejects_zero_width_in_name() {
        // U+200B ZERO WIDTH SPACE — invisible-character class.
        let input = PathBuf::from("/subs/Show\u{200B}.ass");
        let predicted = predict_chain_output_path(&input, "{name}{ext}", None);
        assert!(
            predicted.is_none(),
            "U+200B should reject; got {predicted:?}"
        );
    }

    #[test]
    fn predict_chain_output_path_rejects_unicode_line_separator() {
        // U+2028 LINE SEPARATOR — would wrap stderr/println output
        // across lines if it reached display.
        let input = PathBuf::from("/subs/Show\u{2028}.ass");
        let predicted = predict_chain_output_path(&input, "{name}{ext}", None);
        assert!(
            predicted.is_none(),
            "U+2028 should reject; got {predicted:?}"
        );
    }

    #[test]
    fn predict_chain_output_path_rejects_whitespace_only_name() {
        // Template `{name}` with a whitespace-only stem produces
        // a whitespace-only output_name. TS rejects via
        // `!filename.trim()`; predictor mirrors. (Template must
        // omit `{ext}` so the trim-empty branch fires — `"   .ass"`
        // would trim to `".ass"` not empty.)
        let input = PathBuf::from("/subs/   .ass");
        let predicted = predict_chain_output_path(&input, "{name}", None);
        assert!(
            predicted.is_none(),
            "whitespace-only name should reject; got {predicted:?}"
        );
    }

    #[test]
    fn predict_chain_output_path_rejects_bare_double_dot() {
        // `..` as the entire output filename is a parent-dir
        // traversal at the path-resolution layer. Predictor rejects
        // via the `.` / `..` gate documented in the doc-comment.
        let input = PathBuf::from("/subs/Show.ass");
        let predicted = predict_chain_output_path(&input, "..", None);
        assert!(
            predicted.is_none(),
            "bare `..` template should reject; got {predicted:?}"
        );
    }

    #[test]
    fn predict_chain_output_path_rejects_empty_output_name_after_substitution() {
        // The `output_name.is_empty()` gate at the head of
        // predict_chain_output_path needs explicit coverage: an empty
        // template substitutes to `""` for any input (no tokens to
        // expand). The predictor must reject so the caller falls back
        // to V8 + TS for the authoritative empty-name error — without
        // this test, a refactor that dropped the `.is_empty()` arm
        // would let the cheap-first path build a `parent/` path which
        // `parent.join("")` normalizes back to `parent`, and the
        // existence check would skip the whole batch as "already
        // exists."
        let input = PathBuf::from("/subs/Show.ass");
        let predicted = predict_chain_output_path(&input, "", None);
        assert!(
            predicted.is_none(),
            "empty template should reject; got {predicted:?}"
        );
    }

    // ── display_path is a PURE FORMATTER (operational) ──

    #[test]
    fn display_path_preserves_control_chars_for_filesystem_correctness() {
        // display_path's return value is used operationally — as the
        // input to `read_text_detect_encoding_inner` and as the
        // `HdrPathRequest.input_path` field sent to the engine. If a
        // POSIX filename legitimately contains a control char or
        // U+202E, stripping at display_path would make the CLI open a
        // DIFFERENT file (or the same name minus the special char,
        // which may also exist) — a path-confusion bug. Display-time
        // sanitization belongs in `sanitize_for_display` and is
        // applied only at print sites; display_path itself stays
        // operational-pure.
        let evil = PathBuf::from("/subs/evil\u{202e}.ass");
        let got = display_path(&evil);
        assert!(
            got.contains('\u{202e}'),
            "display_path must preserve U+202E for filesystem correctness; got {got:?}"
        );
        let esc = PathBuf::from("/subs/x\u{001b}[2J.ass");
        let got = display_path(&esc);
        assert!(
            got.contains('\u{001b}'),
            "display_path must preserve ESC for filesystem correctness; got {got:?}"
        );
    }

    #[test]
    fn sanitize_for_display_strips_control_and_bidi() {
        // The display-time sanitizer companion to display_path. Used
        // at `emit_file_report` and `format_oversized_skipped_warning`
        // to laundering paths before stderr/println interpolation,
        // so a crafted argv filename can't corrupt the terminal.
        assert_eq!(
            sanitize_for_display("/subs/evil\u{202e}.ass"),
            "/subs/evil.ass"
        );
        assert_eq!(
            sanitize_for_display("/subs/x\u{001b}[2J.ass"),
            "/subs/x[2J.ass"
        );
        // Normal paths pass through unchanged.
        assert_eq!(
            sanitize_for_display("/home/u/episode.ass"),
            "/home/u/episode.ass"
        );
    }

    fn make_resolved_with_font_name(
        label: &str,
        font_name: &str,
        path: &str,
        index: u32,
        codepoints: &[u32],
    ) -> ResolvedEmbedFont {
        ResolvedEmbedFont {
            label: label.to_string(),
            font_name: font_name.to_string(),
            path: path.to_string(),
            index,
            bold: false,
            italic: false,
            codepoints: codepoints.to_vec(),
        }
    }

    fn make_resolved_with_style(
        label: &str,
        path: &str,
        index: u32,
        bold: bool,
        italic: bool,
        codepoints: &[u32],
    ) -> ResolvedEmbedFont {
        ResolvedEmbedFont {
            label: label.to_string(),
            font_name: format!("{label}.ttf"),
            path: path.to_string(),
            index,
            bold,
            italic,
            codepoints: codepoints.to_vec(),
        }
    }

    fn make_resolved(label: &str, path: &str, index: u32, codepoints: &[u32]) -> ResolvedEmbedFont {
        make_resolved_with_font_name(label, &format!("{label}.ttf"), path, index, codepoints)
    }

    fn diagnostic_for_resolved(font: &ResolvedEmbedFont) -> FontDiagnostic {
        let usage = engine::FontEmbedUsage {
            family: font.label.clone(),
            bold: font.bold,
            italic: font.italic,
            label: font.label.clone(),
            font_name: font.font_name.clone(),
            glyph_count: font.codepoints.len(),
            codepoints: font.codepoints.clone(),
        };
        let mut diagnostic = FontDiagnostic::new(&usage);
        diagnostic.mark_resolved(font.path.clone(), font.index);
        diagnostic
    }

    #[test]
    fn group_resolved_fonts_collapses_aliases_on_same_face() {
        // Canonical alias case: English + Chinese family names both
        // resolved to msyh.ttc face 0. Pre-fix this produced two
        // byte-identical subset calls embedded under different
        // filenames; post-fix it produces one entry with the union
        // of both aliases' codepoints.
        let aliases = vec![
            make_resolved("Microsoft YaHei", "/fonts/msyh.ttc", 0, &[0x41, 0x42]),
            make_resolved("微软雅黑", "/fonts/msyh.ttc", 0, &[0x4f60, 0x597d]),
        ];
        let groups = group_resolved_fonts_by_face(&aliases);
        assert_eq!(groups.len(), 1, "two aliases on same face must collapse");
        let group = groups.values().next().expect("the single group");
        // Both labels are preserved so subset-failure diagnostics
        // can name every Style affected by a failure, not just
        // first-seen.
        assert_eq!(group.labels.len(), 2);
        assert!(group.labels.contains(&"Microsoft YaHei".to_string()));
        assert!(group.labels.contains(&"微软雅黑".to_string()));
        // Aliases list preserves both insertions in order; the
        // merged codepoints (computed on demand) is the union.
        assert_eq!(group.aliases.len(), 2);
        let merged = group.merged_codepoints();
        assert_eq!(merged, [0x41, 0x42, 0x4f60, 0x597d].into_iter().collect());
        // Template is first-occurrence — drives font_name + path
        // for the subsequent subset_font call.
        assert_eq!(group.template().label, "Microsoft YaHei");
    }

    #[test]
    fn effective_embedded_font_name_matches_dedup_template_for_same_face_aliases() {
        let aliases = vec![
            make_resolved_with_font_name(
                "Microsoft YaHei",
                "microsoft_yahei.ttf",
                "/fonts/msyh.ttc",
                0,
                &[0x41, 0x42],
            ),
            make_resolved_with_font_name(
                "微软雅黑",
                "font_1eda5db2.ttf",
                "/fonts/msyh.ttc",
                0,
                &[0x4f60, 0x597d],
            ),
        ];
        let mut diagnostics: Vec<_> = aliases.iter().map(diagnostic_for_resolved).collect();

        apply_effective_embedded_font_names(&aliases, &mut diagnostics);

        assert_eq!(diagnostics[0].embedded_font_name, "microsoft_yahei.ttf");
        assert_eq!(diagnostics[0].requested_embedded_font_name, None);
        assert_eq!(diagnostics[1].embedded_font_name, "microsoft_yahei.ttf");
        assert_eq!(
            diagnostics[1].requested_embedded_font_name.as_deref(),
            Some("font_1eda5db2.ttf")
        );
    }

    #[test]
    fn effective_embedded_font_name_keeps_alias_labels_for_over_cap_fallback() {
        let aliases = vec![
            make_resolved_range("A", "/fonts/face.ttf", 0x010000, 70_000),
            make_resolved_range("B", "/fonts/face.ttf", 0x020000, 70_000),
            make_resolved_range("C", "/fonts/face.ttf", 0x030000, 70_000),
        ];
        let mut diagnostics: Vec<_> = aliases.iter().map(diagnostic_for_resolved).collect();

        apply_effective_embedded_font_names(&aliases, &mut diagnostics);

        assert_eq!(diagnostics[0].embedded_font_name, "A.ttf");
        assert_eq!(diagnostics[1].embedded_font_name, "B.ttf");
        assert_eq!(diagnostics[2].embedded_font_name, "C.ttf");
        assert!(
            diagnostics
                .iter()
                .all(|diagnostic| diagnostic.requested_embedded_font_name.is_none()),
            "over-cap fallback emits per-alias labels, so diagnostics must not rewrite them"
        );
    }

    #[test]
    fn diagnostic_subset_budget_skips_when_call_cap_is_reached() {
        let mut budget = DiagnosticSubsetBudget {
            calls: MAX_DIAGNOSTIC_SUBSET_CALLS,
            bytes: 0,
            exhausted_reason: None,
        };

        let check = budget
            .begin_call()
            .expect("at-cap budget should skip before another subset call");

        assert_eq!(check.status, FontSubsetCheckStatus::Skipped);
        assert!(
            check
                .error
                .as_deref()
                .is_some_and(|error| error.contains("call budget exceeded")),
            "skip reason should mention call budget: {check:?}"
        );
    }

    #[test]
    fn diagnostic_subset_budget_skips_when_byte_cap_would_be_exceeded() {
        let mut budget = DiagnosticSubsetBudget {
            calls: 0,
            bytes: MAX_DIAGNOSTIC_SUBSET_TOTAL_BYTES - 1,
            exhausted_reason: None,
        };

        budget.finish_bytes(2);
        let check = budget
            .begin_call()
            .expect("later checks should skip after byte budget exhaustion");

        assert_eq!(check.status, FontSubsetCheckStatus::Skipped);
        assert!(
            check
                .error
                .as_deref()
                .is_some_and(|error| error.contains("byte budget exceeded")),
            "skip reason should mention byte budget: {check:?}"
        );
    }

    #[test]
    fn subset_check_budget_is_shared_across_diagnostic_files() {
        let first = vec![make_resolved("First", "/fonts/first.ttf", 0, &[0x41])];
        let second = vec![make_resolved("Second", "/fonts/second.ttf", 0, &[0x42])];
        let mut first_diagnostics: Vec<_> = first.iter().map(diagnostic_for_resolved).collect();
        let mut second_diagnostics: Vec<_> = second.iter().map(diagnostic_for_resolved).collect();
        let mut budget = DiagnosticSubsetBudget {
            calls: MAX_DIAGNOSTIC_SUBSET_CALLS,
            bytes: 0,
            exhausted_reason: None,
        };
        let mut emitted = false;

        let first_warnings = apply_subset_checks_to_diagnostics(
            &first,
            &mut first_diagnostics,
            &mut budget,
            &mut emitted,
        );
        let second_warnings = apply_subset_checks_to_diagnostics(
            &second,
            &mut second_diagnostics,
            &mut budget,
            &mut emitted,
        );

        assert_eq!(first_warnings.len(), 1);
        assert!(
            first_warnings[0].contains("call budget exceeded"),
            "first file should surface the command-level budget cap: {first_warnings:?}"
        );
        assert!(
            second_warnings.is_empty(),
            "shared warning gate should avoid one warning per input file"
        );
        for diagnostic in first_diagnostics.iter().chain(second_diagnostics.iter()) {
            let check = diagnostic
                .subset_check
                .as_ref()
                .expect("resolved diagnostics should receive a subset-check result");
            assert_eq!(check.status, FontSubsetCheckStatus::Skipped);
            assert!(
                check
                    .error
                    .as_deref()
                    .is_some_and(|error| error.contains("call budget exceeded")),
                "all skipped diagnostics should cite the shared cap: {check:?}"
            );
        }
    }

    #[test]
    fn group_resolved_fonts_keeps_distinct_face_indices() {
        // Same TTC file, different face_index — these are genuinely
        // distinct faces (face 0 = Microsoft YaHei, face 1 =
        // Microsoft YaHei UI inside msyh.ttc) and must NOT collapse.
        // If a future bug keys dedup on just `path` without `index`,
        // this test catches the regression.
        let inputs = vec![
            make_resolved("Microsoft YaHei", "/fonts/msyh.ttc", 0, &[0x41]),
            make_resolved("Microsoft YaHei UI", "/fonts/msyh.ttc", 1, &[0x42]),
        ];
        let groups = group_resolved_fonts_by_face(&inputs);
        assert_eq!(groups.len(), 2);
        // Both face indices present as map keys.
        let indices: std::collections::BTreeSet<u32> =
            groups.keys().map(|(_, i, _, _)| *i).collect();
        assert_eq!(indices, [0u32, 1u32].into_iter().collect());
    }

    #[test]
    fn group_resolved_fonts_keeps_style_flags_distinct_on_same_face() {
        let inputs = vec![
            make_resolved_with_style(
                "Face Regular",
                "/fonts/family.ttc",
                0,
                false,
                false,
                &[0x41],
            ),
            make_resolved_with_style("Face Bold", "/fonts/family.ttc", 0, true, false, &[0x42]),
            make_resolved_with_style("Face Italic", "/fonts/family.ttc", 0, false, true, &[0x43]),
        ];

        let groups = group_resolved_fonts_by_face(&inputs);

        assert_eq!(
            groups.len(),
            3,
            "same path/index with different ASS style flags must not dedup"
        );
        let styles: std::collections::BTreeSet<(bool, bool)> = groups
            .keys()
            .map(|(_, _, bold, italic)| (*bold, *italic))
            .collect();
        assert_eq!(
            styles,
            [(false, false), (true, false), (false, true)]
                .into_iter()
                .collect()
        );
    }

    #[test]
    fn group_resolved_fonts_deduplicates_repeated_label_for_same_face() {
        // Defensive: if the same label appears multiple times for
        // the same face (shouldn't happen in practice — resolve_embed_fonts
        // emits one ResolvedEmbedFont per ASS-referenced family — but
        // a future refactor could let it through), the labels list
        // must NOT accumulate duplicates. Codepoints still union.
        let inputs = vec![
            make_resolved("Arial", "/fonts/arial.ttf", 0, &[0x41]),
            make_resolved("Arial", "/fonts/arial.ttf", 0, &[0x42]),
        ];
        let groups = group_resolved_fonts_by_face(&inputs);
        assert_eq!(groups.len(), 1);
        let group = groups.values().next().expect("single group");
        assert_eq!(group.labels, vec!["Arial".to_string()]);
        // Aliases list keeps both insertions (labels dedup, aliases don't);
        // the union is still the same set.
        assert_eq!(group.aliases.len(), 2);
        let merged = group.merged_codepoints();
        assert_eq!(merged, [0x41u32, 0x42u32].into_iter().collect());
    }

    #[test]
    fn group_resolved_fonts_preserves_template_codepoints_for_singleton() {
        // Singleton input (one ResolvedEmbedFont, no aliases) — the
        // helper still goes through the BTreeMap path; verify the
        // codepoint set survives intact and labels has exactly one
        // entry. Boundary test for "doesn't break the common case
        // where dedup is a no-op."
        let inputs = vec![make_resolved(
            "Roboto",
            "/fonts/roboto.ttf",
            0,
            &[0x30, 0x31, 0x32],
        )];
        let groups = group_resolved_fonts_by_face(&inputs);
        assert_eq!(groups.len(), 1);
        let group = groups.values().next().expect("single group");
        assert_eq!(group.labels, vec!["Roboto".to_string()]);
        assert_eq!(group.aliases.len(), 1);
        let merged: Vec<u32> = group.merged_codepoints().into_iter().collect();
        assert_eq!(merged, vec![0x30, 0x31, 0x32]);
    }

    #[test]
    fn group_resolved_fonts_empty_input_yields_empty_map() {
        let groups = group_resolved_fonts_by_face(&[]);
        assert_eq!(groups.len(), 0);
    }

    // ── Boundary-pin for the dedup-vs-fallback cap ──

    fn make_resolved_range(label: &str, path: &str, start: u32, count: u32) -> ResolvedEmbedFont {
        let codepoints: Vec<u32> = (0..count).map(|i| start + i).collect();
        ResolvedEmbedFont {
            label: label.to_string(),
            font_name: format!("{label}.ttf"),
            path: path.to_string(),
            index: 0,
            bold: false,
            italic: false,
            codepoints,
        }
    }

    #[test]
    fn merged_codepoints_at_cap_stays_within_subset_limit() {
        // 4 aliases × 50,000 disjoint codepoints each = 200,000
        // exactly at MAX_SUBSET_CODEPOINTS_FOR_DEDUP. The dedup
        // decision uses `> cap`, so at-cap takes the dedup path —
        // one subset_font call with the unioned set. This pins the
        // boundary: a regression that flipped the comparison to
        // `>=` would route at-cap groups to the fallback path
        // unnecessarily and lose the dedup win for inputs that
        // legitimately fit the cap.
        let aliases = vec![
            make_resolved_range("A", "/fonts/face.ttf", 0x010000, 50_000),
            make_resolved_range("B", "/fonts/face.ttf", 0x020000, 50_000),
            make_resolved_range("C", "/fonts/face.ttf", 0x030000, 50_000),
            make_resolved_range("D", "/fonts/face.ttf", 0x040000, 50_000),
        ];
        let groups = group_resolved_fonts_by_face(&aliases);
        assert_eq!(groups.len(), 1);
        let group = groups.values().next().expect("single group");
        let merged = group
            .merged_codepoints_with_cap(MAX_SUBSET_CODEPOINTS_FOR_DEDUP)
            .expect("at-cap union should stay under cap");
        assert_eq!(merged.len(), 200_000);
        assert!(merged.len() <= MAX_SUBSET_CODEPOINTS_FOR_DEDUP);
    }

    #[test]
    fn merged_codepoints_over_cap_signals_fallback() {
        // 4 aliases × 50,001 disjoint codepoints each = 200,004,
        // but the bounded merge must stop at cap+1. The actual
        // subset_font fallback can't be unit-tested without the IPC-
        // bypass setup that the env-var-gated integration test
        // (subset_with_index_handles_ttc) already covers. This test
        // guards both the early-stop resource bound and the contract
        // that per-alias detail survives for fallback.
        let aliases = vec![
            make_resolved_range("A", "/fonts/face.ttf", 0x010000, 50_001),
            make_resolved_range("B", "/fonts/face.ttf", 0x020000, 50_001),
            make_resolved_range("C", "/fonts/face.ttf", 0x030000, 50_001),
            make_resolved_range("D", "/fonts/face.ttf", 0x040000, 50_001),
        ];
        let groups = group_resolved_fonts_by_face(&aliases);
        assert_eq!(groups.len(), 1);
        let group = groups.values().next().expect("single group");
        let over_cap_len = group
            .merged_codepoints_with_cap(MAX_SUBSET_CODEPOINTS_FOR_DEDUP)
            .expect_err("over-cap union should signal fallback");
        assert_eq!(
            over_cap_len,
            MAX_SUBSET_CODEPOINTS_FOR_DEDUP + 1,
            "over-cap union should stop as soon as fallback is known"
        );
        // Aliases preserved so subset_resolved_fonts can iterate
        // them for the per-alias fallback path. Each alias's
        // codepoint count stays under the cap individually.
        assert_eq!(group.aliases.len(), 4);
        for alias in &group.aliases {
            assert!(
                alias.codepoints.len() <= MAX_SUBSET_CODEPOINTS_FOR_DEDUP,
                "fallback subset call must stay within the cap"
            );
        }
    }
}
