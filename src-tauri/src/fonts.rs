use font_kit::family_name::FamilyName;
use font_kit::handle::Handle;
use font_kit::properties::{Properties, Style, Weight};
use font_kit::source::SystemSource;
use std::fs;

/// Find a system font file path by family name, bold, and italic flags.
/// Returns the absolute path to the font file on disk.
#[tauri::command]
pub fn find_system_font(family: String, bold: bool, italic: bool) -> Result<String, String> {
    let source = SystemSource::new();

    let mut props = Properties::new();
    if bold {
        props.weight = Weight::BOLD;
    }
    if italic {
        props.style = Style::Italic;
    }

    let handle = source
        .select_best_match(&[FamilyName::Title(family.clone())], &props)
        .map_err(|e| format!("Font not found: {} (bold={}, italic={}): {}", family, bold, italic, e))?;

    match handle {
        Handle::Path { path, .. } => {
            path.to_str()
                .map(|s| s.to_string())
                .ok_or_else(|| "Font path contains invalid Unicode".to_string())
        }
        Handle::Memory { .. } => {
            Err("Font is memory-only (no file path available)".to_string())
        }
    }
}

/// Subset a font file to only include the specified codepoints.
/// Currently returns the full font file — true subsetting will be added
/// with a dedicated Rust crate in a future update.
#[tauri::command]
pub fn subset_font(font_path: String, _codepoints: Vec<u32>) -> Result<Vec<u8>, String> {
    // TODO: implement actual subsetting with a Rust font crate
    // For now, return the full font file — embedding still works,
    // just with larger file sizes
    fs::read(&font_path)
        .map_err(|e| format!("Failed to read font file {}: {}", font_path, e))
}
