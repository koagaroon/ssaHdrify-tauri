use serde::{Deserialize, Serialize};

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
    pub(super) fn db_value(self) -> i32 {
        match self {
            Self::Shallow => 0,
            Self::Recursive => 1,
        }
    }

    pub(super) fn from_db(value: i32) -> Result<Self, CacheError> {
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

/// Test-only root view for legacy shallow fixtures. Production code must use
/// [`CacheSourceRecord`] so source scope and nested snapshots cannot be lost.
#[cfg(test)]
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

/// Recoverable errors during cache operations. The caller chooses how
/// to react: CLI falls back to no-cache and warns; GUI prompts the user.
///
/// Unified across open/read/write to keep the public API simple — the
/// caller mostly cares about "did it work" + a message. The two
/// structured variants are deliberately narrow: callers may recommend
/// deleting/rebuilding a schema-mismatched or invalid SQLite cache, but
/// must not give that destructive advice for an ordinary permission,
/// lock, or path error.
#[derive(Debug)]
pub enum CacheError {
    /// Filesystem or SQLite-level failure. Includes a human-readable
    /// message embedding the underlying error. During connection setup
    /// and schema verification, SQLite corruption and not-a-database
    /// errors use `InvalidDatabase` instead.
    Io(String),
    /// While opening or verifying the cache, SQLite positively identified
    /// it as corrupt or not a database. This is narrower than `Io`, allowing
    /// callers to offer rebuild guidance without misdiagnosing
    /// permission/open failures.
    InvalidDatabase(String),
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
            Self::InvalidDatabase(msg) => {
                write!(f, "cache database is corrupt or not SQLite: {msg}")
            }
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

/// Preserve the only SQLite error-code distinction during open/schema
/// validation that changes safe recovery advice at the caller boundary.
/// All other SQLite failures — including permission denied, cannot-open,
/// read-only, busy, and locked — remain ordinary `Io` errors and must not
/// prompt automatic deletion.
pub(super) fn cache_sqlite_error(context: impl Into<String>, error: rusqlite::Error) -> CacheError {
    let message = format!("{}: {error}", context.into());
    if matches!(
        error.sqlite_error_code(),
        Some(rusqlite::ErrorCode::DatabaseCorrupt | rusqlite::ErrorCode::NotADatabase)
    ) {
        CacheError::InvalidDatabase(message)
    } else {
        CacheError::Io(message)
    }
}
