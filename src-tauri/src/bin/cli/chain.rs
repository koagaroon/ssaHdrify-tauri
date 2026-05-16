//! Chain feature — Rust shell argv parser, validator, and plan
//! builder. The runtime executor lives in TS
//! (`src/features/chain/chain-runtime.ts`); this module produces the
//! `ChainPlan` that the deno_core op layer marshals across the
//! Rust/TS boundary.
//!
//! Wired through main.rs's `chain` subcommand handler. The flow:
//!   1. clap parses `chain` keyword + chain-level `--output-template`
//!      flag, leaves the rest as raw argv on `ChainArgs::raw_argv`.
//!   2. `parse_chain_argv` splits on `+`, parses each step segment
//!      against its per-feature `Args` wrapper, validates ordering
//!      (warn catalog is intentionally minimal: only shift-after-embed
//!      warns), and returns a `ChainPlan` with steps + input_files +
//!      warnings.
//!   3. main.rs::run_chain iterates `plan.input_files`, calls
//!      `runChain` via the engine per file, and writes the terminal
//!      output via existing fs primitives.
//!
//! See `docs/architecture/ssahdrify_cli_design.md` § "v1.4.1 stable
//! 后续用户反馈" feature #4 for the locked design decisions.

use std::path::PathBuf;

use clap::Parser;

use crate::{EmbedArgs, HdrArgs, ShiftArgs};

/// The token between chain steps. PowerShell-safe (no escaping
/// needed); short and visually distinct from clap's flag prefixes.
const STEP_SEPARATOR: &str = "+";

/// Placeholder positional appended to non-terminal step segments
/// before clap parsing. Per-feature `Args` structs declare
/// `files: Vec<PathBuf>` as required positional, so segments without
/// files would fail clap's required-arg check. The placeholder
/// satisfies the requirement; we clear `files` after parsing and
/// ignore it.
const CHAIN_NONTERMINAL_PLACEHOLDER: &str = "__chain_nonterminal_placeholder__";

/// One parsed step. Variants hold the per-feature `Args` struct
/// directly so the deno_core marshal in `engine.rs` reaches all
/// transform parameters through the existing struct shape.
#[derive(Debug)]
pub enum ParsedStep {
    Hdr(HdrArgs),
    Shift(ShiftArgs),
    Embed(EmbedArgs),
}

impl ParsedStep {
    /// Internal kind identifier — matches the TS-side `StepKind`
    /// values (`"hdr"` / `"shift"` / `"embed"`) for cross-boundary
    /// serialization.
    pub fn kind_name(&self) -> &'static str {
        match self {
            Self::Hdr(_) => "hdr",
            Self::Shift(_) => "shift",
            Self::Embed(_) => "embed",
        }
    }

    /// Suffix used in the chain's stacked-default output template
    /// (`{name}.<suffix1>.<suffix2>...<suffixN>.ass`). Matches the
    /// per-feature default's natural form: HDR→`hdr.ass` so suffix
    /// `hdr`; Shift→`shifted.ass` so `shifted`; Embed→`embed.ass`
    /// so `embed`.
    pub fn stack_suffix(&self) -> &'static str {
        match self {
            Self::Hdr(_) => "hdr",
            Self::Shift(_) => "shifted",
            Self::Embed(_) => "embed",
        }
    }

    /// Serialize this step into the JSON shape consumed by the TS
    /// `runChain` runtime (`{ kind, params }`). Delegates to per-Args
    /// `to_chain_step` methods which live in main.rs alongside the
    /// struct definitions (where the private fields are visible).
    pub fn to_chain_step_json(&self) -> Result<serde_json::Value, String> {
        match self {
            Self::Hdr(args) => Ok(args.to_chain_step()),
            Self::Shift(args) => args.to_chain_step(),
            Self::Embed(args) => Ok(args.to_chain_step()),
        }
    }
}

/// A fully parsed and validated chain ready for runtime execution.
///
/// `warnings` collects soft-fail diagnostics from suspicious-pattern
/// detection (HDR×2, shift-after-embed). The caller emits them to
/// stderr; they do NOT block execution per the locked decision
/// "warn but don't enforce" for step ordering.
#[derive(Debug)]
pub struct ChainPlan {
    pub steps: Vec<ParsedStep>,
    pub output_template: String,
    pub input_files: Vec<PathBuf>,
    pub warnings: Vec<String>,
}

impl ChainPlan {
    /// Build the JSON request payload for the TS-side `runChain` op.
    /// Pairs `(plan, inputPath, content)` per the
    /// `ChainRunRequest` shape in chain-types.ts.
    ///
    /// Note: `input_files` and `warnings` are intentionally NOT
    /// serialized — those are Rust-side concerns. The TS runtime sees
    /// only the plan-as-AST + the one input file currently being
    /// processed (multi-file fanout is handled by the Rust shell).
    pub fn to_runtime_payload(
        &self,
        input_path: &str,
        content: &str,
    ) -> Result<serde_json::Value, String> {
        let steps: Result<Vec<_>, _> = self
            .steps
            .iter()
            .map(ParsedStep::to_chain_step_json)
            .collect();
        Ok(serde_json::json!({
            "plan": {
                "steps": steps?,
                "outputTemplate": self.output_template,
            },
            "inputPath": input_path,
            "content": content,
        }))
    }
}

/// Parse raw argv (post-`chain` keyword, post-clap-global flags)
/// into a validated plan.
///
/// `user_output_template` is the chain-level `--output-template`
/// value if the user supplied one; `None` triggers the stacked
/// default (`{name}.<kind1>...<kindN>.ass`).
pub fn parse_chain_argv(
    raw_argv: &[String],
    user_output_template: Option<String>,
) -> Result<ChainPlan, String> {
    let segments = split_into_step_segments(raw_argv)?;
    if segments.is_empty() {
        return Err("chain requires at least one step".into());
    }
    let last_idx = segments.len() - 1;

    let mut steps: Vec<ParsedStep> = Vec::with_capacity(segments.len());
    let mut input_files: Vec<PathBuf> = Vec::new();

    for (i, segment) in segments.iter().enumerate() {
        let is_terminal = i == last_idx;

        // Locked rule: --output-template is chain-level only; placing
        // it inside any step segment (terminal or not) is a parse-time
        // error. Without this, the terminal step's wrapper would parse
        // the value into its inner Args and silently drop it (the
        // chain-level plan.output_template wins downstream), violating
        // ~/.claude/rules/vibe-coding.md "no silent fallback".
        if segment_has_output_template_token(segment) {
            return Err(format!(
                "step {} ({}): --output-template is a chain-level flag. \
                 Move it before any step (e.g., \
                 `chain --output-template <T> ...`).",
                i + 1,
                first_token_or(segment, "<empty>")
            ));
        }

        let mut step = parse_one_step(segment, is_terminal)?;
        if is_terminal {
            input_files = take_step_files(&mut step);
        }
        steps.push(step);
    }

    if input_files.is_empty() {
        return Err(format!(
            "chain's terminal step ({}) has no input files",
            steps.last().map(ParsedStep::kind_name).unwrap_or("?")
        ));
    }

    // v1 limitation: at most one embed step per chain. Multiple embed
    // steps would each need their own font-source SQLite session,
    // and `init_cli_font_sources` is process-global today; supporting
    // multiple would require teardown/reinit between steps. The
    // realistic use case is one embed at the end of the chain, so
    // this is a small ergonomic restriction. Lift if real users hit it.
    let embed_count = steps
        .iter()
        .filter(|s| matches!(s, ParsedStep::Embed(_)))
        .count();
    if embed_count > 1 {
        return Err(format!(
            "chain may include at most one embed step (got {embed_count}); \
             multiple embed steps are not yet supported. Run separate \
             chains, or combine font sources (--font-dir / --font-file) \
             into a single embed step."
        ));
    }

    let warnings = collect_suspicious_orderings(&steps);
    let output_template = user_output_template.unwrap_or_else(|| derive_stacked_default(&steps));

    Ok(ChainPlan {
        steps,
        output_template,
        input_files,
        warnings,
    })
}

fn split_into_step_segments(argv: &[String]) -> Result<Vec<Vec<String>>, String> {
    let mut segments: Vec<Vec<String>> = vec![Vec::new()];
    for tok in argv {
        if tok == STEP_SEPARATOR {
            // `map_or(true, ...)` covers None + empty Vec. Swap to
            // `is_none_or` only after Cargo.toml `rust-version` ≥ 1.82.
            if segments.last().map_or(true, Vec::is_empty) {
                return Err(
                    "empty step segment around `+` (chain requires `<step1> + <step2>...` form)"
                        .into(),
                );
            }
            segments.push(Vec::new());
        } else {
            segments
                .last_mut()
                .expect("segments non-empty by construction")
                .push(tok.clone());
        }
    }
    if segments.last().is_some_and(Vec::is_empty) {
        return Err("trailing `+` separator with no following step".into());
    }
    if segments.iter().all(Vec::is_empty) {
        return Err("chain requires at least one step".into());
    }
    Ok(segments)
}

// ── Per-step wrappers ────────────────────────────────────────
//
// Each per-feature `Args` struct (HdrArgs etc.) derives `Args`, not
// `Parser`. To call `try_parse_from` on them directly we wrap each
// in a Parser-deriving struct that flattens the inner Args. The
// wrapper is local to chain.rs so main.rs's existing derives stay
// untouched.
//
// `no_binary_name = true` tells clap argv[0] is a real argument, not
// a binary path — important because the segment we feed in starts
// with the first flag (the step name has already been stripped).

#[derive(Parser, Debug)]
#[command(no_binary_name = true)]
struct HdrStepWrapper {
    #[command(flatten)]
    inner: HdrArgs,
}

#[derive(Parser, Debug)]
#[command(no_binary_name = true)]
struct ShiftStepWrapper {
    #[command(flatten)]
    inner: ShiftArgs,
}

#[derive(Parser, Debug)]
#[command(no_binary_name = true)]
struct EmbedStepWrapper {
    #[command(flatten)]
    inner: EmbedArgs,
}

fn parse_one_step(segment: &[String], is_terminal: bool) -> Result<ParsedStep, String> {
    if segment.is_empty() {
        return Err("empty step segment (no step name)".into());
    }
    let kind = segment[0].as_str();
    // segment.len() >= 1 here (guarded by is_empty above), so `1` is
    // always the safe slice start. The previous `1.min(segment.len())`
    // collapsed to the same value.
    let mut tokens: Vec<String> = segment[1..].to_vec();
    if !is_terminal {
        tokens.push(CHAIN_NONTERMINAL_PLACEHOLDER.to_string());
    }

    match kind {
        "hdr" => {
            let mut wrapper = HdrStepWrapper::try_parse_from(&tokens)
                .map_err(|e| format_step_parse_error("hdr", &e))?;
            if !is_terminal {
                wrapper.inner.files.clear();
            }
            Ok(ParsedStep::Hdr(wrapper.inner))
        }
        "shift" => {
            let mut wrapper = ShiftStepWrapper::try_parse_from(&tokens)
                .map_err(|e| format_step_parse_error("shift", &e))?;
            if !is_terminal {
                wrapper.inner.files.clear();
            }
            Ok(ParsedStep::Shift(wrapper.inner))
        }
        "embed" => {
            let mut wrapper = EmbedStepWrapper::try_parse_from(&tokens)
                .map_err(|e| format_step_parse_error("embed", &e))?;
            if !is_terminal {
                wrapper.inner.files.clear();
            }
            Ok(ParsedStep::Embed(wrapper.inner))
        }
        "" => Err("empty step segment (no step name)".into()),
        other => Err(format!(
            "unknown chain step '{other}' (expected: hdr, shift, embed)"
        )),
    }
}

fn take_step_files(step: &mut ParsedStep) -> Vec<PathBuf> {
    match step {
        ParsedStep::Hdr(args) => std::mem::take(&mut args.files),
        ParsedStep::Shift(args) => std::mem::take(&mut args.files),
        ParsedStep::Embed(args) => std::mem::take(&mut args.files),
    }
}

fn segment_has_output_template_token(segment: &[String]) -> bool {
    segment
        .iter()
        .any(|tok| tok == "--output-template" || tok.starts_with("--output-template="))
}

fn first_token_or<'a>(segment: &'a [String], fallback: &'a str) -> &'a str {
    segment.first().map(String::as_str).unwrap_or(fallback)
}

fn format_step_parse_error(kind: &str, err: &clap::Error) -> String {
    // clap formats errors with multiple lines and ANSI codes; surface
    // a single-line message that names the step kind so the user
    // sees which step failed at a glance.
    let body = err.to_string();
    let first_line = body
        .lines()
        .find(|l| !l.trim().is_empty())
        .unwrap_or("parse failed");
    format!("step '{kind}': {first_line}")
}

/// Collect non-blocking warnings for suspicious step orderings.
///
/// v1 catalog is conservative — only mathematically-wrong or
/// 100% no-op patterns are surfaced. Subjective "unusual" orderings
/// (HDR after embed, shift twice with cancelling offsets, etc.) do
/// NOT warn because false positives erode trust: once users learn to
/// ignore one warning, they ignore the legit ones too.
fn collect_suspicious_orderings(steps: &[ParsedStep]) -> Vec<String> {
    let mut warnings = Vec::new();
    let kinds: Vec<&str> = steps.iter().map(ParsedStep::kind_name).collect();

    // Pattern 1: HDR appearing more than once. The color transform
    // is not idempotent — applying it twice doubles the brightness
    // mapping and is almost certainly a user error.
    let hdr_count = kinds.iter().filter(|k| **k == "hdr").count();
    if hdr_count > 1 {
        warnings.push(format!(
            "warning: HDR step appears {hdr_count} times in chain; \
             color transform will be applied {hdr_count}× (likely unintended)"
        ));
    }

    // Pattern 2: shift after embed. Embed only appends a `[Fonts]`
    // section; it does not modify the `[Events]` or `[V4+ Styles]`
    // sections that shift touches. So shift-after-embed produces
    // identical content to shift-before-embed — the order has no
    // semantic effect, only obfuscates the chain.
    //
    // HDR-after-embed has the same mathematical no-op shape (embed
    // doesn't touch color tags either), but it's deliberately NOT
    // included per the locked "warn catalog stays intentionally
    // minimal" stance — empirically, shift-after-embed is the only
    // ordering users actually file as confusing. Adding HDR-after-embed
    // would erode trust without solving a real-user problem.
    for (i, kind) in kinds.iter().enumerate() {
        if *kind == "shift" && kinds[..i].contains(&"embed") {
            warnings.push(format!(
                "warning: shift step at position {} runs after an embed step; \
                 embed does not modify timing, so this shift's effect is \
                 identical to placing it before embed (consider reordering)",
                i + 1
            ));
        }
    }

    warnings
}

/// Build the chain's default output template by stacking each
/// step's natural suffix: `{name}.<suffix1>.<suffix2>...<suffixN>.ass`.
///
/// All chain default outputs use `.ass` extension regardless of input
/// — chains in practice include HDR or embed (both ASS-producing),
/// so an `.ass`-stripped default is wrong for the typical case more
/// often than the input-extension-preserving alternative. Single-step
/// chains on non-ASS input should pass `--output-template` explicitly.
fn derive_stacked_default(steps: &[ParsedStep]) -> String {
    let suffixes: Vec<&str> = steps.iter().map(ParsedStep::stack_suffix).collect();
    format!("{{name}}.{}.ass", suffixes.join("."))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn argv_of(args: &[&str]) -> Vec<String> {
        args.iter().map(|s| (*s).to_string()).collect()
    }

    // ── split_into_step_segments ──────────────────────────────

    #[test]
    fn split_single_step() {
        let argv = argv_of(&["hdr", "--eotf", "pq", "cat.ass"]);
        let segs = split_into_step_segments(&argv).unwrap();
        assert_eq!(segs.len(), 1);
        assert_eq!(segs[0], argv);
    }

    #[test]
    fn split_two_steps() {
        let argv = argv_of(&[
            "hdr", "--eotf", "pq", "+", "shift", "--offset", "+2s", "cat.ass",
        ]);
        let segs = split_into_step_segments(&argv).unwrap();
        assert_eq!(segs.len(), 2);
        assert_eq!(segs[0], argv_of(&["hdr", "--eotf", "pq"]));
        assert_eq!(segs[1], argv_of(&["shift", "--offset", "+2s", "cat.ass"]));
    }

    #[test]
    fn split_three_steps() {
        let argv = argv_of(&[
            "hdr",
            "--eotf",
            "pq",
            "+",
            "shift",
            "--offset",
            "+2s",
            "+",
            "embed",
            "--font-dir",
            "./fonts",
            "cat.ass",
        ]);
        let segs = split_into_step_segments(&argv).unwrap();
        assert_eq!(segs.len(), 3);
    }

    #[test]
    fn split_rejects_leading_plus() {
        let argv = argv_of(&["+", "hdr", "--eotf", "pq", "cat.ass"]);
        let err = split_into_step_segments(&argv).unwrap_err();
        assert!(err.contains("empty step segment"), "got: {err}");
    }

    #[test]
    fn split_rejects_trailing_plus() {
        let argv = argv_of(&["hdr", "--eotf", "pq", "cat.ass", "+"]);
        let err = split_into_step_segments(&argv).unwrap_err();
        assert!(err.contains("trailing"), "got: {err}");
    }

    #[test]
    fn split_rejects_consecutive_plus() {
        let argv = argv_of(&["hdr", "--eotf", "pq", "+", "+", "shift", "cat.ass"]);
        let err = split_into_step_segments(&argv).unwrap_err();
        assert!(err.contains("empty step segment"), "got: {err}");
    }

    // ── parse_one_step ────────────────────────────────────────

    #[test]
    fn parse_terminal_hdr_step_extracts_files() {
        let segment = argv_of(&["hdr", "--eotf", "pq", "cat.ass"]);
        let step = parse_one_step(&segment, true).unwrap();
        match step {
            ParsedStep::Hdr(args) => {
                assert_eq!(args.files, vec![PathBuf::from("cat.ass")]);
            }
            _ => panic!("expected Hdr"),
        }
    }

    #[test]
    fn parse_nonterminal_hdr_step_clears_placeholder_file() {
        let segment = argv_of(&["hdr", "--eotf", "pq"]);
        let step = parse_one_step(&segment, false).unwrap();
        match step {
            ParsedStep::Hdr(args) => {
                assert!(
                    args.files.is_empty(),
                    "non-terminal step files should be empty after clear, got: {:?}",
                    args.files
                );
            }
            _ => panic!("expected Hdr"),
        }
    }

    #[test]
    fn parse_terminal_shift_step_with_multiple_files() {
        let segment = argv_of(&["shift", "--offset", "+2s", "a.ass", "b.ass", "c.ass"]);
        let step = parse_one_step(&segment, true).unwrap();
        match step {
            ParsedStep::Shift(args) => {
                assert_eq!(args.files.len(), 3);
            }
            _ => panic!("expected Shift"),
        }
    }

    #[test]
    fn parse_terminal_embed_step_with_repeatable_font_flags() {
        let segment = argv_of(&[
            "embed",
            "--font-dir",
            "./fonts",
            "--font-dir",
            "C:/MyFonts",
            "--font-file",
            "./SmileySans.ttf",
            "cat.ass",
        ]);
        let step = parse_one_step(&segment, true).unwrap();
        match step {
            ParsedStep::Embed(args) => {
                assert_eq!(args.files, vec![PathBuf::from("cat.ass")]);
                // Other field accesses would require pub(crate) on
                // those fields too; for now the file extraction is
                // sufficient to prove the wrapper-flatten pattern works.
            }
            _ => panic!("expected Embed"),
        }
    }

    #[test]
    fn parse_unknown_kind_errors() {
        let segment = argv_of(&["bogus", "--flag"]);
        let err = parse_one_step(&segment, true).unwrap_err();
        assert!(err.contains("unknown chain step"), "got: {err}");
    }

    #[test]
    fn parse_empty_segment_errors() {
        let segment: Vec<String> = vec![];
        let err = parse_one_step(&segment, true).unwrap_err();
        assert!(err.contains("empty step segment"), "got: {err}");
    }

    // ── segment_has_output_template_token ─────────────────────

    #[test]
    fn detects_space_form_output_template() {
        let segment = argv_of(&["hdr", "--output-template", "x.ass"]);
        assert!(segment_has_output_template_token(&segment));
    }

    #[test]
    fn detects_eq_form_output_template() {
        let segment = argv_of(&["hdr", "--output-template=x.ass"]);
        assert!(segment_has_output_template_token(&segment));
    }

    #[test]
    fn does_not_detect_unrelated_flags() {
        let segment = argv_of(&["hdr", "--eotf", "pq"]);
        assert!(!segment_has_output_template_token(&segment));
    }

    // ── parse_chain_argv (full pipeline) ──────────────────────

    #[test]
    fn full_parse_two_step_chain() {
        let argv = argv_of(&[
            "hdr", "--eotf", "pq", "+", "shift", "--offset", "+2s", "cat.ass",
        ]);
        let plan = parse_chain_argv(&argv, None).unwrap();
        assert_eq!(plan.steps.len(), 2);
        assert_eq!(plan.input_files, vec![PathBuf::from("cat.ass")]);
        assert_eq!(plan.output_template, "{name}.hdr.shifted.ass");
        assert!(
            plan.warnings.is_empty(),
            "got warnings: {:?}",
            plan.warnings
        );
    }

    #[test]
    fn full_parse_user_template_overrides_default() {
        let argv = argv_of(&[
            "hdr", "--eotf", "pq", "+", "shift", "--offset", "+2s", "cat.ass",
        ]);
        let plan = parse_chain_argv(&argv, Some("{name}.processed.ass".into())).unwrap();
        assert_eq!(plan.output_template, "{name}.processed.ass");
    }

    #[test]
    fn full_parse_rejects_nonterminal_output_template() {
        // Locked rule: --output-template on a non-terminal step is a
        // parse-time error.
        let argv = argv_of(&[
            "hdr",
            "--eotf",
            "pq",
            "--output-template",
            "ignored.ass",
            "+",
            "shift",
            "--offset",
            "+2s",
            "cat.ass",
        ]);
        let err = parse_chain_argv(&argv, None).unwrap_err();
        assert!(err.contains("--output-template"), "got: {err}");
        assert!(err.contains("chain-level flag"), "got: {err}");
    }

    #[test]
    fn full_parse_rejects_terminal_step_without_files() {
        // No positional file in last segment. clap surfaces a
        // "required argument" error, prefixed by step kind.
        let argv = argv_of(&["hdr", "--eotf", "pq", "+", "shift", "--offset", "+2s"]);
        let err = parse_chain_argv(&argv, None).unwrap_err();
        assert!(err.contains("step 'shift'"), "got: {err}");
    }

    #[test]
    fn full_parse_rejects_multiple_embed_steps() {
        // v1 limitation: chain may include at most one embed step.
        let argv = argv_of(&[
            "embed",
            "--font-dir",
            "./fonts1",
            "+",
            "embed",
            "--font-dir",
            "./fonts2",
            "cat.ass",
        ]);
        let err = parse_chain_argv(&argv, None).unwrap_err();
        assert!(err.contains("at most one embed step"), "got: {err}");
    }

    #[test]
    fn full_parse_warns_on_double_hdr() {
        let argv = argv_of(&[
            "hdr", "--eotf", "pq", "+", "hdr", "--eotf", "hlg", "cat.ass",
        ]);
        let plan = parse_chain_argv(&argv, None).unwrap();
        assert_eq!(plan.warnings.len(), 1);
        assert!(plan.warnings[0].contains("HDR step appears 2 times"));
    }

    #[test]
    fn full_parse_warns_on_shift_after_embed() {
        let argv = argv_of(&[
            "embed",
            "--font-dir",
            "./fonts",
            "+",
            "shift",
            "--offset",
            "+2s",
            "cat.ass",
        ]);
        let plan = parse_chain_argv(&argv, None).unwrap();
        assert_eq!(plan.warnings.len(), 1);
        assert!(plan.warnings[0].contains("shift step at position 2"));
        assert!(plan.warnings[0].contains("after an embed step"));
    }

    #[test]
    fn full_parse_no_warning_for_hdr_shift_embed_canonical_order() {
        // The "natural" pipeline order — timing → color → resources.
        // No warnings should fire.
        let argv = argv_of(&[
            "hdr",
            "--eotf",
            "pq",
            "+",
            "shift",
            "--offset",
            "+2s",
            "+",
            "embed",
            "--font-dir",
            "./fonts",
            "cat.ass",
        ]);
        let plan = parse_chain_argv(&argv, None).unwrap();
        assert!(
            plan.warnings.is_empty(),
            "got warnings: {:?}",
            plan.warnings
        );
    }

    #[test]
    fn full_parse_no_warning_for_subjective_unusual_orderings() {
        // HDR after embed is "unusual" but not mathematically wrong
        // — the v1 catalog deliberately doesn't warn on this to
        // avoid false positives. If real users complain about it,
        // we'd add the warning later.
        let argv = argv_of(&[
            "embed",
            "--font-dir",
            "./fonts",
            "+",
            "hdr",
            "--eotf",
            "pq",
            "cat.ass",
        ]);
        let plan = parse_chain_argv(&argv, None).unwrap();
        assert!(
            plan.warnings.is_empty(),
            "got warnings: {:?}",
            plan.warnings
        );
    }

    // ── derive_stacked_default ────────────────────────────────

    #[test]
    fn stacked_default_single_step() {
        let segment = argv_of(&["hdr", "--eotf", "pq", "cat.ass"]);
        let step = parse_one_step(&segment, true).unwrap();
        let template = derive_stacked_default(&[step]);
        assert_eq!(template, "{name}.hdr.ass");
    }

    #[test]
    fn stacked_default_multi_step_preserves_order() {
        let s1 = parse_one_step(&argv_of(&["shift", "--offset", "+2s"]), false).unwrap();
        let s2 = parse_one_step(&argv_of(&["hdr", "--eotf", "pq"]), false).unwrap();
        let s3 = parse_one_step(
            &argv_of(&["embed", "--font-dir", "./fonts", "cat.ass"]),
            true,
        )
        .unwrap();
        let template = derive_stacked_default(&[s1, s2, s3]);
        assert_eq!(template, "{name}.shifted.hdr.embed.ass");
    }

    // ── to_runtime_payload (Rust → TS marshaling) ────────────

    #[test]
    fn marshal_hdr_step_matches_ts_shape() {
        let argv = argv_of(&["hdr", "--eotf", "pq", "--nits", "1000", "cat.ass"]);
        let plan = parse_chain_argv(&argv, None).unwrap();
        let payload = plan.to_runtime_payload("/tmp/cat.ass", "ass body").unwrap();
        assert_eq!(payload["inputPath"], "/tmp/cat.ass");
        assert_eq!(payload["content"], "ass body");
        assert_eq!(payload["plan"]["outputTemplate"], "{name}.hdr.ass");
        assert_eq!(payload["plan"]["steps"][0]["kind"], "hdr");
        assert_eq!(payload["plan"]["steps"][0]["params"]["eotf"], "PQ");
        assert_eq!(payload["plan"]["steps"][0]["params"]["brightness"], 1000);
    }

    #[test]
    fn marshal_shift_step_translates_offset_to_ms() {
        let argv = argv_of(&["shift", "--offset", "+2.5s", "cat.ass"]);
        let plan = parse_chain_argv(&argv, None).unwrap();
        let payload = plan.to_runtime_payload("/tmp/cat.ass", "ass body").unwrap();
        assert_eq!(payload["plan"]["steps"][0]["kind"], "shift");
        assert_eq!(payload["plan"]["steps"][0]["params"]["offsetMs"], 2500);
        assert!(payload["plan"]["steps"][0]["params"]["thresholdMs"].is_null());
    }

    #[test]
    fn marshal_shift_with_threshold_translates_after_to_ms() {
        let argv = argv_of(&[
            "shift", "--offset", "-500ms", "--after", "00:10:00", "cat.ass",
        ]);
        let plan = parse_chain_argv(&argv, None).unwrap();
        let payload = plan.to_runtime_payload("/tmp/cat.ass", "ass body").unwrap();
        assert_eq!(payload["plan"]["steps"][0]["params"]["offsetMs"], -500);
        // 00:10:00 = 600_000 ms.
        assert_eq!(
            payload["plan"]["steps"][0]["params"]["thresholdMs"],
            600_000
        );
    }

    #[test]
    fn marshal_embed_step_renames_to_camel_case() {
        let argv = argv_of(&[
            "embed",
            "--font-dir",
            "./fonts",
            "--font-file",
            "./SmileySans.ttf",
            "--no-system-fonts",
            "--on-missing",
            "fail",
            "cat.ass",
        ]);
        let plan = parse_chain_argv(&argv, None).unwrap();
        let payload = plan.to_runtime_payload("/tmp/cat.ass", "ass body").unwrap();
        let params = &payload["plan"]["steps"][0]["params"];
        assert_eq!(params["fontDirs"][0], "./fonts");
        assert_eq!(params["fontFiles"][0], "./SmileySans.ttf");
        assert_eq!(params["noSystemFonts"], true);
        assert_eq!(params["onMissing"], "fail");
    }

    #[test]
    fn marshal_two_step_chain_preserves_order() {
        let argv = argv_of(&[
            "hdr", "--eotf", "pq", "+", "shift", "--offset", "+2s", "cat.ass",
        ]);
        let plan = parse_chain_argv(&argv, None).unwrap();
        let payload = plan.to_runtime_payload("/tmp/cat.ass", "ass body").unwrap();
        let steps = payload["plan"]["steps"].as_array().unwrap();
        assert_eq!(steps.len(), 2);
        assert_eq!(steps[0]["kind"], "hdr");
        assert_eq!(steps[1]["kind"], "shift");
    }

    #[test]
    fn marshal_does_not_include_rust_only_fields() {
        // input_files and warnings are Rust-side concerns; the TS
        // runtime should not see them.
        let argv = argv_of(&[
            "hdr", "--eotf", "pq", "+", "hdr", "--eotf", "hlg", "cat.ass",
        ]);
        let plan = parse_chain_argv(&argv, None).unwrap();
        // Confirm warnings exist Rust-side (HDR×2 fires).
        assert_eq!(plan.warnings.len(), 1);
        let payload = plan.to_runtime_payload("/tmp/cat.ass", "ass body").unwrap();
        assert!(payload["plan"].get("inputFiles").is_none());
        assert!(payload["plan"].get("warnings").is_none());
    }

    #[test]
    fn stacked_default_canonical_three_step() {
        let s1 = parse_one_step(&argv_of(&["hdr", "--eotf", "pq"]), false).unwrap();
        let s2 = parse_one_step(&argv_of(&["shift", "--offset", "+2s"]), false).unwrap();
        let s3 = parse_one_step(
            &argv_of(&["embed", "--font-dir", "./fonts", "cat.ass"]),
            true,
        )
        .unwrap();
        let template = derive_stacked_default(&[s1, s2, s3]);
        assert_eq!(template, "{name}.hdr.shifted.embed.ass");
    }
}
