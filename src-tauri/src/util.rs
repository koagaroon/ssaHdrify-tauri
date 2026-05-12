//! Cross-module helpers shared between IPC commands.
//!
//! Currently houses `is_reparse_point`, used by both `dropzone` and
//! `encoding` to refuse symlinks / Windows junctions / OneDrive
//! placeholders before reading or expanding the path, plus
//! `validate_ipc_path`, the canonical length+character validation for
//! any path coming off the IPC boundary. Both lifted here once a third
//! caller appeared (the streaming font scan), so the previous
//! per-module copies could no longer drift.

use std::path::Path;

/// Cap on the path-list length any IPC command will accept in a single
/// call. Two callers today: `dropzone::expand_dropped_paths` and
/// `fonts::scan_font_files` / `fonts::preflight_font_files`. The OS file
/// picker can't realistically deliver more than a handful of thousand
/// files in one selection, so 1000 is generous for the user-facing flow
/// while bounding worst-case CPU/IO if a future code path or
/// compromised frontend supplies a huge vector. Lifted here once a
/// second module needed the same number — keeping a single definition
/// avoids the two callers drifting independently.
pub const MAX_INPUT_PATHS: usize = 1000;

/// Maximum length for any path string accepted from the IPC boundary,
/// counted as UTF-8 BYTES not chars (`str::len()` is O(1) byte length).
/// Windows path APIs are bounded by `MAX_PATH` (260) for legacy callers
/// and `MAX_PATH_LONG` (≈32767 UTF-16 code units, ≈65k UTF-8 bytes for
/// non-ASCII) when the extended-length `\\?\` prefix is used; 4096
/// bytes is generous against any real user picker (≈1300 CJK chars)
/// while still catching obviously hostile inputs early.
pub const MAX_IPC_PATH_LEN: usize = 4096;

/// Validate a path string just received from the IPC boundary. Rejects:
///
/// 1. Empty, or longer than `MAX_IPC_PATH_LEN` bytes.
/// 2. Any character matched by `char::is_control()` — Unicode general
///    category Cc, which spans U+0000..=U+001F (C0 controls including
///    NUL/CR/LF/HT) and U+007F..=U+009F (C1 controls including NEL
///    U+0085).
/// 3. U+2028 LINE SEPARATOR (Zl) and U+2029 PARAGRAPH SEPARATOR (Zp).
///    These are NOT in Cc, so `is_control()` doesn't catch them — added
///    explicitly because several Rust path libraries treat them
///    ambiguously across platforms (some normalize as line terminators,
///    others pass through verbatim).
/// 4. Zero-width formatting characters U+200B / U+200C / U+200D /
///    U+FEFF. Windows file APIs accept them, so two visually-identical
///    filenames `foo.ass` and `foo\u{200B}.ass` resolve to different
///    paths on disk and bypass `normalizeOutputKey`'s within-batch
///    dedup (it does NFC + slash + lowercase but not zero-width strip).
///    Reject at this boundary so they never reach IPC consumers.
///
/// Unicode noncharacters (U+FFFE, U+FFFF, U+FDD0..=U+FDEF) are
/// intentionally not rejected — Windows file APIs already error with
/// `ERROR_INVALID_NAME` on path components containing them, and
/// surrogates can't appear in a Rust `String` by construction.
///
/// `label` is used in the returned error string so callers can identify
/// which input was bad ("Directory path must be 1-4096 bytes", etc.).
/// Keep this the SINGLE definition; each module previously had its own
/// copy and they drifted.
pub fn validate_ipc_path(path: &str, label: &str) -> Result<(), String> {
    if path.is_empty() || path.len() > MAX_IPC_PATH_LEN {
        return Err(format!("{label} path must be 1-{MAX_IPC_PATH_LEN} bytes"));
    }
    if path.chars().any(|c| {
        c.is_control()
            || matches!(
                c,
                // Line / paragraph separators (Unicode line breaks).
                '\u{2028}' | '\u{2029}'
                // Zero-width joiners / non-joiners / spaces /
                // word joiner / mongolian vowel separator / BOM —
                // all invisible characters that can smuggle past
                // visual review and break path comparisons.
                | '\u{200B}' | '\u{200C}' | '\u{200D}'
                | '\u{2060}' | '\u{180E}' | '\u{FEFF}'
                // BiDi format characters: LRM / RLM marks plus
                // embedding / override / isolate codepoints. U+202E
                // (RIGHT-TO-LEFT OVERRIDE) is the well-known
                // filename-display-reversal vector — `evil\u{202E}txt.exe`
                // displays as `evilexe.txt` in many UIs. Reject the
                // whole 200E-202E + 2066-2069 family for symmetry.
                | '\u{200E}' | '\u{200F}'
                | '\u{202A}' | '\u{202B}' | '\u{202C}' | '\u{202D}' | '\u{202E}'
                | '\u{2066}' | '\u{2067}' | '\u{2068}' | '\u{2069}'
            )
    }) {
        return Err(format!("{label} path contains invalid characters"));
    }
    // Reject Windows DOS device namespaces (\\.\<device>) and the
    // \\?\GLOBALROOT\… kernel-object form. These open raw device
    // handles or kernel-namespace paths — never legitimate user data
    // targets. The legitimate Win32 long-path prefixes (\\?\C:\… and
    // \\?\UNC\server\share\…) remain allowed since they map onto
    // ordinary filesystem paths and font scanning + drag-drop both
    // produce them through canonicalize().
    //
    // Byte-prefix check on raw `path.as_bytes()` (Round 2 A-R2-4) —
    // the previous `path.to_ascii_lowercase()` allocated a fresh
    // String just to compare a ≤16-byte prefix, wasting ~4 KiB on a
    // pathological 4096-byte IPC path. `eq_ignore_ascii_case` runs
    // ASCII case folding in place. Lossless: every prefix listed below
    // is pure ASCII, so byte-level case folding is byte-equivalent to
    // the prior char-level fold.
    let bytes = path.as_bytes();
    let starts_ci = |needle: &[u8]| {
        bytes.len() >= needle.len() && bytes[..needle.len()].eq_ignore_ascii_case(needle)
    };
    let is_dos_device = starts_ci(br"\\.\") || starts_ci(b"//./");
    let is_globalroot = starts_ci(br"\\?\globalroot\") || starts_ci(b"//?/globalroot/");
    if is_dos_device || is_globalroot {
        return Err(format!("{label} path uses a reserved device namespace"));
    }
    Ok(())
}

/// Replace every "visual line break" character with a separator so a
/// string can be safely embedded in a single-line context (terminal
/// stderr, rfd dialog body, log line). Strips:
///
/// - ASCII CR / LF (`\r`, `\n`)
/// - C1 NEL (`U+0085`) — historical newline on EBCDIC; some terminals
///   honor it
/// - Unicode line / paragraph separators (`U+2028` / `U+2029`)
///
/// Bidi-override controls (`U+202A..U+202E`, `U+2066..U+2069`) are NOT
/// stripped — those are a separate Trojan-source class. If a path
/// containing a bidi override becomes a credible threat, extend this
/// helper and apply at every untrusted-output boundary.
pub fn strip_visual_line_breaks(s: &str) -> String {
    s.replace(['\r', '\n', '\u{0085}', '\u{2028}', '\u{2029}'], " — ")
}

/// Detect symlinks AND Windows junctions / OneDrive placeholders.
///
/// On Windows, Rust's `is_symlink()` returns `false` for junctions
/// (IO_REPARSE_TAG_MOUNT_POINT), so a junction-based bypass slips past
/// a naive symlink check. This helper inspects the raw
/// `FILE_ATTRIBUTE_REPARSE_POINT` bit on Windows; on non-Windows
/// platforms it falls back to the standard `is_symlink()` check, which
/// is sufficient there.
///
/// Callers: `dropzone::expand_dropped_paths` (skip top-level dropped
/// reparse points), `dropzone::walk_one_level` (skip reparse points
/// inside walked folders), `encoding::read_text_detect_encoding`
/// (refuse to read text from a reparse point), `fonts::scan_directory_inner`
/// (skip reparse-point siblings during font scan), and
/// `fonts::preflight_directory_inner` (skip reparse points when
/// previewing folder size). All callers want the same "is this a
/// symlink-like thing the OS would chase" answer; keep this helper as
/// the single source of truth.
#[cfg(windows)]
pub fn is_reparse_point(path: &Path) -> bool {
    use std::os::windows::fs::MetadataExt;
    const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x0400;
    std::fs::symlink_metadata(path)
        .map(|m| m.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0)
        .unwrap_or(false)
}

#[cfg(not(windows))]
pub fn is_reparse_point(path: &Path) -> bool {
    std::fs::symlink_metadata(path)
        .map(|m| m.file_type().is_symlink())
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── validate_ipc_path: byte-prefix DOS-device check (A-R2-4) ──

    #[test]
    fn validate_rejects_dos_device_lowercase() {
        let err = validate_ipc_path(r"\\.\PhysicalDrive0", "Test").unwrap_err();
        assert!(err.contains("reserved device namespace"));
    }

    #[test]
    fn validate_rejects_dos_device_mixed_case() {
        // Windows is case-insensitive for device names; the byte-prefix
        // helper must fold case without allocating a lowered string.
        let err = validate_ipc_path(r"\\.\PHYSICALDRIVE0", "Test").unwrap_err();
        assert!(err.contains("reserved device namespace"));
    }

    #[test]
    fn validate_rejects_globalroot_mixed_case() {
        let err = validate_ipc_path(r"\\?\GlobalRoot\Device\Boot", "Test").unwrap_err();
        assert!(err.contains("reserved device namespace"));
    }

    #[test]
    fn validate_accepts_long_path_prefix() {
        // \\?\C:\… is the legitimate long-path form, NOT in the deny set.
        validate_ipc_path(r"\\?\C:\fonts\sample.ttf", "Test").expect("long path should be allowed");
    }

    // ── strip_visual_line_breaks (N-R2-11) ──

    #[test]
    fn strip_replaces_all_documented_breaks() {
        let input = "line1\rline2\nline3\u{0085}line4\u{2028}line5\u{2029}line6";
        let out = strip_visual_line_breaks(input);
        assert!(!out.contains('\r'));
        assert!(!out.contains('\n'));
        assert!(!out.contains('\u{0085}'));
        assert!(!out.contains('\u{2028}'));
        assert!(!out.contains('\u{2029}'));
        // All breaks collapse to the same separator; line tokens
        // remain visible.
        assert!(out.contains("line1"));
        assert!(out.contains("line6"));
    }

    #[test]
    fn strip_passes_through_normal_text() {
        let input = "C:\\Users\\me\\subtitle.ass";
        assert_eq!(strip_visual_line_breaks(input), input);
    }
}
