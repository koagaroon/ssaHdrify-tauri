use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use super::*;

pub(super) fn normalized_canonical_path(path: &Path) -> Result<(PathBuf, String), String> {
    let raw_path = path
        .to_str()
        .ok_or_else(|| "Font source path is not valid UTF-8".to_string())?;
    reject_unsupported_cached_namespace_path(raw_path, "font source path")
        .map_err(|e| e.to_string())?;
    match crate::util::try_is_reparse_point(path) {
        Ok(false) => {}
        Ok(true) => {
            return Err(format!(
                "refusing to track a symlink, junction, or reparse point: {}",
                path.display()
            ));
        }
        Err(e) => {
            return Err(format!(
                "cannot verify source path is not a reparse point {}: {e}",
                path.display()
            ));
        }
    }
    let canonical = path
        .canonicalize()
        .map_err(|e| format!("canonicalize {}: {e}", path.display()))?;
    let canonical_str = canonical
        .to_str()
        .ok_or_else(|| "Canonical font source path is not valid UTF-8".to_string())?;
    let normalized = crate::fonts::normalize_canonical_path(canonical_str);
    crate::util::validate_ipc_path(&normalized, "Font source")?;
    reject_unsupported_cached_namespace_path(&normalized, "font source path")
        .map_err(|e| e.to_string())?;
    Ok((canonical, normalized))
}

/// Resolve one user-selected root into the exact composite key persisted by
/// schema v6, without walking the tree. Useful for CLI dry-run planning and
/// deduplication before the expensive scan begins.
pub fn cache_source_key(
    source_root: &Path,
    scope: FontDirectoryScope,
) -> Result<CacheSourceKey, String> {
    let (_, source_root) = normalized_canonical_path(source_root)?;
    Ok(CacheSourceKey { source_root, scope })
}

pub(super) fn collect_snapshot_children(
    entries: impl IntoIterator<Item = std::io::Result<PathBuf>>,
    directory: &Path,
    visited_entries: &mut usize,
    retained_path_bytes: usize,
) -> Result<Vec<PathBuf>, String> {
    let mut children = Vec::new();
    let mut temporary_path_bytes = 0usize;
    for entry in entries {
        if *visited_entries >= crate::fonts::MAX_PREFLIGHT_ENTRIES {
            return Err(format!(
                "Font source exceeds the {}-entry traversal limit",
                crate::fonts::MAX_PREFLIGHT_ENTRIES
            ));
        }
        *visited_entries += 1;
        let path =
            entry.map_err(|e| format!("read directory entry in {}: {e}", directory.display()))?;
        temporary_path_bytes = temporary_path_bytes.saturating_add(path.as_os_str().len());
        if retained_path_bytes.saturating_add(temporary_path_bytes)
            > crate::fonts::MAX_SCAN_PATH_BYTES
        {
            return Err(format!(
                "Font source exceeds the {}-byte retained-path limit",
                crate::fonts::MAX_SCAN_PATH_BYTES
            ));
        }
        children.push(path);
    }
    children.sort();
    Ok(children)
}

/// Build the metadata-only freshness snapshot used by drift detection and by
/// the publication stability check. Recursive traversal is deterministic and
/// never follows symlinks, junctions, or other reparse points.
pub fn snapshot_source_directories(
    source_root: &Path,
    scope: FontDirectoryScope,
) -> Result<CacheSourceSnapshot, String> {
    let (canonical_root, normalized_root) = normalized_canonical_path(source_root)?;
    let root_metadata = std::fs::metadata(&canonical_root)
        .map_err(|e| format!("read source metadata {}: {e}", source_root.display()))?;
    if !root_metadata.is_dir() {
        return Err(format!(
            "Font source is not a directory: {}",
            source_root.display()
        ));
    }

    let root_mtime = try_modified_at(&canonical_root)
        .ok_or_else(|| format!("Source directory mtime is unreadable: {normalized_root}"))?;
    let mut directories = vec![FolderSnapshot {
        folder_path: normalized_root.clone(),
        folder_mtime: root_mtime,
    }];
    let mut files = Vec::new();

    let mut seen = HashSet::from([canonical_root.clone()]);
    let mut pending = vec![(canonical_root.clone(), 0usize)];
    let mut visited_entries = 0usize;
    let mut retained_path_bytes = normalized_root.len();
    let mut total_candidate_bytes = 0u64;
    while let Some((directory, depth)) = pending.pop() {
        let read_dir = std::fs::read_dir(&directory)
            .map_err(|e| format!("read directory {}: {e}", directory.display()))?;
        let children = collect_snapshot_children(
            read_dir.map(|entry| entry.map(|entry| entry.path())),
            &directory,
            &mut visited_entries,
            retained_path_bytes,
        )?;

        // Reverse push preserves ascending traversal with a LIFO stack. Final
        // output is sorted too, so database contents never depend on OS order.
        for child in children {
            match crate::util::try_is_reparse_point(&child) {
                Ok(false) => {}
                Ok(true) => continue,
                Err(e) => {
                    return Err(format!(
                        "cannot verify directory entry is not a reparse point {}: {e}",
                        child.display()
                    ));
                }
            }
            let metadata = std::fs::symlink_metadata(&child)
                .map_err(|e| format!("read directory-entry metadata {}: {e}", child.display()))?;
            if metadata.is_file() && crate::fonts::has_allowed_font_extension(&child) {
                let canonical_file = child
                    .canonicalize()
                    .map_err(|e| format!("canonicalize font candidate {}: {e}", child.display()))?;
                if !canonical_file.starts_with(&canonical_root) {
                    return Err(format!(
                        "Font candidate escaped the selected source root: {}",
                        child.display()
                    ));
                }
                let canonical_file_str = canonical_file
                    .to_str()
                    .ok_or_else(|| "Font candidate path is not valid UTF-8".to_string())?;
                let normalized = crate::fonts::normalize_canonical_path(canonical_file_str);
                crate::util::validate_ipc_path(&normalized, "Font candidate")?;
                reject_unsupported_cached_font_path(&normalized).map_err(|e| e.to_string())?;
                if files.len() >= crate::fonts::MAX_SCAN_FONT_FILES {
                    return Err(format!(
                        "Font source exceeds the {}-candidate-file cache limit",
                        crate::fonts::MAX_SCAN_FONT_FILES
                    ));
                }
                retained_path_bytes = retained_path_bytes
                    .checked_add(normalized.len())
                    .ok_or_else(|| "Font-source path-byte count overflowed".to_string())?;
                if retained_path_bytes > crate::fonts::MAX_SCAN_PATH_BYTES {
                    return Err(format!(
                        "Font source exceeds the {}-byte retained-path limit",
                        crate::fonts::MAX_SCAN_PATH_BYTES
                    ));
                }
                total_candidate_bytes = total_candidate_bytes
                    .checked_add(metadata.len())
                    .ok_or_else(|| "Font-source candidate-byte count overflowed".to_string())?;
                if total_candidate_bytes > crate::fonts::MAX_SCAN_TOTAL_FONT_BYTES {
                    return Err(format!(
                        "Font source exceeds the {}-byte candidate-font limit",
                        crate::fonts::MAX_SCAN_TOTAL_FONT_BYTES
                    ));
                }
                files.push(FileSnapshot {
                    file_path: normalized,
                    file_size: i64::try_from(metadata.len()).unwrap_or(i64::MAX),
                    file_mtime: try_modified_at(&canonical_file).ok_or_else(|| {
                        format!(
                            "Font candidate mtime is unreadable: {}",
                            canonical_file.display()
                        )
                    })?,
                });
                continue;
            }
            if scope == FontDirectoryScope::Shallow || !metadata.is_dir() {
                continue;
            }
            let child_depth = depth.saturating_add(1);
            if child_depth > crate::fonts::MAX_SCAN_DEPTH {
                return Err(format!(
                    "Font source exceeds the {}-level recursion limit",
                    crate::fonts::MAX_SCAN_DEPTH
                ));
            }
            let canonical_child = child
                .canonicalize()
                .map_err(|e| format!("canonicalize directory {}: {e}", child.display()))?;
            if !canonical_child.starts_with(&canonical_root) {
                return Err(format!(
                    "Directory escaped the selected source root during traversal: {}",
                    child.display()
                ));
            }
            if !seen.insert(canonical_child.clone()) {
                continue;
            }
            if seen.len() > MAX_CACHED_DIRECTORIES {
                return Err(format!(
                    "Font source exceeds the {MAX_CACHED_DIRECTORIES}-directory cache limit"
                ));
            }
            let canonical_child_str = canonical_child
                .to_str()
                .ok_or_else(|| "Nested font directory path is not valid UTF-8".to_string())?;
            let normalized = crate::fonts::normalize_canonical_path(canonical_child_str);
            crate::util::validate_ipc_path(&normalized, "Font directory")?;
            reject_unsupported_cached_directory_path(&normalized).map_err(|e| e.to_string())?;
            retained_path_bytes = retained_path_bytes
                .checked_add(normalized.len())
                .ok_or_else(|| "Font-source path-byte count overflowed".to_string())?;
            if retained_path_bytes > crate::fonts::MAX_SCAN_PATH_BYTES {
                return Err(format!(
                    "Font source exceeds the {}-byte retained-path limit",
                    crate::fonts::MAX_SCAN_PATH_BYTES
                ));
            }
            let modified_at = try_modified_at(&canonical_child)
                .ok_or_else(|| format!("Directory mtime is unreadable: {normalized}"))?;
            directories.push(FolderSnapshot {
                folder_path: normalized,
                folder_mtime: modified_at,
            });
            pending.push((canonical_child, child_depth));
        }
    }
    directories.sort_by(|a, b| a.folder_path.cmp(&b.folder_path));
    files.sort_by(|a, b| a.file_path.cmp(&b.file_path));
    Ok(CacheSourceSnapshot {
        source_root: normalized_root,
        scope,
        directories,
        files,
    })
}

pub(super) fn validate_source_snapshot_shape(
    snapshot: &CacheSourceSnapshot,
    fonts: &[FontMetadata],
) -> Result<(), CacheError> {
    reject_unsupported_cached_folder_path(&snapshot.source_root)?;
    if snapshot.directories.is_empty() {
        return Err(CacheError::Io(
            "source snapshot must contain its root directory".to_string(),
        ));
    }
    if snapshot.directories.len() > MAX_CACHED_DIRECTORIES {
        return Err(CacheError::Io(format!(
            "source snapshot exceeds the {MAX_CACHED_DIRECTORIES}-directory cache limit"
        )));
    }
    if snapshot.scope == FontDirectoryScope::Shallow && snapshot.directories.len() != 1 {
        return Err(CacheError::Io(
            "shallow source snapshot must contain exactly its root directory".to_string(),
        ));
    }
    if snapshot.files.len() > crate::fonts::MAX_SCAN_FONT_FILES {
        return Err(CacheError::Io(format!(
            "source snapshot exceeds the {}-candidate-file limit",
            crate::fonts::MAX_SCAN_FONT_FILES
        )));
    }
    if fonts.len() > crate::fonts::MAX_CACHE_POPULATE_FACES {
        return Err(CacheError::Io(format!(
            "source snapshot exceeds the {}-face cache limit",
            crate::fonts::MAX_CACHE_POPULATE_FACES
        )));
    }

    let source_root = Path::new(&snapshot.source_root);
    let mut directory_paths = HashSet::with_capacity(snapshot.directories.len());
    let mut retained_path_bytes = snapshot.source_root.len();
    for directory in &snapshot.directories {
        reject_unsupported_cached_directory_path(&directory.folder_path)?;
        let directory_path = Path::new(&directory.folder_path);
        if !directory_path.starts_with(source_root) {
            return Err(CacheError::Io(format!(
                "tracked directory is outside its source root: {}",
                directory.folder_path
            )));
        }
        if !directory_paths.insert(directory.folder_path.as_str()) {
            return Err(CacheError::Io(format!(
                "source snapshot contains duplicate directory: {}",
                directory.folder_path
            )));
        }
        retained_path_bytes = retained_path_bytes
            .checked_add(directory.folder_path.len())
            .ok_or_else(|| CacheError::Io("source path-byte count overflowed".to_string()))?;
    }
    if !directory_paths.contains(snapshot.source_root.as_str()) {
        return Err(CacheError::Io(
            "source snapshot does not contain its root directory".to_string(),
        ));
    }

    let mut candidate_paths = HashMap::with_capacity(snapshot.files.len());
    let mut total_candidate_bytes = 0u64;
    for file in &snapshot.files {
        reject_unsupported_cached_font_path(&file.file_path)?;
        let file_path = Path::new(&file.file_path);
        if !file_path.starts_with(source_root) {
            return Err(CacheError::Io(format!(
                "candidate font is outside its source root: {}",
                file.file_path
            )));
        }
        let parent = file_path.parent().ok_or_else(|| {
            CacheError::Io(format!("candidate font has no parent: {}", file.file_path))
        })?;
        let parent = parent.to_str().ok_or_else(|| {
            CacheError::Io(format!(
                "candidate font parent is not valid UTF-8: {}",
                file.file_path
            ))
        })?;
        if !directory_paths.contains(parent) {
            return Err(CacheError::Io(format!(
                "candidate font parent was not visited by the source scan: {}",
                file.file_path
            )));
        }
        if file.file_size < 0
            || file.file_mtime < 0
            || candidate_paths
                .insert(file.file_path.as_str(), (file.file_size, file.file_mtime))
                .is_some()
        {
            return Err(CacheError::Io(format!(
                "candidate font snapshot has invalid or duplicate metadata: {}",
                file.file_path
            )));
        }
        retained_path_bytes = retained_path_bytes
            .checked_add(file.file_path.len())
            .ok_or_else(|| CacheError::Io("source path-byte count overflowed".to_string()))?;
        total_candidate_bytes = total_candidate_bytes
            .checked_add(u64::try_from(file.file_size).unwrap_or(u64::MAX))
            .ok_or_else(|| CacheError::Io("source candidate-byte count overflowed".to_string()))?;
    }
    if retained_path_bytes > crate::fonts::MAX_SCAN_PATH_BYTES {
        return Err(CacheError::Io(format!(
            "source snapshot exceeds the {}-byte retained-path limit",
            crate::fonts::MAX_SCAN_PATH_BYTES
        )));
    }
    if total_candidate_bytes > crate::fonts::MAX_SCAN_TOTAL_FONT_BYTES {
        return Err(CacheError::Io(format!(
            "source snapshot exceeds the {}-byte candidate-font limit",
            crate::fonts::MAX_SCAN_TOTAL_FONT_BYTES
        )));
    }

    let mut faces = HashSet::with_capacity(fonts.len());
    for font in fonts {
        reject_unsupported_cached_font_path(&font.file_path)?;
        let font_path = Path::new(&font.file_path);
        if !font_path.starts_with(source_root) {
            return Err(CacheError::Io(format!(
                "cached font is outside its source root: {}",
                font.file_path
            )));
        }
        let Some((candidate_size, candidate_mtime)) = candidate_paths.get(font.file_path.as_str())
        else {
            return Err(CacheError::Io(format!(
                "parsed font was not present in the candidate-file snapshot: {}",
                font.file_path
            )));
        };
        if font.file_size != *candidate_size || font.file_mtime != *candidate_mtime {
            return Err(CacheError::Io(format!(
                "parsed font metadata differs from its candidate-file snapshot: {}",
                font.file_path
            )));
        }
        let parent = font_path.parent().ok_or_else(|| {
            CacheError::Io(format!(
                "cached font has no parent directory: {}",
                font.file_path
            ))
        })?;
        let parent = parent.to_str().ok_or_else(|| {
            CacheError::Io(format!(
                "cached font parent is not valid UTF-8: {}",
                font.file_path
            ))
        })?;
        if !directory_paths.contains(parent) {
            return Err(CacheError::Io(format!(
                "cached font parent was not visited by the source scan: {}",
                font.file_path
            )));
        }
        if font.face_index < 0 || font.file_size < 0 || font.file_mtime < 0 {
            return Err(CacheError::Io(format!(
                "cached font has invalid negative metadata: {}",
                font.file_path
            )));
        }
        if !faces.insert((font.file_path.as_str(), font.face_index)) {
            return Err(CacheError::Io(format!(
                "source snapshot contains duplicate font face: {}#{}",
                font.file_path, font.face_index
            )));
        }
    }
    Ok(())
}

/// Re-stat a completed scan before publication. This closes the window where
/// files or nested directories change between traversal and the SQLite write;
/// partial or already-stale scan output is never promoted into the cache.
pub fn validate_cache_source_stability(
    snapshot: &CacheSourceSnapshot,
    fonts: &[FontMetadata],
) -> Result<(), CacheError> {
    validate_source_snapshot_shape(snapshot, fonts)?;
    let live = snapshot_source_directories(Path::new(&snapshot.source_root), snapshot.scope)
        .map_err(CacheError::Io)?;
    let mut expected_directories = snapshot.directories.clone();
    expected_directories.sort_by(|a, b| a.folder_path.cmp(&b.folder_path));
    let mut expected_files = snapshot.files.clone();
    expected_files.sort_by(|a, b| a.file_path.cmp(&b.file_path));
    if live.source_root != snapshot.source_root
        || live.directories != expected_directories
        || live.files != expected_files
    {
        return Err(CacheError::Io(
            "font source changed while it was being scanned; scan again before caching".to_string(),
        ));
    }

    Ok(())
}
