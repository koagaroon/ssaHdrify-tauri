//! `chain` command execution, output-path prediction, and dry-run reporting.

use super::*;
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

pub(super) fn run_chain(globals: &GlobalOptions, args: ChainArgs) -> Result<ExitCode, String> {
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
    // pass --output-template. --quiet and --json suppress chatter.
    if !user_supplied_template && !globals.quiet && !globals.json {
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
    // Non-embed chains never reach font resolution, so --no-cache and
    // --cache-file have no effect there; surface that explicitly per
    // no-silent-action. When an embed step IS present, validate --cache-file
    // upfront (including under --dry-run) and consult the persistent font
    // cache in read-only mode below.
    if embed_step_index.is_none() {
        if !globals.quiet {
            let mut inert = Vec::new();
            if globals.no_cache {
                inert.push("--no-cache");
            }
            if globals.cache_file.is_some() {
                inert.push("--cache-file");
            }
            if !inert.is_empty() {
                let flags = inert.join(", ");
                eprintln!(
                    "{}",
                    localize(
                        globals,
                        format!("ℹ {flags} has no effect when chain has no embed step."),
                        format!("ℹ 当 chain 不包含 embed 步骤时，{flags} 不生效。"),
                    )
                );
            }
        }
    } else {
        if !globals.no_cache {
            validate_cache_file_arg(globals)?;
        } else if globals.dry_run && !globals.quiet && !globals.json {
            eprintln!(
                "{}",
                localize(
                    globals,
                    "ℹ Cache disabled (--no-cache). Using explicit font sources / system fonts only."
                        .to_string(),
                    "ℹ 已禁用缓存（--no-cache）。仅使用显式字体源 / 系统字体。".to_string(),
                )
            );
        }
    }

    if globals.dry_run {
        if globals.json {
            return emit_chain_dry_run_json(&plan, globals);
        }
        return emit_chain_dry_run(&plan, globals);
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
    let (_font_db_guard, prepared_cache) = match embed_step_index {
        Some(idx) => {
            let chain::ParsedStep::Embed(embed_args) = &plan.steps[idx] else {
                // find_embed_step_index returns Some only for Embed
                // variants — invariant holds by construction.
                unreachable!("find_embed_step_index returned a non-Embed index");
            };
            let use_user_fonts = embed_args.has_user_font_sources();
            let guard = if use_user_fonts {
                Some(init_cli_font_sources(globals, embed_args)?)
            } else {
                None
            };
            // Consult the persistent font cache in read-only mode so chain never
            // creates schema or alters SQLite journal files while still reusing
            // cached font metadata and falling back cleanly on drift.
            let cache = prepare_font_cache_for_resolution(globals, embed_args, true, true)?;
            (guard, Some(cache))
        }
        None => (None, None),
    };
    let cache = prepared_cache.as_ref().and_then(|p| p.cache.as_ref());

    let mut engine = engine::CliEngine::new()?;
    let mut report = CommandReport::new("chain");
    let mut written = 0usize;
    let mut failed = 0usize;
    let mut skipped = 0usize;
    let mut written_with_warnings = 0usize;
    let mut written_warning_count = 0usize;
    let mut seen_outputs: HashSet<String> = HashSet::new();
    let selected_inputs = selected_input_keys(&plan.input_files);
    let mut chain_aborted = false;
    for (idx, input) in plan.input_files.iter().enumerate() {
        let mut failed_this_input = false;
        let input_report = absolute_path(input)
            .as_deref()
            .map(display_path)
            .unwrap_or_else(|_| input.to_string_lossy().into_owned());
        match process_one_chain_input(
            &mut engine,
            &plan,
            embed_step_index,
            cache,
            input,
            globals,
            &mut seen_outputs,
            &selected_inputs,
        ) {
            ChainFileOutcome::Written {
                output_path: out,
                encoding,
                warnings,
            } => {
                if !warnings.is_empty() {
                    written_with_warnings += 1;
                }
                written_warning_count += warnings.len();
                if !globals.json && !globals.quiet {
                    let input_disp = sanitize_for_display(&input.to_string_lossy());
                    let out_disp = sanitize_for_display(&out.to_string_lossy());
                    println!("✓ {input_disp} → {out_disp}");
                    emit_chain_warnings(globals, &warnings);
                }
                report.push(FileReport {
                    input: input_report,
                    output: Some(display_path(&out)),
                    encoding,
                    status: FileStatus::Written,
                    error: None,
                    warnings: (!warnings.is_empty()).then_some(warnings),
                });
                written += 1;
            }
            ChainFileOutcome::Skipped {
                output_path,
                encoding,
                reason,
                warnings,
            } => {
                if !globals.json && !globals.quiet {
                    let input_disp = sanitize_for_display(&input.to_string_lossy());
                    let reason_disp = sanitize_for_display(&reason);
                    println!("⊘ {input_disp}: {reason_disp}");
                    emit_chain_warnings(globals, &warnings);
                }
                report.push(FileReport {
                    input: input_report,
                    output: output_path.as_deref().map(display_path),
                    encoding,
                    status: FileStatus::Skipped,
                    error: Some(reason),
                    warnings: (!warnings.is_empty()).then_some(warnings),
                });
                skipped += 1;
            }
            ChainFileOutcome::Failed {
                output_path,
                encoding,
                error: err,
                warnings,
            } => {
                if !globals.json {
                    let input_disp = sanitize_for_display(&input.to_string_lossy());
                    let err_disp = sanitize_for_display(&err);
                    eprintln!("✗ {input_disp}: {err_disp}");
                    if !globals.quiet {
                        emit_chain_warnings(globals, &warnings);
                    }
                }
                report.push(FileReport {
                    input: input_report,
                    output: output_path.as_deref().map(display_path),
                    encoding,
                    status: FileStatus::Failed,
                    error: Some(err),
                    warnings: (!warnings.is_empty()).then_some(warnings),
                });
                failed += 1;
                failed_this_input = true;
            }
        }
        if globals.fail_fast && failed_this_input {
            let remaining = plan.input_files.len().saturating_sub(idx + 1);
            emit_fail_fast_abort_notice(globals, remaining);
            report.mark_fail_fast_abort();
            chain_aborted = true;
            break;
        }
    }
    if globals.json {
        emit_report_summary(globals, &report)?;
    } else if !globals.quiet {
        let warning_suffix_en = if written_with_warnings > 0 {
            format!(
                ", {written_with_warnings} written with warnings / incomplete ({written_warning_count} warning(s))"
            )
        } else {
            String::new()
        };
        let warning_suffix_zh = if written_with_warnings > 0 {
            format!(
                "，{written_with_warnings} 个已写入但带警告/不完整（{written_warning_count} 条警告）"
            )
        } else {
            String::new()
        };
        let summary = if chain_aborted {
            localize(
                globals,
                format!(
                    "Summary: {written} written, {skipped} skipped, {failed} failed{warning_suffix_en} (aborted by --fail-fast)"
                ),
                format!(
                    "汇总：{written} 个已写入，{skipped} 个已跳过，{failed} 个失败{warning_suffix_zh}（已被 --fail-fast 中止）"
                ),
            )
        } else {
            localize(
                globals,
                format!(
                    "Summary: {written} written, {skipped} skipped, {failed} failed{warning_suffix_en}"
                ),
                format!(
                    "汇总：{written} 个已写入，{skipped} 个已跳过，{failed} 个失败{warning_suffix_zh}"
                ),
            )
        };
        println!("{summary}");
    }
    Ok(report.exit_code())
}

pub(super) enum ChainFileOutcome {
    Written {
        output_path: PathBuf,
        encoding: Option<String>,
        warnings: Vec<String>,
    },
    Skipped {
        output_path: Option<PathBuf>,
        encoding: Option<String>,
        reason: String,
        warnings: Vec<String>,
    },
    Failed {
        output_path: Option<PathBuf>,
        encoding: Option<String>,
        error: String,
        warnings: Vec<String>,
    },
}

pub(super) struct ChainOutcomeBuilder {
    pub(super) warnings: Vec<String>,
    pub(super) output_path: Option<PathBuf>,
    pub(super) encoding: Option<String>,
}

impl ChainOutcomeBuilder {
    pub(super) fn new() -> Self {
        Self {
            warnings: Vec::new(),
            output_path: None,
            encoding: None,
        }
    }

    pub(super) fn set_output_path(&mut self, output_path: PathBuf) {
        self.output_path = Some(output_path);
    }

    pub(super) fn set_encoding(&mut self, encoding: String) {
        self.encoding = Some(encoding);
    }

    pub(super) fn extend_warnings<I: IntoIterator<Item = String>>(&mut self, iter: I) {
        self.warnings.extend(iter);
    }

    pub(super) fn into_failed(self, err: String) -> ChainFileOutcome {
        ChainFileOutcome::Failed {
            output_path: self.output_path,
            encoding: self.encoding,
            error: err,
            warnings: self.warnings,
        }
    }

    pub(super) fn into_skipped(self, reason: String) -> ChainFileOutcome {
        ChainFileOutcome::Skipped {
            output_path: self.output_path,
            encoding: self.encoding,
            reason,
            warnings: self.warnings,
        }
    }

    pub(super) fn into_written(self, output_path: PathBuf) -> ChainFileOutcome {
        ChainFileOutcome::Written {
            output_path,
            encoding: self.encoding,
            warnings: self.warnings,
        }
    }
}

pub(super) fn find_embed_step_index(plan: &chain::ChainPlan) -> Option<usize> {
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
pub(super) fn emit_chain_warnings(globals: &GlobalOptions, warnings: &[String]) {
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
pub(super) fn substitute_template(template: &str, vars: &[(&str, &str)]) -> Option<String> {
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
pub(super) fn collapse_internal_double_dots(s: &str) -> String {
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
pub(super) fn predict_chain_output_path(
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
    // Trailing ASCII space/dot output names are rejected below under the
    // shared product contract, mirroring TS filename validation.
    // Returning None preserves predictor abstention: V8 + TS supplies the
    // authoritative user-facing rejection instead of using a guessed path.
    //
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
    // dots (".hidden.ass" is a legitimate filename).
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
    if output_name.ends_with(' ') || output_name.ends_with('.') {
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

#[allow(clippy::too_many_arguments)]
pub(super) fn process_one_chain_input(
    engine: &mut engine::CliEngine,
    plan: &chain::ChainPlan,
    embed_step_index: Option<usize>,
    cache: Option<&app_lib::font_cache::FontCache>,
    input: &Path,
    globals: &GlobalOptions,
    seen_outputs: &mut HashSet<String>,
    selected_inputs: &HashSet<String>,
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
    //      or V8 work. Mirrors `reserve_output` in the
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
        builder.set_output_path(predicted.clone());
        let key = normalize_output_key(&predicted);
        let input_key = normalize_output_key(&input_abs);
        if key == input_key {
            return builder.into_failed(format!(
                "refusing self-overwrite: predicted output path {} is the input file; choose a different --output-template or --output-dir",
                predicted.display()
            ));
        }
        if output_matches_selected_input(&predicted, selected_inputs) {
            return builder.into_failed(
                "output path matches a selected input file; choose a different --output-template or --output-dir".to_string(),
            );
        }
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
    builder.set_encoding(read_result.encoding.clone());
    if let Some(warnings) = format_decode_warnings(globals, &read_result) {
        builder.extend_warnings(warnings);
    }

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
            cache,
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
                builder.extend_warnings(partial_warnings);
                return builder.into_failed(err);
            }
        };
        builder.extend_warnings(embed_warnings);
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
        // Serialize the canonical FontSubsetPayload wire type only after
        // enforcing the raw-byte cap above. Its custom serializer owns the
        // dataB64 representation, keeping standalone embed and chain from
        // drifting into two hand-written payload shapes.
        let subsets_json = match serde_json::to_value(&subsets) {
            Ok(value) => value,
            Err(error) => {
                evict_predicted(seen_outputs);
                return builder
                    .into_failed(format!("failed to serialize chain embed subsets: {error}"));
            }
        };
        payload["plan"]["steps"][idx]["params"]["subsets"] = subsets_json;
    }

    let request = engine::ChainRunRequest { payload };

    let result = match engine.run_chain(&request) {
        Ok(r) => r,
        Err(err) => {
            evict_predicted(seen_outputs);
            return builder.into_failed(err);
        }
    };

    // Surface chain's deduplicated skipped-caption count through the
    // same path the standalone HDR / Shift CLIs use — stderr "⚠ ..."
    // line + append to FileReport.warnings (for --json output). An
    // older shape rode along inside an opaque chain note string that
    // printed to stdout under --verbose only, missing both the
    // stderr-routing and the json wire. Embed pre-resolution warnings
    // (collected above) sit in the same vec; both get surfaced via
    // the Written outcome.
    if let Some(msgs) = format_oversized_caption_warning(
        globals,
        result.skipped_count,
        &input_str,
        OversizedCaptionHandling::FormatDependent,
    ) {
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
    builder.set_output_path(output_path.clone());

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
    if output_matches_selected_input(&output_path, selected_inputs) {
        evict_predicted(seen_outputs);
        return builder.into_failed(
            "output path matches a selected input file; choose a different --output-template or --output-dir".to_string(),
        );
    }
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

    if globals.verbose && !globals.json && !globals.quiet {
        for note in &result.notes {
            let n_disp = sanitize_for_display(note);
            println!("  {n_disp}");
        }
    }

    builder.into_written(output_path)
}

fn resolve_chain_dry_run_output(
    abs: &Path,
    output_template: &str,
    output_dir: Option<&Path>,
) -> Result<Option<PathBuf>, String> {
    let Some(unrelocated) = predict_chain_output_path(abs, output_template, None) else {
        let stem = abs.file_stem().and_then(|s| s.to_str()).unwrap_or_default();
        let ext = abs
            .extension()
            .and_then(|e| e.to_str())
            .map(|e| format!(".{e}"))
            .unwrap_or_default();
        if let Some(candidate_name) =
            substitute_template(output_template, &[("name", stem), ("ext", &ext)])
        {
            if let Some(parent) = abs.parent() {
                app_lib::util::validate_output_path_shape(&parent.join(&candidate_name), "Output")?;
            }
            return Err(format!(
                "output template produced an invalid output filename: {}",
                sanitize_for_display(&candidate_name)
            ));
        }
        return Ok(None);
    };
    relocate_output_path(&unrelocated.to_string_lossy(), output_dir).map(Some)
}

pub(super) fn emit_chain_dry_run_json(
    plan: &chain::ChainPlan,
    globals: &GlobalOptions,
) -> Result<ExitCode, String> {
    let selected_inputs = selected_input_keys(&plan.input_files);
    let mut report = CommandReport::new("chain");
    let mut seen_outputs: HashSet<String> = HashSet::new();
    for (idx, input) in plan.input_files.iter().enumerate() {
        let result = match absolute_path(input) {
            Ok(abs) => {
                match resolve_chain_dry_run_output(
                    &abs,
                    &plan.output_template,
                    globals.output_dir.as_deref(),
                ) {
                    Ok(Some(out_path)) => {
                        let out_disp = display_path(&out_path);
                        let key = normalize_output_key(&out_path);
                        let input_key = normalize_output_key(&abs);
                        if key == input_key {
                            failed_report(
                                &abs,
                                Some(out_disp),
                                None,
                                format!(
                                    "refusing self-overwrite: predicted output path {} is the input file; choose a different --output-template or --output-dir",
                                    out_path.display()
                                ),
                            )
                        } else if output_matches_selected_input(&out_path, &selected_inputs) {
                            failed_report(
                                &abs,
                                Some(out_disp),
                                None,
                                "output path matches a selected input file; choose a different --output-template or --output-dir".to_string(),
                            )
                        } else if !seen_outputs.insert(key.clone()) {
                            failed_report(
                                &abs,
                                Some(out_disp),
                                None,
                                format!(
                                    "{} duplicate output path in planned batch",
                                    out_path.display()
                                ),
                            )
                        } else if !globals.overwrite && output_path_exists(globals, &out_path) {
                            seen_outputs.remove(&key);
                            skipped_report(
                                &abs,
                                Some(out_disp),
                                None,
                                format!(
                                    "{} already exists (use --overwrite to replace)",
                                    out_path.display()
                                ),
                            )
                        } else {
                            planned_report(&abs, Some(out_disp), None)
                        }
                    }
                    Ok(None) => planned_report(&abs, None, None),
                    Err(err) => failed_report(&abs, None, None, err),
                }
            }
            Err(err) => failed_report(input, None, None, err),
        };
        let failed = result.status == FileStatus::Failed;
        report.push(result);
        if globals.fail_fast && failed {
            report.mark_fail_fast_abort();
            let _ = idx;
            break;
        }
    }

    emit_report_summary(globals, &report)?;
    Ok(report.exit_code())
}

pub(super) fn emit_chain_dry_run(
    plan: &chain::ChainPlan,
    globals: &GlobalOptions,
) -> Result<ExitCode, String> {
    let selected_inputs = selected_input_keys(&plan.input_files);
    if !globals.quiet {
        println!("Plan (no files written):");
        println!();
        let template_disp = sanitize_for_display(&plan.output_template);
        println!("Output template: {template_disp}");
        println!();
    }

    let mut seen_outputs: HashSet<String> = HashSet::new();
    let mut non_failed = 0usize;
    let mut failed = 0usize;

    for (idx, input) in plan.input_files.iter().enumerate() {
        let input_disp = sanitize_for_display(&input.display().to_string());
        if !globals.quiet {
            println!("  {input_disp}");
        }

        let mut file_error: Option<String> = None;
        match absolute_path(input) {
            Ok(abs) => {
                match resolve_chain_dry_run_output(
                    &abs,
                    &plan.output_template,
                    globals.output_dir.as_deref(),
                ) {
                    Ok(Some(out_path)) => {
                        let out_str = sanitize_for_display(&out_path.display().to_string());
                        let key = normalize_output_key(&out_path);
                        let input_key = normalize_output_key(&abs);
                        if key == input_key {
                            file_error = Some(format!(
                                "refusing self-overwrite: predicted output path {out_str} is the input file; choose a different --output-template or --output-dir"
                            ));
                        } else if output_matches_selected_input(&out_path, &selected_inputs) {
                            file_error = Some(
                                "output path matches a selected input file; choose a different --output-template or --output-dir".to_string(),
                            );
                        } else if !seen_outputs.insert(key.clone()) {
                            file_error =
                                Some(format!("{out_str} duplicate output path in planned batch"));
                        } else if !globals.overwrite && output_path_exists(globals, &out_path) {
                            seen_outputs.remove(&key);
                            non_failed += 1;
                            if !globals.quiet {
                                println!(
                                    "    → {out_str}  (already exists; use --overwrite to replace)"
                                );
                            }
                        } else {
                            non_failed += 1;
                            if !globals.quiet {
                                println!("    → {out_str}");
                            }
                        }
                    }
                    Ok(None) => {
                        non_failed += 1;
                    }
                    Err(err) => {
                        file_error = Some(err);
                    }
                }
            }
            Err(err) => {
                file_error = Some(err);
            }
        }

        if let Some(err) = file_error {
            failed += 1;
            let err_disp = sanitize_for_display(&err);
            if !globals.quiet {
                println!("    → (failed: {err_disp})");
            }
            eprintln!(
                "{}",
                localize(
                    globals,
                    format!("✗ {input_disp}: {err_disp}"),
                    format!("✗ {input_disp}：{err_disp}"),
                )
            );
            if globals.fail_fast {
                let remaining = plan.input_files.len().saturating_sub(idx + 1);
                emit_fail_fast_abort_notice(globals, remaining);
                break;
            }
        } else if !globals.quiet {
            for (i, step) in plan.steps.iter().enumerate() {
                println!("    {}. {}", i + 1, step.kind_name());
            }
        }
    }

    if failed == 0 {
        Ok(ExitCode::SUCCESS)
    } else if non_failed > 0 {
        Ok(ExitCode::from(1))
    } else {
        Ok(ExitCode::from(2))
    }
}
