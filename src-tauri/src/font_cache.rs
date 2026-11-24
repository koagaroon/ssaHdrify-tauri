//! Persistent font cache — metadata index across app lifetimes.
//!
//! Submodules divide the Tauri-free persistent cache by responsibility while
//! keeping the `crate::font_cache::*` surface stable across GUI and CLI:
//! - `types`: source keys, snapshots, lookup results, drift report, and errors
//! - `snapshot`: canonical source-key resolution and bounded directory/file snapshots
//! - `store`: SQLite schema, open/read-only lifecycle, source writes, drift diff, and lookup

mod snapshot;
mod store;
#[cfg(test)]
mod tests;
mod types;

use std::path::{Path, PathBuf};

pub use snapshot::{
    cache_source_key, snapshot_source_directories, validate_cache_source_stability,
};
pub use store::FontCache;
#[cfg(test)]
pub use types::FolderRecord;
pub use types::{
    CacheError, CacheSourceKey, CacheSourceRecord, CacheSourceSnapshot, DriftReport, FamilyKey,
    FileSnapshot, FolderSnapshot, FontDirectoryScope, FontLookupResult, FontMetadata,
};

#[allow(unused_imports)]
use snapshot::*;
#[allow(unused_imports)]
use store::*;
#[allow(unused_imports)]
use types::*;

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
/// (`ssahdrify` GUI + `ssahdrify-cli`): `%APPDATA%/ssahdrify`.
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

/// Windows roaming data directory, shared by the GUI and CLI caches.
fn platform_data_dir() -> Result<PathBuf, String> {
    std::env::var("APPDATA").map(PathBuf::from).map_err(|_| {
        "APPDATA environment variable not set; cannot determine default cache location. Pass --cache-file <PATH> to override."
            .to_string()
    })
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
// Version 6 can contain incomplete scans whose read failures left no recovery marker.
pub const SCHEMA_VERSION: i32 = 7;

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
/// per-stat OS timeout. The cap fires inside `list_sources` and refuses
/// to return the result; downstream `diff_sources` /
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
/// form or case (`Café` vs `CAFÉ`,
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
