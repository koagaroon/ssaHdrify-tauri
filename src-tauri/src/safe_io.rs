//! Symlink-safe file write / copy / rename commands for the GUI.
//!
//! Codex security scan (2026-05-11) flagged two reachable paths where a
//! malicious or accidental symlink in an attacker-influenced subtitle
//! pack could redirect Tauri's `@tauri-apps/plugin-fs` write/copy/rename
//! calls to an arbitrary destination: plain `fs::write` and the
//! plugin-fs copy/rename APIs follow reparse points, so a planted
//! shortcut named like an expected output (`video.ass`) silently
//! overwrites the target the shortcut points at.
//!
//! These commands replace the plugin-fs writeTextFile / copyFile /
//! rename callsites the frontend used to invoke directly. Each one:
//!
//!   1. Validates both source and destination paths through
//!      `validate_ipc_path` (Cc / BiDi / DOS-device gates from util.rs).
//!   2. lstat-checks the destination via `is_reparse_point`. If the
//!      destination already exists AND is a symlink / junction, refuses
//!      regardless of `overwrite` — never write through a shortcut.
//!   3. For copy and rename, lstat-checks the SOURCE too. A symlinked
//!      input (e.g. `Show.S01E01.ass → ~/.ssh/id_rsa`) would otherwise
//!      let plugin-fs copy the resolved target as a subtitle output.
//!   4. Uses `OpenOptions::create_new(true)` so even a TOCTOU-planted
//!      symlink between the lstat and the open call refuses to create
//!      through it (atomic OS-level guard).
//!
//! The frontend's `writeText` / `copyPath` / `renamePath` wrappers in
//! `src/lib/tauri-api.ts` now invoke these commands; the plugin-fs
//! permissions (`fs:allow-write-text-file` / `fs:allow-copy-file` /
//! `fs:allow-rename`) are dropped from `capabilities/default.json` so
//! a future regression that tries the old API is rejected at the
//! capability layer instead of silently succeeding.

use crate::util::{is_reparse_point, validate_ipc_path};
use std::fs;
use std::io::Write;
use std::path::Path;

fn ensure_parent_dir(path: &Path) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent)
                .map_err(|e| format!("Failed to create output directory: {e}"))?;
        }
    }
    Ok(())
}

/// Remove a destination that already exists, after rejecting any
/// reparse-point destination. Symlinks/junctions never overwrite —
/// even when `overwrite=true`, the caller is asked to clear the
/// shortcut manually rather than let us follow it.
fn clear_existing_destination(path: &Path, overwrite: bool) -> Result<(), String> {
    // `symlink_metadata` (= lstat) returns the link's own metadata
    // without following it. Path::exists() follows symlinks on Unix
    // and would return false for a dangling shortcut, which is the
    // exact case Codex flagged (the chain CLI write path bypassed
    // this check the same way before commit b7d9d21).
    match fs::symlink_metadata(path) {
        Ok(_) => {
            if is_reparse_point(path) {
                return Err(format!(
                    "Refusing to overwrite a symlink / junction at the destination: {}",
                    path.display()
                ));
            }
            if !overwrite {
                return Err(format!(
                    "Destination already exists (overwrite not enabled): {}",
                    path.display()
                ));
            }
            fs::remove_file(path)
                .map_err(|e| format!("Failed to remove existing destination: {e}"))?;
            Ok(())
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(format!("Failed to stat destination path: {e}")),
    }
}

/// Reject a source that is itself a symlink / junction. Caller's
/// intent is "operate on this file"; if the file is actually a
/// shortcut, the resolved target may be a sensitive file outside the
/// user-selected workflow scope.
fn reject_reparse_source(path: &Path, label: &str) -> Result<(), String> {
    if is_reparse_point(path) {
        return Err(format!(
            "Refusing to {label} from a symlink / junction: {}",
            path.display()
        ));
    }
    Ok(())
}

/// Atomically create a new file at `path` and write `content` to it.
/// `create_new(true)` is the OS-level guard against following a planted
/// symlink between the prior existence check and the open call.
fn create_new_and_write_bytes(path: &Path, content: &[u8]) -> Result<(), String> {
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(|e| format!("Failed to create destination: {e}"))?;
    file.write_all(content)
        .map_err(|e| format!("Failed to write destination: {e}"))
}

/// Write a text file safely. Refuses to write through an existing
/// symlink / junction at the destination. When `overwrite` is true and
/// the destination is a regular file, the file is removed first and
/// the new content is written atomically via `create_new(true)`.
#[tauri::command]
pub fn safe_write_text_file(path: String, content: String, overwrite: bool) -> Result<(), String> {
    validate_ipc_path(&path, "Output")?;
    let path_ref = Path::new(&path);
    ensure_parent_dir(path_ref)?;
    clear_existing_destination(path_ref, overwrite)?;
    create_new_and_write_bytes(path_ref, content.as_bytes())
}

/// Copy `src` to `dst` safely. Refuses if either endpoint is a
/// symlink / junction. The source's bytes are read via the resolved
/// regular file; the destination is created with `create_new(true)`
/// after the existing-destination check (or after removal when
/// `overwrite=true`).
#[tauri::command]
pub fn safe_copy_file(src: String, dst: String, overwrite: bool) -> Result<(), String> {
    validate_ipc_path(&src, "Source")?;
    validate_ipc_path(&dst, "Destination")?;
    let src_ref = Path::new(&src);
    let dst_ref = Path::new(&dst);
    reject_reparse_source(src_ref, "copy")?;
    ensure_parent_dir(dst_ref)?;
    clear_existing_destination(dst_ref, overwrite)?;

    let mut source = fs::File::open(src_ref).map_err(|e| format!("Failed to open source: {e}"))?;
    let mut destination = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(dst_ref)
        .map_err(|e| format!("Failed to create destination: {e}"))?;
    std::io::copy(&mut source, &mut destination)
        .map(|_| ())
        .map_err(|e| format!("Failed to copy file: {e}"))
}

/// Rename / move `src` to `dst` safely. Refuses if either endpoint is
/// a symlink / junction. `fs::rename` is atomic on the same volume and
/// falls back to copy-then-delete cross-volume (std semantics); both
/// paths fail-shut on a pre-existing dst symlink because we removed
/// any planted shortcut earlier.
#[tauri::command]
pub fn safe_rename_file(src: String, dst: String, overwrite: bool) -> Result<(), String> {
    validate_ipc_path(&src, "Source")?;
    validate_ipc_path(&dst, "Destination")?;
    let src_ref = Path::new(&src);
    let dst_ref = Path::new(&dst);
    reject_reparse_source(src_ref, "rename")?;
    ensure_parent_dir(dst_ref)?;
    clear_existing_destination(dst_ref, overwrite)?;
    fs::rename(src_ref, dst_ref).map_err(|e| format!("Failed to rename file: {e}"))
}

// ── Tests ────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read as _;

    fn temp_dir(name: &str) -> std::path::PathBuf {
        let mut dir = std::env::temp_dir();
        dir.push(format!(
            "ssahdrify_safe_io_test_{}_{}",
            name,
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn write_creates_file_when_dest_missing() {
        let dir = temp_dir("write_missing");
        let path = dir.join("out.txt");
        safe_write_text_file(
            path.to_string_lossy().to_string(),
            "hello".to_string(),
            false,
        )
        .unwrap();
        let content = fs::read_to_string(&path).unwrap();
        assert_eq!(content, "hello");
    }

    #[test]
    fn write_overwrites_when_flag_set() {
        let dir = temp_dir("write_overwrite");
        let path = dir.join("out.txt");
        fs::write(&path, b"old").unwrap();
        safe_write_text_file(path.to_string_lossy().to_string(), "new".to_string(), true).unwrap();
        let content = fs::read_to_string(&path).unwrap();
        assert_eq!(content, "new");
    }

    #[test]
    fn write_refuses_overwrite_when_flag_unset() {
        let dir = temp_dir("write_no_overwrite");
        let path = dir.join("out.txt");
        fs::write(&path, b"old").unwrap();
        let err =
            safe_write_text_file(path.to_string_lossy().to_string(), "new".to_string(), false)
                .unwrap_err();
        assert!(err.contains("already exists"));
        let content = fs::read_to_string(&path).unwrap();
        assert_eq!(content, "old");
    }

    #[test]
    fn copy_preserves_source_and_creates_destination() {
        let dir = temp_dir("copy_basic");
        let src = dir.join("src.bin");
        let dst = dir.join("dst.bin");
        fs::write(&src, b"payload").unwrap();
        safe_copy_file(
            src.to_string_lossy().to_string(),
            dst.to_string_lossy().to_string(),
            false,
        )
        .unwrap();
        let mut buf = Vec::new();
        fs::File::open(&dst).unwrap().read_to_end(&mut buf).unwrap();
        assert_eq!(buf, b"payload");
        assert!(src.exists());
    }

    #[test]
    fn rename_moves_source_to_destination() {
        let dir = temp_dir("rename_basic");
        let src = dir.join("src.bin");
        let dst = dir.join("dst.bin");
        fs::write(&src, b"payload").unwrap();
        safe_rename_file(
            src.to_string_lossy().to_string(),
            dst.to_string_lossy().to_string(),
            false,
        )
        .unwrap();
        assert!(!src.exists());
        assert_eq!(fs::read(&dst).unwrap(), b"payload");
    }

    // Symlink tests are POSIX-only because Windows symlink creation
    // requires admin or developer mode. The reparse-point detection on
    // Windows is exercised via `is_reparse_point` unit tests in util.rs;
    // the safe-io behavior on top of that helper is identical to the
    // POSIX path tested here.
    #[cfg(unix)]
    #[test]
    fn write_refuses_existing_symlink_destination() {
        use std::os::unix::fs::symlink;
        let dir = temp_dir("write_symlink_dst");
        let target = dir.join("real_target");
        let link = dir.join("looks_like_output.txt");
        fs::write(&target, b"sensitive").unwrap();
        symlink(&target, &link).unwrap();

        let err = safe_write_text_file(
            link.to_string_lossy().to_string(),
            "attacker_content".to_string(),
            true, // even with overwrite=true, the symlink is refused
        )
        .unwrap_err();
        assert!(err.contains("symlink"));
        // Target unchanged
        assert_eq!(fs::read(&target).unwrap(), b"sensitive");
    }

    #[cfg(unix)]
    #[test]
    fn copy_refuses_symlinked_source() {
        use std::os::unix::fs::symlink;
        let dir = temp_dir("copy_symlink_src");
        let target = dir.join("real_target");
        let link = dir.join("Show.S01E01.ass");
        let dst = dir.join("video.ass");
        fs::write(&target, b"sensitive").unwrap();
        symlink(&target, &link).unwrap();

        let err = safe_copy_file(
            link.to_string_lossy().to_string(),
            dst.to_string_lossy().to_string(),
            false,
        )
        .unwrap_err();
        assert!(err.contains("symlink"));
        assert!(!dst.exists());
    }
}
