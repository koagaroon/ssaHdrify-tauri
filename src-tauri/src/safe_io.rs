//! Symlink-safe + scope-honoring file write / copy / rename commands for
//! the GUI.
//!
//! Two reachable paths used to let a malicious or accidental symlink
//! in an attacker-influenced subtitle
//! pack could redirect Tauri's `@tauri-apps/plugin-fs` write/copy/rename
//! calls to an arbitrary destination: plain `fs::write` and the
//! plugin-fs copy/rename APIs follow reparse points, so a planted
//! shortcut named like an expected output (`video.ass`) silently
//! overwrites the target the shortcut points at.
//!
//! Initial migration moved the write/copy/rename operations onto these
//! commands, dropping the `fs:allow-write-text-file` / `-copy-file` /
//! `-rename` plugin-fs permission grants. That move also dropped the
//! `fs:scope` deny list as a side effect: the policy was tied to
//! plugin-fs callsites, not to the new commands. A compromised WebView could call
//! `safe_copy_file($HOME/.ssh/id_rsa, /tmp/leak.ass)` and then read the
//! copy through the normal subtitle reader; `safe_write_text_file`
//! could plant a file under Windows Start Menu autostart paths. The
//! current implementation closes both regressions with three layered
//! defenses, applied to BOTH source and destination on copy/rename and
//! to destination on write:
//!
//!   1. **`validate_ipc_path`** (util.rs) — Cc / BiDi / DOS-device
//!      gates. Rejects malformed paths before any fs syscall.
//!   2. **Subtitle-extension whitelist** — text writes must end with
//!      `.ass / .ssa / .srt / .vtt / .sub`, matching
//!      `read_text_detect_encoding` and the TS parser-aligned
//!      `SUBTITLE_EXTS` set. Copy/rename allows those same text
//!      extensions plus opaque file-preserving sidecars such as `.sup`, but
//!      sidecar copies/renames must preserve the sidecar extension
//!      (`.sup -> .sup`) so a binary subtitle cannot be laundered into a
//!      future text-readable `.ass`.
//!      Closes the "Start Menu autostart .desktop / .lnk" persistence
//!      class because those extensions are outside both sets.
//!   3. **App-owned `Scope::is_allowed()`** — reuses Tauri's matcher with
//!      a scope built from `capabilities/default.json`. That JSON
//!      remains the single source of truth for the deny list.
//!      Closes the "exfil credentials via copy" class because
//!      `$HOME/.ssh` and the rest of the deny list refuse on both src
//!      and dst.
//!   4. **`is_reparse_point` rejection + `create_new(true)`** —
//!      original symlink-safety defenses against TOCTOU symlink
//!      planting between the lstat and the open call.
//!
//! Tests pin the gating logic via `*_inner` helpers that take an
//! `is_allowed` closure so the Tauri command's `AppHandle` doesn't have
//! to be mocked. GUI production wraps the app-owned scope's `is_allowed`;
//! the CLI binary (`bin/cli/main.rs`) also routes its write / copy /
//! rename outputs through these helpers with a permissive `|_| true`
//! closure (CLI argv is the user's intent, so there is no
//! Tauri-side scope policy to enforce — but every other defense in the
//! chain still applies). Centralizing the defense set here means
//! future safe_io fixes auto-propagate to both binaries
//! instead of needing parallel fixes in each.

use crate::encoding::{read_text_detect_encoding_inner, ALLOWED_TEXT_EXTENSIONS, MAX_TEXT_SIZE};
use crate::util::{is_reparse_point, validate_ipc_path};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

const ALLOWED_RENAME_SIDECAR_EXTENSIONS: &[&str] = &["sup"];

fn path_extension_lower(path: &Path) -> String {
    path.extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase())
        .unwrap_or_default()
}

fn pretty_ext(ext: &str) -> String {
    if ext.is_empty() {
        "(no extension)".to_string()
    } else {
        format!(".{ext}")
    }
}

fn text_ext_allowed(ext: &str) -> bool {
    ALLOWED_TEXT_EXTENSIONS.contains(&ext)
}

fn sidecar_ext_allowed(ext: &str) -> bool {
    ALLOWED_RENAME_SIDECAR_EXTENSIONS.contains(&ext)
}

fn check_subtitle_extension(path: &Path, label: &str) -> Result<(), String> {
    let ext = path_extension_lower(path);
    if !text_ext_allowed(&ext) {
        let allowed = ALLOWED_TEXT_EXTENSIONS.join(", ");
        return Err(format!(
            "{label} path must end with a subtitle extension; got {pretty} \
             (allowed: {allowed})",
            pretty = pretty_ext(&ext)
        ));
    }
    Ok(())
}

fn check_ass_style_extension(path: &Path, label: &str) -> Result<String, String> {
    let ext = path_extension_lower(path);
    if matches!(ext.as_str(), "ass" | "ssa") {
        return Ok(ext);
    }
    Err(format!(
        "{label} path must end with .ass or .ssa; got {}",
        pretty_ext(&ext)
    ))
}

fn check_copy_rename_extensions(src: &Path, dst: &Path) -> Result<(), String> {
    let src_ext = path_extension_lower(src);
    let dst_ext = path_extension_lower(dst);
    let src_text = text_ext_allowed(&src_ext);
    let dst_text = text_ext_allowed(&dst_ext);
    let src_sidecar = sidecar_ext_allowed(&src_ext);
    let dst_sidecar = sidecar_ext_allowed(&dst_ext);

    if src_text && dst_text {
        return Ok(());
    }

    let allowed = format!(
        "{}, {}",
        ALLOWED_TEXT_EXTENSIONS.join(", "),
        ALLOWED_RENAME_SIDECAR_EXTENSIONS.join(", ")
    );

    if !src_text && !src_sidecar {
        return Err(format!(
            "Source path must end with a subtitle extension; got {} (allowed: {allowed})",
            pretty_ext(&src_ext)
        ));
    }
    if !dst_text && !dst_sidecar {
        return Err(format!(
            "Destination path must end with a subtitle extension; got {} (allowed: {allowed})",
            pretty_ext(&dst_ext)
        ));
    }

    if src_sidecar && dst_sidecar && src_ext == dst_ext {
        return Ok(());
    }

    Err(format!(
        "Sidecar subtitle copy/rename must preserve the sidecar extension; got source {} and destination {}",
        pretty_ext(&src_ext),
        pretty_ext(&dst_ext)
    ))
}

fn check_output_probe_extension(path: &Path, label: &str) -> Result<(), String> {
    let ext = path_extension_lower(path);
    if text_ext_allowed(&ext) || sidecar_ext_allowed(&ext) {
        return Ok(());
    }

    let allowed = format!(
        "{}, {}",
        ALLOWED_TEXT_EXTENSIONS.join(", "),
        ALLOWED_RENAME_SIDECAR_EXTENSIONS.join(", ")
    );
    Err(format!(
        "{label} path must end with a subtitle extension; got {pretty} \
         (allowed: {allowed})",
        pretty = pretty_ext(&ext)
    ))
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

struct ResolvedScopePath {
    path: PathBuf,
    terminal_reparse_parent: Option<PathBuf>,
}

fn scope_resolved_path(path: &Path, label: &str) -> Result<ResolvedScopePath, String> {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .map_err(|e| format!("Failed to resolve current directory for {label}: {e}"))?
            .join(path)
    };

    let mut existing = absolute.as_path();
    let mut missing = Vec::new();
    loop {
        match fs::symlink_metadata(existing) {
            Ok(_) => break,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                let file_name = existing.file_name().ok_or_else(|| {
                    format!(
                        "Cannot resolve {label} path for filesystem scope policy: {}",
                        path.display()
                    )
                })?;
                missing.push(file_name.to_os_string());
                existing = existing.parent().ok_or_else(|| {
                    format!(
                        "Cannot resolve {label} path for filesystem scope policy: {}",
                        path.display()
                    )
                })?;
            }
            Err(e) => {
                return Err(format!(
                    "Failed to stat {label} path for filesystem scope policy: {e}"
                ));
            }
        }
    }

    let mut terminal_reparse_parent = None;
    let mut resolved = match existing.canonicalize() {
        Ok(resolved) => resolved,
        Err(e)
            if e.kind() == std::io::ErrorKind::NotFound
                && missing.is_empty()
                && is_reparse_point(existing) =>
        {
            // A terminal dangling symlink / reparse point occupies the output
            // slot even though its target cannot be canonicalized. Resolve the
            // parent instead so scope checks still see through any live alias
            // ancestors, then append the link's own filename without following
            // its missing target.
            let parent = existing.parent().ok_or_else(|| {
                format!(
                    "Cannot resolve {label} path for filesystem scope policy: {}",
                    path.display()
                )
            })?;
            let file_name = existing.file_name().ok_or_else(|| {
                format!(
                    "Cannot resolve {label} path for filesystem scope policy: {}",
                    path.display()
                )
            })?;
            let mut resolved_parent = parent.canonicalize().map_err(|parent_error| {
                format!(
                    "Failed to resolve {label} parent for filesystem scope policy: {parent_error}"
                )
            })?;
            terminal_reparse_parent = Some(resolved_parent.clone());
            resolved_parent.push(file_name);
            resolved_parent
        }
        Err(e) => {
            return Err(format!(
                "Failed to resolve {label} path for filesystem scope policy: {e}"
            ));
        }
    };
    for component in missing.iter().rev() {
        resolved.push(component);
    }
    Ok(ResolvedScopePath {
        path: resolved,
        terminal_reparse_parent,
    })
}

fn check_scope_allows_resolved(
    is_allowed: &impl Fn(&Path) -> bool,
    path: &Path,
    label: &str,
) -> Result<(), String> {
    let resolved = scope_resolved_path(path, label)?;
    check_resolved_scope_path(is_allowed, &resolved, label)
}

fn check_resolved_scope_path(
    is_allowed: &impl Fn(&Path) -> bool,
    resolved: &ResolvedScopePath,
    label: &str,
) -> Result<(), String> {
    // Tauri's scope helper follows a terminal symlink before matching policy.
    // For a dangling relative target that can discard the canonical parent, so
    // explicitly check the parent captured by the no-follow fallback first.
    if let Some(parent) = &resolved.terminal_reparse_parent {
        check_scope_allows(is_allowed, parent, label)?;
    }
    check_scope_allows(is_allowed, &resolved.path, label)
}

fn ensure_parent_dir(path: &Path) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            // include the parent path in the
            // error so the user can identify which directory failed.
            // Previously a permission / quota / read-only-fs failure
            // surfaced as "Failed to create output directory:
            // <generic os error>" with no actionable signal. Path
            // bytes flow operationally here; downstream callers
            // (CLI's `emit_file_report`, GUI's IPC error path) launder
            // BiDi / control characters at the print boundary via
            // `sanitize_for_display` / `stripUnicodeControls`, so
            // including the raw `parent.display()` here doesn't reopen
            // the sanitize-vs-operational concern.
            fs::create_dir_all(parent).map_err(|e| {
                format!(
                    "Failed to create output directory {}: {e}",
                    parent.display()
                )
            })?;
        }
    }
    Ok(())
}

/// Inspect an existing destination without mutating it. Returns true for a
/// replaceable ordinary file and false when the path is absent.
fn inspect_existing_destination(path: &Path, overwrite: bool) -> Result<bool, String> {
    // `symlink_metadata` (= lstat) returns the link's own metadata
    // without following it. Path::exists() follows symlinks on Unix
    // and would return false for a dangling shortcut, which is the
    // exact regression case: the chain CLI write path used to bypass
    // this check before the shared safe_io helper owned it.
    //
    // The back-to-back syscalls
    // (`symlink_metadata` returning `meta` here, then `is_reparse_point`
    // calling `symlink_metadata` again) look redundant
    // — `meta.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT` on
    // Windows / `meta.file_type().is_symlink()` on POSIX would
    // answer from the already-fetched `meta`. Kept deliberately for
    // call-shape parity with the other `is_reparse_point` usages in
    // this file and across the codebase
    // (`dropzone.rs::walk_one_level` documents the same trade-off).
    // Centralizing into a `is_reparse_point_from_meta`
    // helper would touch 6+ sites with no measurable perf win on a
    // failure-path syscall pattern.
    match fs::symlink_metadata(path) {
        Ok(meta) => {
            if is_reparse_point(path) {
                return Err(format!(
                    "Refusing to overwrite a symlink / junction at the destination: {}",
                    path.display()
                ));
            }
            // Explicit directory check before
            // `fs::remove_file`. Previously a destination that
            // happened to be a directory propagated through
            // `remove_file` as
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
            Ok(true)
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(format!("Failed to stat destination path: {e}")),
    }
}

/// Remove a destination that already exists, after rejecting any
/// reparse-point destination. Symlinks/junctions never overwrite — even when
/// `overwrite=true`, the caller is asked to clear the shortcut manually.
fn clear_existing_destination(path: &Path, overwrite: bool) -> Result<(), String> {
    if !inspect_existing_destination(path, overwrite)? {
        return Ok(());
    }

    // Re-check immediately before deletion to narrow the window in which an
    // ordinary destination could be replaced by a reparse point.
    if is_reparse_point(path) {
        log::warn!(
            "Refusing to overwrite reparse point at remove-time: {}",
            path.display()
        );
        return Err(format!(
            "Refusing to overwrite a symlink / junction at the destination (race-time detection): {}",
            path.display()
        ));
    }
    fs::remove_file(path).map_err(|e| format!("Failed to remove existing destination: {e}"))
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
/// removing/replacing dst would therefore operate on src itself under
/// filesystem-level case-folding. Both paths must canonicalize for the check
/// to fire; a not-yet-existing dst (normal rename to a new name)
/// returns Ok here and the downstream existence checks proceed.
///
/// Hardlinks are intentionally out of scope : two
/// hardlinks to the same inode canonicalize to distinct paths because
/// canonicalize() only resolves symlinks, not hardlink aliases. A
/// rename through one hardlink to another succeeds — net data loss
/// is zero (the file remains accessible via the original hardlink)
/// but the in-place rename semantics break in a way this gate doesn't
/// catch. Detecting hardlinks would require platform-specific
/// `MetadataExt::dev()/ino()` (Unix) and `GetFileInformationByHandle`
/// (Windows). Bounded to local-user filesystem access; fan-sub subtitle
/// workflows don't use hardlinks. Revisit if the threat model shifts to multi-tool
/// pipelines that pre-link files.
///
/// Mixed canonicalize-result cases : when EITHER src
/// OR dst canonicalize-fails (e.g., dst doesn't yet exist, which is
/// the common "rename to a new name" case), this helper returns Ok
/// and the downstream destination inspection plus `fs::rename` / `fs::copy`
/// chain takes over. Mixed-Ok-Err is symmetric with
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
///
/// **Partial-write durability trade-off (accepted).** When
/// `overwrite=true`, the upstream `clear_existing_destination` removes
/// the prior file BEFORE `create_new_and_write_bytes` is called. If
/// `write_all` then fails (disk full mid-write, drive eject, antivirus
/// quarantine, power loss), the destination is left as a partial file
/// while the user's prior data is already gone — they get the error
/// but no recovery path. A tmp-file + atomic-rename pattern would
/// close this gap, but this desktop app's current scope accepts the
/// simpler shape: generated subtitle text outputs are capped at 50 MiB by
/// `MAX_TEXT_SIZE`, local disks are reliable, and the user can rerun the
/// conversion. Don't refactor to tmp+rename without
/// re-checking the scope — the create_new gate ABOVE is load-bearing
/// against symlink races; a naive `fs::write` to a tmp path would
/// need the same gate transplanted onto the tmp file.
fn create_new_and_write_bytes(path: &Path, content: &[u8]) -> Result<(), String> {
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(|e| format!("Failed to create destination: {e}"))?;
    file.write_all(content)
        .map_err(|error| partial_output_error("write destination", &error))
}

fn partial_output_error(action: &str, error: &std::io::Error) -> String {
    format!("Failed to {action}; a partial output may remain: {error}")
}

fn check_plain_text_size(content_len: usize) -> Result<(), String> {
    if content_len > MAX_TEXT_SIZE as usize {
        return Err("Subtitle content exceeds the 50 MB limit".to_string());
    }
    Ok(())
}

fn create_new_and_write_bytes_exclusive(path: &Path, content: &[u8]) -> Result<(), String> {
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(|e| format!("Failed to create destination: {e}"))?;

    // Keep a partial create-new file if write_all fails. Deleting by pathname
    // after closing would race with another process replacing that pathname,
    // which could make error cleanup delete a file we did not create.
    file.write_all(content)
        .map_err(|error| partial_output_error("write destination", &error))?;
    file.sync_all()
        .map_err(|error| partial_output_error("flush destination", &error))
}

fn encode_text_preserving_source(
    content: &str,
    encoding_id: &str,
    had_bom: bool,
) -> Result<Vec<u8>, String> {
    // Bound the IPC string before allocating another encoded buffer; the
    // final encoded byte count is checked too.
    // A single-byte legacy source can expand to three UTF-8 bytes per input
    // byte after decoding. Style edits add only bounded family/size fields,
    // so 4x is a conservative allocation ceiling while the encoded output
    // remains capped at the original 50 MiB file ceiling below.
    const MAX_STYLE_EDIT_UTF8_CONTENT: usize = (MAX_TEXT_SIZE as usize) * 4;
    if content.len() > MAX_STYLE_EDIT_UTF8_CONTENT {
        return Err("Edited subtitle content exceeds the 200 MB text limit".to_string());
    }

    let bom: &[u8] = if had_bom {
        match encoding_id {
            "UTF-8" => &[0xEF, 0xBB, 0xBF],
            "UTF-16LE" => &[0xFF, 0xFE],
            "UTF-16BE" => &[0xFE, 0xFF],
            _ => return Err("Only Unicode source encodings may carry a BOM".to_string()),
        }
    } else {
        &[]
    };

    // encoding_rs intentionally has no UTF-16 encoder: WHATWG text
    // encoding treats UTF-16 labels as decode-only and emits UTF-8 when
    // asked to encode. Preserve UTF-16 explicitly so a BOM is never paired
    // with UTF-8 payload bytes.
    let encoded = match encoding_id {
        "UTF-16LE" => content
            .encode_utf16()
            .flat_map(u16::to_le_bytes)
            .collect::<Vec<_>>(),
        "UTF-16BE" => content
            .encode_utf16()
            .flat_map(u16::to_be_bytes)
            .collect::<Vec<_>>(),
        _ => {
            let encoding =
                encoding_rs::Encoding::for_label(encoding_id.as_bytes()).ok_or_else(|| {
                    "Source encoding is not supported for lossless output".to_string()
                })?;
            if encoding.name() != encoding_id {
                return Err("Source encoding identifier is not canonical".to_string());
            }
            let (encoded, _, had_errors) = encoding.encode(content);
            if had_errors {
                return Err(format!(
                    "The edited text cannot be represented in the original {encoding_id} encoding"
                ));
            }
            encoded.into_owned()
        }
    };

    let output_len = bom
        .len()
        .checked_add(encoded.len())
        .ok_or_else(|| "Edited subtitle size overflow".to_string())?;
    if output_len > MAX_TEXT_SIZE as usize {
        return Err("Encoded output exceeds the 50 MB subtitle limit".to_string());
    }

    let mut output = Vec::with_capacity(output_len);
    output.extend_from_slice(bom);
    output.extend_from_slice(&encoded);
    Ok(output)
}

// ── Inner helpers (testable without an AppHandle) ────────────────

// no top-of-function `is_reparse_point` re-check
// for safe_write_text_file_inner, unlike safe_copy_file_inner and
// safe_rename_file_inner which re-check before the final fs syscall.
// The asymmetry is intentional: write's atomic guarantee comes from
// `OpenOptions::create_new(true)` in `create_new_and_write_bytes` — a
// direct POSIX `O_EXCL` / Windows `CREATE_NEW` open that refuses to
// follow a planted symlink at the destination regardless of whether
// the path was a reparse point when `clear_existing_destination`
// observed it. Copy / rename have no equivalent atomic primitive for
// the source side (open is `read-only`, no exclusive-create) or for
// the rename destination (`fs::rename` is not symlink-aware on
// Windows cross-volume), so they need the late re-check; write
// doesn't. Putting one here would be redundant defense without
// closing a real window.
pub fn safe_output_path_exists_inner(
    path: &str,
    is_allowed: impl Fn(&Path) -> bool,
) -> Result<bool, String> {
    validate_ipc_path(path, "Output")?;
    let path_ref = Path::new(path);
    check_output_probe_extension(path_ref, "Output")?;
    check_scope_allows(&is_allowed, path_ref, "Output")?;
    check_scope_allows_resolved(&is_allowed, path_ref, "Output")?;

    // Use symlink_metadata instead of Path::exists so an output-slot
    // symlink or junction, including a dangling one, still counts as an
    // occupied destination during overwrite preflight.
    match fs::symlink_metadata(path_ref) {
        Ok(_) => Ok(true),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(format!("Failed to check output path existence: {e}")),
    }
}
pub fn safe_write_text_file_inner(
    path: &str,
    content: &str,
    overwrite: bool,
    is_allowed: impl Fn(&Path) -> bool,
) -> Result<(), String> {
    validate_ipc_path(path, "Output")?;
    let path_ref = Path::new(path);
    check_subtitle_extension(path_ref, "Output")?;
    check_scope_allows(&is_allowed, path_ref, "Output")?;
    check_scope_allows_resolved(&is_allowed, path_ref, "Output")?;
    check_plain_text_size(content.len())?;
    ensure_parent_dir(path_ref)?;
    clear_existing_destination(path_ref, overwrite)?;
    create_new_and_write_bytes(path_ref, content.as_bytes())
}

/// Create a new ASS/SSA style-edit output while preserving the source
/// encoding and refusing a stale preview. The expected revision is SHA-256
/// over the exact bytes returned by the earlier encoding-aware read.
pub fn safe_write_style_edit_output_inner(
    source_path: &str,
    expected_revision: &str,
    output_path: &str,
    content: &str,
    is_allowed: impl Fn(&Path) -> bool,
) -> Result<(), String> {
    if expected_revision.len() != 64
        || !expected_revision
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit())
    {
        return Err("Invalid source revision".to_string());
    }

    validate_ipc_path(source_path, "Source")?;
    validate_ipc_path(output_path, "Output")?;
    let source_ref = Path::new(source_path);
    let output_ref = Path::new(output_path);
    let source_ext = check_ass_style_extension(source_ref, "Source")?;
    let output_ext = check_ass_style_extension(output_ref, "Output")?;
    if source_ext != output_ext {
        return Err("Style-edit output must preserve the source .ass/.ssa extension".to_string());
    }
    if source_ref == output_ref {
        return Err("Style-edit output must not be the source file".to_string());
    }
    // Catch case-only or separator/spelling aliases while the source still
    // exists. The later exclusive create-new open remains the race-time gate.
    reject_same_canonical_path(source_ref, output_ref)?;

    let source_parent = source_ref
        .parent()
        .ok_or_else(|| "Source subtitle has no parent directory".to_string())?
        .canonicalize()
        .map_err(|e| format!("Failed to resolve source directory: {e}"))?;
    let output_parent = output_ref
        .parent()
        .ok_or_else(|| "Output subtitle has no parent directory".to_string())?
        .canonicalize()
        .map_err(|e| format!("Failed to resolve output directory: {e}"))?;
    if source_parent != output_parent {
        return Err("Style-edit output must be a sibling of the source file".to_string());
    }

    check_scope_allows(&is_allowed, output_ref, "Output")?;
    check_scope_allows_resolved(&is_allowed, output_ref, "Output")?;

    let source = read_text_detect_encoding_inner(source_path, &is_allowed)?;
    if source.lossy {
        return Err(
            "Source decoding was lossy; refusing to write an irreversible edit".to_string(),
        );
    }
    if source.source_revision != expected_revision.to_ascii_lowercase() {
        return Err(
            "Source changed after preview; review the file again before writing".to_string(),
        );
    }

    let bytes = encode_text_preserving_source(content, &source.encoding_id, source.had_bom)?;
    let final_source = read_text_detect_encoding_inner(source_path, &is_allowed)?;
    if final_source.source_revision != expected_revision.to_ascii_lowercase() {
        return Err(
            "Source changed after preview; review the file again before writing".to_string(),
        );
    }
    // Encoding may be relatively expensive at the 200 MiB decoded-content
    // boundary. Narrow the source/ancestor swap window before the exclusive
    // destination open by repeating the authoritative path and revision
    // checks after that work.
    let final_source_parent = source_ref
        .parent()
        .ok_or_else(|| "Source subtitle has no parent directory".to_string())?
        .canonicalize()
        .map_err(|e| format!("Failed to re-resolve source directory: {e}"))?;
    let final_output_parent = output_ref
        .parent()
        .ok_or_else(|| "Output subtitle has no parent directory".to_string())?
        .canonicalize()
        .map_err(|e| format!("Failed to re-resolve output directory: {e}"))?;
    if final_source_parent != final_output_parent || final_source_parent != source_parent {
        return Err("Style-edit source or output directory changed before writing".to_string());
    }
    check_scope_allows(&is_allowed, output_ref, "Output")?;
    check_scope_allows_resolved(&is_allowed, output_ref, "Output")?;
    // No overwrite path exists for this command. create_new is the
    // authoritative collision/race gate even when preview saw no output.
    create_new_and_write_bytes_exclusive(output_ref, &bytes)
}

/// File-preserving copies intentionally have no decoded-text size cap: they
/// stream bytes and also support opaque subtitle sidecars such as `.sup`.
/// The source is opened and confirmed to be a regular file before an existing
/// destination is removed. Once copying begins, a destination write failure can
/// still leave a partial file; the source remains intact so the user can retry.
pub fn safe_copy_file_inner(
    src: &str,
    dst: &str,
    overwrite: bool,
    is_allowed: impl Fn(&Path) -> bool,
) -> Result<(), String> {
    validate_ipc_path(src, "Source")?;
    validate_ipc_path(dst, "Destination")?;
    let src_ref = Path::new(src);
    let dst_ref = Path::new(dst);
    check_copy_rename_extensions(src_ref, dst_ref)?;
    check_scope_allows(&is_allowed, src_ref, "Source")?;
    check_scope_allows(&is_allowed, dst_ref, "Destination")?;
    check_scope_allows_resolved(&is_allowed, src_ref, "Source")?;
    check_scope_allows_resolved(&is_allowed, dst_ref, "Destination")?;
    reject_reparse_source(src_ref, "copy")?;
    reject_same_canonical_path(src_ref, dst_ref)?;
    ensure_parent_dir(dst_ref)?;

    // Re-check `is_reparse_point` immediately before `File::open` to narrow the
    // TOCTOU window where an
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
    let source_metadata = source
        .metadata()
        .map_err(|e| format!("Failed to inspect opened source: {e}"))?;
    if !source_metadata.is_file() {
        return Err(format!(
            "Source is not a regular file: {}",
            src_ref.display()
        ));
    }

    // Only after the source is ready do we remove an existing destination.
    // Missing, locked, or directory-shaped sources must never destroy the old
    // destination before the copy has even started.
    clear_existing_destination(dst_ref, overwrite)?;
    let mut destination = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(dst_ref)
        .map_err(|e| format!("Failed to create destination: {e}"))?;
    std::io::copy(&mut source, &mut destination)
        .map(|_| ())
        .map_err(|error| partial_output_error("copy file", &error))
}

/// File-preserving renames intentionally have no decoded-text size cap: they
/// move filesystem entries and also support opaque subtitle sidecars such as
/// `.sup`.
pub fn safe_rename_file_inner(
    src: &str,
    dst: &str,
    overwrite: bool,
    is_allowed: impl Fn(&Path) -> bool,
) -> Result<(), String> {
    safe_rename_file_inner_with_rename(src, dst, overwrite, is_allowed, |source, destination| {
        fs::rename(source, destination)
    })
}

fn safe_rename_file_inner_with_rename(
    src: &str,
    dst: &str,
    overwrite: bool,
    is_allowed: impl Fn(&Path) -> bool,
    rename_file: impl FnOnce(&Path, &Path) -> std::io::Result<()>,
) -> Result<(), String> {
    validate_ipc_path(src, "Source")?;
    validate_ipc_path(dst, "Destination")?;
    let src_ref = Path::new(src);
    let dst_ref = Path::new(dst);
    check_copy_rename_extensions(src_ref, dst_ref)?;
    check_scope_allows(&is_allowed, src_ref, "Source")?;
    check_scope_allows(&is_allowed, dst_ref, "Destination")?;
    check_scope_allows_resolved(&is_allowed, src_ref, "Source")?;
    check_scope_allows_resolved(&is_allowed, dst_ref, "Destination")?;
    reject_reparse_source(src_ref, "rename")?;
    reject_same_canonical_path(src_ref, dst_ref)?;
    ensure_parent_dir(dst_ref)?;
    // Keep an ordinary existing destination in place until the OS performs the
    // rename. A cross-volume, permission, or sharing failure must not destroy
    // the user's prior destination.
    inspect_existing_destination(dst_ref, overwrite)?;

    // Re-check `is_reparse_point` immediately before `fs::rename` for
    // parity with `safe_copy_file_inner`'s open-time re-check.
    // Regardless of platform rename semantics, this helper's contract
    // is that a reparse-point source never reaches the final move
    // operation. The extra syscall keeps the source side aligned with
    // the copy path after any late swap.
    if is_reparse_point(src_ref) {
        log::warn!(
            "Refusing to rename possible symlink / junction at rename-time: {}",
            src_ref.display()
        );
        return Err("Refusing to rename symlink / junction (race-time detection)".to_string());
    }
    // Rename destination TOCTOU symmetry: copy's destination
    // is implicitly protected by `OpenOptions::create_new(true)` at
    // open time; rename has no equivalent atomic guard. Between
    // destination inspection and the `fs::rename` below, an attacker can
    // replace dst with a symlink — same window copy already closes.
    // One syscall (`is_reparse_point` = lstat on POSIX, file_attributes
    // on Windows); race window is narrow but the cost is trivial.
    if is_reparse_point(dst_ref) {
        log::warn!(
            "Refusing to rename to possible symlink / junction at rename-time: {}",
            dst_ref.display()
        );
        return Err(
            "Refusing to rename to symlink / junction destination (race-time detection)"
                .to_string(),
        );
    }

    rename_file(src_ref, dst_ref).map_err(|e| format!("Failed to rename file: {e}"))
}

// ── Blocking command implementations ───────────────────────────
// Async Tauri wrappers live in `ipc_commands`; keeping all scope resolution
// and filesystem work here lets the CLI/tests reuse the same exact behavior.

/// Check whether an output path already exists before a GUI overwrite
/// preflight. This intentionally covers subtitle text outputs and
/// rename-only sidecars, but not arbitrary files.
pub fn safe_output_path_exists(app: tauri::AppHandle, path: String) -> Result<bool, String> {
    let scope = crate::fs_policy::app_fs_scope(&app)?;
    safe_output_path_exists_inner(&path, move |p| scope.is_allowed(p))
}
/// Write a text file safely. Layered defenses: scope deny enforcement,
/// subtitle-extension whitelist, symlink rejection on destination,
/// atomic `create_new(true)` open.
pub fn safe_write_text_file(
    app: tauri::AppHandle,
    path: String,
    content: String,
    overwrite: bool,
) -> Result<(), String> {
    let scope = crate::fs_policy::app_fs_scope(&app)?;
    safe_write_text_file_inner(&path, &content, overwrite, move |p| scope.is_allowed(p))
}

/// Write a previewed style edit to a new sibling file. Existing outputs are
/// never replaced; the exclusive create-new open is the final collision gate.
pub fn safe_write_style_edit_output(
    app: tauri::AppHandle,
    source_path: String,
    expected_revision: String,
    output_path: String,
    content: String,
) -> Result<(), String> {
    let scope = crate::fs_policy::app_fs_scope(&app)?;
    safe_write_style_edit_output_inner(
        &source_path,
        &expected_revision,
        &output_path,
        &content,
        move |path| scope.is_allowed(path),
    )
}

/// Copy `src` to `dst` safely. Both endpoints pass the same gates as
/// `safe_write_text_file`'s destination; source is additionally
/// reparse-point-rejected (a symlinked input would otherwise resolve
/// to e.g. `~/.ssh/id_rsa` and copy its bytes as if they were a
/// subtitle).
pub fn safe_copy_file(
    app: tauri::AppHandle,
    src: String,
    dst: String,
    overwrite: bool,
) -> Result<(), String> {
    let scope = crate::fs_policy::app_fs_scope(&app)?;
    safe_copy_file_inner(&src, &dst, overwrite, move |p| scope.is_allowed(p))
}

/// Rename / move `src` to `dst` safely. Same gating as `safe_copy_file`.
/// `fs::rename` is atomic when the platform supports the requested move;
/// unsupported moves fail without pre-deleting an existing destination. Both
/// endpoints are reparse-checked before the final call so planted shortcuts
/// fail shut.
pub fn safe_rename_file(
    app: tauri::AppHandle,
    src: String,
    dst: String,
    overwrite: bool,
) -> Result<(), String> {
    let scope = crate::fs_policy::app_fs_scope(&app)?;
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

    fn scope_alias_paths(name: &str, file_name: &str) -> (PathBuf, PathBuf, PathBuf) {
        let real_dir = temp_dir(name);
        let resolved = real_dir.canonicalize().unwrap().join(file_name);

        #[cfg(unix)]
        let raw = {
            use std::os::unix::fs::symlink;

            let alias_root = temp_dir(&format!("{name}_alias"));
            let alias = alias_root.join("selected");
            symlink(&real_dir, &alias).unwrap();
            alias.join(file_name)
        };

        #[cfg(windows)]
        let raw = real_dir.join(file_name);

        #[cfg(not(any(unix, windows)))]
        let raw = real_dir.join(file_name);

        #[cfg(any(unix, windows))]
        assert_ne!(
            raw, resolved,
            "scope-alias fixture must change after resolution"
        );

        (real_dir, raw, resolved)
    }

    #[test]
    fn partial_output_errors_are_explicit_without_claiming_residue_exists() {
        let error = std::io::Error::other("injected write failure");
        assert_eq!(
            partial_output_error("write destination", &error),
            "Failed to write destination; a partial output may remain: injected write failure"
        );
        assert_eq!(
            partial_output_error("copy file", &error),
            "Failed to copy file; a partial output may remain: injected write failure"
        );
    }

    #[test]
    fn output_exists_probe_reports_existing_and_missing_outputs() {
        let dir = temp_dir("exists_probe_basic");
        let existing = dir.join("existing.ass");
        let missing = dir.join("missing.ass");
        fs::write(&existing, b"old").unwrap();

        assert!(safe_output_path_exists_inner(&existing.to_string_lossy(), allow_all).unwrap());
        assert!(!safe_output_path_exists_inner(&missing.to_string_lossy(), allow_all).unwrap());
    }

    #[test]
    fn output_exists_probe_allows_sup_sidecar_preflight() {
        let dir = temp_dir("exists_probe_sup");
        let path = dir.join("episode.sup");
        fs::write(&path, b"pgs sidecar").unwrap();

        assert!(safe_output_path_exists_inner(&path.to_string_lossy(), allow_all).unwrap());
    }

    #[test]
    fn output_exists_probe_refuses_non_subtitle_extension() {
        let dir = temp_dir("exists_probe_txt");
        let path = dir.join("note.txt");
        let err = safe_output_path_exists_inner(&path.to_string_lossy(), allow_all).unwrap_err();
        assert!(err.contains("subtitle extension"));
    }

    #[test]
    fn output_exists_probe_refuses_when_scope_denies() {
        let dir = temp_dir("exists_probe_scope");
        let path = dir.join("out.ass");
        let err = safe_output_path_exists_inner(&path.to_string_lossy(), deny_all).unwrap_err();
        assert!(err.contains("filesystem scope"));
    }

    #[test]
    fn resolved_scope_check_rejects_denied_terminal_reparse_parent_before_slot() {
        let denied_parent = PathBuf::from("denied-parent");
        let resolved = ResolvedScopePath {
            path: denied_parent.join("out.ass"),
            terminal_reparse_parent: Some(denied_parent.clone()),
        };

        let err =
            check_resolved_scope_path(&|path| path != denied_parent.as_path(), &resolved, "Output")
                .unwrap_err();

        assert!(err.contains("denied by"), "got: {err}");
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
    fn plain_text_size_accepts_content_at_50_mib_limit() {
        assert!(check_plain_text_size(MAX_TEXT_SIZE as usize).is_ok());
    }

    #[test]
    fn write_refuses_content_one_byte_over_limit_before_overwrite() {
        let dir = temp_dir("write_over_limit");
        let path = dir.join("out.ass");
        fs::write(&path, b"keep me").unwrap();
        let oversized = "x".repeat(MAX_TEXT_SIZE as usize + 1);

        let err = safe_write_text_file_inner(&path.to_string_lossy(), &oversized, true, allow_all)
            .unwrap_err();

        assert_eq!(err, "Subtitle content exceeds the 50 MB limit");
        assert_eq!(fs::read(&path).unwrap(), b"keep me");
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
    fn write_refuses_sup_sidecar_extension() {
        let dir = temp_dir("write_sup_ext");
        let path = dir.join("out.sup");
        let err = safe_write_text_file_inner(&path.to_string_lossy(), "binary?", true, allow_all)
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
    fn write_rechecks_scope_after_canonicalizing_existing_parent() {
        let (dir, raw_path, resolved_path) =
            scope_alias_paths("write_scope_canonical_parent", "out.ass");

        let err = safe_write_text_file_inner(&raw_path.to_string_lossy(), "x", false, move |p| {
            p != resolved_path
        })
        .unwrap_err();
        assert!(err.contains("denied by"), "got: {err}");
        assert!(!dir.join("out.ass").exists());
    }

    fn read_revision(path: &Path) -> String {
        read_text_detect_encoding_inner(&path.to_string_lossy(), allow_all)
            .unwrap()
            .source_revision
    }

    #[test]
    fn style_edit_writer_creates_new_output_and_preserves_source() {
        let dir = temp_dir("style_edit_basic");
        let source = dir.join("episode.ass");
        let output = dir.join("episode.styled.ass");
        fs::write(&source, b"[V4+ Styles]\r\nStyle: Default,Arial,48\r\n").unwrap();
        let revision = read_revision(&source);

        safe_write_style_edit_output_inner(
            &source.to_string_lossy(),
            &revision,
            &output.to_string_lossy(),
            "[V4+ Styles]\r\nStyle: Default,Microsoft YaHei,48\r\n",
            allow_all,
        )
        .unwrap();

        assert_eq!(
            fs::read(&source).unwrap(),
            b"[V4+ Styles]\r\nStyle: Default,Arial,48\r\n"
        );
        assert_eq!(
            fs::read(&output).unwrap(),
            b"[V4+ Styles]\r\nStyle: Default,Microsoft YaHei,48\r\n"
        );
    }

    #[test]
    fn style_edit_writer_refuses_existing_output_without_changing_it() {
        let dir = temp_dir("style_edit_collision");
        let source = dir.join("episode.ass");
        let output = dir.join("episode.styled.ass");
        fs::write(&source, b"source").unwrap();
        fs::write(&output, b"keep me").unwrap();
        let revision = read_revision(&source);

        let err = safe_write_style_edit_output_inner(
            &source.to_string_lossy(),
            &revision,
            &output.to_string_lossy(),
            "replacement",
            allow_all,
        )
        .unwrap_err();

        assert!(err.contains("create destination"), "got: {err}");
        assert_eq!(fs::read(&output).unwrap(), b"keep me");
    }

    #[test]
    fn style_edit_writer_refuses_stale_source_revision() {
        let dir = temp_dir("style_edit_stale");
        let source = dir.join("episode.ass");
        let output = dir.join("episode.styled.ass");
        fs::write(&source, b"first").unwrap();
        let revision = read_revision(&source);
        fs::write(&source, b"second").unwrap();

        let err = safe_write_style_edit_output_inner(
            &source.to_string_lossy(),
            &revision,
            &output.to_string_lossy(),
            "planned from first",
            allow_all,
        )
        .unwrap_err();

        assert!(err.contains("changed after preview"), "got: {err}");
        assert!(!output.exists());
    }

    #[test]
    fn style_edit_writer_preserves_utf8_bom() {
        let dir = temp_dir("style_edit_utf8_bom");
        let source = dir.join("episode.ssa");
        let output = dir.join("episode.styled.ssa");
        fs::write(&source, [0xEF, 0xBB, 0xBF, b'o', b'l', b'd']).unwrap();
        let revision = read_revision(&source);

        safe_write_style_edit_output_inner(
            &source.to_string_lossy(),
            &revision,
            &output.to_string_lossy(),
            "new",
            allow_all,
        )
        .unwrap();

        assert_eq!(
            fs::read(&output).unwrap(),
            [0xEF, 0xBB, 0xBF, b'n', b'e', b'w']
        );
    }

    #[test]
    fn style_edit_writer_preserves_utf16le_bom() {
        let dir = temp_dir("style_edit_utf16le");
        let source = dir.join("episode.ass");
        let output = dir.join("episode.styled.ass");
        fs::write(&source, [0xFF, 0xFE, b'A', 0x00]).unwrap();
        let revision = read_revision(&source);

        safe_write_style_edit_output_inner(
            &source.to_string_lossy(),
            &revision,
            &output.to_string_lossy(),
            "AB",
            allow_all,
        )
        .unwrap();

        assert_eq!(
            fs::read(&output).unwrap(),
            [0xFF, 0xFE, b'A', 0x00, b'B', 0x00]
        );
    }

    #[test]
    fn style_edit_writer_preserves_utf16be_bom() {
        let dir = temp_dir("style_edit_utf16be");
        let source = dir.join("episode.ass");
        let output = dir.join("episode.styled.ass");
        fs::write(&source, [0xFE, 0xFF, 0x00, b'A']).unwrap();
        let revision = read_revision(&source);

        safe_write_style_edit_output_inner(
            &source.to_string_lossy(),
            &revision,
            &output.to_string_lossy(),
            "AB",
            allow_all,
        )
        .unwrap();

        assert_eq!(
            fs::read(&output).unwrap(),
            [0xFE, 0xFF, 0x00, b'A', 0x00, b'B']
        );
    }

    #[test]
    fn style_edit_encoding_round_trips_common_legacy_encodings() {
        for (encoding_id, text) in [
            ("GBK", "简体字幕"),
            ("Big5", "繁體字幕"),
            ("Shift_JIS", "日本語字幕"),
        ] {
            let bytes = encode_text_preserving_source(text, encoding_id, false).unwrap();
            let encoding = encoding_rs::Encoding::for_label(encoding_id.as_bytes()).unwrap();
            let (decoded, _, had_errors) = encoding.decode(&bytes);
            assert!(!had_errors, "{encoding_id} output should decode cleanly");
            assert_eq!(decoded, text, "{encoding_id} round trip");
        }
    }

    #[test]
    fn style_edit_writer_refuses_lossy_source() {
        let dir = temp_dir("style_edit_lossy");
        let source = dir.join("episode.ass");
        let output = dir.join("episode.styled.ass");
        fs::write(&source, [0xEF, 0xBB, 0xBF, 0xFF]).unwrap();
        let revision = read_revision(&source);

        let err = safe_write_style_edit_output_inner(
            &source.to_string_lossy(),
            &revision,
            &output.to_string_lossy(),
            "replacement",
            allow_all,
        )
        .unwrap_err();

        assert!(err.contains("lossy"), "got: {err}");
        assert!(!output.exists());
    }

    #[test]
    fn style_edit_encoding_rejects_unrepresentable_text() {
        let err = encode_text_preserving_source("微软雅黑", "windows-1252", false).unwrap_err();
        assert!(err.contains("cannot be represented"), "got: {err}");
    }

    #[test]
    fn style_edit_writer_requires_matching_ass_or_ssa_extension() {
        let dir = temp_dir("style_edit_extension");
        let source = dir.join("episode.ass");
        let output = dir.join("episode.styled.ssa");
        fs::write(&source, b"source").unwrap();
        let revision = read_revision(&source);

        let err = safe_write_style_edit_output_inner(
            &source.to_string_lossy(),
            &revision,
            &output.to_string_lossy(),
            "replacement",
            allow_all,
        )
        .unwrap_err();

        assert!(err.contains("preserve the source"), "got: {err}");
        assert!(!output.exists());
    }

    #[test]
    fn style_edit_writer_refuses_the_source_path_as_output() {
        let dir = temp_dir("style_edit_self_output");
        let source = dir.join("episode.ass");
        fs::write(&source, b"source").unwrap();
        let revision = read_revision(&source);

        let err = safe_write_style_edit_output_inner(
            &source.to_string_lossy(),
            &revision,
            &source.to_string_lossy(),
            "replacement",
            allow_all,
        )
        .unwrap_err();

        assert!(err.contains("must not be the source"), "got: {err}");
        assert_eq!(fs::read(&source).unwrap(), b"source");
    }

    #[test]
    fn style_edit_writer_requires_a_sibling_output() {
        let dir = temp_dir("style_edit_sibling");
        let other = dir.join("other");
        fs::create_dir_all(&other).unwrap();
        let source = dir.join("episode.ass");
        let output = other.join("episode.styled.ass");
        fs::write(&source, b"source").unwrap();
        let revision = read_revision(&source);

        let err = safe_write_style_edit_output_inner(
            &source.to_string_lossy(),
            &revision,
            &output.to_string_lossy(),
            "replacement",
            allow_all,
        )
        .unwrap_err();

        assert!(err.contains("sibling"), "got: {err}");
        assert!(!output.exists());
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
    fn copy_missing_source_preserves_existing_destination() {
        let dir = temp_dir("copy_missing_source");
        let src = dir.join("missing.ass");
        let dst = dir.join("dst.ass");
        fs::write(&dst, b"old destination").unwrap();

        let err = safe_copy_file_inner(
            &src.to_string_lossy(),
            &dst.to_string_lossy(),
            true,
            allow_all,
        )
        .unwrap_err();

        assert!(err.to_ascii_lowercase().contains("source"), "got: {err}");
        assert_eq!(fs::read(&dst).unwrap(), b"old destination");
    }

    #[test]
    fn copy_directory_source_preserves_existing_destination() {
        let dir = temp_dir("copy_directory_source");
        let src = dir.join("folder.ass");
        let dst = dir.join("dst.ass");
        fs::create_dir(&src).unwrap();
        fs::write(&dst, b"old destination").unwrap();

        let err = safe_copy_file_inner(
            &src.to_string_lossy(),
            &dst.to_string_lossy(),
            true,
            allow_all,
        )
        .unwrap_err();

        assert!(err.to_ascii_lowercase().contains("source"), "got: {err}");
        assert_eq!(fs::read(&dst).unwrap(), b"old destination");
    }

    #[test]
    fn copy_allows_sup_sidecar_when_extension_is_preserved() {
        let dir = temp_dir("copy_sup_sidecar");
        let src = dir.join("src.sup");
        let dst = dir.join("dst.sup");
        fs::write(&src, b"pgs-binary").unwrap();
        safe_copy_file_inner(
            &src.to_string_lossy(),
            &dst.to_string_lossy(),
            false,
            allow_all,
        )
        .unwrap();
        assert_eq!(fs::read(&dst).unwrap(), b"pgs-binary");
        assert!(src.exists());
    }

    #[test]
    fn copy_refuses_sup_to_text_extension_laundering() {
        let dir = temp_dir("copy_sup_to_text");
        let src = dir.join("src.sup");
        let dst = dir.join("dst.ass");
        fs::write(&src, b"pgs-binary").unwrap();
        let err = safe_copy_file_inner(
            &src.to_string_lossy(),
            &dst.to_string_lossy(),
            false,
            allow_all,
        )
        .unwrap_err();
        assert!(err.contains("preserve the sidecar extension"), "got: {err}");
        assert!(!dst.exists());
    }

    #[test]
    fn copy_refuses_text_to_sup_extension_laundering() {
        let dir = temp_dir("copy_text_to_sup");
        let src = dir.join("src.ass");
        let dst = dir.join("dst.sup");
        fs::write(&src, b"text").unwrap();
        let err = safe_copy_file_inner(
            &src.to_string_lossy(),
            &dst.to_string_lossy(),
            false,
            allow_all,
        )
        .unwrap_err();
        assert!(err.contains("preserve the sidecar extension"), "got: {err}");
        assert!(!dst.exists());
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
    fn copy_rechecks_destination_scope_after_canonicalizing_existing_parent() {
        let (dir, raw_dst, resolved_dst) =
            scope_alias_paths("copy_scope_canonical_parent", "dst.ass");
        let src = dir.join("src.ass");
        fs::write(&src, b"payload").unwrap();

        let err = safe_copy_file_inner(
            &src.to_string_lossy(),
            &raw_dst.to_string_lossy(),
            false,
            move |p| p != resolved_dst,
        )
        .unwrap_err();
        assert!(err.contains("denied by"), "got: {err}");
        assert!(!dir.join("dst.ass").exists());
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

    #[test]
    fn rename_overwrite_failure_preserves_source_and_existing_destination() {
        let dir = temp_dir("rename_overwrite_failure");
        let src = dir.join("src.ass");
        let dst = dir.join("dst.ass");
        fs::write(&src, b"new payload").unwrap();
        fs::write(&dst, b"old destination").unwrap();

        let err = safe_rename_file_inner_with_rename(
            &src.to_string_lossy(),
            &dst.to_string_lossy(),
            true,
            allow_all,
            |source, destination| {
                assert_eq!(fs::read(source).unwrap(), b"new payload");
                assert_eq!(fs::read(destination).unwrap(), b"old destination");
                Err(std::io::Error::other("simulated cross-volume failure"))
            },
        )
        .unwrap_err();

        assert!(err.contains("simulated cross-volume failure"), "got: {err}");
        assert_eq!(fs::read(&src).unwrap(), b"new payload");
        assert_eq!(fs::read(&dst).unwrap(), b"old destination");
    }

    #[test]
    fn rename_overwrites_existing_destination_when_flag_set() {
        let dir = temp_dir("rename_overwrite_success");
        let src = dir.join("src.ass");
        let dst = dir.join("dst.ass");
        fs::write(&src, b"new payload").unwrap();
        fs::write(&dst, b"old destination").unwrap();

        safe_rename_file_inner(
            &src.to_string_lossy(),
            &dst.to_string_lossy(),
            true,
            allow_all,
        )
        .unwrap();

        assert!(!src.exists());
        assert_eq!(fs::read(&dst).unwrap(), b"new payload");
    }

    #[test]
    fn rename_refuses_existing_destination_before_rename_when_overwrite_is_unset() {
        let dir = temp_dir("rename_no_overwrite");
        let src = dir.join("src.ass");
        let dst = dir.join("dst.ass");
        fs::write(&src, b"new payload").unwrap();
        fs::write(&dst, b"old destination").unwrap();

        let err = safe_rename_file_inner_with_rename(
            &src.to_string_lossy(),
            &dst.to_string_lossy(),
            false,
            allow_all,
            |_, _| -> std::io::Result<()> {
                panic!("rename callback must not run when overwrite is disabled")
            },
        )
        .unwrap_err();

        assert!(err.contains("overwrite not enabled"), "got: {err}");
        assert_eq!(fs::read(&src).unwrap(), b"new payload");
        assert_eq!(fs::read(&dst).unwrap(), b"old destination");
    }

    #[test]
    fn rename_allows_sup_sidecar_when_extension_is_preserved() {
        let dir = temp_dir("rename_sup_sidecar");
        let src = dir.join("src.sup");
        let dst = dir.join("dst.sup");
        fs::write(&src, b"pgs-binary").unwrap();
        safe_rename_file_inner(
            &src.to_string_lossy(),
            &dst.to_string_lossy(),
            false,
            allow_all,
        )
        .unwrap();
        assert!(!src.exists());
        assert_eq!(fs::read(&dst).unwrap(), b"pgs-binary");
    }

    #[test]
    fn rename_refuses_text_to_sup_extension_laundering() {
        let dir = temp_dir("rename_text_to_sup");
        let src = dir.join("src.ass");
        let dst = dir.join("dst.sup");
        fs::write(&src, b"text").unwrap();
        let err = safe_rename_file_inner(
            &src.to_string_lossy(),
            &dst.to_string_lossy(),
            false,
            allow_all,
        )
        .unwrap_err();
        assert!(err.contains("preserve the sidecar extension"), "got: {err}");
        assert!(src.exists());
        assert!(!dst.exists());
    }

    #[test]
    fn rename_refuses_sup_to_text_extension_laundering() {
        let dir = temp_dir("rename_sup_to_text");
        let src = dir.join("src.sup");
        let dst = dir.join("dst.ass");
        fs::write(&src, b"pgs-binary").unwrap();
        let err = safe_rename_file_inner(
            &src.to_string_lossy(),
            &dst.to_string_lossy(),
            false,
            allow_all,
        )
        .unwrap_err();
        assert!(err.contains("preserve the sidecar extension"), "got: {err}");
        assert!(src.exists());
        assert!(!dst.exists());
    }

    #[test]
    fn rename_rechecks_destination_scope_after_canonicalizing_existing_parent() {
        let (dir, raw_dst, resolved_dst) =
            scope_alias_paths("rename_scope_canonical_parent", "dst.ass");
        let src = dir.join("src.ass");
        fs::write(&src, b"payload").unwrap();

        let err = safe_rename_file_inner(
            &src.to_string_lossy(),
            &raw_dst.to_string_lossy(),
            false,
            move |p| p != resolved_dst,
        )
        .unwrap_err();
        assert!(err.contains("denied by"), "got: {err}");
        assert!(src.exists());
        assert!(!dir.join("dst.ass").exists());
    }

    // Symlink tests are POSIX-only because Windows symlink creation may require
    // admin or Developer Mode. Windows still compile-checks the shared
    // reparse-point branches; these fixtures pin the filesystem behavior on
    // platforms where symlink creation is reliable in unprivileged tests.
    #[cfg(unix)]
    #[test]
    fn output_exists_probe_treats_dangling_symlink_as_occupied() {
        use std::os::unix::fs::symlink;
        let dir = temp_dir("exists_probe_dangling_symlink");
        let target = dir.join("missing.ass");
        let link = dir.join("out.ass");
        symlink(&target, &link).unwrap();

        assert!(safe_output_path_exists_inner(&link.to_string_lossy(), allow_all).unwrap());
    }

    #[cfg(unix)]
    #[test]
    fn output_exists_probe_resolves_parent_alias_before_scope_checking_dangling_symlink() {
        use std::os::unix::fs::symlink;
        let dir = temp_dir("exists_probe_dangling_scope");
        let real_parent = dir.join("real");
        let alias_parent = dir.join("alias");
        fs::create_dir(&real_parent).unwrap();
        symlink(&real_parent, &alias_parent).unwrap();
        let target = real_parent.join("missing.ass");
        let real_link = real_parent.join("out.ass");
        symlink(&target, &real_link).unwrap();
        let raw_link = alias_parent.join("out.ass");
        let denied_slot = real_parent.canonicalize().unwrap().join("out.ass");

        let err = safe_output_path_exists_inner(&raw_link.to_string_lossy(), move |path| {
            path != denied_slot
        })
        .unwrap_err();

        assert!(err.contains("denied by"), "got: {err}");
    }

    #[cfg(unix)]
    #[test]
    fn output_exists_probe_checks_denied_parent_before_relative_dangling_target() {
        use std::os::unix::fs::symlink;
        let dir = temp_dir("exists_probe_relative_dangling_scope");
        let denied_parent = dir.canonicalize().unwrap();
        let link = dir.join("out.ass");
        symlink("missing.ass", &link).unwrap();

        // Mirror Tauri 2.11's relevant callback behavior: a terminal symlink
        // is replaced with read_link's result before policy matching. Its
        // relative target therefore looks allowed unless our resolver checks
        // the canonical parent independently.
        let err = safe_output_path_exists_inner(&link.to_string_lossy(), move |path| {
            if path == denied_parent {
                return false;
            }
            if fs::symlink_metadata(path)
                .map(|metadata| metadata.file_type().is_symlink())
                .unwrap_or(false)
            {
                return fs::read_link(path)
                    .map(|target| target == Path::new("missing.ass"))
                    .unwrap_or(false);
            }
            true
        })
        .unwrap_err();

        assert!(err.contains("denied by"), "got: {err}");
    }

    #[cfg(unix)]
    #[test]
    fn write_refuses_dangling_symlink_destination() {
        use std::os::unix::fs::symlink;
        let dir = temp_dir("write_dangling_symlink_dst");
        let target = dir.join("missing.ass");
        let link = dir.join("out.ass");
        symlink(&target, &link).unwrap();

        let err =
            safe_write_text_file_inner(&link.to_string_lossy(), "replacement", true, allow_all)
                .unwrap_err();

        assert!(err.contains("symlink"), "got: {err}");
        assert!(fs::symlink_metadata(&link).is_ok());
        assert!(!target.exists());
    }

    #[cfg(unix)]
    #[test]
    fn rename_refuses_live_and_dangling_symlink_destinations() {
        use std::os::unix::fs::symlink;
        let root = temp_dir("rename_symlink_dst");

        for (case, target_exists) in [("live", true), ("dangling", false)] {
            let dir = root.join(case);
            fs::create_dir(&dir).unwrap();
            let source = dir.join("src.ass");
            let target = dir.join("target.ass");
            let link = dir.join("dst.ass");
            fs::write(&source, b"new payload").unwrap();
            if target_exists {
                fs::write(&target, b"sensitive target").unwrap();
            }
            symlink("target.ass", &link).unwrap();

            let err = safe_rename_file_inner(
                &source.to_string_lossy(),
                &link.to_string_lossy(),
                true,
                allow_all,
            )
            .unwrap_err();

            assert!(err.contains("symlink"), "{case}: got {err}");
            assert_eq!(fs::read(&source).unwrap(), b"new payload", "{case}");
            assert!(fs::symlink_metadata(&link).is_ok(), "{case}");
            if target_exists {
                assert_eq!(fs::read(&target).unwrap(), b"sensitive target", "{case}");
            } else {
                assert!(!target.exists(), "{case}");
            }
        }
    }

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
