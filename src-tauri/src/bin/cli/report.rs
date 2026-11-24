//! Structured JSON/human reports, Font QA summaries, and diagnostic output.

use super::*;
use serde::Serialize;
use std::collections::HashSet;
use std::path::Path;
use std::process::ExitCode;

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct CommandReport {
    pub(super) command: &'static str,
    pub(super) written: usize,
    pub(super) planned: usize,
    pub(super) skipped: usize,
    pub(super) failed: usize,
    pub(super) results: Vec<FileReport>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) diagnostics: Option<CommandDiagnostics>,
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
    pub(super) aborted_by_fail_fast: bool,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct FileReport {
    pub(super) input: String,
    pub(super) output: Option<String>,
    pub(super) encoding: Option<String>,
    pub(super) status: FileStatus,
    pub(super) error: Option<String>,
    /// Non-fatal warnings carried alongside a successful or planned
    /// result. Currently used by embed under `--on-missing warn` to
    /// surface unresolved / failed-to-subset fonts to JSON consumers
    /// (without warnings here, JSON callers couldn't distinguish
    /// "all fonts embedded" from "embedded what we found").
    /// `serde(skip_serializing_if)` keeps the JSON shape backward-
    /// compatible: the field is absent unless something actually
    /// warned.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) warnings: Option<Vec<String>>,
}

#[derive(Debug, Serialize, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub(super) enum FileStatus {
    Written,
    Planned,
    Diagnosed,
    Skipped,
    Failed,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct CommandDiagnostics {
    pub(super) mode: DiagnoseMode,
    pub(super) files_with_warnings: usize,
    pub(super) warning_count: usize,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub(super) notes: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub(super) files: Vec<FileDiagnostic>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) cache: Option<CacheDiagnostic>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) qa: Option<FontQaSummary>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub(super) fonts: Vec<FontDiagnostic>,
}

#[derive(Debug, Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub(super) struct FontQaSummary {
    pub(super) status: FontQaStatus,
    pub(super) file_count: usize,
    pub(super) failed_file_count: usize,
    pub(super) font_reference_count: usize,
    pub(super) resolved_count: usize,
    pub(super) missing_count: usize,
    pub(super) error_count: usize,
    pub(super) subset_checked_count: usize,
    pub(super) subset_ok_count: usize,
    pub(super) subset_failed_count: usize,
    pub(super) subset_skipped_count: usize,
}

#[derive(Debug, Serialize, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub(super) enum FontQaStatus {
    Complete,
    Incomplete,
    Blocked,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct FileDiagnostic {
    pub(super) input: String,
    pub(super) output: Option<String>,
    pub(super) encoding: Option<String>,
    pub(super) status: FileStatus,
    pub(super) error: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub(super) warnings: Vec<String>,
}

#[derive(Debug, Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub(super) struct CacheDiagnostic {
    pub(super) path: Option<String>,
    pub(super) status: CacheDiagnosticStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) message: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) found_schema: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) expected_schema: Option<i64>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub(super) modified_folders: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub(super) removed_folders: Vec<String>,
    /// Scope-aware source identities for new JSON consumers. The legacy
    /// `*Folders` arrays remain raw path strings for backward compatibility.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub(super) modified_sources: Vec<app_lib::font_cache::CacheSourceKey>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub(super) removed_sources: Vec<app_lib::font_cache::CacheSourceKey>,
}

#[derive(Debug, Serialize, Clone, Copy)]
#[serde(rename_all = "kebab-case")]
pub(super) enum CacheDiagnosticStatus {
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
    pub(super) fn new(
        path: Option<String>,
        status: CacheDiagnosticStatus,
        message: Option<String>,
    ) -> Self {
        Self {
            path,
            status,
            message,
            found_schema: None,
            expected_schema: None,
            modified_folders: Vec::new(),
            removed_folders: Vec::new(),
            modified_sources: Vec::new(),
            removed_sources: Vec::new(),
        }
    }
}

#[derive(Debug, Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub(super) struct FontDiagnostic {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) file: Option<String>,
    pub(super) label: String,
    pub(super) family: String,
    pub(super) embedded_font_name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) requested_embedded_font_name: Option<String>,
    pub(super) bold: bool,
    pub(super) italic: bool,
    pub(super) glyph_count: usize,
    pub(super) result: FontResolutionResult,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) index: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) subset_check: Option<FontSubsetCheckDiagnostic>,
    pub(super) tiers: Vec<FontTierDiagnostic>,
}

#[derive(Debug, Serialize, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub(super) enum FontResolutionResult {
    Resolved,
    Missing,
    Error,
}

#[derive(Debug, Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub(super) struct FontSubsetCheckDiagnostic {
    pub(super) status: FontSubsetCheckStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) bytes: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) error: Option<String>,
}

#[derive(Debug, Serialize, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub(super) enum FontSubsetCheckStatus {
    Ok,
    Failed,
    Skipped,
}

#[derive(Debug, Default)]
pub(super) struct DiagnosticSubsetBudget {
    pub(super) calls: usize,
    pub(super) bytes: usize,
    pub(super) exhausted_reason: Option<String>,
}

impl DiagnosticSubsetBudget {
    pub(super) fn skipped(reason: String) -> FontSubsetCheckDiagnostic {
        FontSubsetCheckDiagnostic {
            status: FontSubsetCheckStatus::Skipped,
            bytes: None,
            error: Some(reason),
        }
    }

    pub(super) fn exhausted_check(&self) -> Option<FontSubsetCheckDiagnostic> {
        self.exhausted_reason
            .as_ref()
            .map(|reason| Self::skipped(reason.clone()))
    }

    pub(super) fn begin_call(&mut self) -> Option<FontSubsetCheckDiagnostic> {
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

    pub(super) fn finish_bytes(&mut self, len: usize) {
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
pub(super) struct FontTierDiagnostic {
    pub(super) tier: FontResolveTier,
    pub(super) status: FontTierStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) index: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) reason: Option<String>,
}

#[derive(Debug, Serialize, Clone, Copy)]
#[serde(rename_all = "kebab-case")]
pub(super) enum FontResolveTier {
    Local,
    Cache,
    System,
}

#[derive(Debug, Serialize, Clone, Copy)]
#[serde(rename_all = "kebab-case")]
pub(super) enum FontTierStatus {
    Hit,
    Miss,
    Disabled,
    Unavailable,
    Error,
}

impl FontDiagnostic {
    pub(super) fn new(font: &engine::FontEmbedUsage) -> Self {
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

    pub(super) fn add_tier(
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

    pub(super) fn mark_resolved(&mut self, path: String, index: u32) {
        self.result = FontResolutionResult::Resolved;
        self.path = Some(path);
        self.index = Some(index);
        self.error = None;
    }

    pub(super) fn mark_error(&mut self, error: String) {
        self.result = FontResolutionResult::Error;
        self.error = Some(error);
    }

    pub(super) fn mark_effective_embedded_font_name(&mut self, effective_name: &str) {
        if self.embedded_font_name != effective_name {
            self.requested_embedded_font_name = Some(self.embedded_font_name.clone());
            self.embedded_font_name = effective_name.to_string();
        }
    }
}

impl CommandReport {
    pub(super) fn new(command: &'static str) -> Self {
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

    pub(super) fn push(&mut self, result: FileReport) {
        match result.status {
            FileStatus::Written => self.written += 1,
            FileStatus::Planned => self.planned += 1,
            FileStatus::Diagnosed => self.planned += 1,
            FileStatus::Skipped => self.skipped += 1,
            FileStatus::Failed => self.failed += 1,
        }
        self.results.push(result);
    }

    pub(super) fn mark_fail_fast_abort(&mut self) {
        self.aborted_by_fail_fast = true;
    }

    pub(super) fn exit_code(&self) -> ExitCode {
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
pub(super) fn emit_fail_fast_abort_notice(globals: &GlobalOptions, remaining: usize) {
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

pub(super) fn emit_inert_cache_flags_notice(globals: &GlobalOptions, command: &str) {
    if globals.quiet {
        return;
    }

    let mut inert = Vec::new();
    if globals.no_cache {
        inert.push("--no-cache");
    }
    if globals.cache_file.is_some() {
        inert.push("--cache-file");
    }
    if inert.is_empty() {
        return;
    }

    let flags = inert.join(", ");
    eprintln!(
        "{}",
        localize(
            globals,
            format!(
                "ℹ {command} does not use the persistent font cache; these flags have no effect here: {flags}"
            ),
            format!("ℹ {command} 不使用持久字体缓存；以下参数在此无效：{flags}"),
        )
    );
}

pub(super) fn emit_standalone_font_diagnostics(
    globals: &GlobalOptions,
    diagnostics: &CommandDiagnostics,
) {
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

pub(super) fn finish_command_report(
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

pub(super) fn build_command_diagnostics(
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
        Some(build_font_qa_summary(report, &fonts))
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

pub(super) fn build_font_qa_summary(
    report: &CommandReport,
    fonts: &[FontDiagnostic],
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
    } else if missing_count > 0 || subset_skipped_count > 0 {
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

pub(super) fn warning_counts(report: &CommandReport) -> (usize, usize, usize) {
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

pub(super) fn unresolved_font_count(diagnostics: &CommandDiagnostics) -> usize {
    diagnostics
        .fonts
        .iter()
        .filter(|font| font.result != FontResolutionResult::Resolved)
        .count()
}

pub(super) fn format_font_qa_summary(globals: &GlobalOptions, qa: &FontQaSummary) -> String {
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

pub(super) fn diagnostic_next_actions(
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
                    "Build a font cache with `ssahdrify-cli refresh-fonts --font-dir <DIR>...` for flat folders or `--recursive-font-dir <DIR>...` for library trees.",
                    "用 `ssahdrify-cli refresh-fonts --font-dir <DIR>...` 建立浅层目录缓存，或对字体库树使用 `--recursive-font-dir <DIR>...`。",
                );
            }
            CacheDiagnosticStatus::Drift => {
                push_action(
                    "cache-refresh",
                    "Run `ssahdrify-cli refresh-fonts` with each changed source's original `--font-dir` or `--recursive-font-dir` scope.",
                    "字体源变化后，使用各源原本的 `--font-dir` 或 `--recursive-font-dir` 范围运行 `ssahdrify-cli refresh-fonts`。",
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
            "If you expected these fonts to embed, pass `--font-dir <DIR>` for a flat folder, `--recursive-font-dir <DIR>` for a library tree, or `--font-file <FILE>` for one file.",
            "如果你希望这些字体被嵌入，请为浅层目录传入 `--font-dir <DIR>`、为字体库树传入 `--recursive-font-dir <DIR>`，或为单个文件传入 `--font-file <FILE>`。",
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

pub(super) fn emit_attached_diagnostics(
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

pub(super) fn emit_report_summary(
    globals: &GlobalOptions,
    report: &CommandReport,
) -> Result<(), String> {
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

pub(super) fn emit_file_report(globals: &GlobalOptions, result: &FileReport) {
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

pub(super) fn failed_report(
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

pub(super) fn skipped_report(
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

pub(super) fn planned_report(
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
