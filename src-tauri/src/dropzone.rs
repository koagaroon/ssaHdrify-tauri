//! Drag-drop path expansion.
//!
//! Frontend hands a flat list of paths from a drag-drop event (files,
//! folders, or a mix) and gets back an expanded flat list of file paths.
//! Folders are walked exactly one level deep — typical fan-sub workflow
//! drops one folder per show, never nested hierarchies. Categorization
//! into video vs subtitle is the consumer's job; this command's contract
//! is only "give me the regular files behind these dropped paths".
//!
//! Defense: apply the app-owned filesystem scope before touching each
//! dropped path and before returning discovered children. Hidden entries,
//! symlinks, and reparse points are also skipped so a mistakenly-dropped
//! junction can't fan out into a protected directory (`.ssh`, OneDrive,
//! etc.).

use crate::util::{is_reparse_point, MAX_INPUT_PATHS};
use serde::Serialize;
use std::path::Path;

/// Cap on the number of regular files `expand_dropped_paths` will
/// return in a single call. Truncates beyond this point (with the
/// `ExpandedPaths.truncated` flag set when there's known unprocessed
/// input). `pub` so the CLI shell can interpolate the constant into
/// its truncation warning without hardcoding a number that drifts
/// out of sync.
pub const MAX_RESULT_FILES: usize = 5000;

/// Result of `expand_dropped_paths`. `truncated` surfaces when the
/// expansion stopped at `MAX_RESULT_FILES` so the frontend can render
/// a banner instead of silently presenting an incomplete list (the
/// earlier shape was a bare `Vec<String>` return with a `log::warn`
/// the frontend couldn't see, which made truncation too easy to miss.
///
/// `max_files` carries `MAX_RESULT_FILES` to the frontend so the
/// truncation banner shows the correct number without a TS-side
/// mirrored constant. Previously the frontend hardcoded 5000 with a
/// WHY comment pointing at this Rust const; a bump on this side
/// would have silently drifted the user-facing wording. Threading
/// the value keeps the UI text derived from the Rust source of truth.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExpandedPaths {
    pub files: Vec<String>,
    pub truncated: bool,
    pub max_files: usize,
}

impl Default for ExpandedPaths {
    fn default() -> Self {
        Self {
            files: Vec::new(),
            truncated: false,
            max_files: MAX_RESULT_FILES,
        }
    }
}

/// Expand dropped paths into a flat list of file paths under an injected
/// filesystem-scope policy. The GUI supplies its app-owned scope; the CLI
/// supplies an allow-all predicate because argv paths are user-authored.
pub fn expand_dropped_paths_inner<F>(
    paths: Vec<String>,
    is_allowed: F,
) -> Result<ExpandedPaths, String>
where
    F: Fn(&Path) -> bool,
{
    expand_dropped_paths_inner_with_cap(paths, is_allowed, MAX_RESULT_FILES)
}

fn expand_dropped_paths_inner_with_cap<F>(
    paths: Vec<String>,
    is_allowed: F,
    max_result_files: usize,
) -> Result<ExpandedPaths, String>
where
    F: Fn(&Path) -> bool,
{
    if paths.is_empty() {
        return Ok(ExpandedPaths {
            files: Vec::new(),
            truncated: false,
            max_files: max_result_files,
        });
    }
    if paths.len() > MAX_INPUT_PATHS {
        return Err(format!(
            "Too many paths dropped (got {}, max {MAX_INPUT_PATHS})",
            paths.len()
        ));
    }

    let mut result: Vec<String> = Vec::new();
    let mut truncated = false;
    let mut rejected = 0usize;
    let total_inputs = paths.len();
    for (idx, raw) in paths.iter().enumerate() {
        if crate::util::validate_ipc_path(raw, "Dropped").is_err() {
            rejected += 1;
            continue;
        }
        let p = Path::new(raw);
        if !is_allowed(p) {
            log::warn!("dropzone: skipping path denied by filesystem scope");
            rejected += 1;
            continue;
        }
        let meta = match std::fs::symlink_metadata(p) {
            Ok(m) => m,
            Err(e) => {
                log::warn!("dropzone: stat failed for {raw}: {e}");
                rejected += 1;
                continue;
            }
        };
        if is_reparse_point(p) {
            log::warn!("dropzone: skipping reparse point at {raw}");
            rejected += 1;
            continue;
        }
        if meta.file_type().is_file() {
            result.push(raw.clone());
        } else if meta.file_type().is_dir()
            && walk_one_level(p, &mut result, &is_allowed, max_result_files)
        {
            truncated = true;
        }
        if result.len() >= max_result_files {
            log::warn!("dropzone: result cap {max_result_files} reached, dropping remainder");
            if idx + 1 < total_inputs {
                truncated = true;
            }
            break;
        }
    }
    if rejected > 0 {
        log::warn!(
            "dropzone: dropped {rejected} of {} input path(s) — see warnings above",
            paths.len()
        );
    }
    Ok(ExpandedPaths {
        files: result,
        truncated,
        max_files: max_result_files,
    })
}

/// Blocking command implementation. The async Tauri boundary in
/// `ipc_commands` moves scope resolution and path expansion to the blocking
/// pool before calling this function.
pub fn expand_dropped_paths(
    app: tauri::AppHandle,
    paths: Vec<String>,
) -> Result<ExpandedPaths, String> {
    let scope = crate::fs_policy::app_fs_scope(&app)?;
    expand_dropped_paths_inner(paths, move |path| scope.is_allowed(path))
}

/// Read a directory one level deep, appending regular files. Hidden
/// entries (Unix dotfiles) and reparse points are skipped.
///
/// Dotfile filter is a UX choice — drag-drop users almost never want
/// `.DS_Store`, `.git/`, `.editorconfig`, etc. coming along for the
/// ride. A `.legitimate.ass` placed by a user who actively wanted it
/// will be silently dropped; that's accepted scope. Adversarially this
/// is not exploitable as a read-extension surface — caller already
/// validates extensions downstream.
/// Returns `true` if the walk stopped at MAX_RESULT_FILES mid-folder
/// — used by the outer caller to distinguish "exactly hit the cap"
/// (no truncation) from "would have appended more but capped" (real
/// truncation). The cap could be hit by either an in-walk break OR
/// the very last entry pushing `out` exactly to the cap; in the
/// latter case we report `false` because no remainder existed in
/// this folder. The caller still independently checks `idx + 1 <
/// total_inputs` for the cross-input case.
fn walk_one_level<F>(
    dir: &Path,
    out: &mut Vec<String>,
    is_allowed: &F,
    max_result_files: usize,
) -> bool
where
    F: Fn(&Path) -> bool,
{
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(e) => {
            log::warn!("dropzone: read_dir failed for {dir:?}: {e}");
            return false;
        }
    };
    // Use match-on-Result so per-entry I/O errors are at least logged
    // (`entries.flatten()` would silently swallow them, hiding broken
    // permissions or stale-NFS-handle situations).
    for entry_result in entries {
        if out.len() >= max_result_files {
            // Shared cap with `expand_dropped_paths` outer loop
            // : both layers read the same constant so
            // the per-folder check here participates in the same total
            // budget as the cross-input check there. Splitting them
            // into separate constants would let one layer truncate
            // before the other could even start.
            //
            // Truncated mid-folder — there are more entries in this
            // dir that we'd have appended if there were room.
            return true;
        }
        let entry = match entry_result {
            Ok(e) => e,
            Err(e) => {
                log::warn!("dropzone: read_dir entry failed in {dir:?}: {e}");
                continue;
            }
        };
        let entry_path = entry.path();
        if let Some(name) = entry_path.file_name().and_then(|n| n.to_str()) {
            if name.starts_with('.') {
                continue;
            }
        }
        // A parent directory may be allowed while an exact child path is
        // denied. Filter before metadata/type inspection and never log the
        // child name, otherwise the expansion response or logs would leak
        // the denied entry discovered by read_dir.
        if !is_allowed(&entry_path) {
            continue;
        }
        if is_reparse_point(&entry_path) {
            continue;
        }
        // Note: `is_reparse_point` calls `symlink_metadata` and
        // `entry.file_type()` does its own type lookup. On Windows
        // `DirEntry` caches the file_type so the second call is
        // negligible; on Unix the second call goes through the
        // already-fetched d_type when the FS reports it. Combining
        // into one stat would save a few syscalls on slow FS at the
        // cost of a cross-cfg branch on the reparse-point detection.
        // Accepted as-is for readability.
        let ft = match entry.file_type() {
            Ok(f) => f,
            Err(_) => continue,
        };
        if ft.is_file() {
            if let Some(s) = entry_path.to_str() {
                // Validate the discovered path through
                // `validate_ipc_path` before surfacing.
                // `expand_dropped_paths`'s outer loop runs this gate
                // on every TOP-LEVEL path the frontend hands in;
                // previously the per-folder children added here
                // skipped it — a folder dropped at top-level whose
                // entries
                // happened to carry BiDi / zero-width / control
                // chars in their filenames (untrusted-input: a fan-sub pack
                // could pre-author such files) would push the
                // unscrubbed path into `out`, and the BatchRename
                // pairing UI's `<option>` text / dropdown labels
                // would render the visual-reversal glyph before any
                // downstream IPC re-validation reached it. The path
                // still re-validates on every read attempt (encoding
                // / safe_io / fonts), but the displayed-path
                // surface is the disclosure surface — close it
                // here. Skip-with-log on reject keeps the rest of
                // the folder walkable (a single hostile sibling
                // doesn't poison the legitimate files).
                if let Err(e) = crate::util::validate_ipc_path(s, "Dropzone entry") {
                    log::warn!("dropzone: skipping entry in {dir:?}: {e}");
                    continue;
                }
                out.push(s.to_string());
            }
        }
        // Single-level walk by design — see module doc.
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::io::Write;

    fn make_tempdir(suffix: &str) -> crate::test_support::TestTempDir {
        crate::test_support::TestTempDir::new("ssahdrify_dropzone_test", suffix)
    }

    fn allow_all(_: &std::path::Path) -> bool {
        true
    }

    #[test]
    fn passes_through_files_unchanged() {
        let dir = make_tempdir("passthrough");
        let p = dir.join("a.ass");
        fs::File::create(&p).unwrap().write_all(b"x").unwrap();
        let result =
            expand_dropped_paths_inner(vec![p.to_str().unwrap().to_string()], allow_all).unwrap();
        assert_eq!(result.files.len(), 1);
        assert!(!result.truncated);
        // The response carries the cap so the frontend banner
        // doesn't mirror it as a TS const. Pin both the channel
        // (field present) and the value (sourced from
        // MAX_RESULT_FILES, not a literal).
        assert_eq!(result.max_files, MAX_RESULT_FILES);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn walks_folder_one_level() {
        let dir = make_tempdir("walk");
        for n in ["a.mkv", "b.ass", "c.srt"] {
            let p = dir.join(n);
            fs::File::create(&p).unwrap().write_all(b"x").unwrap();
        }
        // Nested folder + nested file should NOT appear in result.
        let nested = dir.join("nested");
        fs::create_dir(&nested).unwrap();
        fs::File::create(nested.join("ignored.ass"))
            .unwrap()
            .write_all(b"x")
            .unwrap();

        let result =
            expand_dropped_paths_inner(vec![dir.to_str().unwrap().to_string()], allow_all).unwrap();
        assert_eq!(
            result.files.len(),
            3,
            "expected 3 top-level files, got {:?}",
            result.files
        );
        for s in &result.files {
            assert!(!s.contains("ignored.ass"), "nested file leaked: {s}");
        }
        assert!(!result.truncated);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn skips_hidden_entries() {
        let dir = make_tempdir("hidden");
        fs::File::create(dir.join("visible.ass"))
            .unwrap()
            .write_all(b"x")
            .unwrap();
        fs::File::create(dir.join(".hidden.ass"))
            .unwrap()
            .write_all(b"x")
            .unwrap();
        let result =
            expand_dropped_paths_inner(vec![dir.to_str().unwrap().to_string()], allow_all).unwrap();
        assert_eq!(result.files.len(), 1);
        assert!(result.files[0].ends_with("visible.ass"));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn scope_denied_top_level_directory_is_not_enumerated() {
        let dir = make_tempdir("scope_denied_directory");
        fs::File::create(dir.join("must-not-leak.ass"))
            .unwrap()
            .write_all(b"x")
            .unwrap();

        // Deny only the root while allowing its child. If the root gate is
        // ever dropped, the walk will expose must-not-leak.ass and fail.
        let denied_root = dir.to_path_buf();
        let result =
            expand_dropped_paths_inner(vec![dir.to_str().unwrap().to_string()], move |path| {
                path != denied_root.as_path()
            })
            .unwrap();

        assert!(result.files.is_empty());
        assert!(!result.truncated);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn scope_denied_top_level_file_is_not_returned() {
        let dir = make_tempdir("scope_denied_file");
        let file = dir.join("denied.ass");
        fs::File::create(&file).unwrap().write_all(b"x").unwrap();

        let denied_file = file.clone();
        let result =
            expand_dropped_paths_inner(vec![file.to_str().unwrap().to_string()], move |path| {
                path != denied_file.as_path()
            })
            .unwrap();

        assert!(result.files.is_empty());
        assert!(!result.truncated);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn scope_denied_child_is_skipped_without_poisoning_siblings() {
        let dir = make_tempdir("scope_denied_child");
        let allowed = dir.join("allowed.ass");
        let denied = dir.join("denied.ass");
        fs::File::create(&allowed).unwrap().write_all(b"x").unwrap();
        fs::File::create(&denied).unwrap().write_all(b"x").unwrap();

        let denied_child = denied.clone();
        let result =
            expand_dropped_paths_inner(vec![dir.to_str().unwrap().to_string()], move |path| {
                path != denied_child.as_path()
            })
            .unwrap();

        assert_eq!(result.files.len(), 1);
        assert!(result.files[0].ends_with("allowed.ass"));
        assert!(!result.files.iter().any(|path| path.ends_with("denied.ass")));
        assert!(!result.truncated);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn rejects_too_many_input_paths() {
        let many: Vec<String> = (0..(MAX_INPUT_PATHS + 1))
            .map(|i| format!("/tmp/x{i}.ass"))
            .collect();
        assert!(expand_dropped_paths_inner(many, allow_all).is_err());
    }

    /// (boundary-pair discipline): the
    /// over-limit test above pins the rejection direction; this
    /// at-limit counterpart pins that exactly MAX_INPUT_PATHS still
    /// returns Ok (the per-path validation may filter individual
    /// entries, but the input-cap gate itself does not fire).
    /// Without this pair, a refactor that loosened the cap in the
    /// reject direction would silently pass.
    #[test]
    fn accepts_exactly_max_input_paths_boundary() {
        let at_limit: Vec<String> = (0..MAX_INPUT_PATHS)
            .map(|i| format!("/tmp/x{i}.ass"))
            .collect();
        let result = expand_dropped_paths_inner(at_limit, allow_all);
        assert!(
            result.is_ok(),
            "exactly MAX_INPUT_PATHS entries should not trigger the cap-reject; \
             got {result:?}"
        );
    }

    #[test]
    fn rejects_control_chars_in_path() {
        let result =
            expand_dropped_paths_inner(vec!["/tmp/foo\u{0000}bar.ass".to_string()], allow_all)
                .unwrap();
        // Silently skipped, not errored.
        assert!(result.files.is_empty());
        assert!(!result.truncated);
    }

    #[test]
    fn empty_input_returns_empty() {
        let result = expand_dropped_paths_inner(Vec::new(), allow_all).unwrap();
        assert!(result.files.is_empty());
        assert!(!result.truncated);
    }

    #[test]
    fn mixed_file_and_folder_input_combines_both() {
        let dir = make_tempdir("mixed");
        let folder = dir.join("folder");
        fs::create_dir(&folder).unwrap();
        for n in ["x.ass", "y.srt"] {
            fs::File::create(folder.join(n))
                .unwrap()
                .write_all(b"x")
                .unwrap();
        }
        let standalone = dir.join("standalone.mkv");
        fs::File::create(&standalone)
            .unwrap()
            .write_all(b"x")
            .unwrap();
        let result = expand_dropped_paths_inner(
            vec![
                folder.to_str().unwrap().to_string(),
                standalone.to_str().unwrap().to_string(),
            ],
            allow_all,
        )
        .unwrap();
        assert_eq!(
            result.files.len(),
            3,
            "expected 2 from folder + 1 standalone, got {:?}",
            result.files
        );
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn max_result_files_constant_remains_5000() {
        assert_eq!(MAX_RESULT_FILES, 5000);
    }

    #[test]
    fn caps_result_at_max_result_files_and_sets_truncated_flag() {
        let dir = make_tempdir("cap");
        let cap = 25usize;
        let count = cap + 5;
        for i in 0..count {
            fs::File::create(dir.join(format!("f{i}.ass")))
                .unwrap()
                .write_all(b"x")
                .unwrap();
        }
        let result = expand_dropped_paths_inner_with_cap(
            vec![dir.to_str().unwrap().to_string()],
            allow_all,
            cap,
        )
        .unwrap();
        assert_eq!(
            result.files.len(),
            cap,
            "expected cap at {cap}, got {}",
            result.files.len()
        );
        assert_eq!(result.max_files, cap);
        assert!(result.truncated, "truncated flag must be set when cap hits");
    }

    #[test]
    fn exact_cap_with_no_remainder_does_not_set_truncated() {
        let dir = make_tempdir("exact_cap");
        let cap = 25usize;
        for i in 0..cap {
            fs::File::create(dir.join(format!("f{i}.ass")))
                .unwrap()
                .write_all(b"x")
                .unwrap();
        }
        let result = expand_dropped_paths_inner_with_cap(
            vec![dir.to_str().unwrap().to_string()],
            allow_all,
            cap,
        )
        .unwrap();
        assert_eq!(result.files.len(), cap);
        assert!(
            !result.truncated,
            "truncated must NOT be set when exact cap was reached with no remainder"
        );
    }

    #[test]
    fn cross_input_truncation_sets_flag_when_later_paths_remain() {
        let dir = make_tempdir("cross_cap");
        let cap = 10usize;
        let mut inputs = Vec::with_capacity(cap + 2);
        for i in 0..(cap + 2) {
            let p = dir.join(format!("item_{i}.ass"));
            fs::File::create(&p).unwrap().write_all(b"x").unwrap();
            inputs.push(p.to_str().unwrap().to_string());
        }
        let result = expand_dropped_paths_inner_with_cap(inputs, allow_all, cap).unwrap();
        assert_eq!(result.files.len(), cap);
        assert!(result.truncated);
    }
}
