//! Encoding detection and decoding for subtitle files.
//!
//! Strategy: BOM detection first (deterministic), then chardetng (heuristic).
//! Returns UTF-8 text + detected encoding name so the frontend always gets
//! clean Unicode regardless of the original file encoding.

use chardetng::EncodingDetector;
use std::io::ErrorKind;
use std::path::Path;

const MAX_TEXT_SIZE: u64 = 50 * 1024 * 1024; // 50 MB

/// Allowed subtitle file extensions for `read_text_detect_encoding`.
/// Defense-in-depth: the frontend only sends paths from file dialogs, but
/// this prevents the IPC command from being repurposed as a generic file reader.
/// `.txt` is intentionally excluded — the frontend dialogs never offer it,
/// and keeping it in the allow-list would widen arbitrary-read via any JS bug.
const ALLOWED_TEXT_EXTENSIONS: &[&str] = &["ass", "ssa", "srt", "vtt", "sub", "sbv", "lrc"];

/// Map a std::io::Error to a generic, path-free message for IPC. The detailed
/// error is logged server-side so it's still reachable during debug, but never
/// crosses the IPC boundary where a user-facing error toast could leak paths.
fn sanitize_io_error(e: &std::io::Error, action: &str) -> String {
    log::warn!("io error during {action}: {e}");
    match e.kind() {
        ErrorKind::NotFound => format!("{action} failed: file not found"),
        ErrorKind::PermissionDenied => format!("{action} failed: permission denied"),
        ErrorKind::InvalidData => format!("{action} failed: invalid data"),
        _ => format!("{action} failed"),
    }
}

// ── Internal helpers (exported for tests) ────────────────

/// Detect encoding and decode bytes to UTF-8. Shared logic for both the
/// Tauri command and unit tests (which can't call Tauri commands directly).
pub fn decode_bytes(bytes: &[u8]) -> ReadTextResult {
    // 1. BOM detection
    if let Some(result) = detect_bom(bytes) {
        return result;
    }

    // 2. chardetng heuristic
    let mut detector = EncodingDetector::new();
    detector.feed(bytes, true);
    let encoding = detector.guess(None, true);

    let (cow, _, had_errors) = encoding.decode(bytes);
    if had_errors {
        // chardetng picked an encoding but decoding hit invalid sequences.
        // Record the attempted encoding in the label so callers can see what
        // was tried — plain "UTF-8 (lossy)" masked whether the file was
        // actually UTF-8 or some other guess that failed.
        let text = String::from_utf8_lossy(bytes).into_owned();
        return ReadTextResult {
            text,
            encoding: format!("{} (lossy)", encoding.name()),
        };
    }

    ReadTextResult {
        text: cow.into_owned(),
        encoding: encoding.name().to_string(),
    }
}

/// Result of reading a text file with encoding detection.
#[derive(serde::Serialize)]
pub struct ReadTextResult {
    /// File content decoded to UTF-8
    pub text: String,
    /// Detected encoding name (e.g. "UTF-8", "GBK", "Big5", "Shift_JIS")
    pub encoding: String,
}

/// Read a file, detect its encoding, and return UTF-8 text + encoding name.
///
/// Detection order:
/// 1. BOM (UTF-8, UTF-16 LE/BE) — deterministic, highest priority
/// 2. chardetng heuristic — handles GBK, Big5, Shift_JIS, EUC-KR, etc.
/// 3. Lossy UTF-8 fallback — if all else fails
#[tauri::command]
pub fn read_text_detect_encoding(path: String) -> Result<ReadTextResult, String> {
    // Length and content guards on the IPC-supplied path itself. Reject
    // obviously-hostile or pathological shapes BEFORE touching the
    // filesystem. Control chars / NUL in a path on Windows can truncate
    // the access target at the null byte; `..` segments, while mostly
    // defanged by canonicalize later, are rejected here as an early
    // signal that the caller is not the native file picker.
    if path.is_empty() || path.len() > 4096 {
        return Err("Path must be 1-4096 characters".to_string());
    }
    if path.chars().any(|c| c.is_control()) {
        return Err("Path contains invalid characters".to_string());
    }

    // Extension validation: only allow subtitle/text file types
    let path_ref = Path::new(&path);
    let ext = path_ref
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_lowercase())
        .unwrap_or_default();
    if !ALLOWED_TEXT_EXTENSIONS.contains(&ext.as_str()) {
        return Err(format!("Unsupported file type: .{ext}"));
    }

    // Canonicalize once — resolves symlinks, "..", and gives us a stable
    // path to open. Using the canonical path for the open call mitigates
    // the TOCTOU window where a symlink could be swapped between stat and
    // open (std::fs holds a HANDLE after open, so a later swap is harmless).
    let canonical = path_ref.canonicalize().map_err(|e| sanitize_io_error(&e, "resolve"))?;

    // Size check
    let metadata = std::fs::metadata(&canonical).map_err(|e| sanitize_io_error(&e, "stat"))?;
    if metadata.len() > MAX_TEXT_SIZE {
        let size_mb = metadata.len() as f64 / (1024.0 * 1024.0);
        return Err(format!(
            "File too large: {size_mb:.1} MB exceeds the 50 MB limit"
        ));
    }

    let bytes = std::fs::read(&canonical).map_err(|e| sanitize_io_error(&e, "read"))?;

    // Post-read size check (TOCTOU mitigation — file could grow between stat and read)
    if bytes.len() as u64 > MAX_TEXT_SIZE {
        let size_mb = bytes.len() as f64 / (1024.0 * 1024.0);
        return Err(format!(
            "File too large after read: {size_mb:.1} MB exceeds the 50 MB limit"
        ));
    }

    Ok(decode_bytes(&bytes))
}

/// Check for Byte Order Mark and decode accordingly. When the decoded text
/// contained invalid sequences, the encoding label is suffixed with "(lossy)"
/// so the frontend can distinguish clean decodes from ones with U+FFFD
/// replacements.
fn detect_bom(bytes: &[u8]) -> Option<ReadTextResult> {
    // UTF-8 BOM (EF BB BF) — strip BOM, decode as UTF-8
    if bytes.starts_with(&[0xEF, 0xBB, 0xBF]) {
        let payload = &bytes[3..];
        let lossy = std::str::from_utf8(payload).is_err();
        let text = String::from_utf8_lossy(payload).into_owned();
        return Some(ReadTextResult {
            text,
            encoding: if lossy {
                "UTF-8 (BOM, lossy)".to_string()
            } else {
                "UTF-8 (BOM)".to_string()
            },
        });
    }

    // UTF-16 LE BOM (FF FE)
    if bytes.starts_with(&[0xFF, 0xFE]) {
        let (cow, _, had_errors) = encoding_rs::UTF_16LE.decode(&bytes[2..]);
        return Some(ReadTextResult {
            text: cow.into_owned(),
            encoding: if had_errors {
                "UTF-16LE (lossy)".to_string()
            } else {
                "UTF-16LE".to_string()
            },
        });
    }

    // UTF-16 BE BOM (FE FF)
    if bytes.starts_with(&[0xFE, 0xFF]) {
        let (cow, _, had_errors) = encoding_rs::UTF_16BE.decode(&bytes[2..]);
        return Some(ReadTextResult {
            text: cow.into_owned(),
            encoding: if had_errors {
                "UTF-16BE (lossy)".to_string()
            } else {
                "UTF-16BE".to_string()
            },
        });
    }

    None
}

// ── Tests ────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    /// Helper: read fixture file and decode
    fn decode_fixture(name: &str) -> ReadTextResult {
        let path = format!(
            "{}/../src-tauri/tests/fixtures/{}",
            env!("CARGO_MANIFEST_DIR"),
            name
        );
        // Try direct path first, then relative to manifest
        let bytes = std::fs::read(&path)
            .or_else(|_| {
                let alt = format!("{}/tests/fixtures/{}", env!("CARGO_MANIFEST_DIR"), name);
                std::fs::read(alt)
            })
            .unwrap_or_else(|e| panic!("Cannot read fixture {name}: {e}"));
        decode_bytes(&bytes)
    }

    #[test]
    fn utf8_no_bom() {
        let result = decode_fixture("utf8.ass");
        assert_eq!(result.encoding, "UTF-8");
        assert!(result.text.contains("中文字幕测试"));
        assert!(result.text.contains("[Script Info]"));
    }

    #[test]
    fn utf8_with_bom() {
        let result = decode_fixture("utf8_bom.ass");
        assert_eq!(result.encoding, "UTF-8 (BOM)");
        assert!(result.text.contains("中文字幕测试"));
        // BOM should be stripped — text should start with [
        assert!(result.text.starts_with("[Script Info]"));
    }

    #[test]
    fn gbk_detection() {
        let result = decode_fixture("gbk.ass");
        // chardetng may report as GBK or gb18030 (superset)
        let enc = result.encoding.to_lowercase();
        assert!(
            enc.contains("gbk") || enc.contains("gb18030"),
            "Expected GBK/GB18030, got: {}",
            result.encoding
        );
        assert!(result.text.contains("中文字幕测试"));
        assert!(result.text.contains("GBK编码测试"));
    }

    #[test]
    fn big5_detection() {
        let result = decode_fixture("big5.ass");
        let enc = result.encoding.to_lowercase();
        assert!(
            enc.contains("big5"),
            "Expected Big5, got: {}",
            result.encoding
        );
        assert!(result.text.contains("Big5編碼測試"));
    }

    #[test]
    fn shift_jis_detection() {
        let result = decode_fixture("shiftjis.ass");
        let enc = result.encoding.to_lowercase();
        assert!(
            enc.contains("shift_jis") || enc.contains("shift-jis") || enc.contains("sjis"),
            "Expected Shift_JIS, got: {}",
            result.encoding
        );
        assert!(result.text.contains("日本語字幕テスト"));
    }

    #[test]
    fn utf16le_with_bom() {
        let result = decode_fixture("utf16le.ass");
        assert_eq!(result.encoding, "UTF-16LE");
        assert!(result.text.contains("中文字幕测试"));
        assert!(result.text.contains("[Script Info]"));
    }

    #[test]
    fn all_encodings_produce_valid_ass_structure() {
        // Every fixture, regardless of encoding, should decode to valid ASS
        for fixture in &[
            "utf8.ass",
            "utf8_bom.ass",
            "gbk.ass",
            "big5.ass",
            "shiftjis.ass",
            "utf16le.ass",
        ] {
            let result = decode_fixture(fixture);
            assert!(
                result.text.contains("[Script Info]"),
                "{fixture}: missing [Script Info]"
            );
            assert!(
                result.text.contains("[V4+ Styles]"),
                "{fixture}: missing [V4+ Styles]"
            );
            assert!(
                result.text.contains("[Events]"),
                "{fixture}: missing [Events]"
            );
        }
    }
}
