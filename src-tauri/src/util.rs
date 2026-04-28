//! Cross-module helpers shared between IPC commands.
//!
//! Currently houses `is_reparse_point`, used by both `dropzone` and
//! `encoding` to refuse symlinks / Windows junctions / OneDrive
//! placeholders before reading or expanding the path. The function was
//! duplicated in both modules until two stable callers + identical
//! definitions justified the lift to a shared util.

use std::path::Path;

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
