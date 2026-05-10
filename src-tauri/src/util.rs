//! Cross-module helpers shared between IPC commands.
//!
//! Currently houses `is_reparse_point`, used by both `dropzone` and
//! `encoding` to refuse symlinks / Windows junctions / OneDrive
//! placeholders before reading or expanding the path, plus
//! `validate_ipc_path`, the canonical length+character validation for
//! any path coming off the IPC boundary. Both lifted here once a third
//! caller appeared (the streaming font scan), so the previous
//! per-module copies could no longer drift.

use std::path::Path;

/// Cap on the path-list length any IPC command will accept in a single
/// call. Two callers today: `dropzone::expand_dropped_paths` and
/// `fonts::scan_font_files` / `fonts::preflight_font_files`. The OS file
/// picker can't realistically deliver more than a handful of thousand
/// files in one selection, so 1000 is generous for the user-facing flow
/// while bounding worst-case CPU/IO if a future code path or
/// compromised frontend supplies a huge vector. Lifted here once a
/// second module needed the same number — keeping a single definition
/// avoids the two callers drifting independently.
pub const MAX_INPUT_PATHS: usize = 1000;

/// Maximum length for any path string accepted from the IPC boundary,
/// counted as UTF-8 BYTES not chars (`str::len()` is O(1) byte length).
/// Windows path APIs are bounded by `MAX_PATH` (260) for legacy callers
/// and `MAX_PATH_LONG` (≈32767 UTF-16 code units, ≈65k UTF-8 bytes for
/// non-ASCII) when the extended-length `\\?\` prefix is used; 4096
/// bytes is generous against any real user picker (≈1300 CJK chars)
/// while still catching obviously hostile inputs early.
pub const MAX_IPC_PATH_LEN: usize = 4096;

/// Validate a path string just received from the IPC boundary. Rejects:
///
/// 1. Empty, or longer than `MAX_IPC_PATH_LEN` bytes.
/// 2. Any character matched by `char::is_control()` — Unicode general
///    category Cc, which spans U+0000..=U+001F (C0 controls including
///    NUL/CR/LF/HT) and U+007F..=U+009F (C1 controls including NEL
///    U+0085).
/// 3. U+2028 LINE SEPARATOR (Zl) and U+2029 PARAGRAPH SEPARATOR (Zp).
///    These are NOT in Cc, so `is_control()` doesn't catch them — added
///    explicitly because several Rust path libraries treat them
///    ambiguously across platforms (some normalize as line terminators,
///    others pass through verbatim).
/// 4. Zero-width formatting characters U+200B / U+200C / U+200D /
///    U+FEFF. Windows file APIs accept them, so two visually-identical
///    filenames `foo.ass` and `foo\u{200B}.ass` resolve to different
///    paths on disk and bypass `normalizeOutputKey`'s within-batch
///    dedup (it does NFC + slash + lowercase but not zero-width strip).
///    Reject at this boundary so they never reach IPC consumers.
///
/// Unicode noncharacters (U+FFFE, U+FFFF, U+FDD0..=U+FDEF) are
/// intentionally not rejected — Windows file APIs already error with
/// `ERROR_INVALID_NAME` on path components containing them, and
/// surrogates can't appear in a Rust `String` by construction.
///
/// `label` is used in the returned error string so callers can identify
/// which input was bad ("Directory path must be 1-4096 bytes", etc.).
/// Keep this the SINGLE definition; each module previously had its own
/// copy and they drifted.
pub fn validate_ipc_path(path: &str, label: &str) -> Result<(), String> {
    if path.is_empty() || path.len() > MAX_IPC_PATH_LEN {
        return Err(format!("{label} path must be 1-{MAX_IPC_PATH_LEN} bytes"));
    }
    if path.chars().any(|c| {
        c.is_control()
            || matches!(
                c,
                // Line / paragraph separators (Unicode line breaks).
                '\u{2028}' | '\u{2029}'
                // Zero-width joiners / non-joiners / spaces /
                // word joiner / mongolian vowel separator / BOM —
                // all invisible characters that can smuggle past
                // visual review and break path comparisons.
                | '\u{200B}' | '\u{200C}' | '\u{200D}'
                | '\u{2060}' | '\u{180E}' | '\u{FEFF}'
            )
    }) {
        return Err(format!("{label} path contains invalid characters"));
    }
    // Reject Windows DOS device namespaces (\\.\<device>) and the
    // \\?\GLOBALROOT\… kernel-object form. These open raw device
    // handles or kernel-namespace paths — never legitimate user data
    // targets. The legitimate Win32 long-path prefixes (\\?\C:\… and
    // \\?\UNC\server\share\…) remain allowed since they map onto
    // ordinary filesystem paths and font scanning + drag-drop both
    // produce them through canonicalize().
    let lowered = path.to_ascii_lowercase();
    let is_dos_device = lowered.starts_with("\\\\.\\") || lowered.starts_with("//./");
    let is_globalroot =
        lowered.starts_with("\\\\?\\globalroot\\") || lowered.starts_with("//?/globalroot/");
    if is_dos_device || is_globalroot {
        return Err(format!("{label} path uses a reserved device namespace"));
    }
    Ok(())
}

/// Detect symlinks AND Windows junctions / OneDrive placeholders.
///
/// On Windows, Rust's `is_symlink()` returns `false` for junctions
/// (IO_REPARSE_TAG_MOUNT_POINT), so a junction-based bypass slips past
/// a naive symlink check. This helper inspects the raw
/// `FILE_ATTRIBUTE_REPARSE_POINT` bit on Windows; on non-Windows
/// platforms it falls back to the standard `is_symlink()` check, which
/// is sufficient there.
///
/// Callers: `dropzone::expand_dropped_paths` (skip top-level dropped
/// reparse points), `dropzone::walk_one_level` (skip reparse points
/// inside walked folders), `encoding::read_text_detect_encoding`
/// (refuse to read text from a reparse point), `fonts::scan_directory_inner`
/// (skip reparse-point siblings during font scan), and
/// `fonts::preflight_directory_inner` (skip reparse points when
/// previewing folder size). All callers want the same "is this a
/// symlink-like thing the OS would chase" answer; keep this helper as
/// the single source of truth.
#[cfg(windows)]
pub fn is_reparse_point(path: &Path) -> bool {
    use std::os::windows::fs::MetadataExt;
    const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x0400;
    std::fs::symlink_metadata(path)
        .map(|m| m.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0)
        .unwrap_or(false)
}

#[cfg(not(windows))]
pub fn is_reparse_point(path: &Path) -> bool {
    std::fs::symlink_metadata(path)
        .map(|m| m.file_type().is_symlink())
        .unwrap_or(false)
}
