//! CLI path normalization, output reservation, terminal display sanitization,
//! decode/oversized warnings, and safe-I/O wrappers.

use super::*;
use std::collections::HashSet;
use std::fs;
use std::path::{Component, Path, PathBuf};
use unicode_normalization::UnicodeNormalization;

pub(super) fn normalize_global_paths(globals: &mut GlobalOptions) -> Result<(), String> {
    globals.output_dir = globals
        .output_dir
        .as_deref()
        .map(absolute_path)
        .transpose()?;
    globals.cache_file = globals
        .cache_file
        .as_deref()
        .map(absolute_path)
        .transpose()?;
    Ok(())
}

#[derive(Clone, Copy)]
pub(super) enum OversizedCaptionHandling {
    Dropped,
    Preserved,
    FormatDependent,
}

/// Build a bilingual oversized-caption warning for human and JSON output.
/// The caller supplies what happened because structure-preserving ASS/VTT
/// shifts retain the original cue while rebuilding operations omit it.
pub(super) fn format_oversized_caption_warning(
    globals: &GlobalOptions,
    skipped_count: usize,
    input: &str,
    handling: OversizedCaptionHandling,
) -> Option<Vec<String>> {
    if skipped_count == 0 {
        return None;
    }
    // `input` is a raw operational path; sanitize before embedding
    // in the message body so downstream stderr / println emission
    // can't be corrupted by control / BiDi chars from a crafted
    // argv.
    let input_disp = sanitize_for_display(input);
    let message = match handling {
        OversizedCaptionHandling::Dropped => localize(
            globals,
            format!(
                "Dropped {skipped_count} oversized caption(s) from {input_disp}: \
                 text exceeded the 64 KB per-caption processing cap"
            ),
            format!(
                "已丢弃 {skipped_count} 条超大字幕（来自 {input_disp}）：单条文本超过 64 KB 的处理上限"
            ),
        ),
        OversizedCaptionHandling::Preserved => localize(
            globals,
            format!(
                "Left {skipped_count} oversized caption(s) in {input_disp} unchanged: \
                 text exceeded the 64 KB per-caption processing cap, so timing was not adjusted"
            ),
            format!(
                "{input_disp} 中有 {skipped_count} 条超大字幕超过单条 64 KB 的处理上限，已保留原内容且未调整时间"
            ),
        ),
        OversizedCaptionHandling::FormatDependent => localize(
            globals,
            format!(
                "Skipped processing {skipped_count} oversized caption(s) in {input_disp}: \
                 text exceeded the 64 KB per-caption cap; depending on the chain step and format, \
                 original content was left unchanged or omitted"
            ),
            format!(
                "{input_disp} 中有 {skipped_count} 条超大字幕超过单条 64 KB 的处理上限而未处理；根据链式步骤和格式，原内容会保留不变或被省略"
            ),
        ),
    };
    Some(vec![message])
}

pub(super) fn format_decode_warnings(
    globals: &GlobalOptions,
    result: &app_lib::encoding::ReadTextResult,
) -> Option<Vec<String>> {
    let mut warnings = Vec::new();
    if result.inferred_without_bom {
        warnings.push(localize(
            globals,
            format!(
                "Detected BOM-less {} from its byte pattern. This is a best-effort inference; verify the output.",
                result.encoding_id
            ),
            format!(
                "已根据字节模式推测该文件使用无 BOM 的 {} 编码。此结果并非完全确定，请核对输出。",
                result.encoding_id
            ),
        ));
    }
    if result.lossy {
        warnings.push(localize(
            globals,
            format!(
                "Decoding {} replaced malformed source bytes with replacement characters (U+FFFD). Some text may be damaged; verify the output.",
                result.encoding_id
            ),
            format!(
                "解码 {} 时已将格式错误的源字节替换为替代字符（U+FFFD）。部分文本可能已损坏，请核对输出。",
                result.encoding_id
            ),
        ));
    }
    if warnings.is_empty() {
        None
    } else {
        Some(warnings)
    }
}

pub(super) struct OutputReservation<'a> {
    seen_outputs: &'a mut HashSet<String>,
    key: String,
    keep: bool,
}

impl<'a> OutputReservation<'a> {
    pub(super) fn new(seen_outputs: &'a mut HashSet<String>, key: String) -> Self {
        Self {
            seen_outputs,
            key,
            keep: false,
        }
    }

    pub(super) fn keep(&mut self) {
        self.keep = true;
    }
}

impl Drop for OutputReservation<'_> {
    fn drop(&mut self) {
        if !self.keep {
            self.seen_outputs.remove(&self.key);
        }
    }
}

// Reserve a standalone output until the caller either plans or writes it.
// Any Skip or Failed return drops the guard and releases the key, so a later
// input can retry the same path. Boxing the large error keeps Result clippy-
// clean without obscuring the success-path lifetime.
pub(super) fn reserve_output<'a>(
    globals: &GlobalOptions,
    input_path: &Path,
    output_path: &Path,
    output: &str,
    encoding: Option<&str>,
    seen_outputs: &'a mut HashSet<String>,
    selected_inputs: &HashSet<String>,
) -> Result<OutputReservation<'a>, Box<FileReport>> {
    let cloned_encoding = || encoding.map(|s| s.to_string());
    if output_matches_selected_input(output_path, selected_inputs) {
        return Err(Box::new(failed_report(
            input_path,
            Some(output.to_string()),
            cloned_encoding(),
            "output path matches a selected input file; choose a different --output-template or --output-dir".to_string(),
        )));
    }
    let key = normalize_output_key(output_path);
    if !seen_outputs.insert(key.clone()) {
        return Err(Box::new(failed_report(
            input_path,
            Some(output.to_string()),
            cloned_encoding(),
            "duplicate output path in planned batch".to_string(),
        )));
    }
    let reservation = OutputReservation {
        seen_outputs,
        key,
        keep: false,
    };
    if !globals.overwrite && output_path_exists(globals, output_path) {
        return Err(Box::new(skipped_report(
            input_path,
            Some(output.to_string()),
            cloned_encoding(),
            "output exists; pass --overwrite to replace it".to_string(),
        )));
    }
    Ok(reservation)
}

pub(super) fn has_ass_extension(path: &Path) -> bool {
    path.extension()
        .and_then(|ext| ext.to_str())
        .map(|ext| matches!(ext.to_ascii_lowercase().as_str(), "ass" | "ssa"))
        .unwrap_or(false)
}

// Trust model: CLI path arguments are user-controlled. We normalize
// them to absolute form here but DO NOT canonicalize (which
// would resolve symlinks). On Windows, fs::canonicalize returns the
// `\\?\C:\...` extended-path form which surprises downstream tools.
// Filesystem safety checks resolve aliases at their own boundaries.
pub(super) fn absolute_path(path: &Path) -> Result<PathBuf, String> {
    if !path.is_absolute() && matches!(path.components().next(), Some(Component::Prefix(_))) {
        return Err(format!(
            "drive-relative paths are ambiguous; use an absolute path such as C:\\folder\\file or a normal relative path: {}",
            sanitize_for_display(&path.display().to_string())
        ));
    }

    let joined = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .map_err(|err| format!("failed to resolve current directory: {err}"))?
            .join(path)
    };

    // CLI argv is explicitly user-controlled and may naturally contain
    // `.` / `..`. Fold those components before the shared strict IPC path
    // validator sees the path. This is lexical on purpose: output targets
    // may not exist yet, and canonicalization would follow user-managed
    // symlinks plus introduce Windows `\\?\` paths. GUI IPC remains strict
    // and never calls this helper.
    let mut normalized = PathBuf::new();
    for component in joined.components() {
        match component {
            Component::Prefix(prefix) => normalized.push(prefix.as_os_str()),
            Component::RootDir => normalized.push(component.as_os_str()),
            Component::CurDir => {}
            Component::ParentDir => {
                if normalized.file_name().is_some() {
                    normalized.pop();
                } else if !normalized.has_root() {
                    return Err(format!(
                        "path escapes its filesystem root: {}",
                        sanitize_for_display(&joined.display().to_string())
                    ));
                }
            }
            Component::Normal(segment) => normalized.push(segment),
        }
    }

    if !normalized.is_absolute() {
        return Err(format!(
            "failed to resolve an absolute path from: {}",
            sanitize_for_display(&joined.display().to_string())
        ));
    }
    Ok(normalized)
}

// Keep ordinary and extended-length Windows paths within their existing limits.
pub(super) const RELOCATED_PATH_MAX_LEN: usize = 259;
pub(super) const RELOCATED_LONG_PATH_MAX_LEN: usize = 32766;

pub(super) fn relocate_output_path(
    path: &str,
    output_dir: Option<&Path>,
) -> Result<PathBuf, String> {
    let path = PathBuf::from(path);
    let relocated = if let Some(output_dir) = output_dir {
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
        output_dir.join(file_name)
    } else {
        path
    };

    // Re-validate length on the relocated path. The JS validators saw
    // the pre-relocation path and signed off; relocation can grow it
    // beyond MAX_PATH if --output-dir itself is long.
    //
    // Count UTF-16 code units (NOT UTF-8 bytes), matching Windows
    // MAX_PATH semantics. CJK characters take 3 bytes in UTF-8 but
    // typically 1 UTF-16 code unit, so a `display.len()` (byte count)
    // would over-restrict CJK paths the OS would happily accept.
    let display = relocated.to_string_lossy();
    app_lib::util::validate_output_path_shape(&relocated, "Output")?;
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
pub(super) fn output_path_exists(globals: &GlobalOptions, path: &Path) -> bool {
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
//   - write / copy use `clear_existing_destination` before replacement;
//     rename uses non-mutating destination inspection so an OS rename
//     failure cannot erase the prior destination. Both reject reparse points.
//   - copy / rename additionally enforce `reject_reparse_source` +
//     `reject_same_canonical_path` (the case-only NTFS self-overwrite
//     trap closed for the GUI) + the late re-check before
//     `File::open` (copy) / `fs::rename` (rename) for the dst-side
//     reparse swap window.
// Routing through the single source of truth means future safe_io
// fixes auto-propagate here instead of needing parallel fixes.
//
// The `_globals` parameter is preserved on each function for caller-
// site convention (every other CLI fs-helper takes globals); safe_io
// owns all decisions internally. Higher-level callers still use
// `output_path_exists` (preserved as a standalone CLI helper above)
// for the cheap-first skip-when-exists check + `--quiet`-respecting
// stderr WARN on stat failure. safe_io's destination inspection/removal
// helpers provide the second-layer fail-shut on the race between the higher-
// level check and the final filesystem operation. Error wording at the safe_io
// boundary bubbles through `failed_report` and is sanitized at the print
// boundary by `emit_file_report`'s `sanitize_for_display`.
pub(super) fn write_output(
    _globals: &GlobalOptions,
    path: &Path,
    content: &str,
    overwrite: bool,
) -> Result<(), String> {
    let path_str = path.to_string_lossy();
    app_lib::safe_io::safe_write_text_file_inner(&path_str, content, overwrite, |_| true)
}

pub(super) fn copy_file_output(
    _globals: &GlobalOptions,
    input: &Path,
    output: &Path,
    overwrite: bool,
) -> Result<(), String> {
    let src = input.to_string_lossy();
    let dst = output.to_string_lossy();
    app_lib::safe_io::safe_copy_file_inner(&src, &dst, overwrite, |_| true)
}

pub(super) fn rename_file_output(
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
pub(super) fn s_if(n: usize) -> &'static str {
    if n == 1 {
        ""
    } else {
        "s"
    }
}

pub(super) fn display_path(path: &Path) -> String {
    // Pure formatter: PathBuf → String + Windows slash normalization.
    // The returned string is canonical for OS-level filesystem
    // operations — `read_text_detect_encoding_inner`, engine path
    // resolution, FileReport.input field. Character filtering MUST
    // NOT happen here: sanitizing destructively at this layer would
    // make `evil\u{202e}.ass` silently resolve to a sibling
    // `evil.ass`, picking the wrong file at read / write time.
    // Display-time sanitization belongs at the print sites
    // (`emit_file_report`, `format_oversized_caption_warning`, etc.)
    // via `sanitize_for_display`.
    let raw = path.to_string_lossy().into_owned();
    raw.replace('/', "\\")
}

/// Strip control + BiDi + zero-width characters from a path string
/// for safe interpolation into human-readable output (stderr/println).
/// The same filter set as `validate_ipc_path` rejects, but here we
/// strip rather than refuse: argv-supplied filenames may
/// contain invisible Unicode characters, so filter only the display text
/// without changing which filesystem path an operation addresses.
///
/// JSON output doesn't need this helper — `serde_json` escapes control
/// characters as `\uXXXX` automatically, so machine-parseable readers
/// see the original bytes intact. Apply this helper ONLY at the
/// `eprintln!` / `println!` formatting sites where a raw control char
/// would corrupt the terminal (ANSI escapes, U+202E direction reversal).
pub(super) fn sanitize_for_display(s: &str) -> String {
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

pub(super) fn selected_input_keys(inputs: &[PathBuf]) -> HashSet<String> {
    let mut keys = HashSet::new();
    for input in inputs {
        if let Ok(absolute) = absolute_path(input) {
            keys.insert(normalize_output_key(&absolute));
            if let Ok(canonical) = absolute.canonicalize() {
                keys.insert(normalize_output_key(&canonical));
            }
        }
    }
    keys
}

pub(super) fn output_matches_selected_input(
    output: &Path,
    selected_inputs: &HashSet<String>,
) -> bool {
    selected_inputs.contains(&normalize_output_key(output))
        || output
            .canonicalize()
            .is_ok_and(|canonical| selected_inputs.contains(&normalize_output_key(&canonical)))
}

pub(super) fn normalize_output_key(path: &Path) -> String {
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

pub(super) fn normalize_output_key_after_strip(s: &str) -> String {
    let normalized = s.replace('\\', "/").nfc().collect::<String>();
    // Case aliases share one output key on Windows.
    normalized.to_lowercase()
}
