//! Persistent font cache — metadata index across app lifetimes.
//!
//! Memoizes the expensive scan-and-name-table-read step of font-source
//! resolution. The cache stores source-owned directory and candidate-file
//! snapshots, parsed faces, and family-name lookup keys; it does NOT cache subset bytes
//! (subsetting is per-subtitle and depends on glyph sets that vary).
//!
//! Decoupled from the existing GUI session DB (`init_user_font_db` in
//! `fonts.rs`): different lifetime (cross-run vs single-app-run),
//! different access pattern (read-mostly vs write-heavy), different
//! invalidation needs (mtime/size based vs always-fresh). Per-binary
//! storage at the caller-supplied path so GUI and CLI run independently
//! without lock contention.
//!
//! This file owns the Tauri-free cache module: schema (NFC +
//! Unicode-lowercase lookup key + exact-family/face-alias key kind),
//! open / create / version check, per-source scan write, drift detection,
//! family-name lookup. The
//! GUI-only IPC surface lives in `font_cache_commands.rs`; the CLI's
//! `refresh-fonts` and embed-time cache integration live in
//! `bin/cli/main.rs`.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use rusqlite::{params, Connection, OpenFlags};
use serde::{Deserialize, Serialize};

/// Read a filesystem entry's mtime as Unix nanoseconds, returning None when either
/// the metadata stat or the `modified()` call fails. This is the
/// canonical mtime-stat helper for every drift / populate site
/// (GUI's detect / rescan / clear flows + CLI's drift check + every
/// source-publication callsite). Internal callers use this rather than
/// inline `metadata().modified()` so drift detection uses identical
/// stat semantics across all consumers.
///
/// Failure modes route to `None`: path gone (`NotFound`), permission denied,
/// or a filesystem without a readable modification time. Source drift callers
/// separately distinguish a genuinely missing/reparse root from an existing
/// real directory whose complete snapshot could not be read; the latter is
/// conservatively treated as modified so stale rows are not trusted silently.
pub fn try_modified_at(path: &Path) -> Option<i64> {
    let modified = std::fs::metadata(path).ok()?.modified().ok()?;
    // pre-epoch mtimes previously fell through
    // `.unwrap_or(0)` and returned `Some(0)`, but the caller contract
    // (every drift / Phase-1 / Phase-3 stat site) requires `None` for
    // "stat failed" cases so the populate / replace path is skipped.
    // Returning `Some(0)` wrote `folder_mtime=0` into the row; the
    // next drift detect compared `0` against the OS's real positive
    // value and flagged the folder as modified — exactly the
    // empty-folder / pre-epoch loop bug try_modified_at was designed
    // to prevent. `.ok()?` propagates SystemTimeError → None
    // through the same `?` chain as the prior `metadata` / `modified`
    // fail sites above.
    let elapsed = modified.duration_since(std::time::UNIX_EPOCH).ok()?;
    let seconds = i64::try_from(elapsed.as_secs()).ok()?;
    seconds
        .checked_mul(1_000_000_000)?
        .checked_add(i64::from(elapsed.subsec_nanos()))
}

/// Unified per-user data directory shared by both binaries
/// (`ssahdrify` GUI + `ssahdrify-cli`). OS-specific:
/// - Windows: `%APPDATA%/ssahdrify`
/// - macOS:   `$HOME/Library/Application Support/ssahdrify`
/// - Linux:   `${XDG_DATA_HOME:-$HOME/.local/share}/ssahdrify`
///
/// The GUI side used to use Tauri's
/// `app.path().app_data_dir()` which on Windows resolves to the bundle
/// identifier path `%APPDATA%/com.koagaroon.ssahdrify/` — a separate
/// folder from the CLI's `%APPDATA%/ssahdrify/`. Unifying under the
/// short `ssahdrify` name gives users a single app folder to find on
/// disk and lets per-binary cache filenames (`gui_font_cache.sqlite3`
/// vs `cli_font_cache.sqlite3`) coexist there. SQLite lock isolation
/// is preserved by the per-binary filenames; Tauri-managed internal
/// state (plugin storage etc.) keeps using Tauri's own directory and
/// is unaffected.
///
/// Returns an `Err` when the platform's environment for the canonical
/// per-user data directory isn't set (broken environment).
pub fn unified_app_data_dir() -> Result<PathBuf, String> {
    let base = platform_data_dir()?;
    Ok(base.join("ssahdrify"))
}

/// Default cache file path for the CLI binary. Composes the unified
/// data dir + the per-binary cache filename. Caller can override via
/// `--cache-file <PATH>`.
pub fn default_cli_cache_path() -> Result<PathBuf, String> {
    Ok(unified_app_data_dir()?.join("cli_font_cache.sqlite3"))
}

/// Per-user data directory, resolved per-OS without pulling in the
/// `dirs` crate (one usage didn't justify the dep). Mirrors the
/// well-known XDG and platform conventions:
/// - Windows: `%APPDATA%` (Roaming)
/// - macOS:   `$HOME/Library/Application Support`
/// - Linux:   `$XDG_DATA_HOME` if set, else `$HOME/.local/share`
fn platform_data_dir() -> Result<PathBuf, String> {
    #[cfg(target_os = "windows")]
    {
        std::env::var("APPDATA").map(PathBuf::from).map_err(|_| {
            "APPDATA environment variable not set; cannot determine \
                 default cache location. Pass --cache-file <PATH> to override."
                .to_string()
        })
    }
    #[cfg(target_os = "macos")]
    {
        std::env::var("HOME")
            .map(|h| PathBuf::from(h).join("Library").join("Application Support"))
            .map_err(|_| {
                "HOME environment variable not set; cannot determine \
                 default cache location. Pass --cache-file <PATH> to override."
                    .to_string()
            })
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        // XDG Base Dir spec § "Environment variables": "If
        // $XDG_DATA_HOME is either not set or empty, a default equal
        // to $HOME/.local/share should be used." The empty-check is
        // spec-required, not defensive paranoia — don't simplify away
        // the !is_empty() check. The is_absolute() guard rejects
        // exotic values like "." or relative paths from a misconfigured
        // shell, falling through to the HOME default.
        if let Ok(xdg) = std::env::var("XDG_DATA_HOME") {
            if !xdg.is_empty() {
                let xdg_path = PathBuf::from(&xdg);
                if xdg_path.is_absolute() {
                    return Ok(xdg_path);
                }
            }
        }
        std::env::var("HOME")
            .map(|h| PathBuf::from(h).join(".local").join("share"))
            .map_err(|_| {
                "Neither XDG_DATA_HOME nor HOME is set; cannot determine \
                 default cache location. Pass --cache-file <PATH> to override."
                    .to_string()
            })
    }
}

fn cache_sidecar_path(cache_path: &Path, suffix: &str) -> PathBuf {
    let mut path = cache_path.as_os_str().to_os_string();
    path.push(suffix);
    PathBuf::from(path)
}

fn reject_cache_reparse_paths(cache_path: &Path) -> Result<(), CacheError> {
    for path in std::iter::once(cache_path.to_path_buf()).chain(
        ["-journal", "-wal", "-shm"]
            .into_iter()
            .map(|suffix| cache_sidecar_path(cache_path, suffix)),
    ) {
        if crate::util::is_reparse_point(&path) {
            return Err(CacheError::Io(format!(
                "refusing to open cache at a reparse point (symlink / junction): {}. \
                 Inspect and remove the link manually before relaunching.",
                path.display()
            )));
        }
    }
    Ok(())
}

/// Schema version. Bumped when any table layout changes; mismatch on
/// open returns `CacheError::SchemaVersionMismatch` so the caller
/// can rebuild (CLI: drift-equivalent fallback to no-cache; GUI:
/// prompt). Per the locked "no auto-migrate" decision, the cache is
/// never silently migrated — release notes call out version bumps so
/// users intentionally rebuild via `refresh-fonts` or the GUI modal.
///
/// v1 → v2: added `family_name_key` column to
/// `cached_family_keys` storing NFC-normalized lowercase form so
/// lookup hit rate matches the session DB's user_font_key contract.
/// Without it, CJK fonts whose name-table form differs from the
/// ASS \fn / Style Fontname spelling missed every cache lookup.
///
/// v2 → v3: cache population now stores full-face / PostScript aliases
/// such as `Dream Han Serif SC W22` under all bold/italic lookup states.
/// Old cache files do not have these alias rows, so they would silently
/// keep missing TTC faces until rebuilt.
///
/// v4 → v5: duplicate full-face aliases are preserved even when the same
/// normalized string also appears in the family-name table. Old v4 caches can
/// miss fonts such as `Dream Han Serif SC W22` because they kept only the bold
/// style-sensitive family row.
///
/// v3 → v4: full-face / PostScript aliases are stored as style-
/// insensitive alias rows in a separate key kind. Exact family-name
/// rows must win before alias rows so an alias cannot override a real
/// family/style match; aliases are also stored once rather than four
/// duplicated bold/italic rows.
///
/// BUMP this constant when the `family_name_key` normalization or
/// any other on-disk shape changes — even if no DDL changes
/// . The verifier only checks numeric equality, so
/// a stale cache with subtly-different keys would silently produce
/// "font not found" regressions until manual clear/rebuild. If a
/// future migration touches `family_lookup_key`, NFC normalization
/// rules, or face-index encoding, bump first. Long-term: persist a
/// git-describe-derived build_id in cache_meta alongside the
/// version number to catch unbumped semantic shifts.
pub const SCHEMA_VERSION: i32 = 6;

const KEY_KIND_FAMILY: i32 = 0;
const KEY_KIND_FACE_ALIAS: i32 = 1;

/// Filesystem-probe budget for one family lookup. A normal cache has one or
/// two same-family candidates; a hostile or badly stale cache could otherwise
/// make a single lookup hold the cache mutex across thousands of canonicalize
/// and metadata calls (including slow mapped-drive failures).
const MAX_CACHE_LOOKUP_CANDIDATES: usize = 64;

const FAMILY_LOOKUP_SQL: &str =
    "SELECT k.font_path, k.face_index, s.source_root, f.file_size, f.file_mtime \
     FROM cached_family_keys k \
     INNER JOIN cached_fonts f \
       ON f.source_root = k.source_root AND f.scope = k.scope \
      AND f.font_path = k.font_path AND f.face_index = k.face_index \
     INNER JOIN cached_sources s \
       ON s.source_root = f.source_root AND s.scope = f.scope \
     WHERE k.family_name_key = ?1 \
       AND ((k.key_kind = ?2 AND k.bold = ?3 AND k.italic = ?4) \
            OR (k.key_kind = ?5 AND k.bold = 0 AND k.italic = 0)) \
     ORDER BY k.key_kind ASC, s.scope ASC, s.source_order DESC, \
              k.font_path ASC, k.face_index ASC \
     LIMIT ?6";

/// DoS-class sanity cap on the number of `cached_sources` rows
/// `list_sources` / `diff_sources` will return. A hostile cache file
/// (untrusted-input via `--cache-file`) populated with hundreds of fabricated
/// source rows — especially UNC paths to dead servers — would otherwise
/// spin every detect call through a per-row stat loop, bounded only by
/// per-stat OS timeout. The cap fires inside `list_folders` and refuses
/// to return the result; downstream `diff_against` /
/// `detect_font_cache_drift` surface the error and the user is pointed
/// at rebuilding the cache. Realistic working caches hold a handful to
/// dozens of folders, so 256 stays generous without being a practical
/// stat-storm budget.
pub const MAX_CACHED_SOURCES: usize = 256;

/// Compatibility name retained for CLI call sites that still count source roots.
/// In schema v6 a "folder" row means one source root; nested directories live in
/// `cached_directories` and are governed by [`MAX_CACHED_DIRECTORIES`].
pub const MAX_CACHED_FOLDERS: usize = MAX_CACHED_SOURCES;

/// Global sanity cap for real directories tracked across every cached source.
/// Recursive libraries need one row per visited directory so a nested add/remove
/// changes freshness even when the root directory's own mtime does not. The
/// representative 18k-font library has 157 directories; 4096 leaves ample room
/// while bounding startup metadata work for a hostile cache file.
pub const MAX_CACHED_DIRECTORIES: usize = 4_096;

/// Global byte budget for source-root, visited-directory, and candidate-file
/// paths materialized by [`FontCache::list_sources`]. Per-source traversal is
/// already capped at `MAX_SCAN_PATH_BYTES`; this second ceiling prevents many
/// individually-valid sources (or a crafted `--cache-file`) from making one
/// drift check allocate hundreds of megabytes of path text.
const MAX_CACHED_SNAPSHOT_PATH_BYTES: usize = 128 * 1024 * 1024;

fn checked_projected_count(
    existing: i64,
    incoming: usize,
    cap: usize,
    label: &str,
) -> Result<usize, CacheError> {
    let projected = usize::try_from(existing)
        .unwrap_or(usize::MAX)
        .checked_add(incoming)
        .ok_or_else(|| CacheError::Io(format!("{label} row count overflowed")))?;
    if projected > cap {
        return Err(CacheError::Io(format!(
            "{label} would exceed the {cap}-row global sanity cap"
        )));
    }
    Ok(projected)
}

fn snapshot_retained_path_bytes(snapshot: &CacheSourceSnapshot) -> Result<usize, CacheError> {
    std::iter::once(snapshot.source_root.len())
        .chain(
            snapshot
                .directories
                .iter()
                .map(|directory| directory.folder_path.len()),
        )
        .chain(snapshot.files.iter().map(|file| file.file_path.len()))
        .try_fold(0usize, |total, bytes| {
            total
                .checked_add(bytes)
                .ok_or_else(|| CacheError::Io("source path-byte count overflowed".to_string()))
        })
}

fn checked_projected_path_bytes(existing: i64, incoming: usize) -> Result<usize, CacheError> {
    let projected = usize::try_from(existing)
        .unwrap_or(usize::MAX)
        .checked_add(incoming)
        .ok_or_else(|| CacheError::Io("cached snapshot path-byte count overflowed".to_string()))?;
    if projected > MAX_CACHED_SNAPSHOT_PATH_BYTES {
        return Err(CacheError::Io(format!(
            "cached snapshot paths would exceed the {MAX_CACHED_SNAPSHOT_PATH_BYTES}-byte global sanity cap"
        )));
    }
    Ok(projected)
}

fn enforce_cached_path_byte_budgets(
    source_path_bytes: usize,
    total_path_bytes: usize,
) -> Result<(), CacheError> {
    if source_path_bytes > crate::fonts::MAX_SCAN_PATH_BYTES {
        return Err(CacheError::Io(format!(
            "one cached source exceeds the {}-byte retained-path limit; rebuild required",
            crate::fonts::MAX_SCAN_PATH_BYTES
        )));
    }
    if total_path_bytes > MAX_CACHED_SNAPSHOT_PATH_BYTES {
        return Err(CacheError::Io(format!(
            "cached snapshot paths exceed the {MAX_CACHED_SNAPSHOT_PATH_BYTES}-byte global sanity cap; cache file appears corrupted or hostile — rebuild required"
        )));
    }
    Ok(())
}

fn reject_unsupported_cached_namespace_path(path: &str, source: &str) -> Result<(), CacheError> {
    let normalized = path.replace('/', "\\").to_ascii_lowercase();
    let bytes = normalized.as_bytes();
    let is_extended_local_drive = normalized.starts_with("\\\\?\\")
        && bytes.get(4).is_some_and(u8::is_ascii_alphabetic)
        && bytes.get(5) == Some(&b':')
        && bytes.get(6) == Some(&b'\\');
    let uses_unc_or_device_namespace = normalized.starts_with("\\\\") && !is_extended_local_drive;
    if uses_unc_or_device_namespace {
        return Err(CacheError::Io(format!(
            "{source} contains a network or device-namespace path; \
             rebuild the cache from local font folders or run with --no-cache"
        )));
    }
    Ok(())
}

fn reject_unsupported_cached_folder_path(path: &str) -> Result<(), CacheError> {
    crate::util::validate_ipc_path(path, "cached_sources.source_root").map_err(CacheError::Io)?;
    reject_unsupported_cached_namespace_path(path, "cached_sources.source_root")
}

fn reject_unsupported_cached_directory_path(path: &str) -> Result<(), CacheError> {
    crate::util::validate_ipc_path(path, "cached_directories.directory_path")
        .map_err(CacheError::Io)?;
    reject_unsupported_cached_namespace_path(path, "cached_directories.directory_path")
}

fn reject_unsupported_cached_font_path(path: &str) -> Result<(), CacheError> {
    crate::util::validate_ipc_path(path, "cached_fonts.font_path").map_err(CacheError::Io)?;
    reject_unsupported_cached_namespace_path(path, "cached_fonts.font_path")
}

fn validate_cached_font_hit(
    font_path: &str,
    folder_path: &str,
    cached_file_size: i64,
    cached_file_mtime: i64,
) -> Result<(), CacheError> {
    let folder_canonical = Path::new(folder_path).canonicalize().map_err(|e| {
        CacheError::Io(format!(
            "cached_sources.source_root no longer points at a readable folder; rebuild required: {e}"
        ))
    })?;
    let folder_metadata = std::fs::metadata(&folder_canonical).map_err(|e| {
        CacheError::Io(format!(
            "cached_sources.source_root no longer points at a readable folder; rebuild required: {e}"
        ))
    })?;
    if !folder_metadata.is_dir() {
        return Err(CacheError::Io(
            "cached_sources.source_root no longer points at a folder; rebuild required".to_string(),
        ));
    }

    let font_canonical = Path::new(font_path).canonicalize().map_err(|e| {
        CacheError::Io(format!(
            "cached_fonts.font_path no longer points at a readable file; rebuild required: {e}"
        ))
    })?;
    if !font_canonical.starts_with(&folder_canonical) {
        return Err(CacheError::Io(
            "cached_fonts.font_path is outside cached_sources.source_root; \
             cache file appears corrupted or hostile — rebuild required"
                .to_string(),
        ));
    }

    let font_metadata = std::fs::metadata(&font_canonical).map_err(|e| {
        CacheError::Io(format!(
            "cached_fonts.font_path no longer points at a readable file; rebuild required: {e}"
        ))
    })?;
    if !font_metadata.is_file() {
        return Err(CacheError::Io(
            "cached_fonts.font_path no longer points at a file; rebuild required".to_string(),
        ));
    }

    let live_file_size = i64::try_from(font_metadata.len()).unwrap_or(i64::MAX);
    let live_file_mtime = try_modified_at(&font_canonical).ok_or_else(|| {
        CacheError::Io("cached_fonts.font_path mtime is unreadable; rebuild required".to_string())
    })?;
    if live_file_size != cached_file_size || live_file_mtime != cached_file_mtime {
        return Err(CacheError::Io(
            "cached_fonts metadata no longer matches the live font file; rebuild required"
                .to_string(),
        ));
    }

    Ok(())
}

fn validate_cached_font_candidate(
    font_path: &str,
    face_index: i32,
    folder_path: &str,
    cached_file_size: i64,
    cached_file_mtime: i64,
) -> Result<(), CacheError> {
    reject_unsupported_cached_folder_path(folder_path)?;
    reject_unsupported_cached_font_path(font_path)?;
    let face_index_supported =
        u32::try_from(face_index).is_ok_and(|index| index <= crate::fonts::MAX_SUBSET_FONT_INDEX);
    if !face_index_supported {
        return Err(CacheError::Io(
            "cached_fonts.face_index is outside the supported range; cache file appears corrupted or hostile — rebuild required"
                .to_string(),
        ));
    }
    let extension_allowed = Path::new(font_path)
        .extension()
        .and_then(|extension| extension.to_str())
        .map(|extension| extension.to_ascii_lowercase())
        .is_some_and(|extension| {
            crate::fonts::ALLOWED_FONT_EXTENSIONS.contains(&extension.as_str())
        });
    if !extension_allowed {
        return Err(CacheError::Io(
            "cached_fonts.font_path has an unsupported font extension; cache file appears corrupted or hostile — rebuild required"
                .to_string(),
        ));
    }
    validate_cached_font_hit(font_path, folder_path, cached_file_size, cached_file_mtime)
}

fn warn_skipped_cache_candidates(skipped: usize) {
    if skipped > 0 {
        // Deliberately path- and family-free: FontCache is also callable by
        // internal code that has not passed the IPC family-name validator.
        log::warn!(
            "font cache lookup skipped {skipped} stale or invalid higher-priority candidate(s); rebuild the font cache to restore source priority"
        );
    }
}

/// Normalize a family-name string into the lookup key used by
/// `cached_family_keys.family_name_key`: NFC-normalize then full
/// Unicode lowercase (so `É`→`é`, not just ASCII-only `A`→`a`).
/// Mirrors `userFontKey`'s normalization in font-embedder.ts (which
/// uses JS `toLowerCase()`, also full Unicode) so a font's name-table
/// entry and an ASS file's `\fn` reference match regardless of NFC/NFD
/// form (macOS HFS+ NFD vs Windows NFC) or case (`Café` vs `CAFÉ`,
/// `Source Han Sans CN` vs `source han sans cn`). Plain ASCII
/// `to_ascii_lowercase` would miss `É`/`Ñ`/`Ü` etc., breaking the
/// CJK/Latin-extended fonts the cache exists to accelerate.
pub(crate) fn family_lookup_key(family_name: &str) -> String {
    // no codepoint cap here because every caller is
    // upstream-bounded. The IPC boundary runs `validate_font_family`
    // (256-codepoint cap) on argv-derived family names; the cache
    // module's internal callers consume name-table entries that
    // upstream `bounded_font_family_name` (in fonts.rs) clamps to the
    // same length. Adding a debug_assert here would document the
    // invariant but provide no production defense; this comment is
    // the durable record. If a future direct caller appears that
    // doesn't route through either upstream, add the bound at THAT
    // boundary, not here.
    use unicode_normalization::UnicodeNormalization;
    family_name.nfc().collect::<String>().to_lowercase()
}

/// One font face's metadata, ready to be written into the cache by
/// `FontCache::replace_source`. The cache module deliberately does NOT
/// parse fonts — the caller (existing scan path in `app_lib::fonts`,
/// or a test fixture, or future scan code) produces these records and
/// hands them to the cache for persistence. This keeps font-parsing
/// concerns out of the cache module entirely.
#[derive(Debug, Clone)]
pub struct FontMetadata {
    /// Absolute path to the font file.
    pub file_path: String,
    /// File size in bytes from the OS at scan time.
    pub file_size: i64,
    /// File mtime as Unix nanoseconds.
    pub file_mtime: i64,
    /// 0 for non-TTC; >=0 for TrueType Collection (face index inside).
    pub face_index: i32,
    /// Each (family_name, bold, italic) tuple this face advertises.
    /// CJK fonts typically produce multiple entries (Latin + Simplified
    /// Chinese + Traditional + Japanese, etc.) — embed-time lookup must
    /// hit whichever locale's name the subtitle author wrote.
    pub family_keys: Vec<FamilyKey>,
    /// Full-face and PostScript aliases for this exact face. These
    /// are style-insensitive lookup names: a subtitle that references
    /// `Dream Han Serif SC W22` is already naming a concrete face, so
    /// ASS bold/italic flags should not need to match the face attrs.
    pub face_name_aliases: Vec<String>,
}

/// One (family_name, bold, italic) tuple advertised by a font face.
/// Stored 1:N relative to a `FontMetadata` (one face → multiple keys).
#[derive(Debug, Clone)]
pub struct FamilyKey {
    pub family_name: String,
    pub bold: bool,
    pub italic: bool,
}

/// Whether a source contains only its root-level font files or every real
/// subdirectory below the root. Symlinks, junctions, and other reparse points
/// are never part of either scope.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum FontDirectoryScope {
    Shallow,
    Recursive,
}

impl FontDirectoryScope {
    fn db_value(self) -> i32 {
        match self {
            Self::Shallow => 0,
            Self::Recursive => 1,
        }
    }

    fn from_db(value: i32) -> Result<Self, CacheError> {
        match value {
            0 => Ok(Self::Shallow),
            1 => Ok(Self::Recursive),
            _ => Err(CacheError::Io(format!(
                "cached_sources.scope has invalid value {value}; rebuild required"
            ))),
        }
    }
}

/// Stable composite identity for one cached source. Scope is part of the key:
/// users may deliberately add the same canonical root once shallow and once
/// recursively, then remove either source without disturbing the other.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CacheSourceKey {
    pub source_root: String,
    pub scope: FontDirectoryScope,
}

impl PartialEq<&str> for CacheSourceKey {
    fn eq(&self, other: &&str) -> bool {
        self.scope == FontDirectoryScope::Shallow && self.source_root == *other
    }
}

impl PartialEq<String> for CacheSourceKey {
    fn eq(&self, other: &String) -> bool {
        self.scope == FontDirectoryScope::Shallow && self.source_root == *other
    }
}

/// One real directory observed while scanning a source.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FolderSnapshot {
    pub folder_path: String,
    pub folder_mtime: i64,
}

/// One allowed-extension regular file observed during metadata traversal.
/// Candidate files are tracked even when font parsing fails: replacing a
/// malformed/oversized file in place must invalidate the source cache.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileSnapshot {
    pub file_path: String,
    pub file_size: i64,
    pub file_mtime: i64,
}

/// Complete freshness snapshot captured during a successful source scan.
/// Recursive sources contain the root plus every visited real directory;
/// shallow sources contain the root only.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CacheSourceSnapshot {
    pub source_root: String,
    pub scope: FontDirectoryScope,
    pub directories: Vec<FolderSnapshot>,
    pub files: Vec<FileSnapshot>,
}

impl CacheSourceSnapshot {
    pub fn key(&self) -> CacheSourceKey {
        CacheSourceKey {
            source_root: self.source_root.clone(),
            scope: self.scope,
        }
    }
}

/// One source row plus its owned directory snapshot, returned by
/// [`FontCache::list_sources`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CacheSourceRecord {
    pub source_root: String,
    pub scope: FontDirectoryScope,
    pub source_order: i64,
    pub last_scanned_at: i64,
    pub directories: Vec<FolderSnapshot>,
    pub files: Vec<FileSnapshot>,
}

/// Transitional root-only view used by older shallow-only callers. New code
/// should use [`CacheSourceRecord`] so scope and nested snapshots are retained.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FolderRecord {
    pub folder_path: String,
    pub folder_mtime: i64,
    pub last_scanned_at: i64,
}

impl CacheSourceRecord {
    pub fn key(&self) -> CacheSourceKey {
        CacheSourceKey {
            source_root: self.source_root.clone(),
            scope: self.scope,
        }
    }
}

fn normalized_canonical_path(path: &Path) -> Result<(PathBuf, String), String> {
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
        let mut children = Vec::new();
        for entry in read_dir {
            let entry = entry
                .map_err(|e| format!("read directory entry in {}: {e}", directory.display()))?;
            children.push(entry.path());
        }
        children.sort();

        // Reverse push preserves ascending traversal with a LIFO stack. Final
        // output is sorted too, so database contents never depend on OS order.
        for child in children {
            visited_entries = visited_entries.saturating_add(1);
            if visited_entries > crate::fonts::MAX_PREFLIGHT_ENTRIES {
                return Err(format!(
                    "Font source exceeds the {}-entry traversal limit",
                    crate::fonts::MAX_PREFLIGHT_ENTRIES
                ));
            }
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

fn validate_source_snapshot_shape(
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

/// Drift detection result. Each variant lists composite source identities grouped by
/// what change is needed; the caller iterates these to decide actions
/// (rescan modified ones, evict removed ones, scan added ones).
///
/// Empty `added` / `modified` / `removed` collectively mean "cache is
/// in sync with current filesystem state" — caller can use the cache
/// as-is.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DriftReport {
    /// Sources present in the supplied snapshot but not in the cache.
    /// Need a fresh scan to populate cache rows.
    pub added: Vec<CacheSourceKey>,
    /// Sources in both cache and filesystem whose directory or candidate-file
    /// snapshot differs. Need a complete rescan of the source root.
    pub modified: Vec<CacheSourceKey>,
    /// Sources in the cache but not in the supplied snapshot. Need eviction.
    pub removed: Vec<CacheSourceKey>,
}

impl DriftReport {
    /// True when the cache is fully in sync with the filesystem
    /// snapshot — no sources need scanning, rescanning, or eviction.
    /// CLI uses this to decide whether to print the drift warning at
    /// startup; GUI uses it to decide whether to show the modal.
    pub fn is_empty(&self) -> bool {
        self.added.is_empty() && self.modified.is_empty() && self.removed.is_empty()
    }
}

/// Lookup result from `FontCache::lookup_family`. Identifies a single
/// font face (`font_path` + `face_index`) — both pieces are needed
/// for subsetting since TTC files require the face index alongside
/// the file path.
///
/// fields are `pub(crate)` so callers outside
/// app_lib (the CLI binary compiles as a separate crate against
/// app_lib) can't construct a `FontLookupResult` outside of
/// `FontCache::lookup_family`. Combined with
/// `fonts::register_cache_provenance` accepting `&FontLookupResult`
/// instead of `(&str, u32)`, the invariant "only
/// lookup_family hits register in ALLOWED_CACHE_FONT_PATHS" is now
/// enforced at the type layer rather than by manual convention.
/// Narrative comments decay across refactors; types don't.
/// Internal app_lib callers (`font_cache_commands.rs`) still read
/// fields directly under pub(crate); outside callers go through the
/// read-only getters / `into_parts`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FontLookupResult {
    pub(crate) font_path: String,
    pub(crate) face_index: i32,
}

impl FontLookupResult {
    /// Borrowed read access to the canonical font path.
    pub fn font_path(&self) -> &str {
        &self.font_path
    }

    /// Borrowed read access to the face index. Stored as i32 because
    /// SQLite's `cached_fonts.face_index` column is INTEGER (i64 in
    /// rusqlite) narrowed at insert time; runtime values are always
    /// non-negative.
    pub fn face_index(&self) -> i32 {
        self.face_index
    }

    /// Consume the result and yield `(path, face_index_u32)` for
    /// callers that need owned values (CLI's `resolve_embed_font`
    /// returns the tuple after a successful provenance registration).
    /// A negative i32 — which a hostile cache row could carry —
    /// surfaces as an explicit error rather than silently
    /// reinterpreting the bit pattern into a huge u32.
    pub fn into_parts(self) -> Result<(String, u32), String> {
        let face_index = u32::try_from(self.face_index).map_err(|_| {
            format!(
                "FontLookupResult has invalid negative face_index: {}",
                self.face_index
            )
        })?;
        Ok((self.font_path, face_index))
    }
}

/// Persistent font cache backed by SQLite. One instance per binary
/// (gui vs cli) — the caller chooses the file path.
pub struct FontCache {
    conn: Connection,
}

// Manual Debug impl: rusqlite::Connection doesn't derive Debug, so a
// `#[derive(Debug)]` on FontCache fails to compile. The cache's
// internal state isn't useful in panic messages anyway — knowing
// "FontCache existed when the test panicked" is enough.
impl std::fmt::Debug for FontCache {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FontCache").finish_non_exhaustive()
    }
}

/// Recoverable errors during cache operations. The caller chooses how
/// to react: CLI falls back to no-cache and warns; GUI prompts the user.
///
/// Unified across open/read/write to keep the public API simple — the
/// caller mostly cares about "did it work" + a message; specific
/// variant only matters for `SchemaVersionMismatch` which has its own
/// recovery path.
#[derive(Debug)]
pub enum CacheError {
    /// Filesystem or SQLite-level failure. Includes a human-readable
    /// message embedding the underlying error.
    Io(String),
    /// Existing cache file was opened, but its schema_version row
    /// either doesn't match `SCHEMA_VERSION` (different release) or is
    /// missing entirely (corrupt or pre-versioned cache). Both cases
    /// route to the same recovery path: rebuild the cache.
    /// Sentinels: `found = -1` for "row missing", `-2` for
    /// "row present but unparseable", any other value for "actual
    /// version found in the file".
    SchemaVersionMismatch { found: i32, expected: i32 },
}

impl std::fmt::Display for CacheError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(msg) => write!(f, "cache I/O error: {msg}"),
            Self::SchemaVersionMismatch { found, expected } if *found == -1 => write!(
                f,
                "cache schema_version row missing (cache predates version tracking \
                 or is corrupt); expected version {expected}, must rebuild"
            ),
            Self::SchemaVersionMismatch { found, expected } if *found == -2 => write!(
                f,
                "cache schema_version value unparseable (corrupt cache); \
                 expected version {expected}, must rebuild"
            ),
            Self::SchemaVersionMismatch { found, expected } => write!(
                f,
                "cache schema version {found} does not match expected {expected}; \
                 cache is from a different release and must be rebuilt"
            ),
        }
    }
}

impl FontCache {
    /// Open an existing cache file or create a fresh one. The caller
    /// passes the full file path; choosing AppData / temp / a custom
    /// location is the caller's concern (CLI vs GUI vs tests).
    ///
    /// On a fresh create, the schema is initialized and the current
    /// `SCHEMA_VERSION` is written to `cache_meta`.
    ///
    /// On open of an existing file, the schema_version row is verified
    /// against `SCHEMA_VERSION`. Any mismatch (including missing row)
    /// returns `SchemaVersionMismatch`; the caller decides recovery.
    pub fn open_or_create(cache_path: &Path) -> Result<Self, CacheError> {
        // Ensure the parent directory exists. If the caller passed a
        // path under a not-yet-created folder (e.g., %APPDATA%/ssahdrify
        // on a fresh user profile), this avoids a confusing
        // SQLITE_CANTOPEN error in favor of a clear filesystem error.
        if let Some(parent) = cache_path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent).map_err(|e| {
                    CacheError::Io(format!(
                        "creating parent directory {}: {e}",
                        parent.display()
                    ))
                })?;
            }
        }

        reject_cache_reparse_paths(cache_path)?;

        // Residual TOCTOU window between the `is_reparse_point`
        // check above and the `Connection::open` below — an attacker
        // who can swap `cache_path` to a symlink in this window
        // bypasses the guard. Accepted local-user risk: the per-user AppData
        // location requires an attacker who already has write
        // access to %APPDATA%/$XDG_DATA_HOME, which is well outside
        // the single-user-desktop threat model. Sibling acknowledgment
        // lives in `migrate_legacy_gui_cache` (also a single-syscall
        // narrow). Tighter forms would need OS-level
        // `O_NOFOLLOW` / `FILE_FLAG_OPEN_REPARSE_POINT` which
        // rusqlite doesn't expose.
        let already_existed = cache_path.exists();
        let conn = Connection::open(cache_path)
            .map_err(|e| CacheError::Io(format!("opening {}: {e}", cache_path.display())))?;

        // Keep the cross-run persistent cache in rollback-journal mode.
        // `diagnose-fonts` has a read-only contract, and a read-only WAL
        // open either needs SQLite sidecar access or an `immutable=1`
        // lie that can ignore committed WAL content. DELETE mode keeps
        // lookup-only diagnostics side-effect-free without sacrificing
        // cache correctness. The session DB in fonts.rs still uses WAL
        // because it is temp/run-local and write-heavy.
        conn.pragma_update(None, "journal_mode", "DELETE")
            .map_err(|e| CacheError::Io(format!("setting DELETE journal mode: {e}")))?;
        conn.busy_timeout(std::time::Duration::from_secs(5))
            .map_err(|e| CacheError::Io(format!("setting busy_timeout: {e}")))?;
        // Per-connection: SQLite ships with foreign_keys=OFF by default,
        // so the FOREIGN KEY clauses in SCHEMA_SQL would be decorative
        // unless turned on here. The session DB `open_user_font_db`
        // mirrors this PRAGMA for the same reason.
        conn.pragma_update(None, "foreign_keys", "ON")
            .map_err(|e| CacheError::Io(format!("enabling foreign_keys: {e}")))?;

        let cache = Self { conn };
        if already_existed {
            cache.verify_schema_version()?;
        } else {
            cache.init_schema()?;
        }
        Ok(cache)
    }

    /// Open an existing cache for lookup-only diagnostics.
    ///
    /// Unlike `open_or_create`, this does not create parent
    /// directories, initialize schema, or change journal mode. Callers
    /// use it when the command contract is read-only (`diagnose-fonts`).
    pub fn open_existing_read_only(cache_path: &Path) -> Result<Self, CacheError> {
        reject_cache_reparse_paths(cache_path)?;
        let conn = Connection::open_with_flags(cache_path, OpenFlags::SQLITE_OPEN_READ_ONLY)
            .map_err(|e| {
                CacheError::Io(format!("opening {} read-only: {e}", cache_path.display()))
            })?;
        conn.busy_timeout(std::time::Duration::from_secs(5))
            .map_err(|e| CacheError::Io(format!("setting busy_timeout: {e}")))?;
        conn.pragma_update(None, "foreign_keys", "ON")
            .map_err(|e| CacheError::Io(format!("enabling foreign_keys: {e}")))?;

        let cache = Self { conn };
        cache.verify_schema_version()?;
        Ok(cache)
    }

    /// Initialize an empty cache: create the four tables and write the
    /// current `SCHEMA_VERSION` to `cache_meta`. Called once on fresh
    /// create; idempotent if called on an empty DB but never invoked
    /// after open.
    fn init_schema(&self) -> Result<(), CacheError> {
        self.conn
            .execute_batch(SCHEMA_SQL)
            .map_err(|e| CacheError::Io(format!("initializing schema: {e}")))?;
        self.conn
            .execute(
                "INSERT INTO cache_meta(key, value) VALUES('schema_version', ?1)",
                params![SCHEMA_VERSION.to_string()],
            )
            .map_err(|e| CacheError::Io(format!("writing schema_version: {e}")))?;
        Ok(())
    }

    /// Read the schema_version row and compare against `SCHEMA_VERSION`.
    /// A missing or unparseable row counts as mismatch (cache predates
    /// version tracking, or corrupt).
    fn verify_schema_version(&self) -> Result<(), CacheError> {
        let has_cache_meta = self
            .conn
            .table_exists(None::<&str>, "cache_meta")
            .map_err(|e| CacheError::Io(format!("checking cache_meta table: {e}")))?;
        if !has_cache_meta {
            return Err(CacheError::SchemaVersionMismatch {
                found: -1,
                expected: SCHEMA_VERSION,
            });
        }

        let row: Result<String, _> = self.conn.query_row(
            "SELECT value FROM cache_meta WHERE key = 'schema_version'",
            [],
            |r| r.get(0),
        );
        match row {
            Ok(value) => {
                // Parse failure → use -2 sentinel to distinguish from
                // missing-row's -1, in case future diagnostics want to
                // know which way the data was wrong. Both still route
                // to "rebuild the cache."
                let found: i32 = value.parse().unwrap_or(-2);
                if found != SCHEMA_VERSION {
                    Err(CacheError::SchemaVersionMismatch {
                        found,
                        expected: SCHEMA_VERSION,
                    })
                } else {
                    Ok(())
                }
            }
            Err(rusqlite::Error::QueryReturnedNoRows) => Err(CacheError::SchemaVersionMismatch {
                found: -1,
                expected: SCHEMA_VERSION,
            }),
            Err(e) => Err(CacheError::Io(format!("reading schema_version: {e}"))),
        }
    }

    /// Atomically replace one source and every row it owns. Source identity is
    /// `(source_root, scope)`, so overlapping roots and shallow/recursive views
    /// of the same root cannot steal or delete each other's faces.
    pub fn replace_source(
        &mut self,
        snapshot: &CacheSourceSnapshot,
        fonts: &[FontMetadata],
    ) -> Result<(), CacheError> {
        validate_source_snapshot_shape(snapshot, fonts)?;
        let scope = snapshot.scope.db_value();
        let other_source_count: i64 = self
            .conn
            .query_row(
                "SELECT COUNT(*) FROM cached_sources \
                 WHERE NOT (source_root = ?1 AND scope = ?2)",
                params![snapshot.source_root, scope],
                |row| row.get(0),
            )
            .map_err(|e| CacheError::Io(format!("count cached_sources: {e}")))?;
        if usize::try_from(other_source_count).unwrap_or(usize::MAX) >= MAX_CACHED_SOURCES {
            return Err(CacheError::Io(format!(
                "cached_sources is at the {MAX_CACHED_SOURCES}-source sanity cap"
            )));
        }
        let other_directory_count: i64 = self
            .conn
            .query_row(
                "SELECT COUNT(*) FROM cached_directories \
                 WHERE NOT (source_root = ?1 AND scope = ?2)",
                params![snapshot.source_root, scope],
                |row| row.get(0),
            )
            .map_err(|e| CacheError::Io(format!("count cached_directories: {e}")))?;
        checked_projected_count(
            other_directory_count,
            snapshot.directories.len(),
            MAX_CACHED_DIRECTORIES,
            "cached_directories",
        )?;
        let other_file_count: i64 = self
            .conn
            .query_row(
                "SELECT COUNT(*) FROM cached_source_files \
                 WHERE NOT (source_root = ?1 AND scope = ?2)",
                params![snapshot.source_root, scope],
                |row| row.get(0),
            )
            .map_err(|e| CacheError::Io(format!("count cached_source_files: {e}")))?;
        checked_projected_count(
            other_file_count,
            snapshot.files.len(),
            crate::fonts::MAX_PREFLIGHT_ENTRIES,
            "cached_source_files",
        )?;
        let other_path_bytes: i64 = self
            .conn
            .query_row(
                "SELECT \
                   COALESCE((SELECT SUM(length(CAST(source_root AS BLOB))) \
                             FROM cached_sources \
                             WHERE NOT (source_root = ?1 AND scope = ?2)), 0) + \
                   COALESCE((SELECT SUM(length(CAST(directory_path AS BLOB))) \
                             FROM cached_directories \
                             WHERE NOT (source_root = ?1 AND scope = ?2)), 0) + \
                   COALESCE((SELECT SUM(length(CAST(file_path AS BLOB))) \
                             FROM cached_source_files \
                             WHERE NOT (source_root = ?1 AND scope = ?2)), 0)",
                params![snapshot.source_root, scope],
                |row| row.get(0),
            )
            .map_err(|e| CacheError::Io(format!("count cached snapshot path bytes: {e}")))?;
        checked_projected_path_bytes(other_path_bytes, snapshot_retained_path_bytes(snapshot)?)?;

        let now = current_unix_seconds().ok_or_else(|| {
            CacheError::Io("system clock is before Unix epoch (1970-01-01)".to_string())
        })?;
        let source_order: i64 = self
            .conn
            .query_row(
                "SELECT COALESCE(\
                    (SELECT source_order FROM cached_sources \
                     WHERE source_root = ?1 AND scope = ?2), \
                    (SELECT COALESCE(MAX(source_order), 0) + 1 FROM cached_sources)\
                 )",
                params![snapshot.source_root, scope],
                |row| row.get(0),
            )
            .map_err(|e| CacheError::Io(format!("compute source order: {e}")))?;

        let tx = self
            .conn
            .transaction()
            .map_err(|e| CacheError::Io(format!("begin transaction: {e}")))?;
        tx.execute(
            "DELETE FROM cached_sources WHERE source_root = ?1 AND scope = ?2",
            params![snapshot.source_root, scope],
        )
        .map_err(|e| CacheError::Io(format!("delete previous source: {e}")))?;
        tx.execute(
            "INSERT INTO cached_sources(source_root, scope, source_order, last_scanned_at) \
             VALUES(?1, ?2, ?3, ?4)",
            params![snapshot.source_root, scope, source_order, now],
        )
        .map_err(|e| CacheError::Io(format!("insert source: {e}")))?;

        for directory in &snapshot.directories {
            tx.execute(
                "INSERT INTO cached_directories(\
                    source_root, scope, directory_path, directory_mtime\
                 ) VALUES(?1, ?2, ?3, ?4)",
                params![
                    snapshot.source_root,
                    scope,
                    directory.folder_path,
                    directory.folder_mtime
                ],
            )
            .map_err(|e| CacheError::Io(format!("insert tracked directory: {e}")))?;
        }
        for file in &snapshot.files {
            tx.execute(
                "INSERT INTO cached_source_files(\
                    source_root, scope, file_path, file_size, file_mtime\
                 ) VALUES(?1, ?2, ?3, ?4, ?5)",
                params![
                    snapshot.source_root,
                    scope,
                    file.file_path,
                    file.file_size,
                    file.file_mtime
                ],
            )
            .map_err(|e| CacheError::Io(format!("insert candidate file: {e}")))?;
        }
        for font in fonts {
            tx.execute(
                "INSERT INTO cached_fonts(\
                    source_root, scope, font_path, face_index, file_size, file_mtime\
                 ) VALUES(?1, ?2, ?3, ?4, ?5, ?6)",
                params![
                    snapshot.source_root,
                    scope,
                    font.file_path,
                    font.face_index,
                    font.file_size,
                    font.file_mtime,
                ],
            )
            .map_err(|e| CacheError::Io(format!("insert font {}: {e}", font.file_path)))?;

            for key in &font.family_keys {
                tx.execute(
                    "INSERT OR IGNORE INTO cached_family_keys(\
                        source_root, scope, font_path, face_index, family_name, \
                        family_name_key, key_kind, bold, italic\
                     ) VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                    params![
                        snapshot.source_root,
                        scope,
                        font.file_path,
                        font.face_index,
                        key.family_name,
                        family_lookup_key(&key.family_name),
                        KEY_KIND_FAMILY,
                        i32::from(key.bold),
                        i32::from(key.italic),
                    ],
                )
                .map_err(|e| CacheError::Io(format!("insert family key: {e}")))?;
            }
            for alias in &font.face_name_aliases {
                tx.execute(
                    "INSERT OR IGNORE INTO cached_family_keys(\
                        source_root, scope, font_path, face_index, family_name, \
                        family_name_key, key_kind, bold, italic\
                     ) VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, 0, 0)",
                    params![
                        snapshot.source_root,
                        scope,
                        font.file_path,
                        font.face_index,
                        alias,
                        family_lookup_key(alias),
                        KEY_KIND_FACE_ALIAS,
                    ],
                )
                .map_err(|e| CacheError::Io(format!("insert face-name alias: {e}")))?;
            }
        }
        tx.commit()
            .map_err(|e| CacheError::Io(format!("commit source replacement: {e}")))
    }

    /// Delete exactly one `(root, scope)` source and all owned rows. Foreign-key
    /// cascades make this one transaction and preserve overlapping owners.
    pub fn remove_source(
        &mut self,
        source_root: &str,
        scope: FontDirectoryScope,
    ) -> Result<(), CacheError> {
        reject_unsupported_cached_folder_path(source_root)?;
        let tx = self
            .conn
            .transaction()
            .map_err(|e| CacheError::Io(format!("begin transaction: {e}")))?;
        tx.execute(
            "DELETE FROM cached_sources WHERE source_root = ?1 AND scope = ?2",
            params![source_root, scope.db_value()],
        )
        .map_err(|e| CacheError::Io(format!("delete source: {e}")))?;
        tx.commit()
            .map_err(|e| CacheError::Io(format!("commit source removal: {e}")))
    }

    /// Remove every source atomically without dropping/recreating the cache.
    pub fn clear_sources(&mut self) -> Result<usize, CacheError> {
        let tx = self
            .conn
            .transaction()
            .map_err(|e| CacheError::Io(format!("begin transaction: {e}")))?;
        let removed = tx
            .execute("DELETE FROM cached_sources", [])
            .map_err(|e| CacheError::Io(format!("clear cached sources: {e}")))?;
        tx.commit()
            .map_err(|e| CacheError::Io(format!("commit source clear: {e}")))?;
        Ok(removed)
    }

    /// Compatibility wrapper for shallow-only call sites while the CLI moves
    /// to source snapshots. New code should call [`Self::replace_source`].
    pub fn replace_folder(
        &mut self,
        folder_path: &str,
        folder_mtime: i64,
        fonts: &[FontMetadata],
    ) -> Result<(), CacheError> {
        let mut files: Vec<FileSnapshot> = fonts
            .iter()
            .map(|font| FileSnapshot {
                file_path: font.file_path.clone(),
                file_size: font.file_size,
                file_mtime: font.file_mtime,
            })
            .collect();
        files.sort_by(|a, b| a.file_path.cmp(&b.file_path));
        files.dedup_by(|a, b| a.file_path == b.file_path);
        self.replace_source(
            &CacheSourceSnapshot {
                source_root: folder_path.to_string(),
                scope: FontDirectoryScope::Shallow,
                directories: vec![FolderSnapshot {
                    folder_path: folder_path.to_string(),
                    folder_mtime,
                }],
                files,
            },
            fonts,
        )
    }

    /// Compatibility wrapper for legacy shallow-only callers.
    pub fn remove_folder(&mut self, folder_path: &str) -> Result<(), CacheError> {
        self.remove_source(folder_path, FontDirectoryScope::Shallow)
    }

    /// Compatibility comparison for legacy shallow-only callers. New code uses
    /// [`FontCache::diff_sources`] with complete directory and candidate-file
    /// snapshots.
    ///
    /// The drift categories follow the locked design:
    /// - **added**: in the filesystem snapshot but not in the cache.
    ///   Caller scans these and calls `replace_folder` for each.
    /// - **modified**: in both, but `folder_mtime` differs. Caller
    ///   rescans (catches files added/removed/renamed inside the
    ///   folder per the locked stat-based invalidation strategy).
    /// - **removed**: in the cache but not in the filesystem
    ///   snapshot. Caller calls `remove_folder` for each.
    ///
    /// Folders unchanged (in both with matching mtime) are silently
    /// OK and don't appear in any report list.
    pub fn diff_against(
        &self,
        current_folders: &[(String, i64)],
    ) -> Result<DriftReport, CacheError> {
        // Pre-build a map of the shallow compatibility view keyed by root path;
        // subsequent membership checks are O(1).
        let cached: std::collections::HashMap<String, i64> = self
            .list_folders()?
            .into_iter()
            .map(|r| (r.folder_path, r.folder_mtime))
            .collect();

        // Map current folders for O(1) "is this in the snapshot?"
        // lookup when checking the cache side. Last-write-wins on
        // duplicates (caller bug); we don't validate.
        let current: std::collections::HashMap<&str, i64> = current_folders
            .iter()
            .map(|(p, m)| (p.as_str(), *m))
            .collect();

        let mut report = DriftReport::default();

        for (path, current_mtime) in &current {
            match cached.get(*path) {
                None => report.added.push(CacheSourceKey {
                    source_root: (*path).to_string(),
                    scope: FontDirectoryScope::Shallow,
                }),
                Some(cached_mtime) if cached_mtime != current_mtime => {
                    report.modified.push(CacheSourceKey {
                        source_root: (*path).to_string(),
                        scope: FontDirectoryScope::Shallow,
                    });
                }
                Some(_) => {
                    // mtime matches — unchanged, no report entry
                }
            }
        }

        for cached_path in cached.keys() {
            if !current.contains_key(cached_path.as_str()) {
                report.removed.push(CacheSourceKey {
                    source_root: cached_path.clone(),
                    scope: FontDirectoryScope::Shallow,
                });
            }
        }

        // Sort each list for deterministic output (test assertions,
        // reproducible stderr diff reports). Cheap; lists are small.
        report.added.sort();
        report.modified.sort();
        report.removed.sort();

        Ok(report)
    }

    /// Look up a font face by family name + bold/italic flags. Returns
    /// `Some(FontLookupResult { font_path, face_index })` for the
    /// preferred match, or `None` if no font in the cache advertises the
    /// requested family + style combination.
    ///
    /// Match semantics: NFC-normalize + full Unicode lowercase via
    /// `family_lookup_key` on BOTH the query (here) and the storage
    /// path (`replace_folder`). Exact family-name rows must match
    /// bold/italic exactly; full-face/PostScript alias rows are
    /// style-insensitive because the alias already names a concrete
    /// face. Exact family rows sort before alias rows so an alias can
    /// never override a true family/style hit. Mirrors the session
    /// DB lookup contract so a font's
    /// name-table form (often NFC) and an ASS file's `\fn` reference
    /// (often macOS-pasted NFD or arbitrary case) match consistently.
    /// (NOT ASCII-only lowercase — `to_ascii_lowercase` would miss
    /// `É` / `Ñ` / `Ü` and break Latin-extended / CJK lookups.)
    ///
    /// Resolution order matches the in-session database: exact family/style
    /// keys beat face-name aliases; shallow sources beat recursive sources;
    /// the newest source wins within a tier; path and face index break any
    /// remaining tie deterministically. The same source set therefore resolves
    /// the same way before and after an app restart.
    ///
    /// Note on shared helper: persistent cache and session DB use
    /// different schemas (`cached_family_keys` vs `font_family_keys`)
    /// and different column names — extracting a single
    /// `family_lookup(conn, ...)` would require parameterizing the
    /// table/column shape, dragging both consumers' invariants into
    /// one helper without a real win. Both queries are short and
    /// already tested in isolation; intentional duplication.
    pub fn lookup_family(
        &self,
        family_name: &str,
        bold: bool,
        italic: bool,
    ) -> Result<Option<FontLookupResult>, CacheError> {
        let lookup_key = family_lookup_key(family_name);
        let mut statement = self
            .conn
            .prepare(FAMILY_LOOKUP_SQL)
            .map_err(|error| CacheError::Io(format!("prepare lookup_family: {error}")))?;
        let candidate_limit = i64::try_from(MAX_CACHE_LOOKUP_CANDIDATES + 1)
            .expect("cache lookup candidate limit fits i64");
        let mut rows = statement
            .query(params![
                lookup_key,
                KEY_KIND_FAMILY,
                i32::from(bold),
                i32::from(italic),
                KEY_KIND_FACE_ALIAS,
                candidate_limit,
            ])
            .map_err(|error| CacheError::Io(format!("query lookup_family: {error}")))?;

        let mut skipped = 0usize;
        let mut first_validation_error = None;
        while let Some(row) = rows
            .next()
            .map_err(|error| CacheError::Io(format!("read lookup_family row: {error}")))?
        {
            if skipped >= MAX_CACHE_LOOKUP_CANDIDATES {
                warn_skipped_cache_candidates(skipped);
                return Err(first_validation_error.unwrap_or_else(|| {
                    CacheError::Io(format!(
                        "font cache lookup exceeds the {MAX_CACHE_LOOKUP_CANDIDATES}-candidate filesystem-probe budget; rebuild required"
                    ))
                }));
            }
            let candidate = (
                row.get::<_, String>(0),
                row.get::<_, i32>(1),
                row.get::<_, String>(2),
                row.get::<_, i64>(3),
                row.get::<_, i64>(4),
            );
            let (font_path, face_index, folder_path, file_size, file_mtime) = match candidate {
                (Ok(font_path), Ok(face_index), Ok(folder_path), Ok(file_size), Ok(file_mtime)) => {
                    (font_path, face_index, folder_path, file_size, file_mtime)
                }
                _ => {
                    skipped += 1;
                    if first_validation_error.is_none() {
                        first_validation_error = Some(CacheError::Io(
                            "lookup_family row has invalid column types; rebuild required"
                                .to_string(),
                        ));
                    }
                    continue;
                }
            };
            match validate_cached_font_candidate(
                &font_path,
                face_index,
                &folder_path,
                file_size,
                file_mtime,
            ) {
                Ok(()) => {
                    warn_skipped_cache_candidates(skipped);
                    return Ok(Some(FontLookupResult {
                        font_path,
                        face_index,
                    }));
                }
                Err(error) => {
                    skipped += 1;
                    if first_validation_error.is_none() {
                        first_validation_error = Some(error);
                    }
                }
            }
        }

        warn_skipped_cache_candidates(skipped);
        match first_validation_error {
            Some(error) => Err(error),
            None => Ok(None),
        }
    }

    /// List every source with the full directory and candidate-file snapshot.
    pub fn list_sources(&self) -> Result<Vec<CacheSourceRecord>, CacheError> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT source_root, scope, source_order, last_scanned_at \
                 FROM cached_sources ORDER BY source_root, scope",
            )
            .map_err(|e| CacheError::Io(format!("prepare list_sources: {e}")))?;
        let rows = stmt
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, i32>(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, i64>(3)?,
                ))
            })
            .map_err(|e| CacheError::Io(format!("execute list_sources: {e}")))?;
        let mut out = Vec::new();
        let mut total_directory_rows = 0usize;
        let mut total_file_rows = 0usize;
        let mut total_path_bytes = 0usize;
        for row in rows {
            let (source_root, scope_value, source_order, last_scanned_at) =
                row.map_err(|e| CacheError::Io(format!("read source row: {e}")))?;
            reject_unsupported_cached_folder_path(&source_root)?;
            let mut source_path_bytes = source_root.len();
            total_path_bytes = total_path_bytes
                .checked_add(source_root.len())
                .ok_or_else(|| CacheError::Io("cached path-byte count overflowed".to_string()))?;
            enforce_cached_path_byte_budgets(source_path_bytes, total_path_bytes)?;
            let scope = FontDirectoryScope::from_db(scope_value)?;
            let mut directory_stmt = self
                .conn
                .prepare(
                    "SELECT directory_path, directory_mtime FROM cached_directories \
                     WHERE source_root = ?1 AND scope = ?2 ORDER BY directory_path",
                )
                .map_err(|e| CacheError::Io(format!("prepare source directories: {e}")))?;
            let directory_rows = directory_stmt
                .query_map(params![source_root, scope_value], |row| {
                    Ok(FolderSnapshot {
                        folder_path: row.get(0)?,
                        folder_mtime: row.get(1)?,
                    })
                })
                .map_err(|e| CacheError::Io(format!("read source directories: {e}")))?;
            let mut directories = Vec::new();
            for directory in directory_rows {
                let directory =
                    directory.map_err(|e| CacheError::Io(format!("read directory row: {e}")))?;
                reject_unsupported_cached_directory_path(&directory.folder_path)?;
                source_path_bytes = source_path_bytes
                    .checked_add(directory.folder_path.len())
                    .ok_or_else(|| {
                        CacheError::Io("cached source path-byte count overflowed".to_string())
                    })?;
                total_path_bytes = total_path_bytes
                    .checked_add(directory.folder_path.len())
                    .ok_or_else(|| {
                        CacheError::Io("cached path-byte count overflowed".to_string())
                    })?;
                enforce_cached_path_byte_budgets(source_path_bytes, total_path_bytes)?;
                directories.push(directory);
                total_directory_rows = total_directory_rows.saturating_add(1);
                if directories.len() > MAX_CACHED_DIRECTORIES {
                    return Err(CacheError::Io(format!(
                        "cached_directories exceeds the {MAX_CACHED_DIRECTORIES}-row sanity cap"
                    )));
                }
                if total_directory_rows > MAX_CACHED_DIRECTORIES {
                    return Err(CacheError::Io(format!(
                        "cached_directories exceeds the {MAX_CACHED_DIRECTORIES}-row global \
                         sanity cap; cache file appears corrupted or hostile — rebuild required"
                    )));
                }
            }
            let mut file_stmt = self
                .conn
                .prepare(
                    "SELECT file_path, file_size, file_mtime FROM cached_source_files \
                     WHERE source_root = ?1 AND scope = ?2 ORDER BY file_path",
                )
                .map_err(|e| CacheError::Io(format!("prepare source files: {e}")))?;
            let file_rows = file_stmt
                .query_map(params![source_root, scope_value], |row| {
                    Ok(FileSnapshot {
                        file_path: row.get(0)?,
                        file_size: row.get(1)?,
                        file_mtime: row.get(2)?,
                    })
                })
                .map_err(|e| CacheError::Io(format!("read source files: {e}")))?;
            let mut files = Vec::new();
            for file in file_rows {
                let file = file.map_err(|e| CacheError::Io(format!("read file row: {e}")))?;
                reject_unsupported_cached_font_path(&file.file_path)?;
                source_path_bytes = source_path_bytes
                    .checked_add(file.file_path.len())
                    .ok_or_else(|| {
                        CacheError::Io("cached source path-byte count overflowed".to_string())
                    })?;
                total_path_bytes = total_path_bytes
                    .checked_add(file.file_path.len())
                    .ok_or_else(|| {
                        CacheError::Io("cached path-byte count overflowed".to_string())
                    })?;
                enforce_cached_path_byte_budgets(source_path_bytes, total_path_bytes)?;
                files.push(file);
                total_file_rows = total_file_rows.saturating_add(1);
                if files.len() > crate::fonts::MAX_SCAN_FONT_FILES {
                    return Err(CacheError::Io(format!(
                        "one cached source exceeds the {}-candidate-file limit; rebuild required",
                        crate::fonts::MAX_SCAN_FONT_FILES
                    )));
                }
                if total_file_rows > crate::fonts::MAX_PREFLIGHT_ENTRIES {
                    return Err(CacheError::Io(format!(
                        "cached_source_files exceeds the {}-row global sanity cap; \
                         cache file appears corrupted or hostile — rebuild required",
                        crate::fonts::MAX_PREFLIGHT_ENTRIES
                    )));
                }
            }
            out.push(CacheSourceRecord {
                source_root,
                scope,
                source_order,
                last_scanned_at,
                directories,
                files,
            });
            if out.len() > MAX_CACHED_SOURCES {
                return Err(CacheError::Io(format!(
                    "cached_sources table exceeds {MAX_CACHED_SOURCES}-row sanity cap; \
                     cache file appears corrupted or hostile — rebuild required"
                )));
            }
        }
        Ok(out)
    }

    /// Compare complete source snapshots. Any nested directory or candidate-
    /// file delta collapses to the owning source key in `modified`.
    pub fn diff_sources(
        &self,
        current_sources: &[CacheSourceSnapshot],
    ) -> Result<DriftReport, CacheError> {
        let cached: HashMap<CacheSourceKey, (Vec<FolderSnapshot>, Vec<FileSnapshot>)> = self
            .list_sources()?
            .into_iter()
            .map(|source| (source.key(), (source.directories, source.files)))
            .collect();
        let current: HashMap<CacheSourceKey, (&[FolderSnapshot], &[FileSnapshot])> =
            current_sources
                .iter()
                .map(|source| {
                    (
                        source.key(),
                        (source.directories.as_slice(), source.files.as_slice()),
                    )
                })
                .collect();
        let mut report = DriftReport::default();
        for (key, (directories, files)) in &current {
            match cached.get(key) {
                None => report.added.push(key.clone()),
                Some((cached_directories, cached_files))
                    if cached_directories.as_slice() != *directories
                        || cached_files.as_slice() != *files =>
                {
                    report.modified.push(key.clone());
                }
                Some(_) => {}
            }
        }
        for key in cached.keys() {
            if !current.contains_key(key) {
                report.removed.push(key.clone());
            }
        }
        report.added.sort();
        report.modified.sort();
        report.removed.sort();
        Ok(report)
    }

    /// Transitional shallow-root view. New code should call `list_sources`.
    pub fn list_folders(&self) -> Result<Vec<FolderRecord>, CacheError> {
        self.list_sources()?
            .into_iter()
            .map(|source| {
                let root_mtime = source
                    .directories
                    .iter()
                    .find(|directory| directory.folder_path == source.source_root)
                    .map(|directory| directory.folder_mtime)
                    .ok_or_else(|| CacheError::Io("source root snapshot missing".to_string()))?;
                Ok(FolderRecord {
                    folder_path: source.source_root,
                    folder_mtime: root_mtime,
                    last_scanned_at: source.last_scanned_at,
                })
            })
            .collect()
    }
}

/// Current Unix timestamp in seconds. Used for `last_scanned_at` on
/// inserts. Returns `None` if the system clock is somehow before the
/// Unix epoch — impossible in practice, but the symmetric posture
/// matches `try_modified_at` (that helper was tightened from
/// `.unwrap_or(0)` to `.ok()?` for the same sentinel-collision
/// concern). Callers surface `None` as `CacheError::Io` with a
/// "system clock before Unix epoch" message rather than persisting
/// epoch-zero into SQLite.
fn current_unix_seconds() -> Option<i64> {
    // `i64::try_from` (not `as i64`) for posture symmetry
    // with `try_modified_at`. The cast can't overflow until year ~292
    // billion, but the typed conversion makes the "this could fail"
    // contract type-level explicit rather than relying on doc
    // discipline.
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .and_then(|d| i64::try_from(d.as_secs()).ok())
}

/// Schema SQL — one statement per table. Tables match the current
/// persistent font-cache layout.
///
/// `cached_sources.last_scanned_at` is a diagnostic Unix-seconds timestamp.
/// Freshness instead compares nanosecond mtimes and sizes stored in
/// `cached_directories` and `cached_source_files`.
///
/// Every owned table includes `(source_root, scope)` in its key. A single
/// physical face may therefore belong to overlapping sources without either
/// source stealing the other's row. `cached_fonts` additionally keys by
/// `(font_path, face_index)`: a single
/// TTC file (TrueType Collection) holds multiple faces, each with its
/// own family names and addressable independently for subsetting. The
/// composite key lets one font_path appear N times — once per face.
/// `face_index` is 0 for non-TTC files; >=0 for TTC.
///
/// `cached_family_keys` PK includes normalized family/style identity plus
/// source + face identity, so the same face_index of the same file
/// can appear for multiple family aliases — CJK fonts especially
/// advertise family names in several language IDs (Latin + Simplified
/// Chinese + Traditional + Japanese + Korean) on one face. Embed-time
/// lookup must hit whichever locale the subtitle author wrote.
const SCHEMA_SQL: &str = r#"
CREATE TABLE cached_sources (
    source_root     TEXT NOT NULL,
    scope           INTEGER NOT NULL CHECK(scope IN (0, 1)),
    source_order    INTEGER NOT NULL UNIQUE,
    last_scanned_at INTEGER NOT NULL,
    PRIMARY KEY (source_root, scope)
);
CREATE TABLE cached_directories (
    source_root     TEXT NOT NULL,
    scope           INTEGER NOT NULL,
    directory_path TEXT NOT NULL,
    directory_mtime INTEGER NOT NULL,
    PRIMARY KEY (source_root, scope, directory_path),
    FOREIGN KEY (source_root, scope) REFERENCES cached_sources(source_root, scope)
        ON DELETE CASCADE
);
CREATE TABLE cached_source_files (
    source_root     TEXT NOT NULL,
    scope           INTEGER NOT NULL,
    file_path       TEXT NOT NULL,
    file_size       INTEGER NOT NULL,
    file_mtime      INTEGER NOT NULL,
    PRIMARY KEY (source_root, scope, file_path),
    FOREIGN KEY (source_root, scope) REFERENCES cached_sources(source_root, scope)
        ON DELETE CASCADE
);
CREATE TABLE cached_fonts (
    source_root     TEXT NOT NULL,
    scope           INTEGER NOT NULL,
    font_path       TEXT NOT NULL,
    face_index      INTEGER NOT NULL CHECK(face_index >= 0),
    file_size       INTEGER NOT NULL CHECK(file_size >= 0),
    file_mtime      INTEGER NOT NULL CHECK(file_mtime >= 0),
    PRIMARY KEY (source_root, scope, font_path, face_index),
    FOREIGN KEY (source_root, scope) REFERENCES cached_sources(source_root, scope)
        ON DELETE CASCADE,
    FOREIGN KEY (source_root, scope, font_path)
        REFERENCES cached_source_files(source_root, scope, file_path)
        ON DELETE CASCADE
);
CREATE TABLE cached_family_keys (
    source_root      TEXT NOT NULL,
    scope             INTEGER NOT NULL,
    font_path         TEXT NOT NULL,
    face_index        INTEGER NOT NULL,
    family_name       TEXT NOT NULL,
    family_name_key   TEXT NOT NULL,
    key_kind          INTEGER NOT NULL CHECK(key_kind IN (0, 1)),
    bold              INTEGER NOT NULL,
    italic            INTEGER NOT NULL,
    PRIMARY KEY (
        source_root, scope, family_name_key, key_kind, bold, italic,
        font_path, face_index
    ),
    FOREIGN KEY (source_root, scope, font_path, face_index)
        REFERENCES cached_fonts(source_root, scope, font_path, face_index)
        ON DELETE CASCADE
);
CREATE INDEX idx_cached_family_lookup ON cached_family_keys(
    family_name_key, key_kind, bold, italic,
    source_root, scope, font_path, face_index
);
CREATE TABLE cache_meta (
    key             TEXT PRIMARY KEY,
    value           TEXT NOT NULL
);
"#;

#[cfg(test)]
mod tests {
    use super::*;
    use std::{fs, path::Path};

    /// RAII guard for one test's temporary cache directory. Drop
    /// removes the entire dir + WAL sidecars, even on test panic —
    /// the previous bare-PathBuf helper relied on OS temp cleanup
    /// for panic paths and accumulated stale dirs across runs.
    ///
    /// Cross-reference: a sibling `TempCacheDir` lives at
    /// `font_cache_commands.rs::tests::TempCacheDir` for the GUI-side
    /// IPC tests. Same posture, NOT identical: the commands-side
    /// version takes a `name: &str` constructor arg, this one takes
    /// no args; the seed-strength forms also diverge (`subsec_nanos`
    /// in commands-side vs `as_nanos` here — the latter is wider
    /// entropy and is the better default for parallel-test collision
    /// avoidance, but the difference hasn't surfaced as a real
    /// collision yet). Don't conflate the two when reading either
    /// side's WHY block.
    ///
    /// Other test modules (`dropzone.rs::tests`,
    /// `safe_io.rs::tests::temp_dir`, `fonts.rs::tests`) use inline
    /// `std::env::temp_dir().join(...)` without RAII — those test
    /// bodies rely on per-test `fs::remove_dir_all` calls. A refactor
    /// extracting a single shared `#[cfg(test)] mod test_helpers`
    /// would need to harmonize the suffix shapes (each module's
    /// per-suite prefix carries diagnostic value when stale dirs need
    /// attribution); the cost-benefit hasn't tipped toward
    /// consolidation.
    struct TempCacheDir(std::path::PathBuf);

    impl TempCacheDir {
        fn new() -> Self {
            let stamp = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0);
            let dir = std::env::temp_dir().join(format!(
                "ssahdrify-font-cache-test-{}-{}",
                std::process::id(),
                stamp
            ));
            fs::create_dir_all(&dir).expect("create test temp dir");
            Self(dir)
        }

        fn cache_path(&self) -> std::path::PathBuf {
            self.0.join("cache.sqlite3")
        }
    }

    impl Drop for TempCacheDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    /// Convenience for tests that want the path directly. Returns
    /// (guard, path) — guard MUST stay in scope for the test's
    /// duration; binding it as `_` would drop it immediately.
    fn temp_cache_path() -> (TempCacheDir, std::path::PathBuf) {
        let guard = TempCacheDir::new();
        let path = guard.cache_path();
        (guard, path)
    }

    /// Like `temp_cache_path`, but the cache file path lives one
    /// directory deeper than the guard — used to verify
    /// `open_or_create` creates missing parent directories. The guard
    /// owns the OUTER directory and Drop-cleans the entire tree.
    fn temp_nested_cache_path() -> (TempCacheDir, std::path::PathBuf) {
        let guard = TempCacheDir::new();
        let nested_path = guard.0.join("nested").join("cache.sqlite3");
        (guard, nested_path)
    }

    fn sqlite_sidecar_path(path: &Path, suffix: &str) -> std::path::PathBuf {
        let mut sidecar = path.as_os_str().to_os_string();
        sidecar.push(suffix);
        std::path::PathBuf::from(sidecar)
    }

    #[test]
    fn fresh_open_creates_schema_and_writes_version() {
        let (_guard, path) = temp_cache_path();
        let cache = FontCache::open_or_create(&path).expect("fresh cache opens");

        // All six schema-v6 tables present.
        let table_count: i32 = cache
            .conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name IN \
                 ('cached_sources', 'cached_directories', 'cached_source_files', \
                  'cached_fonts', 'cached_family_keys', 'cache_meta')",
                [],
                |r| r.get(0),
            )
            .expect("query schema tables");
        assert_eq!(table_count, 6, "expected all six tables created");

        // schema_version row written.
        let version: String = cache
            .conn
            .query_row(
                "SELECT value FROM cache_meta WHERE key = 'schema_version'",
                [],
                |r| r.get(0),
            )
            .expect("query schema_version");
        assert_eq!(version, SCHEMA_VERSION.to_string());
    }

    #[test]
    fn family_lookup_uses_the_family_first_index() {
        let (_guard, path) = temp_cache_path();
        let cache = FontCache::open_or_create(&path).expect("fresh cache opens");
        let explain_sql = format!("EXPLAIN QUERY PLAN {FAMILY_LOOKUP_SQL}");
        let mut statement = cache.conn.prepare(&explain_sql).unwrap();
        let details: Vec<String> = statement
            .query_map(
                params![
                    "demo",
                    0,
                    0,
                    0,
                    1,
                    i64::try_from(MAX_CACHE_LOOKUP_CANDIDATES + 1).unwrap()
                ],
                |row| row.get(3),
            )
            .unwrap()
            .map(Result::unwrap)
            .collect();
        assert!(
            details
                .iter()
                .any(|detail| detail.contains("idx_cached_family_lookup")),
            "lookup plan must use the family-first index: {details:?}"
        );
    }

    #[test]
    fn projected_global_cache_budgets_accept_boundary_and_reject_one_over() {
        assert_eq!(
            checked_projected_count(199_999, 1, 200_000, "candidate rows").unwrap(),
            200_000
        );
        assert!(checked_projected_count(200_000, 1, 200_000, "candidate rows").is_err());

        assert_eq!(
            checked_projected_path_bytes(
                i64::try_from(MAX_CACHED_SNAPSHOT_PATH_BYTES - 1).unwrap(),
                1,
            )
            .unwrap(),
            MAX_CACHED_SNAPSHOT_PATH_BYTES
        );
        assert!(checked_projected_path_bytes(
            i64::try_from(MAX_CACHED_SNAPSHOT_PATH_BYTES).unwrap(),
            1,
        )
        .is_err());
        assert!(enforce_cached_path_byte_budgets(
            crate::fonts::MAX_SCAN_PATH_BYTES,
            MAX_CACHED_SNAPSHOT_PATH_BYTES,
        )
        .is_ok());
        assert!(enforce_cached_path_byte_budgets(
            crate::fonts::MAX_SCAN_PATH_BYTES + 1,
            MAX_CACHED_SNAPSHOT_PATH_BYTES,
        )
        .is_err());
        assert!(enforce_cached_path_byte_budgets(
            crate::fonts::MAX_SCAN_PATH_BYTES,
            MAX_CACHED_SNAPSHOT_PATH_BYTES + 1,
        )
        .is_err());
    }

    #[test]
    fn read_only_open_does_not_create_sqlite_sidecars() {
        let (_guard, path) = temp_cache_path();
        FontCache::open_or_create(&path).expect("fresh cache opens");

        for suffix in ["-wal", "-shm"] {
            let sidecar = sqlite_sidecar_path(&path, suffix);
            match fs::remove_file(&sidecar) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => panic!(
                    "failed to remove setup sidecar {}: {error}",
                    sidecar.display()
                ),
            }
            assert!(
                !sidecar.exists(),
                "setup should start without {suffix} sidecar"
            );
        }

        let cache = FontCache::open_existing_read_only(&path).expect("read-only cache opens");
        assert!(cache
            .list_folders()
            .expect("read-only query works")
            .is_empty());
        drop(cache);

        for suffix in ["-wal", "-shm"] {
            let sidecar = sqlite_sidecar_path(&path, suffix);
            assert!(
                !sidecar.exists(),
                "read-only diagnostics must not create {}",
                sidecar.display()
            );
        }
    }

    #[test]
    fn read_only_open_reads_committed_wal_content_from_older_cache() {
        let (_guard, path) = temp_cache_path();
        let writer = Connection::open(&path).expect("open writer");
        writer
            .pragma_update(None, "journal_mode", "WAL")
            .expect("enable WAL for older-cache fixture");
        writer
            .pragma_update(None, "wal_autocheckpoint", 0)
            .expect("disable auto-checkpoint");
        writer
            .execute_batch(SCHEMA_SQL)
            .expect("create schema in WAL");
        writer
            .execute(
                "INSERT INTO cache_meta(key, value) VALUES('schema_version', ?1)",
                params![SCHEMA_VERSION.to_string()],
            )
            .expect("write schema version in WAL");

        assert!(
            sqlite_sidecar_path(&path, "-wal").exists(),
            "fixture should keep committed content in a WAL sidecar"
        );

        let reader = FontCache::open_existing_read_only(&path).expect("read-only cache opens");
        assert!(reader
            .list_folders()
            .expect("read-only query sees WAL schema")
            .is_empty());
        drop(reader);
        drop(writer);
    }

    #[cfg(unix)]
    #[test]
    fn open_refuses_reparse_sidecar_paths() {
        use std::os::unix::fs::symlink;

        let (_guard, path) = temp_cache_path();
        let target = path.with_extension("wal-target");
        fs::write(&target, b"not sqlite").unwrap();
        symlink(&target, cache_sidecar_path(&path, "-wal")).unwrap();

        let err = FontCache::open_or_create(&path).unwrap_err();
        assert!(
            err.to_string().contains("reparse point"),
            "expected sidecar reparse refusal, got {err}"
        );
    }

    #[test]
    fn reopen_of_valid_cache_succeeds() {
        let (_guard, path) = temp_cache_path();
        // Create.
        FontCache::open_or_create(&path).expect("first open creates");
        // Reopen.
        FontCache::open_or_create(&path).expect("second open reuses existing");
    }

    #[test]
    fn schema_version_mismatch_detected_on_old_cache() {
        let (_guard, path) = temp_cache_path();
        // Create with current version.
        FontCache::open_or_create(&path).expect("first open");
        // Simulate an older release writing version 0.
        {
            let conn = Connection::open(&path).unwrap();
            conn.execute(
                "UPDATE cache_meta SET value = '0' WHERE key = 'schema_version'",
                [],
            )
            .unwrap();
        }
        // Reopen detects mismatch.
        match FontCache::open_or_create(&path) {
            Err(CacheError::SchemaVersionMismatch { found, expected }) => {
                assert_eq!(found, 0);
                assert_eq!(expected, SCHEMA_VERSION);
            }
            other => panic!("expected SchemaVersionMismatch, got {other:?}"),
        }
    }

    #[test]
    fn missing_schema_version_row_treated_as_mismatch() {
        let (_guard, path) = temp_cache_path();
        FontCache::open_or_create(&path).expect("first open");
        // Delete the schema_version row to simulate a pre-versioning
        // cache or a corrupt write.
        {
            let conn = Connection::open(&path).unwrap();
            conn.execute("DELETE FROM cache_meta WHERE key = 'schema_version'", [])
                .unwrap();
        }
        match FontCache::open_or_create(&path) {
            Err(CacheError::SchemaVersionMismatch { found, expected }) => {
                assert_eq!(found, -1, "missing row sentinel");
                assert_eq!(expected, SCHEMA_VERSION);
            }
            other => panic!("expected SchemaVersionMismatch, got {other:?}"),
        }
    }

    #[test]
    fn missing_cache_meta_table_treated_as_mismatch_for_both_open_modes() {
        let (_guard, path) = temp_cache_path();
        {
            let conn = Connection::open(&path).unwrap();
            conn.execute("CREATE TABLE unrelated(value TEXT)", [])
                .unwrap();
        }

        let assert_mismatch = |mode: &str, result: Result<FontCache, CacheError>| {
            match result {
                Err(CacheError::SchemaVersionMismatch { found, expected }) => {
                    assert_eq!(found, -1, "missing-table sentinel in {mode} mode");
                    assert_eq!(expected, SCHEMA_VERSION);
                }
                other => panic!(
                    "expected SchemaVersionMismatch for missing cache_meta in {mode} mode, got {other:?}"
                ),
            }
        };

        assert_mismatch("read-write", FontCache::open_or_create(&path));
        assert_mismatch("read-only", FontCache::open_existing_read_only(&path));
    }

    #[test]
    fn unparseable_schema_version_treated_as_mismatch() {
        let (_guard, path) = temp_cache_path();
        FontCache::open_or_create(&path).expect("first open");
        // Write garbage to the version row.
        {
            let conn = Connection::open(&path).unwrap();
            conn.execute(
                "UPDATE cache_meta SET value = 'not-a-number' WHERE key = 'schema_version'",
                [],
            )
            .unwrap();
        }
        match FontCache::open_or_create(&path) {
            Err(CacheError::SchemaVersionMismatch { found, expected }) => {
                assert_eq!(found, -2, "unparseable sentinel");
                assert_eq!(expected, SCHEMA_VERSION);
            }
            other => panic!("expected SchemaVersionMismatch, got {other:?}"),
        }
    }

    /// Synthetic font metadata for tests — no real font file required.
    fn synthetic_font(file_path: &str, family: &str) -> FontMetadata {
        FontMetadata {
            file_path: file_path.to_string(),
            file_size: 100_000,
            file_mtime: 1_700_000_000,
            face_index: 0,
            family_keys: vec![FamilyKey {
                family_name: family.to_string(),
                bold: false,
                italic: false,
            }],
            face_name_aliases: Vec::new(),
        }
    }

    fn family_key(family_name: &str, bold: bool, italic: bool) -> FamilyKey {
        FamilyKey {
            family_name: family_name.to_string(),
            bold,
            italic,
        }
    }

    fn canonical_path_string(path: &Path) -> String {
        let canonical = path
            .canonicalize()
            .expect("canonicalize test path")
            .to_string_lossy()
            .into_owned();
        crate::fonts::normalize_canonical_path(&canonical)
    }

    fn real_folder_path(folder: &Path) -> String {
        fs::create_dir_all(folder).expect("create test font folder");
        canonical_path_string(folder)
    }

    fn real_font_metadata(
        folder: &Path,
        file_name: &str,
        face_index: i32,
        family_keys: Vec<FamilyKey>,
        face_name_aliases: Vec<String>,
    ) -> FontMetadata {
        fs::create_dir_all(folder).expect("create test font folder");
        let file_path = folder.join(file_name);
        fs::write(
            &file_path,
            format!("fake font cache fixture: {file_name}:{face_index}"),
        )
        .expect("write test font file");
        let metadata = fs::metadata(&file_path).expect("stat test font file");
        let file_mtime = try_modified_at(&file_path).expect("test font mtime");
        FontMetadata {
            file_path: canonical_path_string(&file_path),
            file_size: i64::try_from(metadata.len()).unwrap_or(i64::MAX),
            file_mtime,
            face_index,
            family_keys,
            face_name_aliases,
        }
    }

    fn source_snapshot_for_fonts(
        source_root: &str,
        scope: FontDirectoryScope,
        fonts: &[FontMetadata],
    ) -> CacheSourceSnapshot {
        let mut files: Vec<FileSnapshot> = fonts
            .iter()
            .map(|font| FileSnapshot {
                file_path: font.file_path.clone(),
                file_size: font.file_size,
                file_mtime: font.file_mtime,
            })
            .collect();
        files.sort_by(|a, b| a.file_path.cmp(&b.file_path));
        files.dedup_by(|a, b| a.file_path == b.file_path);
        CacheSourceSnapshot {
            source_root: source_root.to_string(),
            scope,
            directories: vec![FolderSnapshot {
                folder_path: source_root.to_string(),
                folder_mtime: try_modified_at(Path::new(source_root)).unwrap_or(1),
            }],
            files,
        }
    }

    fn insert_raw_lookup_fixture(
        cache: &FontCache,
        source_root: &str,
        font_path: &str,
        file_size: i64,
        file_mtime: i64,
        family: &str,
    ) {
        cache
            .conn
            .pragma_update(None, "foreign_keys", "OFF")
            .unwrap();
        cache
            .conn
            .execute(
                "INSERT INTO cached_sources(source_root, scope, source_order, last_scanned_at) \
                 VALUES(?1, 0, 1, 1)",
                params![source_root],
            )
            .unwrap();
        cache
            .conn
            .execute(
                "INSERT INTO cached_fonts(\
                    source_root, scope, font_path, face_index, file_size, file_mtime\
                 ) VALUES(?1, 0, ?2, 0, ?3, ?4)",
                params![source_root, font_path, file_size, file_mtime],
            )
            .unwrap();
        cache
            .conn
            .execute(
                "INSERT INTO cached_family_keys(\
                    source_root, scope, font_path, face_index, family_name, \
                    family_name_key, key_kind, bold, italic\
                 ) VALUES(?1, 0, ?2, 0, ?3, ?4, ?5, 0, 0)",
                params![
                    source_root,
                    font_path,
                    family,
                    family_lookup_key(family),
                    KEY_KIND_FAMILY
                ],
            )
            .unwrap();
        cache
            .conn
            .pragma_update(None, "foreign_keys", "ON")
            .unwrap();
    }

    #[test]
    fn replace_folder_with_no_fonts_inserts_empty_folder_row() {
        let (_guard, path) = temp_cache_path();
        let mut cache = FontCache::open_or_create(&path).expect("open");
        cache
            .replace_folder("/test/empty", 1_700_000_000, &[])
            .expect("replace empty");
        let folders = cache.list_folders().expect("list");
        assert_eq!(folders.len(), 1);
        assert_eq!(folders[0].folder_path, "/test/empty");
        assert_eq!(folders[0].folder_mtime, 1_700_000_000);
    }

    #[test]
    fn replace_folder_inserts_fonts_and_family_keys() {
        let (_guard, path) = temp_cache_path();
        let mut cache = FontCache::open_or_create(&path).expect("open");
        let fonts = vec![
            synthetic_font("/test/dir/font_a.otf", "Source Han Sans CN"),
            synthetic_font("/test/dir/font_b.ttf", "Arial"),
        ];
        cache
            .replace_folder("/test/dir", 1_700_000_000, &fonts)
            .expect("replace");

        // Verify font rows.
        let count: i32 = cache
            .conn
            .query_row("SELECT COUNT(*) FROM cached_fonts", [], |r| r.get(0))
            .expect("count fonts");
        assert_eq!(count, 2);

        // Verify family_key rows.
        let count: i32 = cache
            .conn
            .query_row("SELECT COUNT(*) FROM cached_family_keys", [], |r| r.get(0))
            .expect("count keys");
        assert_eq!(count, 2);
    }

    #[test]
    fn replace_folder_replaces_previous_rows() {
        let (_guard, path) = temp_cache_path();
        let mut cache = FontCache::open_or_create(&path).expect("open");
        // First scan.
        cache
            .replace_folder(
                "/test/dir",
                1_700_000_000,
                &[
                    synthetic_font("/test/dir/old1.otf", "Old1"),
                    synthetic_font("/test/dir/old2.otf", "Old2"),
                ],
            )
            .expect("first replace");
        // Second scan with different fonts.
        cache
            .replace_folder(
                "/test/dir",
                1_800_000_000,
                &[synthetic_font("/test/dir/new.otf", "New")],
            )
            .expect("second replace");

        // Should have only the new font + key.
        let font_count: i32 = cache
            .conn
            .query_row("SELECT COUNT(*) FROM cached_fonts", [], |r| r.get(0))
            .expect("count fonts");
        assert_eq!(font_count, 1);
        let family: String = cache
            .conn
            .query_row("SELECT family_name FROM cached_family_keys", [], |r| {
                r.get(0)
            })
            .expect("read family");
        assert_eq!(family, "New");
        // Folder mtime should be updated.
        let folders = cache.list_folders().expect("list");
        assert_eq!(folders[0].folder_mtime, 1_800_000_000);
    }

    #[test]
    fn replace_folder_with_multiple_family_keys_per_font() {
        // CJK fonts: one face advertises Latin + CJK names.
        let (_guard, path) = temp_cache_path();
        let mut cache = FontCache::open_or_create(&path).expect("open");
        let cjk_font = FontMetadata {
            file_path: "/test/cjk/SourceHanSans.otf".to_string(),
            file_size: 10_000_000,
            file_mtime: 1_700_000_000,
            face_index: 0,
            family_keys: vec![
                FamilyKey {
                    family_name: "Source Han Sans CN".to_string(),
                    bold: false,
                    italic: false,
                },
                FamilyKey {
                    family_name: "思源黑体 CN".to_string(),
                    bold: false,
                    italic: false,
                },
                FamilyKey {
                    family_name: "Noto Sans CJK SC".to_string(),
                    bold: false,
                    italic: false,
                },
            ],
            face_name_aliases: Vec::new(),
        };
        cache
            .replace_folder("/test/cjk", 1_700_000_000, &[cjk_font])
            .expect("replace");

        let key_count: i32 = cache
            .conn
            .query_row("SELECT COUNT(*) FROM cached_family_keys", [], |r| r.get(0))
            .expect("count keys");
        assert_eq!(key_count, 3, "all three family aliases should be indexed");
    }

    /// `INSERT OR IGNORE` on `cached_family_keys` ensures duplicate
    /// raw family names that NFC-normalize + lowercase to the same
    /// `family_name_key` (e.g. the NFC and NFD forms of `Café`, or
    /// two case variants of an English name) don't violate the PK
    /// `(family_name_key, key_kind, bold, italic, font_path, face_index)`.
    /// Without OR IGNORE this would throw, aborting the whole
    /// `replace_folder` transaction.
    #[test]
    fn replace_folder_dedupes_normalize_equal_family_keys() {
        let (_guard, path) = temp_cache_path();
        let mut cache = FontCache::open_or_create(&path).expect("open");
        let font = FontMetadata {
            file_path: "/test/dedupe/normalize.otf".to_string(),
            file_size: 1_000,
            file_mtime: 1_700_000_000,
            face_index: 0,
            family_keys: vec![
                FamilyKey {
                    family_name: "Café".to_string(), // NFC: U+00E9
                    bold: false,
                    italic: false,
                },
                FamilyKey {
                    family_name: "Cafe\u{0301}".to_string(), // NFD: e + U+0301
                    bold: false,
                    italic: false,
                },
                FamilyKey {
                    family_name: "CAFÉ".to_string(), // case variant
                    bold: false,
                    italic: false,
                },
                FamilyKey {
                    family_name: "Other Font".to_string(), // distinct
                    bold: false,
                    italic: false,
                },
            ],
            face_name_aliases: Vec::new(),
        };
        cache
            .replace_folder("/test/dedupe", 1_700_000_000, &[font])
            .expect("replace");

        let key_count: i32 = cache
            .conn
            .query_row("SELECT COUNT(*) FROM cached_family_keys", [], |r| r.get(0))
            .expect("count keys");
        assert_eq!(
            key_count, 2,
            "three Café variants should collapse to one row + one Other Font row"
        );
    }

    #[test]
    fn remove_folder_clears_all_related_rows() {
        let (_guard, path) = temp_cache_path();
        let mut cache = FontCache::open_or_create(&path).expect("open");
        cache
            .replace_folder(
                "/test/a",
                1_700_000_000,
                &[synthetic_font("/test/a/f1.otf", "F1")],
            )
            .expect("replace a");
        cache
            .replace_folder(
                "/test/b",
                1_700_000_000,
                &[synthetic_font("/test/b/f2.otf", "F2")],
            )
            .expect("replace b");

        cache.remove_folder("/test/a").expect("remove a");

        // /test/a's rows gone, /test/b's intact.
        let folders = cache.list_folders().expect("list");
        assert_eq!(folders.len(), 1);
        assert_eq!(folders[0].folder_path, "/test/b");
        let font_count: i32 = cache
            .conn
            .query_row("SELECT COUNT(*) FROM cached_fonts", [], |r| r.get(0))
            .expect("count fonts");
        assert_eq!(font_count, 1);
        let key_count: i32 = cache
            .conn
            .query_row("SELECT COUNT(*) FROM cached_family_keys", [], |r| r.get(0))
            .expect("count keys");
        assert_eq!(key_count, 1);
    }

    #[test]
    fn list_folders_returns_in_path_order() {
        let (_guard, path) = temp_cache_path();
        let mut cache = FontCache::open_or_create(&path).expect("open");
        // Insert in non-alphabetical order.
        cache
            .replace_folder("/test/zzz", 1_700_000_000, &[])
            .unwrap();
        cache
            .replace_folder("/test/aaa", 1_700_000_000, &[])
            .unwrap();
        cache
            .replace_folder("/test/mmm", 1_700_000_000, &[])
            .unwrap();
        let folders = cache.list_folders().expect("list");
        let paths: Vec<&str> = folders.iter().map(|f| f.folder_path.as_str()).collect();
        assert_eq!(paths, vec!["/test/aaa", "/test/mmm", "/test/zzz"]);
    }

    #[test]
    fn cache_rejects_un_normalized_extended_windows_paths() {
        let (_guard, path) = temp_cache_path();
        let mut cache = FontCache::open_or_create(&path).expect("open");
        let err = cache
            .replace_folder(r"\\?\C:\Fonts\Anime", 1_700_000_000, &[])
            .unwrap_err();
        assert!(err.to_string().contains("reserved device namespace"));
    }

    #[test]
    fn list_folders_rejects_network_namespace_rows_before_stat_callers() {
        let (_guard, path) = temp_cache_path();
        let mut cache = FontCache::open_or_create(&path).expect("open");
        let err = cache
            .replace_folder(r"\\?\UNC\dead-host\Fonts", 1_700_000_000, &[])
            .unwrap_err();
        assert!(
            err.to_string().contains("reserved device namespace"),
            "expected cached folder namespace rejection, got: {err}"
        );
    }

    #[test]
    fn list_folders_rejects_over_sanity_cap_before_stat_callers() {
        let (_guard, path) = temp_cache_path();
        let cache = FontCache::open_or_create(&path).expect("open");
        for i in 0..=MAX_CACHED_FOLDERS {
            cache
                .conn
                .execute(
                    "INSERT INTO cached_sources(source_root, scope, source_order, last_scanned_at) \
                     VALUES(?1, 0, ?2, ?3)",
                    params![
                        format!("/hostile/{i:04}"),
                        i64::try_from(i).unwrap(),
                        1_700_000_001_i64
                    ],
                )
                .unwrap();
        }

        let err = cache.list_folders().unwrap_err();
        assert!(
            err.to_string().contains(&format!(
                "cached_sources table exceeds {MAX_CACHED_FOLDERS}-row sanity cap"
            )),
            "expected cached_folders sanity-cap error, got: {err}"
        );
    }

    #[test]
    fn replace_folder_refuses_to_create_unreadable_over_cap_cache() {
        let (_guard, path) = temp_cache_path();
        let mut cache = FontCache::open_or_create(&path).expect("open");
        for i in 0..MAX_CACHED_FOLDERS {
            cache
                .replace_folder(&format!("/cache/{i:04}"), 1_700_000_000, &[])
                .unwrap();
        }

        cache
            .replace_folder("/cache/0000", 1_700_000_123, &[])
            .expect("replacing an existing folder at the cap should remain legal");

        let err = cache
            .replace_folder("/cache/overflow", 1_700_000_000, &[])
            .unwrap_err();
        assert!(
            err.to_string().contains(&format!(
                "cached_sources is at the {MAX_CACHED_FOLDERS}-source sanity cap"
            )),
            "expected replace_folder cap error, got: {err}"
        );
    }

    #[test]
    fn last_scanned_at_set_to_current_time() {
        let (_guard, path) = temp_cache_path();
        let mut cache = FontCache::open_or_create(&path).expect("open");
        // Test-only `.expect` — running on a sane system clock is a
        // baseline test-environment assumption; if SystemTime is
        // broken, every other test in the suite would also fail.
        let before = current_unix_seconds().expect("sane system clock");
        cache
            .replace_folder("/test/timing", 1_700_000_000, &[])
            .unwrap();
        let after = current_unix_seconds().expect("sane system clock");
        let folders = cache.list_folders().expect("list");
        assert!(folders[0].last_scanned_at >= before);
        assert!(folders[0].last_scanned_at <= after);
    }

    // ── Drift detection ─────────────────────────────────────

    #[test]
    fn diff_against_empty_cache_reports_all_as_added() {
        let (_guard, path) = temp_cache_path();
        let cache = FontCache::open_or_create(&path).expect("open");
        let snapshot = vec![("/test/a".to_string(), 100), ("/test/b".to_string(), 200)];
        let report = cache.diff_against(&snapshot).expect("diff");
        assert_eq!(report.added, vec!["/test/a", "/test/b"]);
        assert!(report.modified.is_empty());
        assert!(report.removed.is_empty());
        assert!(!report.is_empty());
    }

    #[test]
    fn diff_against_perfect_match_is_empty() {
        let (_guard, path) = temp_cache_path();
        let mut cache = FontCache::open_or_create(&path).expect("open");
        cache.replace_folder("/test/a", 100, &[]).unwrap();
        cache.replace_folder("/test/b", 200, &[]).unwrap();
        let snapshot = vec![("/test/a".to_string(), 100), ("/test/b".to_string(), 200)];
        let report = cache.diff_against(&snapshot).expect("diff");
        assert!(report.is_empty(), "expected no drift, got {report:?}");
    }

    #[test]
    fn diff_against_detects_modified_folders_via_mtime() {
        let (_guard, path) = temp_cache_path();
        let mut cache = FontCache::open_or_create(&path).expect("open");
        cache.replace_folder("/test/a", 100, &[]).unwrap();
        cache.replace_folder("/test/b", 200, &[]).unwrap();
        // /test/a's mtime drifted; /test/b unchanged.
        let snapshot = vec![("/test/a".to_string(), 150), ("/test/b".to_string(), 200)];
        let report = cache.diff_against(&snapshot).expect("diff");
        assert_eq!(report.modified, vec!["/test/a"]);
        assert!(report.added.is_empty());
        assert!(report.removed.is_empty());
    }

    #[test]
    fn diff_against_detects_removed_folders() {
        let (_guard, path) = temp_cache_path();
        let mut cache = FontCache::open_or_create(&path).expect("open");
        cache.replace_folder("/test/a", 100, &[]).unwrap();
        cache.replace_folder("/test/b", 200, &[]).unwrap();
        // Snapshot only has /test/a; /test/b vanished from FS.
        let snapshot = vec![("/test/a".to_string(), 100)];
        let report = cache.diff_against(&snapshot).expect("diff");
        assert_eq!(report.removed, vec!["/test/b"]);
        assert!(report.added.is_empty());
        assert!(report.modified.is_empty());
    }

    #[test]
    fn diff_against_detects_added_folders() {
        let (_guard, path) = temp_cache_path();
        let mut cache = FontCache::open_or_create(&path).expect("open");
        cache.replace_folder("/test/a", 100, &[]).unwrap();
        // Snapshot has /test/a + a new /test/c.
        let snapshot = vec![("/test/a".to_string(), 100), ("/test/c".to_string(), 300)];
        let report = cache.diff_against(&snapshot).expect("diff");
        assert_eq!(report.added, vec!["/test/c"]);
        assert!(report.modified.is_empty());
        assert!(report.removed.is_empty());
    }

    #[test]
    fn diff_against_handles_all_three_categories_at_once() {
        let (_guard, path) = temp_cache_path();
        let mut cache = FontCache::open_or_create(&path).expect("open");
        // Cache has a/b/c.
        cache.replace_folder("/test/a", 100, &[]).unwrap();
        cache.replace_folder("/test/b", 200, &[]).unwrap();
        cache.replace_folder("/test/c", 300, &[]).unwrap();
        // Snapshot: a unchanged, b modified, c removed, d added.
        let snapshot = vec![
            ("/test/a".to_string(), 100),
            ("/test/b".to_string(), 250), // mtime drifted
            ("/test/d".to_string(), 400), // new
        ];
        let report = cache.diff_against(&snapshot).expect("diff");
        assert_eq!(report.added, vec!["/test/d"]);
        assert_eq!(report.modified, vec!["/test/b"]);
        assert_eq!(report.removed, vec!["/test/c"]);
        assert!(!report.is_empty());
    }

    #[test]
    fn diff_against_lists_are_sorted_for_deterministic_output() {
        let (_guard, path) = temp_cache_path();
        let mut cache = FontCache::open_or_create(&path).expect("open");
        // Cache has folders in non-alpha order.
        cache.replace_folder("/test/zzz", 100, &[]).unwrap();
        cache.replace_folder("/test/aaa", 100, &[]).unwrap();
        // Snapshot adds in non-alpha order; doesn't include the
        // cached ones (so they all become removed).
        let snapshot = vec![
            ("/test/yyy".to_string(), 100),
            ("/test/bbb".to_string(), 100),
            ("/test/mmm".to_string(), 100),
        ];
        let report = cache.diff_against(&snapshot).expect("diff");
        assert_eq!(report.added, vec!["/test/bbb", "/test/mmm", "/test/yyy"]);
        assert_eq!(report.removed, vec!["/test/aaa", "/test/zzz"]);
    }

    #[test]
    fn drift_report_is_empty_method() {
        let empty = DriftReport::default();
        assert!(empty.is_empty());
        let with_added = DriftReport {
            added: vec![CacheSourceKey {
                source_root: "x".to_string(),
                scope: FontDirectoryScope::Shallow,
            }],
            ..Default::default()
        };
        assert!(!with_added.is_empty());
        let with_modified = DriftReport {
            modified: vec![CacheSourceKey {
                source_root: "x".to_string(),
                scope: FontDirectoryScope::Shallow,
            }],
            ..Default::default()
        };
        assert!(!with_modified.is_empty());
        let with_removed = DriftReport {
            removed: vec![CacheSourceKey {
                source_root: "x".to_string(),
                scope: FontDirectoryScope::Shallow,
            }],
            ..Default::default()
        };
        assert!(!with_removed.is_empty());
    }

    // ── Family-name lookup ──────────────────────────────────

    #[test]
    fn lookup_family_returns_match() {
        let (guard, path) = temp_cache_path();
        let mut cache = FontCache::open_or_create(&path).expect("open");
        let font_dir = guard.0.join("lookup-match");
        let arial = real_font_metadata(
            &font_dir,
            "arial.ttf",
            0,
            vec![family_key("Arial", false, false)],
            Vec::new(),
        );
        cache
            .replace_folder(
                &real_folder_path(&font_dir),
                100,
                std::slice::from_ref(&arial),
            )
            .unwrap();
        let result = cache
            .lookup_family("Arial", false, false)
            .expect("lookup")
            .expect("hit expected");
        assert_eq!(result.font_path, arial.file_path);
        assert_eq!(result.face_index, 0);
    }

    #[test]
    fn lookup_family_returns_none_for_missing_family() {
        let (guard, path) = temp_cache_path();
        let mut cache = FontCache::open_or_create(&path).expect("open");
        let font_dir = guard.0.join("lookup-miss");
        let arial = real_font_metadata(
            &font_dir,
            "arial.ttf",
            0,
            vec![family_key("Arial", false, false)],
            Vec::new(),
        );
        cache
            .replace_folder(&real_folder_path(&font_dir), 100, &[arial])
            .unwrap();
        let result = cache
            .lookup_family("Helvetica", false, false)
            .expect("lookup ok");
        assert!(result.is_none(), "expected None, got {result:?}");
    }

    #[test]
    fn lookup_family_ignores_orphan_family_key_rows() {
        let (_guard, path) = temp_cache_path();
        let cache = FontCache::open_or_create(&path).expect("open");
        cache
            .conn
            .pragma_update(None, "foreign_keys", "OFF")
            .unwrap();
        cache
            .conn
            .execute(
                "INSERT INTO cached_family_keys(\
                    source_root, scope, font_path, face_index, family_name, \
                    family_name_key, key_kind, bold, italic\
                 ) VALUES('/orphan', 0, ?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                params![
                    "/orphan/arial.ttf",
                    0,
                    "Arial",
                    family_lookup_key("Arial"),
                    KEY_KIND_FAMILY,
                    0,
                    0,
                ],
            )
            .unwrap();
        cache
            .conn
            .pragma_update(None, "foreign_keys", "ON")
            .unwrap();

        let result = cache
            .lookup_family("Arial", false, false)
            .expect("lookup ok");
        assert!(
            result.is_none(),
            "orphan family key row must not bypass cached_fonts/cached_folders anchoring"
        );
    }

    #[test]
    fn lookup_family_rejects_network_namespace_font_path_rows() {
        let (_guard, path) = temp_cache_path();
        let cache = FontCache::open_or_create(&path).expect("open");
        let bad_font_path = r"\\?\UNC\dead-host\Fonts\arial.ttf";
        insert_raw_lookup_fixture(
            &cache,
            "/test/dir",
            bad_font_path,
            100_000,
            1_700_000_000,
            "Arial",
        );

        let err = cache.lookup_family("Arial", false, false).unwrap_err();
        assert!(
            err.to_string().contains("reserved device namespace"),
            "expected cached font namespace rejection, got: {err}"
        );
    }

    #[test]
    fn lookup_family_rejects_network_namespace_folder_path_rows() {
        let (_guard, path) = temp_cache_path();
        let cache = FontCache::open_or_create(&path).expect("open");
        let bad_folder_path = r"\\?\UNC\dead-host\Fonts";
        let local_font_path = r"C:\Fonts\arial.ttf";
        insert_raw_lookup_fixture(
            &cache,
            bad_folder_path,
            local_font_path,
            100_000,
            1_700_000_000,
            "Arial",
        );

        let err = cache.lookup_family("Arial", false, false).unwrap_err();
        assert!(
            err.to_string().contains("reserved device namespace"),
            "expected cached folder namespace rejection, got: {err}"
        );
    }

    #[test]
    fn lookup_family_rejects_font_path_outside_cached_folder() {
        let (guard, path) = temp_cache_path();
        let cache = FontCache::open_or_create(&path).expect("open");
        let trusted_dir = guard.0.join("trusted-fonts");
        let outside_dir = guard.0.join("outside-fonts");
        let trusted_folder_path = real_folder_path(&trusted_dir);
        let outside_font = real_font_metadata(
            &outside_dir,
            "private.ttf",
            0,
            vec![family_key("Private Sans", false, false)],
            Vec::new(),
        );
        let outside_font_path = outside_font.file_path.clone();

        insert_raw_lookup_fixture(
            &cache,
            &trusted_folder_path,
            &outside_font_path,
            outside_font.file_size,
            outside_font.file_mtime,
            "Private Sans",
        );

        let err = cache
            .lookup_family("Private Sans", false, false)
            .unwrap_err();
        assert!(
            err.to_string()
                .contains("cached_fonts.font_path is outside cached_sources.source_root"),
            "expected outside-folder cache rejection, got: {err}"
        );
    }

    #[test]
    fn lookup_family_rejects_stale_cached_font_metadata() {
        let (guard, path) = temp_cache_path();
        let mut cache = FontCache::open_or_create(&path).expect("open");
        let font_dir = guard.0.join("stale-fonts");
        let font = real_font_metadata(
            &font_dir,
            "arial.ttf",
            0,
            vec![family_key("Arial", false, false)],
            Vec::new(),
        );
        cache
            .replace_folder(
                &real_folder_path(&font_dir),
                100,
                std::slice::from_ref(&font),
            )
            .unwrap();

        fs::write(Path::new(&font.file_path), b"changed font fixture bytes")
            .expect("mutate font file");

        let err = cache.lookup_family("Arial", false, false).unwrap_err();
        assert!(
            err.to_string()
                .contains("cached_fonts metadata no longer matches the live font file"),
            "expected stale cached font metadata rejection, got: {err}"
        );
    }

    #[test]
    fn lookup_family_skips_stale_newest_candidate_and_returns_valid_older_source() {
        let (guard, path) = temp_cache_path();
        let mut cache = FontCache::open_or_create(&path).expect("open");
        let older_dir = guard.0.join("fallback-older");
        let newer_dir = guard.0.join("fallback-newer");
        let older = real_font_metadata(
            &older_dir,
            "older.ttf",
            0,
            vec![family_key("Fallback Sans", false, false)],
            Vec::new(),
        );
        let newer = real_font_metadata(
            &newer_dir,
            "newer.ttf",
            0,
            vec![family_key("Fallback Sans", false, false)],
            Vec::new(),
        );
        cache
            .replace_folder(
                &real_folder_path(&older_dir),
                100,
                std::slice::from_ref(&older),
            )
            .unwrap();
        cache
            .replace_folder(
                &real_folder_path(&newer_dir),
                200,
                std::slice::from_ref(&newer),
            )
            .unwrap();
        fs::remove_file(&newer.file_path).expect("make highest-priority cache row stale");

        let result = cache
            .lookup_family("Fallback Sans", false, false)
            .expect("a valid lower-ranked cache candidate should recover the lookup")
            .expect("older source should match");
        assert_eq!(result.font_path, older.file_path);
        assert_eq!(result.face_index, 0);
    }

    #[test]
    fn lookup_family_skips_malformed_top_row_and_returns_valid_lower_source() {
        let (guard, path) = temp_cache_path();
        let mut cache = FontCache::open_or_create(&path).expect("open");
        let valid_dir = guard.0.join("malformed-row-valid");
        let malformed_dir = guard.0.join("malformed-row-top");
        let valid = real_font_metadata(
            &valid_dir,
            "valid.ttf",
            0,
            vec![family_key("Malformed Fallback", false, false)],
            Vec::new(),
        );
        cache
            .replace_folder(
                &real_folder_path(&valid_dir),
                100,
                std::slice::from_ref(&valid),
            )
            .unwrap();
        let malformed =
            real_font_metadata(&malformed_dir, "malformed.ttf", 0, Vec::new(), Vec::new());
        let malformed_root = real_folder_path(&malformed_dir);
        cache
            .conn
            .pragma_update(None, "foreign_keys", "OFF")
            .unwrap();
        cache
            .conn
            .execute(
                "INSERT INTO cached_sources(source_root, scope, source_order, last_scanned_at) \
                 VALUES(?1, 0, 999, 1)",
                params![&malformed_root],
            )
            .unwrap();
        cache
            .conn
            .execute(
                "INSERT INTO cached_fonts(\
                    source_root, scope, font_path, face_index, file_size, file_mtime\
                 ) VALUES(?1, 0, ?2, 'not-an-int', ?3, ?4)",
                params![
                    &malformed_root,
                    &malformed.file_path,
                    malformed.file_size,
                    malformed.file_mtime
                ],
            )
            .unwrap();
        cache
            .conn
            .execute(
                "INSERT INTO cached_family_keys(\
                    source_root, scope, font_path, face_index, family_name, \
                    family_name_key, key_kind, bold, italic\
                 ) VALUES(?1, 0, ?2, 'not-an-int', ?3, ?4, ?5, 0, 0)",
                params![
                    &malformed_root,
                    &malformed.file_path,
                    "Malformed Fallback",
                    family_lookup_key("Malformed Fallback"),
                    KEY_KIND_FAMILY
                ],
            )
            .unwrap();
        cache
            .conn
            .pragma_update(None, "foreign_keys", "ON")
            .unwrap();

        let result = cache
            .lookup_family("Malformed Fallback", false, false)
            .expect("malformed top row should not suppress a valid candidate")
            .expect("valid lower source should match");
        assert_eq!(result.font_path, valid.file_path);
        assert_eq!(result.face_index, 0);
    }

    #[test]
    fn lookup_family_skips_over_limit_face_index_and_returns_valid_lower_source() {
        let (guard, path) = temp_cache_path();
        let mut cache = FontCache::open_or_create(&path).expect("open");
        let valid_dir = guard.0.join("index-limit-valid");
        let invalid_dir = guard.0.join("index-limit-top");
        let valid = real_font_metadata(
            &valid_dir,
            "valid.ttf",
            0,
            vec![family_key("Index Fallback", false, false)],
            Vec::new(),
        );
        cache
            .replace_folder(
                &real_folder_path(&valid_dir),
                100,
                std::slice::from_ref(&valid),
            )
            .unwrap();
        let invalid = real_font_metadata(
            &invalid_dir,
            "invalid.ttf",
            i32::try_from(crate::fonts::MAX_SUBSET_FONT_INDEX + 1).unwrap(),
            vec![family_key("Index Fallback", false, false)],
            Vec::new(),
        );
        cache
            .replace_folder(
                &real_folder_path(&invalid_dir),
                200,
                std::slice::from_ref(&invalid),
            )
            .unwrap();

        let result = cache
            .lookup_family("Index Fallback", false, false)
            .expect("over-limit top row should not suppress a valid candidate")
            .expect("valid lower source should match");
        assert_eq!(result.font_path, valid.file_path);
        assert_eq!(result.face_index, 0);
    }

    #[test]
    fn lookup_family_distinguishes_bold_and_italic() {
        let (guard, path) = temp_cache_path();
        let mut cache = FontCache::open_or_create(&path).expect("open");
        let font_dir = guard.0.join("lookup-style");
        // Two synthetic faces of "Source Han Sans": regular and bold.
        let regular = real_font_metadata(
            &font_dir,
            "SHS-Regular.otf",
            0,
            vec![family_key("Source Han Sans", false, false)],
            Vec::new(),
        );
        let bold = real_font_metadata(
            &font_dir,
            "SHS-Bold.otf",
            0,
            vec![family_key("Source Han Sans", true, false)],
            Vec::new(),
        );
        let regular_path = regular.file_path.clone();
        let bold_path = bold.file_path.clone();
        cache
            .replace_folder(&real_folder_path(&font_dir), 100, &[regular, bold])
            .unwrap();

        // Regular query hits regular file.
        let r = cache
            .lookup_family("Source Han Sans", false, false)
            .unwrap()
            .unwrap();
        assert_eq!(r.font_path, regular_path);
        // Bold query hits bold file.
        let b = cache
            .lookup_family("Source Han Sans", true, false)
            .unwrap()
            .unwrap();
        assert_eq!(b.font_path, bold_path);
        // Italic-not-present query misses.
        let i = cache.lookup_family("Source Han Sans", false, true).unwrap();
        assert!(i.is_none());
    }

    #[test]
    fn lookup_family_prefers_exact_family_before_face_alias() {
        let (guard, path) = temp_cache_path();
        let mut cache = FontCache::open_or_create(&path).expect("open");
        let font_dir = guard.0.join("lookup-alias");
        let exact = real_font_metadata(
            &font_dir,
            "zzz-exact-shared-sans.otf",
            0,
            vec![family_key("Shared Sans", false, false)],
            Vec::new(),
        );
        let alias = real_font_metadata(
            &font_dir,
            "aaa-alias-face.otf",
            0,
            vec![family_key("Other Sans", true, false)],
            vec!["Shared Sans".into()],
        );
        let exact_path = exact.file_path.clone();
        let alias_path = alias.file_path.clone();
        cache
            .replace_folder(&real_folder_path(&font_dir), 100, &[exact, alias])
            .unwrap();

        let exact_result = cache
            .lookup_family("Shared Sans", false, false)
            .unwrap()
            .unwrap();
        assert_eq!(exact_result.font_path, exact_path);

        let alias_result = cache
            .lookup_family("Shared Sans", true, true)
            .unwrap()
            .unwrap();
        assert_eq!(alias_result.font_path, alias_path);

        let alias_row_count: i32 = cache
            .conn
            .query_row(
                "SELECT COUNT(*) FROM cached_family_keys \
                 WHERE key_kind = ?1 AND family_name_key = ?2",
                params![KEY_KIND_FACE_ALIAS, family_lookup_key("Shared Sans")],
                |r| r.get(0),
            )
            .expect("count alias rows");
        assert_eq!(
            alias_row_count, 1,
            "style-insensitive face aliases should be stored once"
        );
    }

    #[test]
    fn lookup_family_finds_cjk_alias() {
        // CJK font advertises multiple family aliases on the same face.
        // Lookup must hit any of them.
        let (guard, path) = temp_cache_path();
        let mut cache = FontCache::open_or_create(&path).expect("open");
        let font_dir = guard.0.join("lookup-cjk");
        let cjk = real_font_metadata(
            &font_dir,
            "SourceHanSans.otf",
            0,
            vec![
                family_key("Source Han Sans CN", false, false),
                family_key("思源黑体 CN", false, false),
                family_key("Noto Sans CJK SC", false, false),
            ],
            Vec::new(),
        );
        let cjk_path = cjk.file_path.clone();
        cache
            .replace_folder(&real_folder_path(&font_dir), 100, &[cjk])
            .unwrap();

        for name in &["Source Han Sans CN", "思源黑体 CN", "Noto Sans CJK SC"] {
            let result = cache
                .lookup_family(name, false, false)
                .unwrap()
                .unwrap_or_else(|| panic!("expected hit for {name}"));
            assert_eq!(result.font_path, cjk_path);
        }
    }

    #[test]
    fn ttc_file_with_multiple_faces_is_supported() {
        // TrueType Collection: one file, multiple faces, each its
        // own family. Schema's composite PK on (font_path,
        // face_index) lets all faces coexist.
        let (guard, path) = temp_cache_path();
        let mut cache = FontCache::open_or_create(&path).expect("open");
        let font_dir = guard.0.join("lookup-ttc");
        let mingliu_face0 = real_font_metadata(
            &font_dir,
            "MingLiU.ttc",
            0,
            vec![family_key("MingLiU", false, false)],
            Vec::new(),
        );
        let mut mingliu_face1 = mingliu_face0.clone();
        mingliu_face1.face_index = 1;
        mingliu_face1.family_keys = vec![family_key("PMingLiU", false, false)];
        let mingliu_path = mingliu_face0.file_path.clone();
        cache
            .replace_folder(
                &real_folder_path(&font_dir),
                100,
                &[mingliu_face0, mingliu_face1],
            )
            .expect("TTC with 2 faces inserts cleanly");

        // Both family names resolve, each to the right face.
        let m0 = cache
            .lookup_family("MingLiU", false, false)
            .unwrap()
            .unwrap();
        assert_eq!(m0.font_path, mingliu_path);
        assert_eq!(m0.face_index, 0);
        let m1 = cache
            .lookup_family("PMingLiU", false, false)
            .unwrap()
            .unwrap();
        assert_eq!(m1.font_path, mingliu_path);
        assert_eq!(m1.face_index, 1);
    }

    #[test]
    fn lookup_family_is_deterministic_across_collisions() {
        // Two different files claim the same family name (rare in
        // practice — alternate vendor's "Arial" — but the API must
        // produce the same answer across runs).
        let (guard, path) = temp_cache_path();
        let mut cache = FontCache::open_or_create(&path).expect("open");
        let font_dir = guard.0.join("lookup-collision");
        let zzz = real_font_metadata(
            &font_dir,
            "zzz_arial.ttf",
            0,
            vec![family_key("Arial", false, false)],
            Vec::new(),
        );
        let aaa = real_font_metadata(
            &font_dir,
            "aaa_arial.ttf",
            0,
            vec![family_key("Arial", false, false)],
            Vec::new(),
        );
        let aaa_path = aaa.file_path.clone();
        cache
            .replace_folder(&real_folder_path(&font_dir), 100, &[zzz, aaa])
            .unwrap();
        // ORDER BY font_path → "aaa..." comes first.
        let result = cache.lookup_family("Arial", false, false).unwrap().unwrap();
        assert_eq!(result.font_path, aaa_path);
    }

    #[test]
    fn lookup_family_matches_across_case_and_nfc_form() {
        // Cache lookup must NFC-normalize + lowercase BOTH the stored
        // key and the query so a font's name-table form (often NFC)
        // matches an ASS file's `\fn` reference regardless of NFD/NFC
        // or case.
        let (guard, path) = temp_cache_path();
        let mut cache = FontCache::open_or_create(&path).expect("open");
        let font_dir = guard.0.join("lookup-normalized");
        // Store an NFC-form precomposed family name.
        let cafe = real_font_metadata(
            &font_dir,
            "cafe.ttf",
            0,
            vec![family_key("Café", false, false)],
            Vec::new(),
        );
        cache
            .replace_folder(&real_folder_path(&font_dir), 100, &[cafe])
            .unwrap();
        // Query in different case → hits.
        let by_case = cache.lookup_family("CAFÉ", false, false).unwrap();
        assert!(
            by_case.is_some(),
            "case-insensitive lookup should match Café"
        );
        // Query in NFD form (decomposed e + combining acute) → hits.
        let nfd = "Cafe\u{0301}";
        let by_nfd = cache.lookup_family(nfd, false, false).unwrap();
        assert!(by_nfd.is_some(), "NFD-form lookup should match NFC store");
    }

    #[test]
    fn open_creates_parent_directory_if_missing() {
        // Don't pre-create the inner `nested/` — let open_or_create
        // do it. TempCacheDir owns the outer dir and cleans the tree
        // on Drop, including whatever open_or_create added under it.
        let (_guard, path) = temp_nested_cache_path();
        FontCache::open_or_create(&path).expect("creates nested parents");
        assert!(path.exists());
    }

    #[test]
    fn try_modified_at_preserves_nanosecond_precision() {
        let guard = TempCacheDir::new();
        let file = guard.0.join("mtime.ttf");
        fs::write(&file, b"mtime fixture").unwrap();
        let modified = fs::metadata(&file)
            .unwrap()
            .modified()
            .unwrap()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap();
        let expected = i64::try_from(modified.as_secs()).unwrap() * 1_000_000_000
            + i64::from(modified.subsec_nanos());
        assert_eq!(try_modified_at(&file), Some(expected));
    }

    #[test]
    fn candidate_file_change_marks_recursive_source_modified() {
        let (guard, path) = temp_cache_path();
        let mut cache = FontCache::open_or_create(&path).unwrap();
        let root = guard.0.join("library");
        let nested = root.join("nested");
        fs::create_dir_all(&nested).unwrap();
        let malformed = nested.join("broken.ttf");
        fs::write(&malformed, b"bad").unwrap();

        let original = snapshot_source_directories(&root, FontDirectoryScope::Recursive).unwrap();
        cache.replace_source(&original, &[]).unwrap();
        // Size is a second independent freshness signal, so this remains
        // deterministic even on filesystems with coarse mtime resolution.
        fs::write(&malformed, b"now a different candidate").unwrap();
        let changed = snapshot_source_directories(&root, FontDirectoryScope::Recursive).unwrap();
        let report = cache.diff_sources(&[changed]).unwrap();
        assert_eq!(report.modified, vec![original.key()]);
    }

    #[test]
    fn same_root_scopes_own_faces_independently() {
        let (guard, path) = temp_cache_path();
        let mut cache = FontCache::open_or_create(&path).unwrap();
        let root = guard.0.join("shared-root");
        let font = real_font_metadata(
            &root,
            "shared.ttf",
            0,
            vec![family_key("Shared Sans", false, false)],
            Vec::new(),
        );
        let source_root = real_folder_path(&root);
        let shallow = source_snapshot_for_fonts(
            &source_root,
            FontDirectoryScope::Shallow,
            std::slice::from_ref(&font),
        );
        let recursive = source_snapshot_for_fonts(
            &source_root,
            FontDirectoryScope::Recursive,
            std::slice::from_ref(&font),
        );
        cache
            .replace_source(&shallow, std::slice::from_ref(&font))
            .unwrap();
        cache
            .replace_source(&recursive, std::slice::from_ref(&font))
            .unwrap();
        let owned_faces: i64 = cache
            .conn
            .query_row("SELECT COUNT(*) FROM cached_fonts", [], |row| row.get(0))
            .unwrap();
        assert_eq!(owned_faces, 2);

        cache
            .remove_source(&source_root, FontDirectoryScope::Recursive)
            .unwrap();
        let remaining_faces: i64 = cache
            .conn
            .query_row("SELECT COUNT(*) FROM cached_fonts", [], |row| row.get(0))
            .unwrap();
        assert_eq!(remaining_faces, 1);
        assert!(cache
            .lookup_family("Shared Sans", false, false)
            .unwrap()
            .is_some());
    }

    #[test]
    fn transactional_clear_success_leaves_no_cached_sources() {
        let (guard, path) = temp_cache_path();
        let mut cache = FontCache::open_or_create(&path).unwrap();
        let root = guard.0.join("clear-source");
        let font = real_font_metadata(
            &root,
            "clear.ttf",
            0,
            vec![family_key("Clear Sans", false, false)],
            Vec::new(),
        );
        let source_root = real_folder_path(&root);
        let snapshot = source_snapshot_for_fonts(
            &source_root,
            FontDirectoryScope::Shallow,
            std::slice::from_ref(&font),
        );
        cache
            .replace_source(&snapshot, std::slice::from_ref(&font))
            .unwrap();

        assert_eq!(cache.clear_sources().unwrap(), 1);
        assert!(cache.list_sources().unwrap().is_empty());
        assert!(cache
            .lookup_family("Clear Sans", false, false)
            .unwrap()
            .is_none());
    }

    #[test]
    fn lookup_prefers_shallow_then_newest_source() {
        let (guard, path) = temp_cache_path();
        let mut cache = FontCache::open_or_create(&path).unwrap();
        let shallow_old_dir = guard.0.join("shallow-old");
        let recursive_dir = guard.0.join("recursive-newer");
        let shallow_new_dir = guard.0.join("shallow-newest");
        let shallow_old = real_font_metadata(
            &shallow_old_dir,
            "font.ttf",
            0,
            vec![family_key("Priority Sans", false, false)],
            Vec::new(),
        );
        let recursive = real_font_metadata(
            &recursive_dir,
            "font.ttf",
            0,
            vec![family_key("Priority Sans", false, false)],
            Vec::new(),
        );
        let shallow_new = real_font_metadata(
            &shallow_new_dir,
            "font.ttf",
            0,
            vec![family_key("Priority Sans", false, false)],
            Vec::new(),
        );
        let shallow_old_root = real_folder_path(&shallow_old_dir);
        let recursive_root = real_folder_path(&recursive_dir);
        let shallow_new_root = real_folder_path(&shallow_new_dir);
        cache
            .replace_source(
                &source_snapshot_for_fonts(
                    &shallow_old_root,
                    FontDirectoryScope::Shallow,
                    std::slice::from_ref(&shallow_old),
                ),
                std::slice::from_ref(&shallow_old),
            )
            .unwrap();
        cache
            .replace_source(
                &source_snapshot_for_fonts(
                    &recursive_root,
                    FontDirectoryScope::Recursive,
                    std::slice::from_ref(&recursive),
                ),
                std::slice::from_ref(&recursive),
            )
            .unwrap();
        let hit = cache
            .lookup_family("Priority Sans", false, false)
            .unwrap()
            .unwrap();
        assert_eq!(hit.font_path, shallow_old.file_path);

        cache
            .replace_source(
                &source_snapshot_for_fonts(
                    &shallow_new_root,
                    FontDirectoryScope::Shallow,
                    std::slice::from_ref(&shallow_new),
                ),
                std::slice::from_ref(&shallow_new),
            )
            .unwrap();
        // Refreshing an older source updates its content but not its addition
        // order; the newer shallow source must remain the winner.
        cache
            .replace_source(
                &source_snapshot_for_fonts(
                    &shallow_old_root,
                    FontDirectoryScope::Shallow,
                    std::slice::from_ref(&shallow_old),
                ),
                std::slice::from_ref(&shallow_old),
            )
            .unwrap();
        let hit = cache
            .lookup_family("Priority Sans", false, false)
            .unwrap()
            .unwrap();
        assert_eq!(hit.font_path, shallow_new.file_path);
    }

    #[test]
    fn lookup_prefers_exact_family_across_scopes_before_shallow_alias() {
        let (guard, path) = temp_cache_path();
        let mut cache = FontCache::open_or_create(&path).unwrap();
        let shallow_dir = guard.0.join("shallow-alias");
        let recursive_dir = guard.0.join("recursive-exact");
        let shallow_alias = real_font_metadata(
            &shallow_dir,
            "alias.ttf",
            0,
            vec![family_key("Unrelated Family", false, false)],
            vec!["Cross Scope Sans".to_string()],
        );
        let recursive_exact = real_font_metadata(
            &recursive_dir,
            "exact.ttf",
            0,
            vec![family_key("Cross Scope Sans", false, false)],
            Vec::new(),
        );
        let shallow_root = real_folder_path(&shallow_dir);
        let recursive_root = real_folder_path(&recursive_dir);
        cache
            .replace_source(
                &source_snapshot_for_fonts(
                    &shallow_root,
                    FontDirectoryScope::Shallow,
                    std::slice::from_ref(&shallow_alias),
                ),
                std::slice::from_ref(&shallow_alias),
            )
            .unwrap();
        cache
            .replace_source(
                &source_snapshot_for_fonts(
                    &recursive_root,
                    FontDirectoryScope::Recursive,
                    std::slice::from_ref(&recursive_exact),
                ),
                std::slice::from_ref(&recursive_exact),
            )
            .unwrap();

        let hit = cache
            .lookup_family("Cross Scope Sans", false, false)
            .unwrap()
            .unwrap();
        assert_eq!(hit.font_path, recursive_exact.file_path);
    }

    #[test]
    fn cache_accepts_representative_20130_face_source() {
        let (_guard, path) = temp_cache_path();
        let mut cache = FontCache::open_or_create(&path).unwrap();
        let mut fonts = Vec::with_capacity(20_130);
        let mut files = Vec::with_capacity(20_130);
        for index in 0..20_130 {
            let file_path = format!("/large/font-{index:05}.ttf");
            fonts.push(synthetic_font(&file_path, "Large Library Sans"));
            files.push(FileSnapshot {
                file_path,
                file_size: 100_000,
                file_mtime: 1_700_000_000_000_000_000,
            });
        }
        let snapshot = CacheSourceSnapshot {
            source_root: "/large".to_string(),
            scope: FontDirectoryScope::Recursive,
            directories: vec![FolderSnapshot {
                folder_path: "/large".to_string(),
                folder_mtime: 1_700_000_000_000_000_000,
            }],
            files,
        };
        // Align synthetic helper's legacy seconds fixture with schema-v6 ns.
        for font in &mut fonts {
            font.file_mtime = 1_700_000_000_000_000_000;
        }
        cache.replace_source(&snapshot, &fonts).unwrap();
        let face_count: i64 = cache
            .conn
            .query_row("SELECT COUNT(*) FROM cached_fonts", [], |row| row.get(0))
            .unwrap();
        assert_eq!(face_count, 20_130);
    }

    #[test]
    fn cache_accepts_4096_directory_snapshot() {
        let (_guard, path) = temp_cache_path();
        let mut cache = FontCache::open_or_create(&path).unwrap();
        let mut directories = vec![FolderSnapshot {
            folder_path: "/tree".to_string(),
            folder_mtime: 1,
        }];
        directories.extend((1..MAX_CACHED_DIRECTORIES).map(|index| FolderSnapshot {
            folder_path: format!("/tree/d-{index:04}"),
            folder_mtime: i64::try_from(index).unwrap(),
        }));
        cache
            .replace_source(
                &CacheSourceSnapshot {
                    source_root: "/tree".to_string(),
                    scope: FontDirectoryScope::Recursive,
                    directories,
                    files: Vec::new(),
                },
                &[],
            )
            .unwrap();
        let directory_count: i64 = cache
            .conn
            .query_row("SELECT COUNT(*) FROM cached_directories", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(directory_count, MAX_CACHED_DIRECTORIES as i64);
    }

    #[test]
    fn snapshot_rejects_bidi_candidate_path() {
        let guard = TempCacheDir::new();
        let root = guard.0.join("bidi-library");
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join("evil\u{202e}ttf.ttf"), b"candidate").unwrap();
        let err = snapshot_source_directories(&root, FontDirectoryScope::Shallow).unwrap_err();
        assert!(err.contains("contains invalid characters"), "got: {err}");
    }

    #[cfg(unix)]
    #[test]
    fn snapshot_rejects_non_utf8_source_root() {
        use std::ffi::OsString;
        use std::os::unix::ffi::OsStringExt;
        let guard = TempCacheDir::new();
        let invalid = guard.0.join(OsString::from_vec(vec![b'f', 0xff]));
        fs::create_dir_all(&invalid).unwrap();
        let err = snapshot_source_directories(&invalid, FontDirectoryScope::Shallow).unwrap_err();
        assert!(err.contains("not valid UTF-8"), "got: {err}");
    }

    #[cfg(unix)]
    #[test]
    fn snapshot_rejects_symlink_root_before_canonicalizing() {
        use std::os::unix::fs::symlink;
        let guard = TempCacheDir::new();
        let target = guard.0.join("real-library");
        let linked = guard.0.join("linked-library");
        fs::create_dir_all(&target).unwrap();
        symlink(&target, &linked).unwrap();
        let err = snapshot_source_directories(&linked, FontDirectoryScope::Recursive).unwrap_err();
        assert!(err.contains("reparse point"), "got: {err}");
    }
}
