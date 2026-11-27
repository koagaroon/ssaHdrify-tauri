//! `refresh-fonts` command, persistent CLI font-cache opening/drift checks,
//! and temporary CLI session font-index lifecycle.

use super::*;
use std::collections::HashSet;
use std::fs;
use std::io::ErrorKind;
use std::path::Path;
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::SystemTime;
use std::time::UNIX_EPOCH;

pub(super) fn refresh_font_directory_sources(
    args: &RefreshFontsArgs,
) -> impl Iterator<Item = (&Path, app_lib::fonts::FontDirectoryScope, &'static str)> {
    args.recursive_font_dirs
        .iter()
        .map(|path| {
            (
                path.as_path(),
                app_lib::fonts::FontDirectoryScope::Recursive,
                "--recursive-font-dir",
            )
        })
        .chain(args.font_dirs.iter().map(|path| {
            (
                path.as_path(),
                app_lib::fonts::FontDirectoryScope::Shallow,
                "--font-dir",
            )
        }))
}

pub(super) struct PreparedFontCache {
    pub(super) cache: Option<app_lib::font_cache::FontCache>,
    pub(super) diagnostic: CacheDiagnostic,
}

pub(super) struct TempFontDbDir(pub(super) PathBuf);

impl Drop for TempFontDbDir {
    fn drop(&mut self) {
        app_lib::fonts::clear_user_font_db_path();
        // Pattern-over-enumeration : a future SQLite
        // version adding a new sidecar suffix would leak files past
        // the suffix list. The dir is owned exclusively by this temp
        // (CLI_FONT_DB_DIR_PREFIX + random suffix), so `remove_dir_all`
        // is strictly more correct than the per-suffix enumeration and
        // cleans up any unexpected sidecar in one go.
        let _ = fs::remove_dir_all(&self.0);
    }
}

pub(super) fn validate_cache_file_arg(globals: &GlobalOptions) -> Result<(), String> {
    if let Some(ref cache_file) = globals.cache_file {
        let cache_str = cache_file.to_str().ok_or_else(|| {
            "--cache-file: path contains non-UTF-8 bytes; refuse upfront so the IPC \
             validator and the subsequent SQLite open agree on the same byte sequence"
                .to_string()
        })?;
        app_lib::util::validate_ipc_path(cache_str, "--cache-file")?;
    }
    Ok(())
}

pub(super) fn format_refresh_cache_schema_error(
    cache_path: &Path,
    found: i32,
    expected: i32,
) -> String {
    let cache_path_disp = sanitize_for_display(&cache_path.to_string_lossy());
    let schema_detail = match found {
        -1 => "has no readable schema-version record".to_string(),
        -2 => "has an unparseable schema-version record".to_string(),
        version => format!("has schema version {version}"),
    };
    format!(
        "Cache at {} {schema_detail}, but this CLI requires schema version {expected}.\n\
         The cache is incompatible with this release and must be rebuilt.\n\
         Delete the cache file at {}, then rerun the same `refresh-fonts` command.",
        cache_path_disp, cache_path_disp,
    )
}

pub(super) fn format_refresh_cache_access_error(
    cache_path: &Path,
    action: &str,
    error: app_lib::font_cache::CacheError,
) -> String {
    let cache_path_disp = sanitize_for_display(&cache_path.to_string_lossy());
    match error {
        app_lib::font_cache::CacheError::InvalidDatabase(detail) => format!(
            "The font cache at {} is invalid or corrupt: {detail}.\n\
             Delete the cache file at {}, then rerun the same `refresh-fonts` command to rebuild it.",
            cache_path_disp,
            cache_path_disp,
        ),
        other => format!(
            "{action} the font cache at {} failed: {other}. Check the cache path and file permissions; the cache was not deleted.",
            cache_path_disp,
        ),
    }
}

pub(super) fn run_refresh_fonts(
    globals: &GlobalOptions,
    args: RefreshFontsArgs,
) -> Result<ExitCode, String> {
    // refresh-fonts's whole purpose is to write to the cache. Running
    // with --no-cache is contradictory; surface as a clear error
    // rather than silently doing nothing (per no-silent-action).
    if globals.no_cache {
        return Err("refresh-fonts requires the cache; --no-cache contradicts \
             this subcommand's purpose. Remove --no-cache or use a \
             different subcommand."
            .to_string());
    }

    // This subcommand has no JSON report contract. Hard-fail even under
    // --quiet so a machine consumer never receives an exit-0 empty/plain-text
    // response after explicitly asking for JSON.
    if globals.json {
        return Err(
            "refresh-fonts does not implement --json output; remove --json and use its stderr status report"
                .to_string(),
        );
    }

    // --output-dir / --overwrite / --fail-fast affect output-file writing,
    // while --verbose has no additional refresh detail to enable;
    // refresh-fonts writes only to the cache, so they're inert here. Surface
    // a notice rather than ignoring them silently (no-silent-action). Unlike
    // --no-cache (contradictory → hard error) these are harmless, so a
    // non-fatal stderr line is the right level. --cache-file / --no-cache are
    // the documented handled/bounded-ignore flags and stay excluded.
    if !globals.quiet {
        let mut inert: Vec<&str> = Vec::new();
        if globals.output_dir.is_some() {
            inert.push("--output-dir");
        }
        if globals.overwrite {
            inert.push("--overwrite");
        }
        if globals.fail_fast {
            inert.push("--fail-fast");
        }
        if globals.verbose {
            inert.push("--verbose");
        }
        if !inert.is_empty() {
            let flags = inert.join(", ");
            eprintln!(
                "{}",
                localize(
                    globals,
                    format!(
                        "ℹ refresh-fonts writes only to the cache; these flags have no effect here: {flags}"
                    ),
                    format!("ℹ refresh-fonts 仅写入缓存；以下参数在此无效：{flags}"),
                )
            );
        }
    }

    validate_cache_file_arg(globals)?;

    // Validate each directory-source argv early —
    // fail-fast before opening the cache or starting any scan. The
    // downstream scan_directory_collecting now validates too (defense
    // in depth), but the early check produces a cleaner per-arg
    // error message at the right level. `--cache-file` (also argv
    // untrusted-input) was normalized before dispatch and is validated above.
    for (dir, _scope, flag_name) in refresh_font_directory_sources(&args) {
        // Refuse non-UTF-8 paths upfront via `to_str()` rather than
        // routing `to_string_lossy()` through the validator. With
        // lossy substitution, WTF-16-surrogate / non-UTF-8 bytes
        // become U+FFFD for the validate call while downstream scan /
        // cache writes consume the lossless normalized PathBuf — different
        // bytes than what validate_ipc_path checked. Sibling pattern to the
        // `--cache-file` refusal above.
        let normalized_dir = absolute_path(dir)?;
        let dir_str = normalized_dir.to_str().ok_or_else(|| {
            format!(
                "{flag_name}: path contains non-UTF-8 bytes; refuse upfront so the IPC \
                 validator and the subsequent scan / cache write agree on the same \
                 byte sequence"
            )
        })?;
        app_lib::util::validate_ipc_path(dir_str, flag_name)?;
    }

    // Resolve cache file path: user override (--cache-file) or default
    // Windows path. Both come from globals now (they're global flags).
    let cache_path = match &globals.cache_file {
        Some(p) => p.clone(),
        None => app_lib::font_cache::default_cli_cache_path()?,
    };

    let source_count = args.font_dirs.len() + args.recursive_font_dirs.len();
    let mut canonical_font_dirs: Vec<(
        PathBuf,
        String,
        app_lib::font_cache::CacheSourceKey,
        &'static str,
    )> = Vec::with_capacity(source_count);
    let mut final_source_keys: HashSet<app_lib::font_cache::CacheSourceKey> = HashSet::new();
    for (dir, scope, flag_name) in refresh_font_directory_sources(&args) {
        let abs_dir = absolute_path(dir)?;
        match app_lib::util::try_is_reparse_point(&abs_dir) {
            Ok(false) => {}
            Ok(true) => {
                return Err(format!(
                    "{flag_name}: refusing a symlink, junction, or reparse-point root: {}",
                    sanitize_for_display(&abs_dir.display().to_string())
                ));
            }
            Err(error) => {
                return Err(format!(
                    "{flag_name}: cannot verify that the source root is not a reparse point ({}): {error}",
                    sanitize_for_display(&abs_dir.display().to_string())
                ));
            }
        }
        let canonical = abs_dir
            .canonicalize()
            .map_err(|e| format!("cannot canonicalize {}: {e}", abs_dir.display()))?;
        let folder_path_str = display_path(&canonical);
        let source_key = app_lib::font_cache::cache_source_key(&canonical, scope)?;
        final_source_keys.insert(source_key.clone());
        canonical_font_dirs.push((canonical, folder_path_str, source_key, flag_name));
    }
    if final_source_keys.len() > app_lib::font_cache::MAX_CACHED_SOURCES {
        return Err(format!(
            "refresh-fonts would track {} unique sources, exceeding the {}-source cache sanity cap. Reduce --font-dir / --recursive-font-dir inputs and rebuild a smaller cache.",
            final_source_keys.len(),
            app_lib::font_cache::MAX_CACHED_SOURCES
        ));
    }

    // --dry-run previews planned work without touching persisted state,
    // matching the cheap-first dry-run contract of the other subcommands:
    // gate BEFORE the expensive scan and mutate nothing. The existing cache
    // is read READ-ONLY (no file is created when absent), so the folder-cap
    // preview reflects reality without the open_or_create side effect.
    if globals.dry_run {
        if cache_path.exists() {
            match app_lib::font_cache::FontCache::open_existing_read_only(&cache_path) {
                Ok(existing) => {
                    for source in existing.list_sources().map_err(|error| {
                        format_refresh_cache_access_error(&cache_path, "Reading", error)
                    })? {
                        final_source_keys.insert(source.key());
                    }
                }
                Err(app_lib::font_cache::CacheError::SchemaVersionMismatch { found, expected }) => {
                    return Err(format_refresh_cache_schema_error(
                        &cache_path,
                        found,
                        expected,
                    ));
                }
                Err(error) => {
                    return Err(format_refresh_cache_access_error(
                        &cache_path,
                        "Opening",
                        error,
                    ));
                }
            }
        }
        if final_source_keys.len() > app_lib::font_cache::MAX_CACHED_SOURCES {
            return Err(format!(
                "refresh-fonts would track {} total cached sources, exceeding the {}-source cache sanity cap. Delete the cache file or rebuild it with fewer directory sources.",
                final_source_keys.len(),
                app_lib::font_cache::MAX_CACHED_SOURCES
            ));
        }
        if !globals.quiet {
            let n = source_count;
            eprintln!(
                "{}",
                localize(
                    globals,
                    format!(
                        "ℹ Dry run: would scan {n} source root{} and update the cache. No changes written:",
                        s_if(n)
                    ),
                    format!("ℹ 预演：将扫描 {n} 个源根目录并更新缓存。未写入任何更改："),
                )
            );
            for (dir, _scope, flag_name) in refresh_font_directory_sources(&args) {
                let dir_disp = sanitize_for_display(&dir.to_string_lossy());
                eprintln!("    {flag_name} {dir_disp}");
            }
            let cache_disp = sanitize_for_display(&cache_path.display().to_string());
            eprintln!(
                "{}",
                localize(
                    globals,
                    format!("  Cache file: {cache_disp}"),
                    format!("  缓存文件：{cache_disp}"),
                )
            );
        }
        return Ok(ExitCode::SUCCESS);
    }

    if !globals.quiet {
        // refresh-fonts stderr now flows through
        // `localize()` for the en/zh switch, matching every other CLI
        // subcommand. Previously refresh-fonts was English-only — a
        // Chinese-locale user saw localized status lines from
        // hdr / shift / embed / rename / chain but English from this
        // subcommand. Cache file path interpolation stays sanitized via
        // sanitize_for_display per the existing print-boundary contract.
        let n = source_count;
        eprintln!(
            "{}",
            localize(
                globals,
                format!("ℹ Refreshing cache. Scanning {n} source root{}:", s_if(n)),
                format!("ℹ 正在刷新缓存。扫描 {n} 个源根目录："),
            )
        );
        for (dir, _scope, flag_name) in refresh_font_directory_sources(&args) {
            // Sanitize the per-source-root header print (argv is now
            // validate_ipc_path-clean above, but double-sanitize on
            // already-clean strings is a no-op and matches every other
            // refresh-fonts print site).
            let dir_disp = sanitize_for_display(&dir.to_string_lossy());
            eprintln!("    {flag_name} {dir_disp}");
        }
    }

    // Open or create cache. Schema version mismatch on existing file
    // is treated as drift-equivalent: tell user, suggest rebuild via
    // wiping the file. Per the no-silent-action principle, we don't
    // auto-delete.
    let mut cache = match app_lib::font_cache::FontCache::open_or_create(&cache_path) {
        Ok(c) => c,
        Err(app_lib::font_cache::CacheError::SchemaVersionMismatch { found, expected }) => {
            return Err(format_refresh_cache_schema_error(
                &cache_path,
                found,
                expected,
            ));
        }
        Err(error) => {
            return Err(format_refresh_cache_access_error(
                &cache_path,
                "Opening",
                error,
            ));
        }
    };

    for source in cache
        .list_sources()
        .map_err(|error| format_refresh_cache_access_error(&cache_path, "Reading", error))?
    {
        final_source_keys.insert(source.key());
    }
    if final_source_keys.len() > app_lib::font_cache::MAX_CACHED_SOURCES {
        return Err(format!(
            "refresh-fonts would track {} total cached sources, exceeding the {}-source cache sanity cap. Delete the cache file or rebuild it with fewer directory sources.",
            final_source_keys.len(),
            app_lib::font_cache::MAX_CACHED_SOURCES
        ));
    }

    let mut total_fonts: usize = 0;
    let mut total_sources: usize = 0;

    for (canonical, folder_path_str, source_key, flag_name) in &canonical_font_dirs {
        // Capture every directory and candidate font file before parsing.
        // Recursive sources need the nested snapshot because a root folder's
        // own mtime does not reliably change when a grandchild changes.
        let source_snapshot = match app_lib::font_cache::snapshot_source_directories(
            canonical,
            source_key.scope,
        ) {
            Ok(snapshot) => snapshot,
            Err(err) => {
                cache
                        .remove_source(&source_key.source_root, source_key.scope)
                        .map_err(|remove_error| {
                            let folder_disp = sanitize_for_display(folder_path_str);
                            localize(
                                globals,
                                format!(
                                    "{flag_name} {folder_disp} failed and its stale cache row could not be evicted: {remove_error}"
                                ),
                                format!(
                                    "{flag_name} {folder_disp} 失败，且无法清除其过期缓存记录：{remove_error}"
                                ),
                            )
                        })?;
                if !globals.quiet {
                    let folder_disp = sanitize_for_display(folder_path_str);
                    let err_disp = sanitize_for_display(&err);
                    eprintln!(
                        "{}",
                        localize(
                            globals,
                            format!("  ⚠ {flag_name} {folder_disp}: skipped — {err_disp}"),
                            format!("  ⚠ {flag_name} {folder_disp}：已跳过——{err_disp}"),
                        )
                    );
                }
                continue;
            }
        };

        // Scan with the explicit per-source scope. Per-source errors
        // are logged and skipped so one bad source doesn't abort the
        // whole refresh run — refresh-fonts is multi-source by design.
        let entries = match app_lib::fonts::scan_directory_collecting_with_scope(
            canonical,
            source_key.scope,
        ) {
            Ok(e) => e,
            Err(err) => {
                cache
                    .remove_source(&source_key.source_root, source_key.scope)
                    .map_err(|remove_error| {
                        let folder_disp = sanitize_for_display(folder_path_str);
                        localize(
                            globals,
                            format!(
                                "{flag_name} {folder_disp} failed and its stale cache row could not be evicted: {remove_error}"
                            ),
                            format!(
                                "{flag_name} {folder_disp} 失败，且无法清除其过期缓存记录：{remove_error}"
                            ),
                        )
                    })?;
                if !globals.quiet {
                    let folder_disp = sanitize_for_display(folder_path_str);
                    let err_disp = sanitize_for_display(&err);
                    eprintln!(
                        "{}",
                        localize(
                            globals,
                            format!("  ⚠ {flag_name} {folder_disp}: skipped — {err_disp}"),
                            format!("  ⚠ {flag_name} {folder_disp}：已跳过——{err_disp}"),
                        )
                    );
                }
                continue;
            }
        };

        // Shared GUI/CLI helper: per-file mtime dedup (TTC files
        // contribute multiple entries with one path) + saturating cast
        // discipline + family-key flattening.
        let metadata: Vec<app_lib::font_cache::FontMetadata> =
            match app_lib::fonts::entries_to_cache_metadata(&entries) {
                Ok(metadata) => metadata,
                Err(err) => {
                    cache
                        .remove_source(&source_key.source_root, source_key.scope)
                        .map_err(|remove_error| {
                            let folder_disp = sanitize_for_display(folder_path_str);
                            localize(
                                globals,
                                format!(
                                    "{flag_name} {folder_disp} became unreadable and its stale cache row could not be evicted: {remove_error}"
                                ),
                                format!(
                                    "{flag_name} {folder_disp} 变得无法读取，且无法清除其过期缓存记录：{remove_error}"
                                ),
                            )
                        })?;
                    if !globals.quiet {
                        let folder_disp = sanitize_for_display(folder_path_str);
                        let err_disp = sanitize_for_display(&err);
                        eprintln!(
                            "{}",
                            localize(
                                globals,
                                format!("  ⚠ {flag_name} {folder_disp}: skipped — {err_disp}"),
                                format!("  ⚠ {flag_name} {folder_disp}：已跳过——{err_disp}"),
                            )
                        );
                    }
                    continue;
                }
            };

        if metadata.is_empty() {
            cache
                .remove_source(&source_key.source_root, source_key.scope)
                .map_err(|error| {
                    localize(
                        globals,
                        format!("evict empty font source cache: {error}"),
                        format!("清除空字体来源缓存失败：{error}"),
                    )
                })?;
            if !globals.quiet {
                let folder_disp = sanitize_for_display(folder_path_str);
                eprintln!(
                    "{}",
                    localize(
                        globals,
                        format!(
                            "  ⚠ {flag_name} {folder_disp}: skipped — no readable font faces found"
                        ),
                        format!("  ⚠ {flag_name} {folder_disp}：已跳过——未找到可读取的字体面"),
                    )
                );
            }
            continue;
        }

        let font_count = metadata.len();
        if let Err(error) =
            app_lib::font_cache::validate_cache_source_stability(&source_snapshot, &metadata)
        {
            cache
                .remove_source(&source_key.source_root, source_key.scope)
                .map_err(|remove_error| {
                    let folder_disp = sanitize_for_display(folder_path_str);
                    localize(
                        globals,
                        format!(
                            "validating cache source {folder_disp} failed ({error}) and stale-row eviction also failed: {remove_error}"
                        ),
                        format!(
                            "验证缓存字体来源 {folder_disp} 失败（{error}），且清除过期记录也失败：{remove_error}"
                        ),
                    )
                })?;
            if !globals.quiet {
                let folder_disp = sanitize_for_display(folder_path_str);
                let error_disp = sanitize_for_display(&error.to_string());
                eprintln!(
                    "{}",
                    localize(
                        globals,
                        format!("  ⚠ {flag_name} {folder_disp}: skipped — {error_disp}"),
                        format!("  ⚠ {flag_name} {folder_disp}：已跳过——{error_disp}"),
                    )
                );
            }
            continue;
        }
        if let Err(error) = cache.replace_source(&source_snapshot, &metadata) {
            cache
                .remove_source(&source_key.source_root, source_key.scope)
                .map_err(|remove_error| {
                    let folder_disp = sanitize_for_display(folder_path_str);
                    localize(
                        globals,
                        format!(
                            "writing cache for {folder_disp} failed ({error}) and stale-row eviction also failed: {remove_error}"
                        ),
                        format!(
                            "写入 {folder_disp} 的缓存失败（{error}），且清除过期记录也失败：{remove_error}"
                        ),
                    )
                })?;
            if !globals.quiet {
                let folder_disp = sanitize_for_display(folder_path_str);
                let error_disp = sanitize_for_display(&error.to_string());
                eprintln!(
                    "{}",
                    localize(
                        globals,
                        format!("  ⚠ {flag_name} {folder_disp}: skipped — {error_disp}"),
                        format!("  ⚠ {flag_name} {folder_disp}：已跳过——{error_disp}"),
                    )
                );
            }
            continue;
        }

        if !globals.quiet {
            // sanitize_for_display on the success line too — same
            // callsite shape as the error path above, then localize.
            let folder_disp = sanitize_for_display(folder_path_str);
            eprintln!(
                "{}",
                localize(
                    globals,
                    format!(
                        "  ✓ {flag_name} {folder_disp}: indexed {font_count} font face{}",
                        s_if(font_count)
                    ),
                    format!("  ✓ {flag_name} {folder_disp}：已索引 {font_count} 个字体面"),
                )
            );
        }
        total_fonts += font_count;
        total_sources += 1;
    }

    if total_sources == 0 {
        return Err(localize(
            globals,
            format!(
                "refresh-fonts could not index any font source; all {source_count} directory-source argument{} were skipped. Pass at least one readable --font-dir or --recursive-font-dir.",
                s_if(source_count),
            ),
            format!(
                "refresh-fonts 未能索引任何字体来源；{source_count} 个目录来源参数均已跳过。请至少传入一个可读取的 --font-dir 或 --recursive-font-dir。"
            ),
        ));
    }

    if !globals.quiet {
        // localize.
        eprintln!(
            "{}",
            localize(
                globals,
                format!(
                    "✓ Cache updated: {total_fonts} font face{} indexed across {total_sources} font source{}.",
                    s_if(total_fonts),
                    s_if(total_sources)
                ),
                format!(
                    "✓ 缓存已更新：在 {total_sources} 个字体来源中索引了 {total_fonts} 个字体面。"
                ),
            )
        );
        // cache_path.display() is also sanitized — `cache_path` comes
        // from globals.cache_file (argv untrusted-input) or default_cli_cache_path
        // (env-var-resolved).
        let cache_disp = sanitize_for_display(&cache_path.display().to_string());
        eprintln!(
            "{}",
            localize(
                globals,
                format!("  Cache file: {cache_disp}"),
                format!("  缓存文件：{cache_disp}"),
            )
        );
    }

    Ok(ExitCode::SUCCESS)
}

pub(super) fn prepare_font_cache_for_resolution(
    globals: &GlobalOptions,
    args: &EmbedArgs,
    skip_for_dry_run: bool,
    read_only: bool,
) -> Result<PreparedFontCache, String> {
    if globals.no_cache {
        if !globals.quiet {
            // Route through localize() like every sibling cache line — a bare
            // eprintln! rendered English under --lang zh while the rest of the
            // cache output was Chinese.
            eprintln!(
                "{}",
                localize(
                    globals,
                    "ℹ Cache disabled (--no-cache). Using explicit font sources / system fonts only."
                        .to_string(),
                    "ℹ 已禁用缓存（--no-cache）。仅使用显式字体源 / 系统字体。".to_string(),
                )
            );
        }
        return Ok(PreparedFontCache {
            cache: None,
            diagnostic: CacheDiagnostic::new(
                globals.cache_file.as_ref().map(|path| display_path(path)),
                CacheDiagnosticStatus::Disabled,
                Some("disabled by --no-cache".to_string()),
            ),
        });
    }

    if skip_for_dry_run && globals.dry_run {
        return Ok(PreparedFontCache {
            cache: None,
            diagnostic: CacheDiagnostic::new(
                globals.cache_file.as_ref().map(|path| display_path(path)),
                CacheDiagnosticStatus::DryRun,
                Some("skipped because --dry-run does not resolve fonts".to_string()),
            ),
        });
    }

    validate_cache_file_arg(globals)?;
    Ok(prepare_embed_cache(globals, args, read_only))
}

/// Resolve cache path, open the cache, detect drift, announce status
/// to stderr per the locked transparency design, and return
/// `Some(cache)` if usable for this run, or `None` to fall back to
/// no-cache mode.
///
/// `read_only` is true for `diagnose-fonts` and `chain`, which inspect an
/// existing cache without creating schema or changing SQLite journal
/// mode. `embed` uses the normal opener, matching the historical cache
/// behavior. Cache content writes remain exclusive to `refresh-fonts`.
pub(super) fn prepare_embed_cache(
    globals: &GlobalOptions,
    args: &EmbedArgs,
    read_only: bool,
) -> PreparedFontCache {
    // Resolve path: --cache-file override or default Windows path.
    let cache_path = match &globals.cache_file {
        Some(p) => p.clone(),
        None => match app_lib::font_cache::default_cli_cache_path() {
            Ok(p) => p,
            Err(e) => {
                if !globals.quiet {
                    // Every prepare_embed_cache eprintln that
                    // interpolates `cache_path.display()` or a drift
                    // folder string (sourced from the SQLite cache,
                    // which is untrusted-input under --cache-file argv override)
                    // sanitizes at the print boundary. The error `e`
                    // from default_cli_cache_path can carry env-var
                    // resolution failure text that includes path
                    // fragments.
                    //
                    // Every prepare_embed_cache stderr line also
                    // routes through `localize` so cache-related
                    // output respects `--lang zh` for parity with the
                    // refresh-fonts and per-file ✓/⊘/✗ paths.
                    let e_disp = sanitize_for_display(&e);
                    eprintln!(
                        "{}",
                        localize(
                            globals,
                            format!("⚠ Cannot resolve cache path: {e_disp}"),
                            format!("⚠ 无法解析字体缓存路径：{e_disp}"),
                        )
                    );
                    eprintln!(
                        "{}",
                        localize(
                            globals,
                            "  Skipping cache for this run.".to_string(),
                            "  本次运行将跳过字体缓存。".to_string(),
                        )
                    );
                }
                return PreparedFontCache {
                    cache: None,
                    diagnostic: CacheDiagnostic::new(
                        None,
                        CacheDiagnosticStatus::PathError,
                        Some(format!("cannot resolve cache path: {e}")),
                    ),
                };
            }
        },
    };
    let cache_path_json = display_path(&cache_path);

    // Pre-compute the sanitized cache-path display once; reused at
    // every stderr site below. cache_path itself stays operational.
    let cache_path_disp = sanitize_for_display(&cache_path.display().to_string());

    if !cache_path.exists() {
        // No cache yet (first-ever invocation, or user wiped it).
        // Per locked design: distinct messaging from drift, same
        // behavior (skip cache + suggest refresh-fonts).
        if !globals.quiet {
            eprintln!(
                "{}",
                localize(
                    globals,
                    format!("ℹ No font cache exists yet at {cache_path_disp}."),
                    format!("ℹ 尚未在 {cache_path_disp} 建立字体缓存。"),
                )
            );
            eprintln!(
                "{}",
                localize(
                    globals,
                    "  Run `ssahdrify-cli refresh-fonts --font-dir <DIR>...` for flat folders or use repeatable `--recursive-font-dir <DIR>` for library trees.".to_string(),
                    "  运行 `ssahdrify-cli refresh-fonts --font-dir <DIR>...` 建立浅层目录缓存，或对字体库树使用可重复的 `--recursive-font-dir <DIR>`。".to_string(),
                )
            );
        }
        return PreparedFontCache {
            cache: None,
            diagnostic: CacheDiagnostic::new(
                Some(cache_path_json),
                CacheDiagnosticStatus::Missing,
                Some("no font cache exists yet".to_string()),
            ),
        };
    }

    let cache_result = if read_only {
        app_lib::font_cache::FontCache::open_existing_read_only(&cache_path)
    } else {
        app_lib::font_cache::FontCache::open_or_create(&cache_path)
    };
    let cache = match cache_result {
        Ok(c) => c,
        Err(app_lib::font_cache::CacheError::SchemaVersionMismatch { found, expected }) => {
            if !globals.quiet {
                eprintln!(
                    "{}",
                    localize(
                        globals,
                        format!(
                            "⚠ Font cache schema mismatch (found {found}, expected {expected})."
                        ),
                        format!("⚠ 字体缓存 schema 版本不匹配（发现 {found}，期望 {expected}）。"),
                    )
                );
                eprintln!(
                    "{}",
                    localize(
                        globals,
                        "  Cache is from a different release; skipping for this run.".to_string(),
                        "  缓存来自另一发行版本；本次运行将跳过。".to_string(),
                    )
                );
                eprintln!(
                    "{}",
                    localize(
                        globals,
                        format!("  Delete {cache_path_disp} and run `refresh-fonts` to rebuild."),
                        format!("  删除 {cache_path_disp} 后运行 `refresh-fonts` 重新建立缓存。"),
                    )
                );
            }
            let mut diagnostic = CacheDiagnostic::new(
                Some(cache_path_json),
                CacheDiagnosticStatus::SchemaMismatch,
                Some("font cache schema mismatch".to_string()),
            );
            diagnostic.found_schema = Some(i64::from(found));
            diagnostic.expected_schema = Some(i64::from(expected));
            return PreparedFontCache {
                cache: None,
                diagnostic,
            };
        }
        Err(app_lib::font_cache::CacheError::InvalidDatabase(detail)) => {
            if !globals.quiet {
                let detail_disp = sanitize_for_display(&detail);
                eprintln!(
                    "{}",
                    localize(
                        globals,
                        format!(
                            "⚠ Font cache at {cache_path_disp} is invalid or corrupt: {detail_disp}"
                        ),
                        format!("⚠ 位于 {cache_path_disp} 的字体缓存无效或已损坏：{detail_disp}"),
                    )
                );
                eprintln!(
                    "{}",
                    localize(
                        globals,
                        format!(
                            "  Delete {cache_path_disp}, then run `refresh-fonts` to rebuild it. Skipping cache for this run."
                        ),
                        format!(
                            "  删除 {cache_path_disp}，然后运行 `refresh-fonts` 以重建缓存。本次运行将跳过缓存。"
                        ),
                    )
                );
            }
            return PreparedFontCache {
                cache: None,
                diagnostic: CacheDiagnostic::new(
                    Some(cache_path_json),
                    CacheDiagnosticStatus::OpenError,
                    Some(format!("font cache is invalid or corrupt: {detail}")),
                ),
            };
        }
        Err(e) => {
            if !globals.quiet {
                let e_disp = sanitize_for_display(&e.to_string());
                eprintln!(
                    "{}",
                    localize(
                        globals,
                        format!("⚠ Cannot open font cache at {cache_path_disp}: {e_disp}"),
                        format!("⚠ 无法打开位于 {cache_path_disp} 的字体缓存：{e_disp}"),
                    )
                );
                eprintln!(
                    "{}",
                    localize(
                        globals,
                        "  Skipping cache for this run.".to_string(),
                        "  本次运行将跳过字体缓存。".to_string(),
                    )
                );
            }
            return PreparedFontCache {
                cache: None,
                diagnostic: CacheDiagnostic::new(
                    Some(cache_path_json),
                    CacheDiagnosticStatus::OpenError,
                    Some(format!("cannot open font cache at {cache_path_disp}: {e}")),
                ),
            };
        }
    };

    // Drift check: rebuild a metadata-only snapshot for every cached source.
    // Recursive sources include all visited real directories and candidate
    // font files, so nested changes are visible even when the root mtime is not.
    let drift = match check_cache_drift(&cache) {
        Ok(report) => report,
        Err(e) => {
            if !globals.quiet {
                let e_disp = sanitize_for_display(&e);
                eprintln!(
                    "{}",
                    localize(
                        globals,
                        format!("⚠ Cannot validate cache: {e_disp}"),
                        format!("⚠ 无法校验字体缓存：{e_disp}"),
                    )
                );
                eprintln!(
                    "{}",
                    localize(
                        globals,
                        "  Skipping cache for this run.".to_string(),
                        "  本次运行将跳过字体缓存。".to_string(),
                    )
                );
            }
            return PreparedFontCache {
                cache: None,
                diagnostic: CacheDiagnostic::new(
                    Some(cache_path_json),
                    CacheDiagnosticStatus::ValidationError,
                    Some(format!("cannot validate cache: {e}")),
                ),
            };
        }
    };

    if !drift.is_empty() {
        if !globals.quiet {
            let drift_count = drift.modified.len() + drift.removed.len();
            eprintln!(
                "{}",
                localize(
                    globals,
                    format!(
                        "⚠ Cache drift detected — {drift_count} source(s) changed since last refresh:"
                    ),
                    format!("⚠ 检测到字体缓存漂移 —— 自上次刷新以来 {drift_count} 个字体源已变化："),
                )
            );
            for source in &drift.modified {
                // Source roots originate from SQLite. Under --cache-file argv
                // override those rows are untrusted-input, so sanitize at the
                // terminal boundary and surface scope to disambiguate a root
                // cached both shallowly and recursively.
                let f_disp = format_cache_source_for_display(source);
                eprintln!(
                    "{}",
                    localize(
                        globals,
                        format!("    ~ {f_disp}  (modified)"),
                        format!("    ~ {f_disp}  （已修改）"),
                    )
                );
            }
            for source in &drift.removed {
                let f_disp = format_cache_source_for_display(source);
                eprintln!(
                    "{}",
                    localize(
                        globals,
                        format!("    - {f_disp}  (removed)"),
                        format!("    - {f_disp}  （已删除）"),
                    )
                );
            }
            eprintln!(
                "{}",
                localize(
                    globals,
                    "  Skipping cache for this run; using explicit font sources / system fonts only."
                        .to_string(),
                    "  本次运行将跳过缓存，仅使用显式字体源 / 系统字体。".to_string(),
                )
            );
            eprintln!(
                "{}",
                localize(
                    globals,
                    "  Run `refresh-fonts` to update the cache.".to_string(),
                    "  运行 `refresh-fonts` 更新缓存。".to_string(),
                )
            );
        }
        let mut diagnostic = CacheDiagnostic::new(
            Some(cache_path_json),
            CacheDiagnosticStatus::Drift,
            Some("cached font sources changed since last refresh".to_string()),
        );
        diagnostic.modified_folders = drift
            .modified
            .iter()
            .map(|source| source.source_root.clone())
            .collect();
        diagnostic.removed_folders = drift
            .removed
            .iter()
            .map(|source| source.source_root.clone())
            .collect();
        diagnostic.modified_sources = drift.modified.clone();
        diagnostic.removed_sources = drift.removed.clone();
        return PreparedFontCache {
            cache: None,
            diagnostic,
        };
    }

    // Cache is valid. Announce per locked transparency design:
    // Situation A (an explicit source provided) → "cache + sources"
    // announcement; Situation B (no explicit source) → implicit cache
    // use announcement.
    let user_supplied_sources = args.has_user_font_sources();
    if !globals.quiet {
        if user_supplied_sources {
            eprintln!(
                "{}",
                localize(
                    globals,
                    format!(
                        "ℹ Using font cache (at {cache_path_disp}) plus the --font-dir / --recursive-font-dir / --font-file paths you supplied."
                    ),
                    format!(
                        "ℹ 正在使用字体缓存（位于 {cache_path_disp}）以及你通过 --font-dir / --recursive-font-dir / --font-file 指定的路径。"
                    ),
                )
            );
        } else {
            eprintln!(
                "{}",
                localize(
                    globals,
                    format!("ℹ Using font cache (at {cache_path_disp})."),
                    format!("ℹ 正在使用字体缓存（位于 {cache_path_disp}）。"),
                )
            );
            eprintln!(
                "{}",
                localize(
                    globals,
                    "  Pass --no-cache to use system fonts only.".to_string(),
                    "  传 --no-cache 可改为仅使用系统字体。".to_string(),
                )
            );
        }
    }
    PreparedFontCache {
        cache: Some(cache),
        diagnostic: CacheDiagnostic::new(
            Some(cache_path_json),
            CacheDiagnosticStatus::Usable,
            Some("font cache usable for this run".to_string()),
        ),
    }
}

pub(super) fn format_cache_source_for_display(
    source: &app_lib::font_cache::CacheSourceKey,
) -> String {
    let root = sanitize_for_display(&source.source_root);
    let scope = match source.scope {
        app_lib::font_cache::FontDirectoryScope::Shallow => "shallow",
        app_lib::font_cache::FontDirectoryScope::Recursive => "recursive",
    };
    format!("{root} [{scope}]")
}

/// Rebuild the complete metadata snapshot for every cached source. A genuinely
/// missing or reparse-point root is removed. A real directory whose nested
/// snapshot cannot be completed is conservatively modified, matching the GUI
/// drift classifier. `added` remains empty because this check starts from
/// cache-owned roots only.
pub(super) fn check_cache_drift(
    cache: &app_lib::font_cache::FontCache,
) -> Result<app_lib::font_cache::DriftReport, String> {
    let cached_sources = cache
        .list_sources()
        .map_err(|e| format!("list cached font sources: {e}"))?;
    let inspection = app_lib::font_cache::inspect_cached_sources_live(&cached_sources);
    let mut report = cache
        .diff_sources(&inspection.snapshots)
        .map_err(|e| format!("compute source drift: {e}"))?;
    app_lib::font_cache::apply_live_source_inspection(&mut report, &inspection);
    Ok(report)
}

pub(super) fn init_cli_font_sources(
    globals: &GlobalOptions,
    args: &EmbedArgs,
) -> Result<TempFontDbDir, String> {
    // Wrap the temp dir in TempFontDbDir IMMEDIATELY so any `?` in the
    // init/import sequence below drops the guard and runs the cleanup.
    // The earlier shape (return Ok(TempFontDbDir(db_dir)) only at the
    // end) leaked the directory on every failure between create and
    // return.
    let guard = TempFontDbDir(create_cli_font_db_dir()?);
    app_lib::fonts::init_user_font_db(&guard.0)?;

    // IIFE so a single `?` in any import step routes to the cleanup
    // path below — the static `USER_FONT_DB_PATH` set by
    // `init_user_font_db` must be cleared alongside the temp dir wipe,
    // otherwise the slot points at a deleted file after the guard's
    // Drop runs (latent bug, no current caller retries, but the
    // helper makes the cleanup explicit).
    let import_result: Result<(), String> = (|| -> Result<(), String> {
        // Import by ascending source kind: recursive libraries first,
        // shallow folders second, explicit files last. The backend orders
        // lookup by source kind and then newest source order, giving explicit
        // files the strongest override while preserving deterministic
        // last-argument-wins behavior within each kind.
        for (index, dir) in args.recursive_font_dirs.iter().enumerate() {
            let dir = absolute_path(dir)?;
            let source_id = format!("cli-recursive-dir-{index}");
            let summary = app_lib::fonts::import_font_directory_for_cli_with_scope(
                &dir,
                &source_id,
                app_lib::fonts::FontDirectoryScope::Recursive,
            )?;
            require_complete_cli_font_source(
                globals,
                "recursive font library",
                "递归字体库",
                Some(&dir),
                &summary,
            )?;
            emit_font_source_summary(
                globals,
                "recursive font library",
                "递归字体库",
                Some(&dir),
                &summary,
            );
        }

        for (index, dir) in args.font_dirs.iter().enumerate() {
            let dir = absolute_path(dir)?;
            let source_id = format!("cli-dir-{index}");
            let summary = app_lib::fonts::import_font_directory_for_cli_with_scope(
                &dir,
                &source_id,
                app_lib::fonts::FontDirectoryScope::Shallow,
            )?;
            require_complete_cli_font_source(
                globals,
                "font dir",
                "字体目录",
                Some(&dir),
                &summary,
            )?;
            emit_font_source_summary(globals, "font dir", "字体目录", Some(&dir), &summary);
        }

        if !args.font_files.is_empty() {
            let paths: Result<Vec<String>, String> = args
                .font_files
                .iter()
                .map(|path| absolute_path(path).map(|path| display_path(&path)))
                .collect();
            let summary = app_lib::fonts::import_font_files_for_cli(paths?, "cli-files")?;
            require_complete_cli_font_source(globals, "font files", "字体文件", None, &summary)?;
            // Funnel through emit_font_source_summary so ScanStopReason
            // (UserCancel / CeilingHit) surfaces with the same suffix as
            // font-dir summaries. Path is None — font files are a flat
            // list without a single "source path."
            emit_font_source_summary(globals, "font files", "字体文件", None, &summary);
        }
        Ok(())
    })();

    match import_result {
        Ok(()) => Ok(guard),
        Err(e) => {
            app_lib::fonts::clear_user_font_db_path();
            // `guard` drops here, wiping the temp dir alongside the
            // path-slot reset above.
            Err(e)
        }
    }
}

pub(super) fn require_complete_cli_font_source(
    globals: &GlobalOptions,
    label_en: &str,
    label_zh: &str,
    path: Option<&Path>,
    summary: &app_lib::fonts::FontSourceImportSummary,
) -> Result<(), String> {
    if summary.reason == app_lib::fonts::ScanStopReason::Natural {
        return Ok(());
    }
    let reason_en = match summary.reason {
        app_lib::fonts::ScanStopReason::Natural => unreachable!(),
        app_lib::fonts::ScanStopReason::UserCancel => "was cancelled",
        app_lib::fonts::ScanStopReason::CeilingHit => "hit a safety ceiling",
        app_lib::fonts::ScanStopReason::IncompleteIo => {
            "encountered a changing or unreadable entry"
        }
    };
    let reason_zh = match summary.reason {
        app_lib::fonts::ScanStopReason::Natural => unreachable!(),
        app_lib::fonts::ScanStopReason::UserCancel => "已取消",
        app_lib::fonts::ScanStopReason::CeilingHit => "达到安全上限",
        app_lib::fonts::ScanStopReason::IncompleteIo => "遇到发生变化或无法读取的项目",
    };
    let path_en = path
        .map(|value| format!(" ({})", sanitize_for_display(&display_path(value))))
        .unwrap_or_default();
    let path_zh = path
        .map(|value| format!("（{}）", sanitize_for_display(&display_path(value))))
        .unwrap_or_default();
    Err(localize(
        globals,
        format!(
            "{label_en}{path_en} scan {reason_en}; partial font results were discarded. Retry with a smaller, stable, readable source."
        ),
        format!(
            "{label_zh}{path_zh}扫描{reason_zh}；已丢弃不完整的字体结果。请使用更小、稳定且可读取的来源后重试。"
        ),
    ))
}

pub(super) fn create_cli_font_db_dir() -> Result<PathBuf, String> {
    let base = std::env::temp_dir();
    let pid = std::process::id();
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();

    for attempt in 0..1000u16 {
        let candidate = base.join(format!("{CLI_FONT_DB_DIR_PREFIX}-{pid}-{stamp}-{attempt}"));
        match fs::create_dir(&candidate) {
            Ok(()) => return Ok(candidate),
            Err(error) if error.kind() == ErrorKind::AlreadyExists => continue,
            Err(error) => {
                return Err(format!(
                    "failed to create temporary font database directory: {error}"
                ));
            }
        }
    }

    Err("failed to allocate a unique temporary font database directory".to_string())
}

pub(super) fn emit_font_source_summary(
    globals: &GlobalOptions,
    label_en: &str,
    label_zh: &str,
    path: Option<&Path>,
    summary: &app_lib::fonts::FontSourceImportSummary,
) {
    if !globals.verbose || globals.json || globals.quiet {
        return;
    }
    let (reason_en, reason_zh) = match summary.reason {
        app_lib::fonts::ScanStopReason::Natural => ("", ""),
        app_lib::fonts::ScanStopReason::UserCancel => (" (cancelled)", "（已取消）"),
        app_lib::fonts::ScanStopReason::CeilingHit => (" (ceiling hit)", "（已达上限）"),
        app_lib::fonts::ScanStopReason::IncompleteIo => (
            " (incomplete: unreadable entry)",
            "（不完整：存在不可读项目）",
        ),
    };
    // Path suffix is optional: font dirs always have one; font files
    // are a flat list with no single "source path" so the suffix is
    // omitted in that case.
    //
    // sanitize_for_display is required here because this site is a
    // stdout/println interpolation and `display_path` is a pure
    // formatter. A crafted font-dir name containing ANSI escape
    // sequences or U+202E would otherwise reach the terminal verbatim
    // and corrupt the verbose summary.
    let (path_suffix_en, path_suffix_zh) = match path {
        Some(p) => {
            let display = sanitize_for_display(&display_path(p));
            (format!(" ({display})"), format!("（{display}）"))
        }
        None => (String::new(), String::new()),
    };
    println!(
        "{}",
        localize(
            globals,
            format!(
                "{label_en}: {} faces scanned, {} added, {} duplicated{reason_en}{path_suffix_en}",
                summary.total, summary.added, summary.duplicated
            ),
            format!(
                "{label_zh}：扫描 {} 个字体，{} 个已添加，{} 个已去重{reason_zh}{path_suffix_zh}",
                summary.total, summary.added, summary.duplicated
            ),
        )
    );
}
