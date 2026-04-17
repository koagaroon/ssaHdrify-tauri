pub mod encoding;
pub mod fonts;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_fs::init())
        .setup(|app| {
            if cfg!(debug_assertions) {
                app.handle().plugin(
                    tauri_plugin_log::Builder::default()
                        .level(log::LevelFilter::Info)
                        .build(),
                )?;
            }
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            encoding::read_text_detect_encoding,
            fonts::find_system_font,
            fonts::subset_font,
            fonts::scan_font_directory,
            fonts::scan_font_files,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
