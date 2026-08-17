//! Async Tauri boundary for operations that use blocking filesystem, SQLite,
//! system-font, or font-parser APIs.
//!
//! The synchronous implementations stay in their domain modules so the CLI
//! and focused Rust tests can call them without starting an async runtime.
//! Every GUI command here moves the complete operation onto Tauri's dedicated
//! blocking pool; keeping the wrapper layer centralized makes it difficult to
//! accidentally register a blocking implementation directly.

use crate::dropzone::ExpandedPaths;
use crate::encoding::ReadTextResult;
use crate::font_cache_commands::{CacheStatus, DriftReport, RescanResult};
use crate::fonts::FontLookupResult;

async fn run_blocking_command<T, F>(command: &'static str, operation: F) -> Result<T, String>
where
    T: Send + 'static,
    F: FnOnce() -> Result<T, String> + Send + 'static,
{
    tauri::async_runtime::spawn_blocking(operation)
        .await
        .map_err(|error| format!("{command} worker failed: {error}"))?
}

#[tauri::command]
pub async fn expand_dropped_paths(
    app: tauri::AppHandle,
    paths: Vec<String>,
) -> Result<ExpandedPaths, String> {
    run_blocking_command("expand_dropped_paths", move || {
        crate::dropzone::expand_dropped_paths(app, paths)
    })
    .await
}

#[tauri::command]
pub async fn read_text_detect_encoding(
    app: tauri::AppHandle,
    path: String,
) -> Result<ReadTextResult, String> {
    run_blocking_command("read_text_detect_encoding", move || {
        crate::encoding::read_text_detect_encoding(app, path)
    })
    .await
}

#[tauri::command]
pub async fn find_system_font(
    family: String,
    bold: bool,
    italic: bool,
) -> Result<FontLookupResult, String> {
    run_blocking_command("find_system_font", move || {
        crate::fonts::find_system_font(family, bold, italic)
    })
    .await
}

#[tauri::command]
pub async fn subset_font_b64(
    font_path: String,
    font_index: u32,
    codepoints: Vec<u32>,
) -> Result<String, String> {
    run_blocking_command("subset_font_b64", move || {
        crate::fonts::subset_font_b64(font_path, font_index, codepoints)
    })
    .await
}

#[tauri::command]
pub async fn resolve_user_font(
    family: String,
    bold: bool,
    italic: bool,
) -> Result<Option<FontLookupResult>, String> {
    run_blocking_command("resolve_user_font", move || {
        crate::fonts::resolve_user_font(family, bold, italic)
    })
    .await
}

#[tauri::command]
pub async fn remove_font_source(source_id: String, kind: Option<String>) -> Result<(), String> {
    run_blocking_command("remove_font_source", move || {
        crate::fonts::remove_font_source(source_id, kind)
    })
    .await
}

#[tauri::command]
pub async fn clear_font_sources() -> Result<(), String> {
    run_blocking_command("clear_font_sources", crate::fonts::clear_font_sources).await
}

#[tauri::command]
pub async fn open_font_cache() -> Result<CacheStatus, String> {
    run_blocking_command(
        "open_font_cache",
        crate::font_cache_commands::open_font_cache,
    )
    .await
}

#[tauri::command]
pub async fn detect_font_cache_drift() -> Result<DriftReport, String> {
    run_blocking_command(
        "detect_font_cache_drift",
        crate::font_cache_commands::detect_font_cache_drift,
    )
    .await
}

#[tauri::command]
pub async fn rescan_font_cache_drift() -> Result<RescanResult, String> {
    run_blocking_command(
        "rescan_font_cache_drift",
        crate::font_cache_commands::rescan_font_cache_drift,
    )
    .await
}

#[tauri::command]
pub async fn clear_font_cache() -> Result<(), String> {
    run_blocking_command(
        "clear_font_cache",
        crate::font_cache_commands::clear_font_cache,
    )
    .await
}

#[tauri::command]
pub async fn lookup_font_family(
    family: String,
    bold: bool,
    italic: bool,
) -> Result<Option<FontLookupResult>, String> {
    run_blocking_command("lookup_font_family", move || {
        crate::font_cache_commands::lookup_font_family(family, bold, italic)
    })
    .await
}

#[tauri::command]
pub async fn safe_output_path_exists(app: tauri::AppHandle, path: String) -> Result<bool, String> {
    run_blocking_command("safe_output_path_exists", move || {
        crate::safe_io::safe_output_path_exists(app, path)
    })
    .await
}

#[tauri::command]
pub async fn safe_write_text_file(
    app: tauri::AppHandle,
    path: String,
    content: String,
    overwrite: bool,
) -> Result<(), String> {
    run_blocking_command("safe_write_text_file", move || {
        crate::safe_io::safe_write_text_file(app, path, content, overwrite)
    })
    .await
}

#[tauri::command]
pub async fn safe_write_style_edit_output(
    app: tauri::AppHandle,
    source_path: String,
    expected_revision: String,
    output_path: String,
    content: String,
) -> Result<(), String> {
    run_blocking_command("safe_write_style_edit_output", move || {
        crate::safe_io::safe_write_style_edit_output(
            app,
            source_path,
            expected_revision,
            output_path,
            content,
        )
    })
    .await
}

#[tauri::command]
pub async fn safe_copy_file(
    app: tauri::AppHandle,
    src: String,
    dst: String,
    overwrite: bool,
) -> Result<(), String> {
    run_blocking_command("safe_copy_file", move || {
        crate::safe_io::safe_copy_file(app, src, dst, overwrite)
    })
    .await
}

#[tauri::command]
pub async fn safe_rename_file(
    app: tauri::AppHandle,
    src: String,
    dst: String,
    overwrite: bool,
) -> Result<(), String> {
    run_blocking_command("safe_rename_file", move || {
        crate::safe_io::safe_rename_file(app, src, dst, overwrite)
    })
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blocking_dispatch_runs_on_a_worker_thread() {
        let caller_thread = std::thread::current().id();
        let worker_thread =
            tauri::async_runtime::block_on(run_blocking_command("thread_probe", || {
                Ok(std::thread::current().id())
            }))
            .expect("thread probe should complete");

        assert_ne!(caller_thread, worker_thread);
    }

    #[test]
    fn blocking_dispatch_preserves_operation_errors() {
        let error =
            tauri::async_runtime::block_on(run_blocking_command::<(), _>("error_probe", || {
                Err("original operation error".to_string())
            }))
            .expect_err("operation error should be returned");

        assert_eq!(error, "original operation error");
    }
}
