//! Symlink-safe + scope-honoring file write / copy / rename commands for
//! the GUI.
//!
//! Codex security scan (2026-05-11) flagged two reachable paths where a
//! malicious or accidental symlink in an attacker-influenced subtitle
//! pack could redirect Tauri's `@tauri-apps/plugin-fs` write/copy/rename
//! calls to an arbitrary destination: plain `fs::write` and the
//! plugin-fs copy/rename APIs follow reparse points, so a planted
//! shortcut named like an expected output (`video.ass`) silently
//! overwrites the target the shortcut points at.
//!
//! Initial migration moved the write/copy/rename operations onto these
//! commands, dropping the `fs:allow-write-text-file` / `-copy-file` /
//! `-rename` plugin-fs permission grants. A follow-up Codex finding
//! (2ec537b0, HIGH) noticed that move ALSO dropped the `fs:scope` deny
//! list as a side effect: the policy was tied to plugin-fs callsites,
//! not to the new commands. A compromised WebView could call
//! `safe_copy_file($HOME/.ssh/id_rsa, /tmp/leak.ass)` and then read the
//! copy through the normal subtitle reader; `safe_write_text_file`
//! could plant a file under Windows Start Menu autostart paths. The
//! current implementation closes both regressions with three layered
//! defenses, applied to BOTH source and destination on copy/rename and
//! to destination on write:
//!
//!   1. **`validate_ipc_path`** (util.rs) — Cc / BiDi / DOS-device
//!      gates. Rejects malformed paths before any fs syscall.
//!   2. **Subtitle-extension whitelist** — destinations (and copy/rename
//!      sources) must end with `.ass / .ssa / .srt / .vtt / .sub /
//!      .sbv / .lrc`. Matches `read_text_detect_encoding`'s pattern;
//!      closes the "Start Menu autostart .desktop / .lnk" persistence
//!      class because those extensions are outside the set.
//!   3. **`fs_scope().is_allowed()`** — reuses Tauri's plugin-fs
//!      allow/deny policy verbatim (no manual port of the 50-entry deny
//!      list; single source of truth in `capabilities/default.json`).
//!      Closes the "exfil credentials via copy" class because
//!      `$HOME/.ssh` and the rest of the deny list refuse on both src
//!      and dst.
//!   4. **`is_reparse_point` rejection + `create_new(true)`** —
//!      original symlink-safety defenses against TOCTOU symlink
//!      planting between the lstat and the open call.
//!
//! Tests pin the gating logic via `*_inner` helpers that take an
//! `is_allowed` closure so the Tauri command's `AppHandle` doesn't have
//! to be mocked. Production wraps `app.fs_scope().is_allowed(...)`.

use crate::encoding::ALLOWED_TEXT_EXTENSIONS;
use crate::util::{is_reparse_point, validate_ipc_path};
use std::fs;
use std::io::Write;
use std::path::Path;
use tauri_plugin_fs::FsExt;

fn check_subtitle_extension(path: &Path, label: &str) -> Result<(), String> {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase())
        .unwrap_or_default();
    if !ALLOWED_TEXT_EXTENSIONS.contains(&ext.as_str()) {
        let pretty = if ext.is_empty() {
            "(no extension)".to_string()
        } else {
            format!(".{ext}")
        };
        return Err(format!(
            "{label} path must end with a subtitle extension; got {pretty} \
             (allowed: {})",
            ALLOWED_TEXT_EXTENSIONS.join(", ")
        ));
    }
    Ok(())
}

fn check_scope_allows(
    is_allowed: &impl Fn(&Path) -> bool,
    path: &Path,
    label: &str,
) -> Result<(), String> {
    if !is_allowed(path) {
        return Err(format!(
            "{label} path is denied by the application's filesystem scope \
             policy: {}",
            path.display()
        ));
    }
    Ok(())
}

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
        Ok(meta) => {
            if is_reparse_point(path) {
                return Err(format!(
                    "Refusing to overwrite a symlink / junction at the destination: {}",
                    path.display()
                ));
            }
            // Round 10 A-R10-005: explicit directory check before
            // `fs::remove_file`. Pre-R10 a destination that happened
            // to be a directory propagated through `remove_file` as
            // an opaque "Failed to remove existing destination:
            // Access is denied" (Windows) / EISDIR (POSIX) — users
            // received a permission-shaped error for a structural
            // mismatch. Surface it with a clearer message that
            // names what the caller actually expected (a file).
            if meta.is_dir() {
                return Err(format!(
                    "Destination is a directory; expected a file: {}",
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

/// Reject when the destination resolves to the same on-disk file as
/// the source. Canonicalize asks the OS to walk symlinks AND case-fold
/// per the actual filesystem the file lives on — so on Linux with a
/// case-insensitive mount (NTFS via ntfs-3g, exFAT, HFS+ on a removable
/// drive) this still sees `Episode.ass` and `episode.ass` as the same
/// file. That's a blind spot of process-level OS-gated heuristics like
/// the TS-side `isCaseInsensitiveFs`, which derives from `process.platform`
/// rather than the mount's behavior. Without this gate,
/// `clear_existing_destination(dst, overwrite=true)` removes dst first
/// — which IS src under FS-level case-fold — then `fs::rename` /
/// `fs::copy` fails because src is gone, silently destroying the user's
/// input (Codex a274852e). Both paths must canonicalize for the check
/// to fire; a not-yet-existing dst (normal rename to a new name)
/// returns Ok here and the downstream existence checks proceed.
///
/// Hardlinks are intentionally out of scope (Round 2 N-R2-5): two
/// hardlinks to the same inode canonicalize to distinct paths because
/// canonicalize() only resolves symlinks, not hardlink aliases. A
/// rename through one hardlink to another succeeds — net data loss
/// is zero (the file remains accessible via the original hardlink)
/// but the in-place rename semantics break in a way this gate doesn't
/// catch. Detecting hardlinks would require platform-specific
/// `MetadataExt::dev()/ino()` (Unix) and `GetFileInformationByHandle`
/// (Windows). P1a-bounded — fan-sub subtitle workflows don't use
/// hardlinks; revisit if the threat model shifts to multi-tool
/// pipelines that pre-link files.
///
/// Mixed canonicalize-result cases (Round 3 N-R3-11): when EITHER src
/// OR dst canonicalize-fails (e.g., dst doesn't yet exist, which is
/// the common "rename to a new name" case), this helper returns Ok
/// and the downstream `clear_existing_destination` + `fs::rename` /
/// `fs::copy` chain takes over. Mixed-Ok-Err is symmetric with
/// both-Err: the gate only fires when both sides resolve. The
/// `fs::rename` /  `fs::copy` calls will surface a focused error if
/// the side that canonicalize-failed turns out to be the problem,
/// so the predictable downstream failure is the contract.
fn reject_same_canonical_path(src: &Path, dst: &Path) -> Result<(), String> {
    if let (Ok(src_canon), Ok(dst_canon)) = (src.canonicalize(), dst.canonicalize()) {
        if src_canon == dst_canon {
            return Err(format!(
                "Refusing to operate: source and destination resolve to the same file on disk: {}",
                src.display()
            ));
        }
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

// ── Inner helpers (testable without an AppHandle) ────────────────

fn safe_write_text_file_inner(
    path: &str,
    content: &str,
    overwrite: bool,
    is_allowed: impl Fn(&Path) -> bool,
) -> Result<(), String> {
    validate_ipc_path(path, "Output")?;
    let path_ref = Path::new(path);
    check_subtitle_extension(path_ref, "Output")?;
    check_scope_allows(&is_allowed, path_ref, "Output")?;
    ensure_parent_dir(path_ref)?;
    clear_existing_destination(path_ref, overwrite)?;
    create_new_and_write_bytes(path_ref, content.as_bytes())
}

fn safe_copy_file_inner(
    src: &str,
    dst: &str,
    overwrite: bool,
    is_allowed: impl Fn(&Path) -> bool,
) -> Result<(), String> {
    validate_ipc_path(src, "Source")?;
    validate_ipc_path(dst, "Destination")?;
    let src_ref = Path::new(src);
    let dst_ref = Path::new(dst);
    check_subtitle_extension(src_ref, "Source")?;
    check_subtitle_extension(dst_ref, "Destination")?;
    check_scope_allows(&is_allowed, src_ref, "Source")?;
    check_scope_allows(&is_allowed, dst_ref, "Destination")?;
    reject_reparse_source(src_ref, "copy")?;
    reject_same_canonical_path(src_ref, dst_ref)?;
    ensure_parent_dir(dst_ref)?;
    clear_existing_destination(dst_ref, overwrite)?;

    // Round 6 Wave 6.3 #11: re-check `is_reparse_point` immediately
    // before `File::open` to close the TOCTOU window where an
    // attacker could swap the source for a symlink between
    // `reject_reparse_source` (above) and the open below. Mirrors the
    // pattern `encoding.rs::read_text_detect_encoding` already
    // applies (stat-time re-check after canonicalize) — same race,
    // same fix shape. Bounded by the same single-user trust model;
    // re-check is cheap (one syscall) and parallels the symmetric
    // scrub already enforced on the read side.
    if is_reparse_point(src_ref) {
        log::warn!(
            "Refusing to copy possible symlink / junction at open-time: {}",
            src_ref.display()
        );
        return Err("Refusing to copy symlink / junction (race-time detection)".to_string());
    }

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

fn safe_rename_file_inner(
    src: &str,
    dst: &str,
    overwrite: bool,
    is_allowed: impl Fn(&Path) -> bool,
) -> Result<(), String> {
    validate_ipc_path(src, "Source")?;
    validate_ipc_path(dst, "Destination")?;
    let src_ref = Path::new(src);
    let dst_ref = Path::new(dst);
    check_subtitle_extension(src_ref, "Source")?;
    check_subtitle_extension(dst_ref, "Destination")?;
    check_scope_allows(&is_allowed, src_ref, "Source")?;
    check_scope_allows(&is_allowed, dst_ref, "Destination")?;
    reject_reparse_source(src_ref, "rename")?;
    reject_same_canonical_path(src_ref, dst_ref)?;
    ensure_parent_dir(dst_ref)?;
    clear_existing_destination(dst_ref, overwrite)?;

    // Round 8 N-R8-N3-1: re-check `is_reparse_point` immediately before
    // `fs::rename` for parity with `safe_copy_file_inner`'s open-time
    // re-check (Round 6 Wave 6.3 #11). `fs::rename` on POSIX moves the
    // symlink itself (doesn't chase the target) and Windows behaves
    // similarly for same-volume renames — but cross-volume `rename`
    // falls back to copy-then-delete (std semantics), which DOES
    // follow the link. Closing the TOCTOU window symmetrically with
    // the copy path eliminates the "rename across volumes after
    // swap" edge case at near-zero cost (one syscall).
    if is_reparse_point(src_ref) {
        log::warn!(
            "Refusing to rename possible symlink / junction at rename-time: {}",
            src_ref.display()
        );
        return Err("Refusing to rename symlink / junction (race-time detection)".to_string());
    }

    fs::rename(src_ref, dst_ref).map_err(|e| format!("Failed to rename file: {e}"))
}

// ── Tauri commands (production) ────────────────────────────────

/// Write a text file safely. Layered defenses: scope deny enforcement,
/// subtitle-extension whitelist, symlink rejection on destination,
/// atomic `create_new(true)` open.
#[tauri::command]
pub fn safe_write_text_file(
    app: tauri::AppHandle,
    path: String,
    content: String,
    overwrite: bool,
) -> Result<(), String> {
    let scope = app.fs_scope();
    safe_write_text_file_inner(&path, &content, overwrite, move |p| scope.is_allowed(p))
}

/// Copy `src` to `dst` safely. Both endpoints pass the same gates as
/// `safe_write_text_file`'s destination; source is additionally
/// reparse-point-rejected (a symlinked input would otherwise resolve
/// to e.g. `~/.ssh/id_rsa` and copy its bytes as if they were a
/// subtitle).
#[tauri::command]
pub fn safe_copy_file(
    app: tauri::AppHandle,
    src: String,
    dst: String,
    overwrite: bool,
) -> Result<(), String> {
    let scope = app.fs_scope();
    safe_copy_file_inner(&src, &dst, overwrite, move |p| scope.is_allowed(p))
}

/// Rename / move `src` to `dst` safely. Same gating as `safe_copy_file`.
/// `fs::rename` is atomic on the same volume and falls back to
/// copy-then-delete cross-volume (std semantics); both paths fail-shut
/// on a pre-existing dst symlink because we removed any planted
/// shortcut earlier.
#[tauri::command]
pub fn safe_rename_file(
    app: tauri::AppHandle,
    src: String,
    dst: String,
    overwrite: bool,
) -> Result<(), String> {
    let scope = app.fs_scope();
    safe_rename_file_inner(&src, &dst, overwrite, move |p| scope.is_allowed(p))
}

// ── Tests ────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read as _;

    fn allow_all(_: &Path) -> bool {
        true
    }

    fn deny_all(_: &Path) -> bool {
        false
    }

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
        let path = dir.join("out.ass");
        safe_write_text_file_inner(&path.to_string_lossy(), "hello", false, allow_all).unwrap();
        let content = fs::read_to_string(&path).unwrap();
        assert_eq!(content, "hello");
    }

    #[test]
    fn write_overwrites_when_flag_set() {
        let dir = temp_dir("write_overwrite");
        let path = dir.join("out.ass");
        fs::write(&path, b"old").unwrap();
        safe_write_text_file_inner(&path.to_string_lossy(), "new", true, allow_all).unwrap();
        let content = fs::read_to_string(&path).unwrap();
        assert_eq!(content, "new");
    }

    #[test]
    fn write_refuses_overwrite_when_flag_unset() {
        let dir = temp_dir("write_no_overwrite");
        let path = dir.join("out.ass");
        fs::write(&path, b"old").unwrap();
        let err = safe_write_text_file_inner(&path.to_string_lossy(), "new", false, allow_all)
            .unwrap_err();
        assert!(err.contains("already exists"));
        let content = fs::read_to_string(&path).unwrap();
        assert_eq!(content, "old");
    }

    #[test]
    fn write_refuses_non_subtitle_extension() {
        let dir = temp_dir("write_bad_ext");
        let path = dir.join("malicious.desktop");
        let err = safe_write_text_file_inner(
            &path.to_string_lossy(),
            "[Desktop Entry]\nExec=/tmp/payload",
            true,
            allow_all,
        )
        .unwrap_err();
        assert!(err.contains("subtitle extension"));
        assert!(!path.exists());
    }

    #[test]
    fn write_refuses_when_scope_denies() {
        let dir = temp_dir("write_scope_deny");
        let path = dir.join("out.ass");
        let err =
            safe_write_text_file_inner(&path.to_string_lossy(), "x", false, deny_all).unwrap_err();
        assert!(err.contains("denied by"));
        assert!(!path.exists());
    }

    #[test]
    fn copy_preserves_source_and_creates_destination() {
        let dir = temp_dir("copy_basic");
        let src = dir.join("src.ass");
        let dst = dir.join("dst.ass");
        fs::write(&src, b"payload").unwrap();
        safe_copy_file_inner(
            &src.to_string_lossy(),
            &dst.to_string_lossy(),
            false,
            allow_all,
        )
        .unwrap();
        let mut buf = Vec::new();
        fs::File::open(&dst).unwrap().read_to_end(&mut buf).unwrap();
        assert_eq!(buf, b"payload");
        assert!(src.exists());
    }

    #[test]
    fn copy_refuses_when_scope_denies_destination() {
        let dir = temp_dir("copy_scope_deny");
        let src = dir.join("src.ass");
        let dst = dir.join("dst.ass");
        fs::write(&src, b"payload").unwrap();
        // Allow source, deny destination — simulates a scope policy that
        // permits reading the input but rejects the proposed output
        // location.
        let dst_str = dst.to_string_lossy().to_string();
        let dst_str_for_closure = dst_str.clone();
        let err = safe_copy_file_inner(&src.to_string_lossy(), &dst_str, false, move |p| {
            p.to_string_lossy() != dst_str_for_closure
        })
        .unwrap_err();
        assert!(err.contains("denied by"));
        assert!(!dst.exists());
    }

    #[test]
    fn rename_moves_source_to_destination() {
        let dir = temp_dir("rename_basic");
        let src = dir.join("src.ass");
        let dst = dir.join("dst.ass");
        fs::write(&src, b"payload").unwrap();
        safe_rename_file_inner(
            &src.to_string_lossy(),
            &dst.to_string_lossy(),
            false,
            allow_all,
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
        let target = dir.join("real_target.ass");
        let link = dir.join("looks_like_output.ass");
        fs::write(&target, b"sensitive").unwrap();
        symlink(&target, &link).unwrap();

        let err = safe_write_text_file_inner(
            &link.to_string_lossy(),
            "attacker_content",
            true,
            allow_all,
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
        let target = dir.join("real_target.ass");
        let link = dir.join("Show.S01E01.ass");
        let dst = dir.join("video.ass");
        fs::write(&target, b"sensitive").unwrap();
        symlink(&target, &link).unwrap();

        let err = safe_copy_file_inner(
            &link.to_string_lossy(),
            &dst.to_string_lossy(),
            false,
            allow_all,
        )
        .unwrap_err();
        assert!(err.contains("symlink"));
        assert!(!dst.exists());
    }

    // Case-only self-overwrite tests are gated to Windows because NTFS
    // is case-insensitive by default — the OS reports the same canonical
    // path for `Episode.ass` and `episode.ass`. On Linux ext4 these are
    // distinct files, so the canonicalize check correctly does not fire
    // and the test would not exercise the regression. macOS APFS would
    // also fire the gate but there's no test machine available.
    #[cfg(target_os = "windows")]
    #[test]
    fn rename_refuses_case_only_self_overwrite() {
        let dir = temp_dir("rename_case_self");
        let src = dir.join("Episode.ass");
        let dst = dir.join("episode.ass");
        fs::write(&src, b"payload").unwrap();
        let err = safe_rename_file_inner(
            &src.to_string_lossy(),
            &dst.to_string_lossy(),
            true,
            allow_all,
        )
        .unwrap_err();
        assert!(err.contains("same file"));
        assert_eq!(fs::read(&src).unwrap(), b"payload");
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn copy_refuses_case_only_self_overwrite() {
        let dir = temp_dir("copy_case_self");
        let src = dir.join("Episode.ass");
        let dst = dir.join("episode.ass");
        fs::write(&src, b"payload").unwrap();
        let err = safe_copy_file_inner(
            &src.to_string_lossy(),
            &dst.to_string_lossy(),
            true,
            allow_all,
        )
        .unwrap_err();
        assert!(err.contains("same file"));
        assert_eq!(fs::read(&src).unwrap(), b"payload");
    }
}
