//! System font lookup, in-process font provenance sets, and `fontcull`
//! glyph subsetting.

use super::*;
use font_kit::family_name::FamilyName;
use font_kit::handle::Handle;
use font_kit::properties::{Properties, Style as FontKitStyle, Weight as FontKitWeight};
use font_kit::source::SystemSource;
use once_cell::sync::Lazy;
use std::collections::HashSet;
use std::fs;
use std::path::Path;
use std::sync::Mutex;

/// Provenance cache: tracks (font path, face index) pairs returned by
/// `find_system_font`. Only entries here are allowed to be read by
/// `subset_font`'s system-font branch — together with the system-fonts-
/// dir restriction below, this is two layers of defense against
/// arbitrary-file-read via IPC.
///
/// Keyed by `(path, face_index)` so multi-face TTC registrations only
/// authorize the exact face index that was resolved.
///
/// Never evicted — the set is bounded by the number of unique system
/// fonts (typically < 1000), and eviction would introduce TOCTOU
/// windows.
pub(super) static ALLOWED_FONT_PATHS: Lazy<Mutex<HashSet<(String, u32)>>> =
    Lazy::new(|| Mutex::new(HashSet::new()));

/// Separate provenance set for persistent-cache lookups. When
/// `lookup_family` returns a hit, the (path, face_index) pair is
/// registered here so `subset_font` can accept it. Kept apart from
/// `ALLOWED_FONT_PATHS` because:
///
/// 1. The dir restriction that applies to system fonts MUST NOT apply
///    to cache hits — cached paths point at user-picked folders, not
///    system dirs, and folding them into one set would either drop the
///    dir defense for system fonts (bad) or break cache lookups (worse).
/// 2. The threat-model classification is different: system fonts are
///    discovered via font-kit which guarantees the path lives under a
///    known dir; cache hits are trusted on the strength of the
///    in-process lookup having succeeded against an opened SQLite file
///    (local-user, single-user, AppData-local).
///
/// Trusting cache hits in-process restores the design-locked CLI
/// Situation B ("no --font-dir + cache exists → implicit cache use")
/// and the GUI's lookup tier 2 that an earlier change broke by
/// rejecting all cache rows as untrusted.
/// See `register_cache_provenance` for the entry point.
pub(super) static ALLOWED_CACHE_FONT_PATHS: Lazy<Mutex<HashSet<(String, u32)>>> =
    Lazy::new(|| Mutex::new(HashSet::new()));

/// Find a system font file path by family name, bold, and italic flags.
/// Returns the path + face index. Prefers TTF/TTC over OTF/OTC for subtitle
/// renderer compatibility (libass/VSFilter don't support OTF bold).
pub fn find_system_font(
    family: String,
    bold: bool,
    italic: bool,
) -> Result<FontLookupResult, String> {
    // Input validation: reject empty, oversized, or control-char-containing
    // names. Use char count (codepoints), NOT byte length — a 100-char CJK
    // family name is 300+ UTF-8 bytes and is perfectly legitimate.
    // `parse_local_font_file` already counts codepoints when ingesting
    // names; the lookup gate must agree, otherwise a font that scans into
    // the index successfully gets rejected at lookup time.
    crate::util::validate_font_family(&family)?;

    let source = SystemSource::new();

    let mut props = Properties::new();
    if bold {
        props.weight = FontKitWeight::BOLD;
    }
    if italic {
        props.style = FontKitStyle::Italic;
    }

    let handle = source
        .select_best_match(&[FamilyName::Title(family.clone())], &props)
        .map_err(|e| {
            // Log the detailed error server-side; return a generic message
            // to the frontend so OS-level paths never surface in user toasts.
            // INFO not WARN: a missed system lookup is normal flow when the
            // user hasn't picked local font sources yet, and the frontend
            // surfaces "Missing" badges per font anyway. Bumping to warn
            // would spam dev logs every time a batch is analyzed before
            // sources are added; release builds (Warn+) hide info entirely.
            log::info!("font lookup failed for '{family}' (bold={bold}, italic={italic}): {e}");
            // The "Font not found:" prefix is parsed by the CLI shell
            // (`bin/cli/main.rs` `resolve_embed_font`) to distinguish a
            // benign system-miss from a real error under
            // `--on-missing warn`. Any change to this prefix MUST update
            // the matcher there in lockstep — the GUI doesn't care, but
            // the CLI breaks silently.
            format!("Font not found: {family} (bold={bold}, italic={italic})")
        })?;

    match handle {
        Handle::Path { path, font_index } => {
            let ext = path
                .extension()
                .and_then(|e| e.to_str())
                // Use ascii-only fold for consistency with
                // `has_allowed_font_extension` and `parse_local_font_file`
                // — all font extensions are pure ASCII,
                // so locale-aware `to_lowercase()` is unnecessary alloc.
                .map(|e| e.to_ascii_lowercase())
                .unwrap_or_default();

            // OTF/OTC warning: libass/VSFilter don't support OTF bold rendering.
            // font-kit's select_best_match returns the system's preferred match
            // and has no API to filter by format. Enumerating the family and
            // loading each face to check style properties is too expensive for
            // a font lookup hot path. Accept the OTF and warn.
            if ext == "otf" || ext == "otc" {
                log::warn!(
                    "Using OTF font for '{}' — bold may not render in libass/VSFilter",
                    family
                );
            }

            register_font_path(&path, font_index)
        }
        Handle::Memory { .. } => Err("Font is memory-only (no file path available)".to_string()),
    }
}

/// Register a font path in the provenance cache and return the lookup result.
pub(super) fn register_font_path(path: &Path, font_index: u32) -> Result<FontLookupResult, String> {
    let canonical = path.canonicalize().map_err(|e| {
        log::warn!("canonicalize font path failed: {e}");
        "Cannot resolve font path".to_string()
    })?;
    let canonical_string = normalize_canonical_path(&canonical.to_string_lossy());
    // validate the canonical path through
    // `validate_ipc_path` for parity with `register_cache_provenance`
    // (above). System-font paths come from font-kit's OS enumeration
    // and pass through canonicalize() + the system-fonts-dir constraint
    // — both stronger gates than cache rows have — but the symmetric
    // validate call is cheap defense-in-depth that closes the asymmetry.
    // If a future change to font-kit, the system
    // font enumeration, or normalize_canonical_path lets through a
    // path with control chars / BiDi / `..` segments, this is the
    // final gate before the trust set commits.
    crate::util::validate_ipc_path(&canonical_string, "System font")?;
    insert_with_cap(
        &ALLOWED_FONT_PATHS,
        ProvenanceSetKind::System,
        canonical_string.clone(),
        font_index,
    )?;
    Ok(FontLookupResult {
        path: canonical_string,
        index: font_index,
    })
}

/// Shared (path, face_index) insertion helper used by both provenance
/// sets. `cache` is the target set; `canonical_string` and `face_index`
/// are the entry. The typed set kind keeps the recovery advice aligned
/// with the operation that can actually clear that set.
/// Enforces
/// `MAX_PROVENANCE_CACHE_SIZE` per-set as a rollback-on-overflow contract.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum ProvenanceSetKind {
    System,
    Cache,
}

pub(super) fn provenance_overflow_message(kind: ProvenanceSetKind) -> String {
    let (label, recovery) = match kind {
        ProvenanceSetKind::System => (
            "system",
            "Restart the app to clear the system-font registration set, then retry.",
        ),
        ProvenanceSetKind::Cache => (
            "cache",
            "Clear font sources or clear the font cache, then retry.",
        ),
    };
    format!(
        "Too many registered font paths in {label} set (> {MAX_PROVENANCE_CACHE_SIZE}). \
         {recovery}"
    )
}

pub(super) fn insert_with_cap(
    cache: &Lazy<Mutex<HashSet<(String, u32)>>>,
    kind: ProvenanceSetKind,
    canonical_string: String,
    face_index: u32,
) -> Result<(), String> {
    let mut set = cache
        .lock()
        .map_err(|_| "Internal error: font path cache corrupted".to_string())?;
    // Single HashSet hit via `insert` (returns true if newly added).
    // Was previously `contains` + `insert` — two lookups for the
    // common case. `insert` returning true means the slot was free
    // before; cache.len() now reflects the post-insert count, so the
    // cap check uses `>` (strictly above the pre-insert size limit).
    let entry = (canonical_string, face_index);
    let newly_added = set.insert(entry.clone());
    if newly_added && set.len() > MAX_PROVENANCE_CACHE_SIZE {
        // Roll back the speculative insert so the cap is firm.
        set.remove(&entry);
        return Err(provenance_overflow_message(kind));
    }
    Ok(())
}

/// Drop every entry from `ALLOWED_CACHE_FONT_PATHS`. Called when a
/// cache-wide reset happens: `clear_font_cache` rebuilds the SQLite
/// file but previously left in-process provenance rows behind —
/// combined with a hostile `--cache-file` swap (untrusted-input), stale trust
/// entries would persist until app restart and let `subset_font`
/// accept paths the freshly-cleared cache no longer references.
/// `clear_font_sources` already does the same eviction at
/// session-DB-clear time; this is the symmetric helper for the
/// persistent-cache clear path. ALLOWED_FONT_PATHS (system fonts) is
/// intentionally NOT touched here — system fonts don't depend on cache
/// state.
pub fn clear_cache_provenance() {
    if let Ok(mut cache) = ALLOWED_CACHE_FONT_PATHS.lock() {
        cache.clear();
    }
}

#[cfg(test)]
pub(crate) fn cache_provenance_contains(path: &str, face_index: u32) -> bool {
    ALLOWED_CACHE_FONT_PATHS
        .lock()
        .is_ok_and(|cache| cache.contains(&(path.to_string(), face_index)))
}

/// Register a persistent-cache lookup hit into the cache provenance
/// set. CLI's `resolve_embed_font` and the GUI's `lookup_font_family`
/// call this so a path returned by the persistent cache passes
/// `subset_font`'s gate, closing a design-vs-implementation conflict:
///
/// - CLI Situation B (no `--font-dir` + cache exists → implicit cache
///   use) and the GUI's lookup tier 2 both depended on cache-returned
///   paths being subsequently subsettable. An earlier post-cache-gate
///   change rejected them as untrusted, breaking the documented
///   behavior.
///
/// - Per the personal-desktop threat model, a cache file can be stale,
///   corrupt, or same-user modified. Trust is therefore limited to rows
///   returned by `lookup_family` in THIS process, after path and
///   face-index validation. This is basic fail-safe protection, not a
///   high-assurance defense against a process that can race every
///   filesystem operation. Revisit if the project later ships a server
///   or multi-user mode.
///
/// Routes to `ALLOWED_CACHE_FONT_PATHS` — kept apart from the system-
/// font set so the system-fonts-dir defense still applies to
/// `find_system_font` registrations.
///
/// Accepts `&font_cache::FontLookupResult` (whose fields are `pub(crate)`),
/// enforcing at the type level that only `FontCache::lookup_family` hits can
/// register in `ALLOWED_CACHE_FONT_PATHS`.
///
/// Cache row paths are canonicalized upstream by `replace_source`;
/// this function re-validates via `validate_ipc_path` anyway so a
/// hostile `--cache-file` swap can't smuggle crafted bytes past the
/// trust set.
pub fn register_cache_provenance(hit: &crate::font_cache::FontLookupResult) -> Result<(), String> {
    // re-validate the canonical path through the
    // IPC validator before insertion. A hostile cache file supplied
    // via `--cache-file` (untrusted-input) could carry crafted rows with control
    // chars, BiDi, parent-directory segments, or DOS device prefixes;
    // catching at insert time keeps the trust set clean.
    crate::util::validate_ipc_path(hit.font_path(), "Cache font")?;
    // face_index is stored as i32; reject
    // negative values explicitly rather than reinterpreting the bit
    // pattern into a huge u32.
    let face_index = u32::try_from(hit.face_index()).map_err(|_| {
        format!(
            "Cache lookup has invalid negative face_index: {}",
            hit.face_index()
        )
    })?;
    insert_with_cap(
        &ALLOWED_CACHE_FONT_PATHS,
        ProvenanceSetKind::Cache,
        hit.font_path().to_string(),
        face_index,
    )
}

/// True when `path` equals `dir` or lives under it (using `sep` as the
/// separator). Matched via `starts_with` only — no `contains` — so that
/// directories whose names merely include "fonts" never leak through.
pub(super) fn path_under_dir(path: &str, dir: &str, sep: &str) -> bool {
    path == dir || path.starts_with(&format!("{dir}{sep}"))
}

/// Cached, normalized system-fonts dir derived from `SYSTEMROOT` + `\Fonts`.
///
/// Captured eagerly at app startup via `init_system_dirs` (see `lib.rs`) so
/// the value is locked in before any user action could indirectly trigger
/// an `std::env::set_var` call. The cache is `Lazy` rather than a plain
/// `OnceCell` so that any code path which somehow runs before
/// `init_system_dirs` (e.g., a unit test in this module) still gets a
/// well-defined value rather than a panic.
///
/// Defense-in-depth note: a deeper hardening would call
/// `GetSystemWindowsDirectoryW` directly, which reads from a kernel-set
/// process buffer immune to env-var manipulation. We rely on the eager
/// init plus the project's no-`set_var` policy instead, since adding Win32
/// FFI for one path read is not justified at the current threat model
/// (attacker would already need code execution to mutate env vars).
pub(super) fn normalize_system_font_dir_path(base: &Path, suffix: &[&str]) -> String {
    let joined = suffix
        .iter()
        .fold(base.to_path_buf(), |acc, component| acc.join(component));
    let resolved = joined.canonicalize().unwrap_or(joined);
    normalize_canonical_path(&resolved.to_string_lossy())
        .replace('/', "\\")
        .trim_end_matches('\\')
        .to_lowercase()
}

pub(super) static WINDOWS_SYSTEM_FONTS_DIR: Lazy<String> = Lazy::new(|| {
    let sys_root = std::env::var("SYSTEMROOT").unwrap_or_else(|_| "C:\\Windows".to_string());
    normalize_system_font_dir_path(Path::new(&sys_root), &["Fonts"])
});

/// Cached, normalized per-user fonts dir (Windows 10 1809+). `None` if
/// `LOCALAPPDATA` was unset at startup. Same caching rationale as
/// `WINDOWS_SYSTEM_FONTS_DIR`.
pub(super) static WINDOWS_USER_FONTS_DIR: Lazy<Option<String>> = Lazy::new(|| {
    std::env::var("LOCALAPPDATA").ok().map(|local_app_data| {
        normalize_system_font_dir_path(
            Path::new(&local_app_data),
            &["Microsoft", "Windows", "Fonts"],
        )
    })
});

/// Force-initialize the cached system-fonts directory paths. Called from
/// `lib.rs::run` at app startup so the env-var snapshot is taken before
/// any user action could indirectly mutate the process environment.
pub fn init_system_dirs() {
    Lazy::force(&WINDOWS_SYSTEM_FONTS_DIR);
    Lazy::force(&WINDOWS_USER_FONTS_DIR);
}

/// Check whether a canonicalized path is under a known system fonts directory.
pub(super) fn is_in_system_fonts_dir(canonical: &Path) -> bool {
    let canonical_str = normalize_canonical_path(&canonical.to_string_lossy());
    // Full Unicode lowercase matches `normalize_system_font_dir_path` so non-ASCII
    // Windows user profile folders fold identically on both sides.
    let lower = canonical_str.replace('/', "\\").to_lowercase();
    let under = |dir: &str| path_under_dir(&lower, dir, "\\");

    if under(&WINDOWS_SYSTEM_FONTS_DIR) {
        return true;
    }
    WINDOWS_USER_FONTS_DIR
        .as_ref()
        .is_some_and(|user_dir| under(user_dir))
}

/// Subset a font file to only include the specified codepoints.
///
/// Uses fontcull (Google's klippa engine) for pure-Rust subsetting.
/// For TTC files with face index > 0, uses fontcull's internal crates directly
/// to select the correct face. Always includes ASCII printable (0x0020–0x007E)
/// and CJK fullwidth forms (0xFF01–0xFF5E) as safety padding.
/// Returns an error on subsetting failure instead of embedding the full font.
///
/// Public IPC entry point + the CLI's standalone-embed callsite both
/// invoke this function as a regular `pub fn`; the GUI reaches it through
/// `ipc_commands::subset_font_bytes`, which runs this function on the blocking
/// pool and returns a raw `tauri::ipc::Response` to avoid expanding each
/// byte into a JSON number.
/// CLI's chain mode marshals subsets via base64 inline (see
/// `process_one_chain_input`); CLI's standalone embed bundles them
/// into `engine::FontSubsetPayload` and ships through the engine's
/// JSON-payload boundary (where the expansion is bounded by per-font
/// caps, not the cumulative ceiling).
///
/// **IMPORTANT **: do NOT add `#[tauri::command]`
/// to this function. The Vec<u8> return shape would JSON-encode as
/// `[byte, byte, ...]` over the GUI IPC wire, hitting the same ~4-5×
/// expansion the target-aware wrapper exists specifically to dodge. The
/// GUI-side IPC path MUST go through `ipc_commands::subset_font_bytes`; adding
/// the attribute here would silently bypass that guard and pressure the JS heap
/// on every embed.
/// Future direct exposure must go through an explicit security and
/// resource-budget audit.
pub fn subset_font(
    font_path: String,
    font_index: u32,
    codepoints: Vec<u32>,
) -> Result<Vec<u8>, String> {
    // IPC boundary validation: font_index and codepoints come from untrusted JS.
    // font_path also from JS — validate length / control-char / DOS-device
    // shape before any allocation, matching find_system_font's posture.
    crate::util::validate_ipc_path(&font_path, "Font")?;
    if font_index > MAX_SUBSET_FONT_INDEX {
        return Err(format!(
            "Invalid font face index: {font_index} (max {MAX_SUBSET_FONT_INDEX})"
        ));
    }
    if codepoints.len() > MAX_SUBSET_CODEPOINTS {
        return Err(format!(
            "Too many codepoints: {} (max {MAX_SUBSET_CODEPOINTS})",
            codepoints.len()
        ));
    }
    // Reject out-of-range codepoints. Unicode tops out at U+10FFFF; anything
    // larger is a JS-side bug or a crafted IPC payload and must not reach
    // fontcull's IntSet, which would happily allocate for absurd values.
    if let Some(&bad) = codepoints.iter().find(|&&cp| cp > 0x10FFFF) {
        return Err(format!("Invalid codepoint: U+{bad:X} (max U+10FFFF)"));
    }

    let path = Path::new(&font_path);
    let filename = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("<unknown>");

    // Validate file extension against allowed font types.
    // ASCII-only fold matches `has_allowed_font_extension` /
    // `parse_local_font_file` / `find_system_font::Handle::Path` arms
    // — every entry in ALLOWED_FONT_EXTENSIONS is
    // pure ASCII so locale-aware lowercase is unnecessary alloc.
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase())
        .unwrap_or_default();
    if !ALLOWED_FONT_EXTENSIONS.contains(&ext.as_str()) {
        return Err(format!(
            "Invalid font file type '{}' for '{}'. Allowed extensions: {}",
            ext,
            filename,
            ALLOWED_FONT_EXTENSIONS.join(", ")
        ));
    }

    // PRE-canonicalize reparse-point reject. On
    // Windows, `Path::canonicalize` follows symlinks / junctions to
    // resolve the target, so the POST-canonicalize check below sees
    // the target (typically not a reparse point) — for the documented
    // `evil.ttf -> /etc/shadow` attack class, the canonical IS the
    // target and the post-check is a no-op. Adding the pre-check
    // brings parity with `dropzone.rs` / scan paths, which all
    // refuse early on `entry.path()` before canonicalize. The
    // post-check below remains for reparse-point chains where the
    // canonicalize step itself resolves through a reparse target.
    if crate::util::is_reparse_point(path) {
        log::warn!(
            "Refusing to subset font through symlink / junction: '{}'",
            path.display()
        );
        return Err("Refusing to subset font through symlink / junction".to_string());
    }

    // Canonicalize to resolve symlinks, "..", and normalize the path
    let canonical = path.canonicalize().map_err(|e| {
        log::warn!("canonicalize font path failed for '{filename}': {e}");
        "Cannot resolve font path".to_string()
    })?;

    // Post-canonicalize reparse-point reject — belt-and-suspenders
    // with the pre-check above. Covers reparse-point chains (e.g.,
    // symlink → symlink → file) where the FIRST hop is
    // canonicalize-resolved cleanly and the SECOND hop is a reparse
    // point pointing at non-font content. Without this, the 4-byte
    // magic-byte sniff opens the file just long enough to read 4 bytes
    // of deny-listed content. The full-body read is already gated by
    // the sniff (returns Err on non-font header) and the no-fallback
    // policy on parse failure, but the 4-byte probe is a residual
    // partial-content read. Closing it here keeps subset_font symmetric
    // with every other file-read entry in the codebase.
    if crate::util::is_reparse_point(&canonical) {
        log::warn!(
            "Refusing to subset font through symlink / junction: '{}'",
            canonical.display()
        );
        return Err("Refusing to subset font through symlink / junction".to_string());
    }

    // Provenance guard: the (path, face_index) pair must have been
    // discovered by one of three trusted entry points in THIS process:
    //
    //   1. `find_system_font` → registers in ALLOWED_FONT_PATHS
    //      (system fonts; also subject to system-fonts-dir restriction
    //      below for defense-in-depth).
    //   2. `scan_font_directory` / `scan_font_files` → records in the
    //      session SQLite (user-picked paths from THIS session's scan).
    //   3. `lookup_family` cache hit (CLI's resolve_embed_font or
    //      GUI's lookup_font_family) → registers in
    //      ALLOWED_CACHE_FONT_PATHS via `register_cache_provenance`.
    //
    // keying by (path, face_index) instead of
    // path alone closes a face-index injection where attacker-influenced
    // ASS would request `subset_font(arial.ttc, 5, ...)` against a path
    // registered for face 0. TTC files contain multiple faces, and the
    // gate used to pass on path alone, letting the wrong face's bytes
    // ship in the [Fonts] section.
    //
    // Persistent cache rows ARE a provenance source — but ONLY for
    // entries `lookup_family` returned during THIS process (the second
    // set, ALLOWED_CACHE_FONT_PATHS). Nothing accepts a path that
    // merely appears in the SQLite file but was not actually looked up
    // this run. An earlier gate rejected all cache rows, which broke
    // the design-locked CLI Situation B and GUI lookup tier 2. The
    // personal-desktop threat model accepts this in-process trust only
    // after lookup-time path / face-index validation; it is not a
    // server-grade hostile-process boundary.
    let canonical_string = normalize_canonical_path(&canonical.to_string_lossy());
    let registered_key = (canonical_string.clone(), font_index);
    // (readability): name each tier flag for what
    // it asserts when `true` — "accepted via this tier" — rather than
    // "is this tier's nature." Each variable evaluates lazily: once a
    // prior tier accepts, subsequent ones short-circuit to `false`.
    // The downstream gate (the system-fonts-dir directory-residency
    // check + the magic-byte sniff skip later in `subset_font`)
    // keys off `accepted_via_system`
    // specifically because system fonts get the extra dir-residency
    // check on top of the registration. Cache + user tiers are
    // checked only when system didn't accept and are NOT individually
    // referenced after the gate; collapsing them into one boolean
    // would lose the lazy-evaluation ordering this relies on.
    let accepted_via_system = ALLOWED_FONT_PATHS
        .lock()
        .map_err(|_| "Internal error: font path cache corrupted".to_string())?
        .contains(&registered_key);
    let accepted_via_cache = if accepted_via_system {
        false
    } else {
        ALLOWED_CACHE_FONT_PATHS
            .lock()
            .map_err(|_| "Internal error: font path cache corrupted".to_string())?
            .contains(&registered_key)
    };
    let accepted_via_user = if accepted_via_system || accepted_via_cache {
        false
    } else {
        is_user_font_face_registered(&canonical_string, font_index)?
    };
    if !accepted_via_system && !accepted_via_cache && !accepted_via_user {
        return Err("Font path was not discovered by a scan command".to_string());
    }

    // Defense-in-depth: system-discovered paths must live under a known
    // system fonts directory. User-picked paths (session DB) and cache
    // hits skip this check — the whole point of those tiers is to
    // accept user-chosen directories — but they had to pass their own
    // provenance step above, so random file reads via IPC are still
    // blocked.
    if accepted_via_system && !is_in_system_fonts_dir(&canonical) {
        return Err("System font path is not in a system fonts directory".to_string());
    }

    // Pre-read size check — rejects obvious oversize before allocating the Vec.
    let metadata = fs::metadata(&canonical).map_err(|e| {
        log::warn!("stat font file failed for '{filename}': {e}");
        "Cannot stat font file".to_string()
    })?;
    // Defense-symmetry: every file-read entry path in this module
    // gates on is_file() before touching content —
    // `parse_local_font_file` via its callers (scan_directory_inner,
    // scan_files_inner, preflight_files_inner), and now `subset_font`
    // here. Pre-this-check `subset_font` relied on extension + reparse
    // + provenance + magic-byte sniff and could be reached with a
    // non-regular inode if any of those upstream defenses ever
    // loosened. No reachable exploit today (validate_ipc_path's
    // DOS-device reject + the reparse check close `/dev/null`-class
    // device paths; a directory named `evil.ttf` would fall through
    // to the magic-byte probe which fails on the read_exact). Reuses
    // the metadata already fetched above — no extra syscall.
    if !metadata.is_file() {
        log::warn!(
            "Refusing to subset non-regular font path '{}'",
            canonical.display()
        );
        return Err("Font path is not a regular file".to_string());
    }
    if metadata.len() > MAX_FONT_DATA_SIZE {
        return Err(format!(
            "Font file too large ({:.1} MB, max {} MB)",
            metadata.len() as f64 / (1024.0 * 1024.0),
            MAX_FONT_DATA_SIZE / 1024 / 1024
        ));
    }

    // magic-byte sniff for non-system tiers as a
    // defense-in-depth gate before the full file read. The extension
    // check above rejects `/etc/passwd` (no `.ttf` extension), but
    // attacker-crafted cache rows pointing at `/etc/passwd.ttf` (or
    // any non-font local file renamed / symlinked to a font extension)
    // would otherwise pass extension + provenance + size gates and
    // reach `fs::read` for a 64 MB buffer copy. The no-fallback policy
    // already closes the byte-exfil layer (subset returns Err on
    // fontcull failure
    // with no fallback), but reading arbitrary local files into
    // process memory is itself a primitive worth closing at the
    // source — especially for `--cache-file`-supplied paths (untrusted-input).
    //
    // System fonts (`accepted_via_system=true`) skip this sniff: they
    // already passed the stricter `is_in_system_fonts_dir` gate above,
    // and OS font directories are guaranteed to contain real fonts.
    // Cache + user tiers go through the sniff.
    if !accepted_via_system {
        use std::io::Read;
        let mut header = [0u8; 4];
        let mut probe = fs::File::open(&canonical).map_err(|e| {
            log::warn!("open font for sniff failed for '{filename}': {e}");
            format!("Cannot open font file '{filename}'")
        })?;
        probe.read_exact(&mut header).map_err(|e| {
            log::warn!("read font header failed for '{filename}': {e}");
            format!("Cannot read font header for '{filename}'")
        })?;
        // Recognized font magic numbers per the OpenType / TrueType
        // specs: `\0\1\0\0` (sfnt-flavored TrueType), `OTTO` (sfnt
        // with CFF outlines), `ttcf` (collection), `true` (Apple
        // TrueType). `typ1` (PostScript Type 1) intentionally omitted —
        // not supported by fontcull / skrifa, would fail parse anyway,
        // and is so rare that the false-reject cost is bounded to
        // archive-only fonts.
        // renamed from `is_font` — true here means
        // "has a recognized OpenType / TrueType signature", which is
        // narrower than "is a font". PostScript Type 1 fonts ARE
        // fonts but lack any of these magic numbers; the comment
        // above documents the deliberate Type 1 exclusion.
        let has_recognized_font_signature =
            matches!(&header, b"\x00\x01\x00\x00" | b"OTTO" | b"ttcf" | b"true");
        if !has_recognized_font_signature {
            return Err(format!(
                "Font file '{filename}' has no recognized font signature"
            ));
        }
        drop(probe);
    }

    // bounded-read TOCTOU mitigation. Parity
    // with the same change in `parse_local_font_file` above —
    // `fs::read` is unbounded and a hostile file swap between the
    // earlier metadata fetch and this read could force a
    // multi-GB allocation before the post-read recheck below
    // catches it. `File::open + take(MAX_FONT_DATA_SIZE + 1) +
    // read_to_end` caps the buffer at the OS layer; the +1 byte
    // disambiguates "at cap" from "over cap" so the recheck below
    // cleanly identifies an over-cap read.
    let font_data = {
        use std::io::Read;
        let file = std::fs::File::open(&canonical).map_err(|e| {
            log::warn!("open font file failed for '{filename}': {e}");
            format!("Failed to read font file '{filename}'")
        })?;
        let mut buf = Vec::new();
        let mut limited = file.take(MAX_FONT_DATA_SIZE + 1);
        limited.read_to_end(&mut buf).map_err(|e| {
            log::warn!("read font file failed for '{filename}': {e}");
            format!("Failed to read font file '{filename}'")
        })?;
        buf
    };

    // Post-read size check (TOCTOU mitigation — file could grow between stat and read).
    // kept as belt-and-suspenders for the bounded-read above.
    if font_data.len() as u64 > MAX_FONT_DATA_SIZE {
        return Err(format!(
            "Font file too large after read ({:.1} MB, max {} MB)",
            font_data.len() as f64 / (1024.0 * 1024.0),
            MAX_FONT_DATA_SIZE / 1024 / 1024
        ));
    }

    // Build codepoint set: caller's codepoints + safety padding
    let mut all_codepoints = codepoints;
    // ASCII printable — always needed for punctuation, numbers, basic latin
    all_codepoints.extend(0x0020u32..=0x007Eu32);
    // CJK fullwidth forms — common in CJK subtitle typesetting (，。！？etc.)
    all_codepoints.extend(0xFF01u32..=0xFF5Eu32);
    all_codepoints.sort();
    all_codepoints.dedup();

    // Attempt subsetting. On failure, return Err; never embed raw
    // full-font bytes as a fallback.
    //
    // catch_unwind around the fontcull call: the klippa engine is in
    // active development and has had documented panics on malformed
    // input (corrupted CFF, bad TTC face counts). User-picked path
    // means a malformed user font crashing fontcull would otherwise
    // panic the IPC command and surface to the frontend as a generic
    // "command failed" with no actionable text. AssertUnwindSafe is
    // sound here: `font_data` is owned (Vec<u8>) and not mutated by
    // the closure (fontcull takes a slice), so unwinding can't leave
    // it in a torn state. Convert any panic into a structured error
    // string the frontend's existing IPC error path can render.
    let subset_result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        // Single-face files always have one face at index 0; the
        // `font_index` parameter is only meaningful for TTC inputs.
        // `FontRef::from_index(data, 0)` works on both single-face
        // SFNT (`\0\1\0\0` / `OTTO` / `true`) and TTC at offset
        // table entry 0, so the dispatch is just "what index to
        // slice." The is_ttc_data check is still load-bearing
        // because a TTC with `font_index > 0` must NOT collapse to
        // index 0 — that would silently swap the wrong face's
        // bytes into the output.
        let effective_index = if is_ttc_data(&font_data) {
            font_index
        } else {
            0
        };
        subset_with_index(&font_data, effective_index, &all_codepoints)
    }))
    .unwrap_or_else(|panic_payload| {
        // Convert panic payload (Box<dyn Any>) into a string for the log
        // and IPC return. Most panics produce &str (`panic!("...")`) or
        // String (`panic!("{}", x)`). The Box<dyn Error+Send+Sync> arm
        // catches the narrow case of `panic_any(Box::new(some_err))` —
        // explicit boxed-error panics, NOT all error types thrown via
        // `.expect()` (which produce String). The std::io::Error arm
        // catches `panic_any(io_err)`. A bare `anyhow::Error` panic or
        // other typed payload hits the unknown-payload fallback (which
        // surfaces TypeId for diagnostic triage). We do NOT pull anyhow
        // as a dep just for the downcast — fontcull doesn't panic with
        // anyhow::Error today, and the fallback is diagnostic-actionable.
        let panic_msg = if let Some(s) = panic_payload.downcast_ref::<&str>() {
            (*s).to_string()
        } else if let Some(s) = panic_payload.downcast_ref::<String>() {
            s.clone()
        } else if let Some(e) =
            panic_payload.downcast_ref::<Box<dyn std::error::Error + Send + Sync>>()
        {
            e.to_string()
        } else if let Some(e) = panic_payload.downcast_ref::<std::io::Error>() {
            e.to_string()
        } else {
            // No `panic_payload.type_id()` call — raw TypeId values are
            // opaque hex without a known type registry, so the WARN log
            // line below (with filename + face index) is more
            // actionable than printing the type id. The downcast
            // cascade above covers the panic shapes fontcull is known
            // to produce; this arm is the structural fallback for
            // "fontcull added a panic type we don't handle". A future
            // need for finer panic-type diagnosis would install a
            // `panic::set_hook` to capture `PanicInfo::location` /
            // `PanicInfo::message`, not chase the unboxed type here.
            "fontcull panicked with unknown payload type".to_string()
        };
        log::warn!(
            "fontcull panicked while subsetting '{filename}' (face {font_index}): {panic_msg}"
        );
        Err(format!("Subset panic: {panic_msg}"))
    });

    match subset_result {
        Ok(subsetted) => {
            log::info!(
                "Subsetted '{}' (face {}): {} → {} bytes ({} codepoints)",
                filename,
                font_index,
                font_data.len(),
                subsetted.len(),
                all_codepoints.len()
            );
            Ok(subsetted)
        }
        Err(e) => {
            // on subsetting
            // failure, return Err instead of falling back to raw
            // `font_data`. The fallback path was a corner-case
            // accommodation for "corrupt fonts in user-trusted dirs"
            // — but it also turned every readable local file with
            // an allowed font extension into a data-disclosure
            // primitive when paired with the cache provenance trust.
            // An attacker-supplied `--cache-file` (or a
            // tampered SQLite cache pointing `arial.ttf` at
            // `/etc/passwd.ttf`) could read arbitrary local files
            // and embed the raw bytes into the output ASS via this
            // fallback. Closing the disclosure primitive at the
            // subset layer is simpler than authenticating cache
            // rows on every lookup, and the cost is small: a font
            // that fontcull cannot subset is reported as failed-
            // to-embed in the log, and the user re-picks (the same
            // outcome as a missing font, which is already a known
            // workflow). The per-file `MAX_FONT_FALLBACK_SIZE` and
            // cumulative `CUMULATIVE_FALLBACK_BYTES` budgets that
            // used to bound this path are also gone — see the
            // module-top constants block.
            log::warn!(
                "Subsetting failed for '{}' (face {}): {} — embed will skip this font because unsafe full-font fallback is disabled",
                filename,
                font_index,
                e,
            );
            Err(format!("Subsetting failed: {e}"))
        }
    }
}

/// Returns true if `font_data` begins with the **TrueType / OpenType
/// Collection** magic. A TTC packs N faces (each with its own SFNT
/// header) inside one file behind a `ttcf` header + offset table;
/// any face — including index 0 — must be sliced out via
/// `FontRef::from_index` before subsetting, because fontcull's
/// single-face entry point expects SFNT magic at offset 0 and
/// rejects the `ttcf` container with `InvalidSfnt(0x74746366)`.
///
/// `typ1` is Apple's PostScript Type 1 collection — not supported
/// by fontcull / skrifa and rejected upstream by the magic-byte
/// sniff (the recognized-signature gate before this is reached),
/// so it's intentionally absent from this check.
pub(super) fn is_ttc_data(font_data: &[u8]) -> bool {
    font_data.starts_with(b"ttcf")
}

/// Subset a single face from a font file. Handles both single-face
/// SFNT (`.ttf` / `.otf`, callers pass index 0) and TTC collections
/// (callers pass the requested face index). The plan tuning below
/// applies to BOTH paths — single-face and TTC subsets come out the
/// same shape.
///
/// Plan tuning rationale (from a measured comparison against
/// assfonts/AssFontSubset on the same input):
///
/// - **Drop `vmtx`, `LTSH`, `kern`.** `vmtx` (vertical metrics) is
///   ~60 KB per CJK face — one record per `maxp.numGlyphs`, but
///   subtitle rendering is horizontal (libass doesn't consult `vmtx`
///   for ASS \fr* tags). `LTSH` (Linear ThreshHold) is a Win9x
///   screen-rendering optimization modern engines ignore; ~30 KB per
///   CJK face. `kern` is the legacy kerning table superseded by GPOS;
///   our subset doesn't trim per-glyph kerning so the kept table is
///   dead weight. assfonts (libharfbuzz-subset defaults) and
///   AssFontSubset (pyftsubset defaults) both drop these.
/// - **Keep ALL name-table records (all name IDs, all languages).**
///   Pre-fix, our subset had a 6-byte `name` stub — libass could
///   only match by filename heuristic, which fails for non-ASCII
///   alternative family names (`微软雅黑` hashes to `font_<hex>.ttf`
///   under our naming, with no fallback). Preserving the full name
///   table lets libass match every localized family name in the
///   original font, which in turn lets the TS embed layer dedup
///   aliases that resolve to the same face — without dedup, the
///   same face would be subset and embedded once per family alias.
pub(super) fn is_gpos_subset_error(error: &fontcull_klippa::SubsetError) -> bool {
    matches!(
        error,
        fontcull_klippa::SubsetError::SubsetTableError(tag)
            if *tag == fontcull_skrifa::Tag::new(b"GPOS")
    )
}

pub(super) fn subset_with_index(
    font_data: &[u8],
    index: u32,
    codepoints: &[u32],
) -> Result<Vec<u8>, String> {
    use fontcull_klippa::{subset_font, Plan, SubsetError, SubsetFlags};
    use fontcull_read_fonts::collections::IntSet;
    use fontcull_skrifa::{FontRef, GlyphId, Tag};
    use fontcull_write_fonts::types::NameId;

    // Defense-in-depth: if the file is a TTC, peek `numFonts` at the
    // documented offset 8..=11 (TTCHeader version 1.0/2.0, ULONG
    // big-endian) and reject `index >= numFonts` upfront.
    // `MAX_SUBSET_FONT_INDEX = 255` at the IPC layer bounds JS-supplied
    // indices, but internal callers (chain's
    // resolve_chain_embed_subsets, embed batch) reach here via face
    // indices recorded in the session DB / cache rows, where the
    // upper bound is `MAX_TTC_FACES = 16` at scan time. This peek is
    // a structural defense inside `subset_with_index` itself so the
    // contract holds regardless of caller — a crafted TTC with a
    // declared `numFonts` huge enough to OOM-pressure
    // `FontRef::from_index`'s offset-table walk now bounces out
    // before fontcull's parser allocates anything. The catch_unwind
    // upstream caught panics; this peek closes the silent-OOM-then-
    // InvalidSfnt path the panic guard cannot.
    if is_ttc_data(font_data) {
        if font_data.len() < 12 {
            return Err(format!(
                "Truncated TTC header: {} bytes (need 12 for numFonts)",
                font_data.len()
            ));
        }
        let num_fonts =
            u32::from_be_bytes([font_data[8], font_data[9], font_data[10], font_data[11]]);
        if index >= num_fonts {
            return Err(format!(
                "TTC face index out of range: requested {index}, file declares {num_fonts} face(s)"
            ));
        }
    }

    let (font_data, sanitized_cmap_groups) = sanitize_cmap12_invalid_groups(font_data, index)?;
    if sanitized_cmap_groups > 0 {
        log::warn!(
            "Ignored {sanitized_cmap_groups} invalid cmap format-12 group(s) outside Unicode range while subsetting face {index}"
        );
    }
    let font_data = font_data.as_ref();

    // Display, not Debug — Debug repr leaks internal struct fields,
    // table tags, byte offsets into a frontend-visible error.
    let font = FontRef::from_index(font_data, index)
        .map_err(|e| format!("Cannot parse font face {index}: {e}"))?;

    let mut unicode_set: IntSet<u32> = IntSet::empty();
    for &cp in codepoints {
        unicode_set.insert(cp);
    }

    // Preserve the full name-table — every name ID, every language.
    // `IntSet::all()` is the inverse of `IntSet::empty()`; harfbuzz-
    // subset's SETS convention treats `all()` as "retain everything",
    // matching assfonts' explicit `HB_SUBSET_SETS_NAME_LANG_ID` inversion.
    let name_ids: IntSet<NameId> = IntSet::all();
    let langs: IntSet<u16> = IntSet::all();

    let empty_gids: IntSet<GlyphId> = IntSet::empty();
    let layout_scripts: IntSet<Tag> = IntSet::all();
    // Required features may use tags outside the defaults; retain their glyph closure too.
    let layout_features: IntSet<Tag> = IntSet::all();

    let subset_once = |drop_gpos: bool| {
        // Drop the three "dead in subtitle rendering" tables. The set
        // contains exactly the tags we want fontcull to omit from the
        // output font. GPOS is kept on the first attempt for shaping
        // quality, then dropped only as a compatibility retry for
        // legacy CJK fonts whose positioning table makes fontcull fail.
        let mut drop_tables: IntSet<Tag> = IntSet::empty();
        drop_tables.insert(Tag::new(b"vmtx"));
        drop_tables.insert(Tag::new(b"LTSH"));
        drop_tables.insert(Tag::new(b"kern"));
        if drop_gpos {
            drop_tables.insert(Tag::new(b"GPOS"));
        }

        let plan = Plan::new(
            &empty_gids,
            &unicode_set,
            &font,
            SubsetFlags::default(),
            &drop_tables,
            &layout_scripts,
            &layout_features,
            &name_ids,
            &langs,
        );

        subset_font(&font, &plan)
    };

    let format_subset_error =
        |error: &SubsetError| format!("Subset failed for face {index}: {error}");

    match subset_once(false) {
        Ok(subsetted) => Ok(subsetted),
        Err(error) if is_gpos_subset_error(&error) => {
            let initial_error = format_subset_error(&error);
            log::warn!(
                "Subsetting face {index} failed on GPOS; retrying with the GPOS table dropped"
            );
            subset_once(true).map_err(|fallback_error| {
                format!(
                    "{initial_error}; retry without GPOS also failed: {}",
                    format_subset_error(&fallback_error)
                )
            })
        }
        Err(error) => Err(format_subset_error(&error)),
    }
}
