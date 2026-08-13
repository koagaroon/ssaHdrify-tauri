use std::fs;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use app_lib::fonts::ALLOWED_FONT_EXTENSIONS;

#[cfg(windows)]
fn os_font_roots() -> Vec<PathBuf> {
    vec![
        PathBuf::from(std::env::var("SYSTEMROOT").unwrap_or_else(|_| "C:\\Windows".to_string()))
            .join("Fonts"),
    ]
}

#[cfg(target_os = "macos")]
fn os_font_roots() -> Vec<PathBuf> {
    vec![
        PathBuf::from("/System/Library/Fonts"),
        PathBuf::from("/Library/Fonts"),
    ]
}

#[cfg(all(unix, not(target_os = "macos")))]
fn os_font_roots() -> Vec<PathBuf> {
    vec![
        PathBuf::from("/usr/share/fonts"),
        PathBuf::from("/usr/local/share/fonts"),
    ]
}

fn real_os_font() -> &'static PathBuf {
    static FONT: OnceLock<PathBuf> = OnceLock::new();
    FONT.get_or_init(|| {
        let mut pending = os_font_roots();
        let mut visited = 0usize;
        while let Some(directory) = pending.pop() {
            visited += 1;
            assert!(
                visited <= 10_000,
                "operating-system font search exceeded 10,000 directories"
            );
            let Ok(read_dir) = fs::read_dir(&directory) else {
                continue;
            };
            let mut entries: Vec<_> = read_dir.flatten().collect();
            entries.sort_by_key(|entry| entry.path());
            for entry in entries {
                let path = entry.path();
                if app_lib::util::try_is_reparse_point(&path).unwrap_or(true) {
                    continue;
                }
                let Ok(file_type) = entry.file_type() else {
                    continue;
                };
                if file_type.is_dir() {
                    pending.push(path);
                    continue;
                }
                let Some(extension) = path
                    .extension()
                    .and_then(|value| value.to_str())
                    .map(|value| value.to_ascii_lowercase())
                else {
                    continue;
                };
                if file_type.is_file() && ALLOWED_FONT_EXTENSIONS.contains(&extension.as_str()) {
                    return path;
                }
            }
        }
        panic!(
            "no readable operating-system font found under {:?}",
            os_font_roots()
        )
    })
}

/// Copy one installed font into a disposable test source. The source file is
/// never committed or shipped; each integration test removes its temp tree.
pub fn make_real_font_dir(parent: &Path) -> PathBuf {
    let font_dir = parent.join("fonts");
    fs::create_dir_all(&font_dir).expect("failed to create fonts subdir");
    let source = real_os_font();
    let file_name = source
        .file_name()
        .expect("OS font path should have a file name");
    fs::copy(source, font_dir.join(file_name)).expect("failed to copy OS font into test source");
    font_dir
}
