pub mod dropzone;
pub mod encoding;
pub mod fonts;
pub mod util;

use tauri::Manager;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_fs::init())
        .setup(|app| {
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
                rfd::MessageDialog::new()
                    .set_level(rfd::MessageLevel::Error)
                    .set_title("SSA HDRify — startup failure")
                    .set_description(format!(
                        "Failed to initialize the user-font index at\n{}\n\n{e}\n\nThe app cannot start.",
                        app_data_dir.display()
                    ))
                    .show();
                return Err(std::io::Error::other(e).into());
            }

            // Dev: INFO-level for full visibility while iterating.
            // Release: WARN/ERROR only — keeps crash-diagnostic signals in
            // bug reports without spamming healthy runs.
            let level = if cfg!(debug_assertions) {
                log::LevelFilter::Info
            } else {
                log::LevelFilter::Warn
            };
            // UseLocal so terminal log timestamps match the user's wall
            // clock instead of UTC — at UTC+8 the default-UTC output
            // looks 8 hours off, which reads as a real bug at first
            // glance even though the times are technically correct.
            app.handle().plugin(
                tauri_plugin_log::Builder::default()
                    .level(level)
                    .timezone_strategy(tauri_plugin_log::TimezoneStrategy::UseLocal)
                    .build(),
            )?;
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            dropzone::expand_dropped_paths,
            encoding::read_text_detect_encoding,
            fonts::find_system_font,
            fonts::subset_font,
            fonts::preflight_font_directory,
            fonts::preflight_font_files,
            fonts::scan_font_directory,
            fonts::scan_font_files,
            fonts::cancel_font_scan,
            fonts::resolve_user_font,
            fonts::remove_font_source,
            fonts::clear_font_sources,
        ])
        .run(tauri::generate_context!())
        .unwrap_or_else(|e| {
            eprintln!("fatal: tauri runtime failed to start: {e}");
            std::process::exit(1);
        });
}
