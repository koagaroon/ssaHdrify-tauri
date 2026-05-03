//! Drag-drop path expansion.
//!
//! Frontend hands a flat list of paths from a drag-drop event (files,
//! folders, or a mix) and gets back an expanded flat list of file paths.
//! Folders are walked exactly one level deep — typical fan-sub workflow
//! drops one folder per show, never nested hierarchies. Categorization
//! into video vs subtitle is the consumer's job; this command's contract
//! is only "give me the regular files behind these dropped paths".
//!
//! Defense: skip hidden entries, symlinks, and reparse points so a
//! mistakenly-dropped junction can't fan out into a protected directory
//! (`.ssh`, OneDrive, etc.). The downstream readers (encoding.rs,
//! fonts.rs) enforce their own extension/provenance allowlists, so even
//! a path leak here can't be turned into an arbitrary read.

use crate::util::{is_reparse_point, MAX_INPUT_PATHS};
use std::path::Path;

const MAX_RESULT_FILES: usize = 5000;

/// Expand dropped paths into a flat list of file paths.
#[tauri::command]
pub fn expand_dropped_paths(paths: Vec<String>) -> Result<Vec<String>, String> {
    if paths.is_empty() {
        return Ok(Vec::new());
    }
    if paths.len() > MAX_INPUT_PATHS {
        return Err(format!(
            "Too many paths dropped (got {}, max {MAX_INPUT_PATHS})",
            paths.len()
        ));
    }

    let mut result: Vec<String> = Vec::new();
    for raw in &paths {
        // Skip silently rather than fail — native drag-drop shouldn't
        // produce empty / oversize / control-char paths, but the IPC
        // boundary trusts no caller, and dropping ONE bad path should
        // not abort the whole batch the user just dropped.
        if crate::util::validate_ipc_path(raw, "Dropped").is_err() {
            continue;
        }
        let p = Path::new(raw);
        let meta = match std::fs::symlink_metadata(p) {
            Ok(m) => m,
            Err(e) => {
                log::warn!("dropzone: stat failed for {raw}: {e}");
                continue;
            }
        };
        // Skip top-level reparse points so a junction → /etc style trick
        // can't expand into a protected location.
        if is_reparse_point(p) {
            log::warn!("dropzone: skipping reparse point at {raw}");
            continue;
        }
        if meta.file_type().is_file() {
            result.push(raw.clone());
        } else if meta.file_type().is_dir() {
            walk_one_level(p, &mut result);
        }
        if result.len() >= MAX_RESULT_FILES {
            // "Truncating" is loose — we actually stop adding more, the
            // already-added entries pass through unchanged. The user-
            // visible effect is "the rest of your drop got dropped on
            // the floor"; consumers can compare drop count vs result
            // count if they want to detect this.
            log::warn!("dropzone: result cap {MAX_RESULT_FILES} reached, dropping remainder");
            break;
        }
    }
    Ok(result)
}

/// Read a directory one level deep, appending regular files. Hidden
/// entries and reparse points are skipped.
fn walk_one_level(dir: &Path, out: &mut Vec<String>) {
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(e) => {
            log::warn!("dropzone: read_dir failed for {dir:?}: {e}");
            return;
        }
    };
    // Use match-on-Result so per-entry I/O errors are at least logged
    // (`entries.flatten()` would silently swallow them, hiding broken
    // permissions or stale-NFS-handle situations).
    for entry_result in entries {
        if out.len() >= MAX_RESULT_FILES {
            break;
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
        if is_reparse_point(&entry_path) {
            continue;
        }
        let ft = match entry.file_type() {
            Ok(f) => f,
            Err(_) => continue,
        };
        if ft.is_file() {
            if let Some(s) = entry_path.to_str() {
                out.push(s.to_string());
            }
        }
        // Single-level walk by design — see module doc.
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::io::Write;

    fn make_tempdir(suffix: &str) -> std::path::PathBuf {
        let mut dir = std::env::temp_dir();
        dir.push(format!("ssahdrify_dropzone_test_{suffix}"));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn passes_through_files_unchanged() {
        let dir = make_tempdir("passthrough");
        let p = dir.join("a.ass");
        fs::File::create(&p).unwrap().write_all(b"x").unwrap();
        let result = expand_dropped_paths(vec![p.to_str().unwrap().to_string()]).unwrap();
        assert_eq!(result.len(), 1);
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

        let result = expand_dropped_paths(vec![dir.to_str().unwrap().to_string()]).unwrap();
        assert_eq!(
            result.len(),
            3,
            "expected 3 top-level files, got {result:?}"
        );
        for s in &result {
            assert!(!s.contains("ignored.ass"), "nested file leaked: {s}");
        }
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
        let result = expand_dropped_paths(vec![dir.to_str().unwrap().to_string()]).unwrap();
        assert_eq!(result.len(), 1);
        assert!(result[0].ends_with("visible.ass"));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn rejects_too_many_input_paths() {
        let many: Vec<String> = (0..(MAX_INPUT_PATHS + 1))
            .map(|i| format!("/tmp/x{i}.ass"))
            .collect();
        assert!(expand_dropped_paths(many).is_err());
    }

    #[test]
    fn rejects_control_chars_in_path() {
        let result = expand_dropped_paths(vec!["/tmp/foo\u{0000}bar.ass".to_string()]).unwrap();
        // Silently skipped, not errored.
        assert!(result.is_empty());
    }

    #[test]
    fn empty_input_returns_empty() {
        let result = expand_dropped_paths(Vec::new()).unwrap();
        assert!(result.is_empty());
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
        let result = expand_dropped_paths(vec![
            folder.to_str().unwrap().to_string(),
            standalone.to_str().unwrap().to_string(),
        ])
        .unwrap();
        assert_eq!(result.len(), 3, "expected 2 from folder + 1 standalone, got {result:?}");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn caps_result_at_max_result_files() {
        let dir = make_tempdir("cap");
        // Create MAX_RESULT_FILES + 100 files in one folder; expanding the
        // folder must stop at the cap, not silently overflow.
        let count = MAX_RESULT_FILES + 100;
        for i in 0..count {
            fs::File::create(dir.join(format!("f{i}.ass")))
                .unwrap()
                .write_all(b"x")
                .unwrap();
        }
        let result = expand_dropped_paths(vec![dir.to_str().unwrap().to_string()]).unwrap();
        assert_eq!(
            result.len(),
            MAX_RESULT_FILES,
            "expected cap at MAX_RESULT_FILES, got {}",
            result.len()
        );
        let _ = fs::remove_dir_all(&dir);
    }
}
