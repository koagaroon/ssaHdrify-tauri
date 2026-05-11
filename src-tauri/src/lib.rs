pub mod dropzone;
pub mod encoding;
pub mod font_cache;
pub mod font_cache_commands;
pub mod fonts;
pub mod safe_io;
pub mod util;

use tauri::Manager;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_fs::init())
        .setup(|app| {
            // Install the log plugin FIRST so any `log::warn!` / `log::info!`
            // calls from `init_system_dirs`, `init_user_font_db`,
            // `init_gui_font_cache`, or any helper they call have a
            // subscriber to receive them. Without this ordering, early-init
            // diagnostics (e.g. a font-DB schema mismatch's warning) are
            // dropped silently.
            //
            // Dev: INFO-level for full visibility while iterating.
            // Release: WARN/ERROR only — keeps crash-diagnostic signals in
            // bug reports without spamming healthy runs.
            // UseLocal so terminal log timestamps match the user's wall
            // clock instead of UTC — at UTC+8 the default-UTC output
            // looks 8 hours off, which reads as a real bug at first
            // glance even though the times are technically correct.
            let level = if cfg!(debug_assertions) {
                log::LevelFilter::Info
            } else {
                log::LevelFilter::Warn
            };
            app.handle().plugin(
                tauri_plugin_log::Builder::default()
                    .level(level)
                    .timezone_strategy(tauri_plugin_log::TimezoneStrategy::UseLocal)
                    .build(),
            )?;

            // Snapshot env-derived system-fonts paths eagerly, before any
            // user action can run. Defense-in-depth against post-launch
            // env-var manipulation; see fonts::init_system_dirs.
            fonts::init_system_dirs();
            let app_data_dir = app.path().app_data_dir()?;
            if let Err(e) = fonts::init_user_font_db(&app_data_dir) {
                // Windows GUI subsystem has no visible stderr — without
                // a native dialog the app would exit silently and the
                // user would have no way to know why. Block on a
                // MessageBox so the failure is unmissable. rfd uses the
                // OS-native chrome (Win32 MessageBox / NSAlert / GTK
                // dialog) and works before the WebView2 window exists.
                //
                // Collapse \r and \n in the underlying error string
                // before injection: rfd's `set_description` honors them
                // as real line breaks, and a long Windows extended-
                // length path combined with a multi-line error chain
                // could push the dialog body off-screen. Plain dash
                // separator keeps the error readable on a single block.
                // No other escape — rfd renders text plainly, no markup
                // to bypass.
                // Strip ALL Unicode line breaks rfd would honor as
                // real newlines: ASCII CR/LF, NEL (U+0085), and the
                // Unicode line/paragraph separators (U+2028 / U+2029).
                // Plain `replace(['\r','\n'], ...)` left the wide
                // separators in, so a crafted string containing one
                // would still push the dialog body off-screen.
                // Apply to BOTH the error message AND the path display:
                // a hostile or pathological app_data_dir (rare but
                // possible on locale-mangled Windows profiles or
                // OS-resolved env-var rewrites) could otherwise sneak
                // wide line breaks through the path slot.
                fn one_line(s: &str) -> String {
                    s.replace(['\r', '\n', '\u{0085}', '\u{2028}', '\u{2029}'], " — ")
                }
                let error_one_line = one_line(&e);
                let path_one_line = one_line(&app_data_dir.display().to_string());
                rfd::MessageDialog::new()
                    .set_level(rfd::MessageLevel::Error)
                    .set_title("SSA HDRify — startup failure")
                    .set_description(format!(
                        "Failed to initialize the user-font index at\n{path_one_line}\n\n{error_one_line}\n\nThe app cannot start."
                    ))
                    .show();
                return Err(std::io::Error::other(e).into());
            }

            // GUI persistent font cache. Init failure is non-fatal —
            // log a warning and continue with cache unavailable. The
            // app keeps working (embed falls through to system fonts,
            // matching pre-#5 behavior); the user just doesn't get
            // cache acceleration this session.
            if let Err(e) = font_cache_commands::init_gui_font_cache(&app_data_dir) {
                log::warn!("GUI font cache init failed: {e}. Cache will be unavailable.");
            }
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            dropzone::expand_dropped_paths,
            encoding::read_text_detect_encoding,
            fonts::find_system_font,
            fonts::subset_font_b64,
            fonts::preflight_font_directory,
            fonts::preflight_font_files,
            fonts::scan_font_directory,
            fonts::scan_font_files,
            fonts::cancel_font_scan,
            fonts::resolve_user_font,
            fonts::remove_font_source,
            fonts::clear_font_sources,
            font_cache_commands::open_font_cache,
            font_cache_commands::detect_font_cache_drift,
            font_cache_commands::rescan_font_cache_drift,
            font_cache_commands::clear_font_cache,
            font_cache_commands::lookup_font_family,
            safe_io::safe_write_text_file,
            safe_io::safe_copy_file,
            safe_io::safe_rename_file,
        ])
        .run(tauri::generate_context!())
        .unwrap_or_else(|e| {
            eprintln!("fatal: tauri runtime failed to start: {e}");
            std::process::exit(1);
        });
}
