//! Command-line interface binary entry point for SSA HDRify (`ssahdrify-cli`).
//!
//! Submodules organize CLI responsibilities while keeping `main.rs` focused on
//! startup, logging, and top-level command dispatch:
//! - `args`: clap CLI schema, duration/timestamp parsers, and localization
//! - `chain` / `cmd_chain`: `+`-separated chain parser and execution pipeline
//! - `cmd_hdr_shift`: `hdr` and `shift` workflows
//! - `cmd_embed`: `embed`, `diagnose-fonts`, and font resolution/subsetting
//! - `cmd_font_cache`: `refresh-fonts` and persistent/session font-cache setup
//! - `cmd_rename`: `rename` pairing and file operations
//! - `engine`: embedded `deno_core` / V8 bridge to the shared TS engine
//! - `io_support`: path normalization, output reservation, and safe-I/O helpers
//! - `report`: human/JSON command reports and Font QA diagnostics

mod args;
mod chain;
mod cmd_chain;
mod cmd_embed;
mod cmd_font_cache;
mod cmd_hdr_shift;
mod cmd_rename;
mod cmd_style_edit;
mod engine;
mod io_support;
mod report;

use std::process::ExitCode;

use clap::Parser;

#[allow(unused_imports)]
pub(crate) use args::*;
#[allow(unused_imports)]
pub(crate) use cmd_chain::*;
#[allow(unused_imports)]
pub(crate) use cmd_embed::*;
#[allow(unused_imports)]
pub(crate) use cmd_font_cache::*;
#[allow(unused_imports)]
pub(crate) use cmd_hdr_shift::*;
#[allow(unused_imports)]
pub(crate) use cmd_rename::*;
#[allow(unused_imports)]
pub(crate) use cmd_style_edit::*;
#[allow(unused_imports)]
pub(crate) use io_support::*;
#[allow(unused_imports)]
pub(crate) use report::*;

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
    let Cli {
        mut globals,
        command,
    } = Cli::parse();
    if !matches!(command, Command::Licenses) {
        normalize_global_paths(&mut globals)?;
    }

    match command {
        Command::Licenses => {
            if globals.json {
                return Err("licenses does not implement --json output".to_string());
            }
            if globals.dry_run {
                return Err("licenses does not support --dry-run".to_string());
            }
            print!(
                "{}",
                include_str!(concat!(env!("OUT_DIR"), "/cli-third-party-notices.txt"))
            );
            Ok(ExitCode::SUCCESS)
        }
        Command::Hdr(args) => {
            emit_inert_cache_flags_notice(&globals, "hdr");
            run_hdr(&globals, args.args, args.diagnose.mode())
        }
        Command::Shift(args) => {
            emit_inert_cache_flags_notice(&globals, "shift");
            run_shift(&globals, args.args, args.diagnose.mode())
        }
        Command::Embed(args) => run_embed(&globals, args.args, args.diagnose.mode()),
        Command::Rename(args) => {
            emit_inert_cache_flags_notice(&globals, "rename");
            run_rename(&globals, args.args, args.diagnose.mode())
        }
        Command::StyleEdit(args) => {
            emit_inert_cache_flags_notice(&globals, "style-edit");
            run_style_edit(&globals, args.args, args.diagnose.mode())
        }
        Command::DiagnoseFonts(args) => run_diagnose_fonts(&globals, args),
        Command::Chain(args) => run_chain(&globals, args),
        Command::RefreshFonts(args) => run_refresh_fonts(&globals, args),
    }
}

#[cfg(test)]
mod tests;
