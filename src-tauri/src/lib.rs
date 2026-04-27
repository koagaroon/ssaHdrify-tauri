pub mod dropzone;
pub mod encoding;
pub mod fonts;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_fs::init())
        .setup(|app| {
            // Dev: INFO-level for full visibility while iterating.
            // Release: WARN/ERROR only — keeps crash-diagnostic signals in
            // bug reports without spamming healthy runs.
            let level = if cfg!(debug_assertions) {
                log::LevelFilter::Info
            } else {
                log::LevelFilter::Warn
            };
            app.handle()
                .plugin(tauri_plugin_log::Builder::default().level(level).build())?;
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            dropzone::expand_dropped_paths,
            encoding::read_text_detect_encoding,
            fonts::find_system_font,
            fonts::subset_font,
            fonts::scan_font_directory,
            fonts::scan_font_files,
        ])
        .run(tauri::generate_context!())
        .unwrap_or_else(|e| {
            eprintln!("fatal: tauri runtime failed to start: {e}");
            std::process::exit(1);
        });
}
