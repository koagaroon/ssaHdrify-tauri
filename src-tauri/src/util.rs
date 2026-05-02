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

/// Maximum length for any path string accepted from the IPC boundary,
/// counted as UTF-8 BYTES not chars (`str::len()` is O(1) byte length).
/// Windows path APIs are bounded by `MAX_PATH` (260) for legacy callers
/// and `MAX_PATH_LONG` (≈32767 UTF-16 code units, ≈65k UTF-8 bytes for
/// non-ASCII) when the extended-length `\\?\` prefix is used; 4096
/// bytes is generous against any real user picker (≈1300 CJK chars)
/// while still catching obviously hostile inputs early.
pub const MAX_IPC_PATH_LEN: usize = 4096;

/// Validate a path string just received from the IPC boundary. Rejects
/// empty, oversize, and any path containing characters known to break
/// downstream parsers: ASCII / Unicode control characters (covered by
/// `char::is_control()`, which spans Cc — U+0000..=U+001F, U+007F..=
/// U+009F, including U+0085 NEXT LINE) plus U+2028 LINE SEPARATOR and
/// U+2029 PARAGRAPH SEPARATOR. The Zl/Zp pair is added explicitly
/// because Rust's `is_control()` does NOT include them — several path
/// libraries treat them ambiguously across platforms.
///
/// Unicode noncharacters (U+FFFE, U+FFFF, U+FDD0..=U+FDEF) are
/// intentionally not rejected — Windows file APIs already error with
/// `ERROR_INVALID_NAME` on path components containing them, and
/// surrogates can't appear in a Rust `String` by construction.
///
/// `label` is used in the returned error string so callers can identify
/// which input was bad ("Directory path must be 1-4096 characters",
/// etc.). Keep this the SINGLE definition; each module previously had
/// its own copy and they drifted.
pub fn validate_ipc_path(path: &str, label: &str) -> Result<(), String> {
    if path.is_empty() || path.len() > MAX_IPC_PATH_LEN {
        return Err(format!(
            "{label} path must be 1-{MAX_IPC_PATH_LEN} characters"
        ));
    }
    if path
        .chars()
        .any(|c| c.is_control() || matches!(c, '\u{2028}' | '\u{2029}'))
    {
        return Err(format!("{label} path contains invalid characters"));
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
