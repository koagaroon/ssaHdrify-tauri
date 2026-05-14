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
                // U+061C Arabic Letter Mark — Cf, bidi format
                // character. The TS unicode-controls set includes
                // it (Round 5 Wave 5.1); Round 6 Wave 6.2 parity sweep
                // adds the same here so the two sides match.
                | '\u{061C}'
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
    // Byte-prefix check on raw `path.as_bytes()`. The previous form
    // called `path.to_ascii_lowercase()` which allocated a fresh
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
    // Reject `..` path components (Round 5 A-R5-RUSTGUI-02). A raw IPC
    // path like `C:\Allowed\..\Denied\file.ass` matches an
    // `allow=**` fs:scope rule literally (the deny patterns like
    // `$HOME/.ssh/**` don't string-match a `..`-bearing path), so the
    // upstream `is_allowed` check passes. The OS then resolves the
    // `..` at read time and lands on the deny-listed target. The
    // encoding.rs canonicalize-success branch re-checks `is_allowed`
    // against the canonical form and catches this, but the
    // canonicalize-FAILS branch falls back to the raw path with
    // `..` intact and reads it — bypassing the scope policy.
    // Rejecting `..` at the IPC entry kills the attack vector for
    // every callsite (encoding.rs / fonts.rs / safe_io.rs / dropzone.rs)
    // in one place. CLI paths don't use validate_ipc_path; they have
    // their own argv normalization. All current GUI callers receive
    // absolute paths from pickers / drag-drop, which never contain
    // `..` segments — so this rejection has no legitimate-traffic cost.
    //
    // Detect `..` as a path COMPONENT (between separators or at the
    // ends), not a substring — `foo..bar.txt` is a legitimate filename.
    let has_dotdot_component = path.split(['/', '\\']).any(|seg| seg == "..");
    if has_dotdot_component {
        return Err(format!("{label} path contains parent-directory segments"));
    }
    // Reject Windows reserved device names in any path segment (Round 8
    // A-R8-A3-8). The TS-side `assertSafeOutputFilename` already
    // rejects these for templated outputs; here we mirror the check for
    // any IPC path so a non-TS caller (CLI argv, future deno_core
    // engine entry) can't slip `C:\foo\CON.ass` through. Win32 routes
    // device names regardless of extension (`NUL.txt` opens NUL too),
    // so write commands like `safe_write_text_file_inner` would
    // otherwise open the device handle.
    //
    // Per-segment + first-dot stem so `NUL.ass` and `CON.txt` both
    // reject; trailing whitespace / dots are stripped because
    // `CON ` and `CON.` resolve to the device as well.
    let has_reserved_segment = path.split(['/', '\\']).any(|seg| {
        let first_dot = seg.find('.').unwrap_or(seg.len());
        let stem = seg[..first_dot]
            .trim_end_matches(|c: char| c.is_whitespace() || c == '.')
            .to_ascii_uppercase();
        matches!(
            stem.as_str(),
            "CON" | "PRN" | "AUX" | "NUL" | "CONIN$" | "CONOUT$"
        ) || (stem.len() == 4
            && (stem.starts_with("COM") || stem.starts_with("LPT"))
            && stem.as_bytes()[3].is_ascii_digit())
    });
    if has_reserved_segment {
        return Err(format!(
            "{label} path contains a Windows reserved device name"
        ));
    }
    Ok(())
}

/// Validate a font family name received from the IPC boundary or
/// from upstream collection. Rejects empty / over-length / control-
/// character names. Codepoint-counted (not byte-counted) so CJK
/// names (3 bytes per char in UTF-8) that fit the 256-codepoint
/// intent stay valid. Consolidated from four inline copies in
/// `find_system_font` / `resolve_user_font` / `lookup_font_family`
/// / `parse_local_font_file` to a single shared helper.
///
/// Rejected character classes (all flow to the same "invalid
/// characters" error so the caller doesn't have to discriminate):
///
/// - Unicode category **Cc** (controls) via `is_control()`:
///   U+0000..=U+001F (C0 + NUL/CR/LF/HT) and U+007F..=U+009F (DEL +
///   C1 incl. NEL U+0085).
/// - **Bidi-override format characters** (Trojan-Source class,
///   CVE-2021-42574): U+200E..U+200F (LTR/RTL marks),
///   U+202A..U+202E (LRE/RLE/PDF/LRO/RLO),
///   U+2066..U+2069 (LRI/RLI/FSI/PDI). `is_control()` is Cc only and
///   does NOT match these — Cf is a different category.
/// - **Zero-width / invisible** chars that visually-identical strings
///   can use to bypass dedup or impersonate names: U+200B..U+200D
///   (ZWSP/ZWNJ/ZWJ), U+2060 (WORD JOINER), U+180E (Mongolian Vowel
///   Separator), U+FEFF (ZWNBSP / BOM-in-middle).
///
/// Mirrors `validate_ipc_path`'s rejection set so family names
/// originating from ASS \fn references (P1b: content-source
/// attacker) OR font name-table entries (also P1b) can't smuggle
/// these into the session DB / persistent cache and from there to
/// status messages / log lines that don't sanitize at render. The
/// drift modal's separate `sanitizeForDialog` wrap is the
/// belt-and-suspenders render-time defense for paths that pre-date
/// this entry-time filter.
pub fn validate_font_family(family: &str) -> Result<(), String> {
    if family.is_empty() {
        return Err("Font family name is empty".to_string());
    }
    if family.chars().count() > 256 {
        return Err("Font family name exceeds 256 characters".to_string());
    }
    if family.chars().any(|c| {
        c.is_control()
            || matches!(
                c,
                // Bidi controls (Trojan-Source class).
                '\u{200E}' | '\u{200F}'
                | '\u{202A}'..='\u{202E}'
                | '\u{2066}'..='\u{2069}'
                // Zero-width / invisible.
                | '\u{200B}'..='\u{200D}'
                | '\u{2060}'
                | '\u{180E}'
                | '\u{FEFF}'
                // Round 6 Wave 6.2 parity sweep — these three were
                // present in `validate_ipc_path` and the TS
                // `unicode-controls` set but missing here, leaving a
                // gap where a family name carrying any of them would
                // be accepted by validate_font_family then fail
                // validate_ipc_path downstream (or worse: never reach
                // ipc validation if the family flows only to UI / log
                // sites). U+2028 / U+2029 are line / paragraph
                // separators (Zl / Zp — `is_control()` is Cc only and
                // misses them); U+061C is the Arabic Letter Mark, a
                // Cf bidi format char.
                | '\u{2028}' | '\u{2029}'
                | '\u{061C}'
            )
    }) {
        return Err("Font family name contains invalid characters".to_string());
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

    // ── validate_ipc_path: per-guard pins ──
    //
    // The integration test in fonts.rs feeds an array of invalid paths
    // and asserts the streaming scan skips them all silently. That's
    // good coverage of the streaming contract but doesn't pin WHICH
    // guard rejects each shape — a refactor that moves the length
    // check elsewhere OR raises MAX_IPC_PATH_LEN would still see that
    // test pass while breaking the per-guard contract. The per-guard
    // tests below name each rejection reason in the test title so a
    // future refactor can't conflate them. Empty + oversized share
    // one guard (single if-branch with `||`), so they get one combined
    // test that asserts the rejection message mentions the byte range
    // constraint.

    #[test]
    fn validate_rejects_empty_path() {
        let err = validate_ipc_path("", "Test").unwrap_err();
        // Empty hits the same guard as oversized — the message names
        // the byte range (1-MAX_IPC_PATH_LEN).
        assert!(err.contains(&MAX_IPC_PATH_LEN.to_string()));
    }

    #[test]
    fn validate_rejects_oversized_path() {
        let err = validate_ipc_path(&"x".repeat(MAX_IPC_PATH_LEN + 1), "Test").unwrap_err();
        assert!(err.contains(&MAX_IPC_PATH_LEN.to_string()));
    }

    #[test]
    fn validate_rejects_control_char_in_path() {
        let err = validate_ipc_path("has\u{0000}control.ass", "Test").unwrap_err();
        // Different guard than length — uses "invalid characters" wording.
        assert!(err.to_lowercase().contains("invalid"));
    }

    #[test]
    fn validate_rejects_bidi_override_in_path() {
        // U+202E RIGHT-TO-LEFT OVERRIDE (Trojan-Source class). `is_control()`
        // is Cc-only; the explicit bidi enumeration in validate_ipc_path's
        // matches! block is what catches this. A refactor that dropped the
        // bidi arms while leaving the control-char arm would slip past every
        // existing test until this pin.
        let err = validate_ipc_path("evil\u{202E}.ass", "Test").unwrap_err();
        assert!(err.to_lowercase().contains("invalid"));
    }

    #[test]
    fn validate_rejects_line_separator_in_path() {
        // U+2028 LINE SEPARATOR — Zl category, not Cc. validate_ipc_path
        // calls them out explicitly because several Rust path libraries
        // treat them ambiguously across platforms.
        let err = validate_ipc_path("multi\u{2028}line.ass", "Test").unwrap_err();
        assert!(err.to_lowercase().contains("invalid"));
    }

    #[test]
    fn validate_rejects_arabic_letter_mark_in_path() {
        // U+061C ARABIC LETTER MARK — Cf bidi format char. Not in
        // is_control() (Cc) and was originally absent from the
        // explicit matches! list, leaving a gap with the TS
        // unicode-controls set and `validate_font_family` (which also
        // missed it pre-Round-6). Round 6 Wave 6.2 parity sweep
        // closed both sides.
        let err = validate_ipc_path("evil\u{061C}.ass", "Test").unwrap_err();
        assert!(err.to_lowercase().contains("invalid"));
    }

    #[test]
    fn validate_accepts_normal_path() {
        validate_ipc_path("C:\\fonts\\sample.ttf", "Test").expect("normal path should validate");
    }

    #[test]
    fn validate_rejects_dotdot_segment_in_path() {
        // Closes a fs:scope bypass: a raw `C:\Allowed\..\Denied\file.ass`
        // could pass an `allow=**` literal match and let the OS resolve the
        // `..` at read time inside encoding.rs's canonicalize-fails branch.
        // Rejecting `..` segments at IPC entry kills the vector for every
        // callsite (encoding / fonts / safe_io / dropzone).
        let err = validate_ipc_path(r"C:\Allowed\..\Denied\file.ass", "Test").unwrap_err();
        assert!(err.to_lowercase().contains("parent-directory"));
        // Forward-slash form (POSIX + Windows mixed paths) also rejected.
        let err2 = validate_ipc_path("/home/u/../etc/passwd", "Test").unwrap_err();
        assert!(err2.to_lowercase().contains("parent-directory"));
    }

    // Round 8 A-R8-A3-8 — Windows reserved-device-name reject (per
    // segment, case-insensitive, pre-first-dot stem, trailing-ws/dot
    // strip). The TS-side `assertSafeOutputFilename` already rejects
    // these; this pins parity for non-TS callers (CLI argv, future
    // engine).
    #[test]
    fn validate_rejects_reserved_segment_anywhere_in_path() {
        let err = validate_ipc_path(r"C:\foo\CON.ass", "Test").unwrap_err();
        assert!(err.to_lowercase().contains("reserved"));
        let err2 = validate_ipc_path(r"C:\NUL.txt", "Test").unwrap_err();
        assert!(err2.to_lowercase().contains("reserved"));
    }

    #[test]
    fn validate_rejects_reserved_with_trailing_space_or_dot() {
        // `CON ` and `CON.` both resolve to the device on Win32; the
        // trailing-whitespace + trailing-dot strip catches them.
        let err1 = validate_ipc_path(r"C:\foo\CON .ass", "Test").unwrap_err();
        assert!(err1.to_lowercase().contains("reserved"));
        let err2 = validate_ipc_path(r"C:\foo\CON..ass", "Test").unwrap_err();
        assert!(err2.to_lowercase().contains("reserved"));
    }

    #[test]
    fn validate_rejects_com_and_lpt_digit_variants() {
        for name in ["COM1", "COM9", "LPT0", "LPT5"] {
            let path = format!(r"C:\foo\{name}.ass");
            let err = validate_ipc_path(&path, "Test").unwrap_err();
            assert!(
                err.to_lowercase().contains("reserved"),
                "{path} should reject as reserved"
            );
        }
    }

    #[test]
    fn validate_accepts_non_reserved_lookalikes() {
        // Names that LOOK reserved but aren't (CON has 3 chars; CONS is
        // 4; COMA isn't COM+digit; LPTX isn't LPT+digit). Pin so a
        // future refactor doesn't widen the match accidentally.
        validate_ipc_path(r"C:\foo\CONS.ass", "Test").expect("CONS is not reserved");
        validate_ipc_path(r"C:\foo\COMA.ass", "Test").expect("COMA is not reserved");
        validate_ipc_path(r"C:\foo\LPTX.ass", "Test").expect("LPTX is not reserved");
    }

    #[test]
    fn validate_accepts_dotdot_inside_filename() {
        // Substring `..` inside a filename component is legitimate
        // (`foo..bar.txt`), not a parent-dir traversal. The component-
        // level split-and-compare distinguishes them.
        validate_ipc_path("C:\\fonts\\foo..bar.ttf", "Test")
            .expect("dotdot inside a name segment should pass");
        validate_ipc_path("/home/u/file..name.ass", "Test")
            .expect("dotdot inside a name segment should pass on POSIX shape too");
    }

    // ── validate_ipc_path: byte-prefix DOS-device check ──
    // (Byte-level eq_ignore_ascii_case avoids per-call String alloc;
    // pre-consolidation form called to_ascii_lowercase on the full
    // path string just to compare ≤16-byte prefixes.)

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

    // ── strip_visual_line_breaks ──

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

    // ── validate_font_family ──

    #[test]
    fn validate_font_family_accepts_normal_name() {
        validate_font_family("Arial").expect("Arial should validate");
        validate_font_family("微软雅黑").expect("CJK name should validate");
        // 256 codepoint boundary — exactly 256 chars is OK.
        let exactly_256 = "x".repeat(256);
        validate_font_family(&exactly_256).expect("256 chars boundary OK");
    }

    #[test]
    fn validate_font_family_rejects_empty() {
        let err = validate_font_family("").unwrap_err();
        assert!(err.contains("empty"));
    }

    #[test]
    fn validate_font_family_rejects_overlong() {
        let err = validate_font_family(&"x".repeat(257)).unwrap_err();
        assert!(err.contains("exceeds 256"));
    }

    #[test]
    fn validate_font_family_rejects_c0_control() {
        let err = validate_font_family("Ari\x01al").unwrap_err();
        assert!(err.contains("invalid"));
    }

    #[test]
    fn validate_font_family_rejects_del() {
        // U+007F is in is_control()'s set; the previous duplicated
        // `c == '\x7f'` clause was redundant. Pin that fact here so
        // a future "just use a regex" refactor doesn't lose the
        // coverage.
        let err = validate_font_family("Ari\x7fal").unwrap_err();
        assert!(err.contains("invalid"));
    }

    #[test]
    fn validate_font_family_rejects_c1_nel() {
        // NEL is in is_control()'s set.
        let err = validate_font_family("Ari\u{0085}al").unwrap_err();
        assert!(err.contains("invalid"));
    }

    #[test]
    fn validate_font_family_rejects_bidi_override() {
        // U+202E RTL OVERRIDE — Trojan-Source class. `is_control()`
        // (Cc) doesn't match this codepoint (Cf). Round 4 extension
        // mirrors `validate_ipc_path`'s rejection set.
        let err = validate_font_family("Ari\u{202E}al").unwrap_err();
        assert!(err.contains("invalid"));
    }

    #[test]
    fn validate_font_family_rejects_zero_width() {
        // U+200B ZERO WIDTH SPACE — two visually-identical family
        // names `Arial` and `Ari\u{200B}al` would resolve to distinct
        // session-DB rows; the validator must reject upstream.
        let err = validate_font_family("Ari\u{200B}al").unwrap_err();
        assert!(err.contains("invalid"));
    }

    #[test]
    fn validate_font_family_rejects_line_paragraph_separators() {
        // U+2028 / U+2029 are Zl / Zp, not Cc — pre-Round-6 the
        // matches! list omitted them, so a family name carrying
        // either would slip past validate_font_family and then trip
        // validate_ipc_path on the path side. Round 6 Wave 6.2 sweep
        // closes the asymmetry.
        let err1 = validate_font_family("Ari\u{2028}al").unwrap_err();
        assert!(err1.contains("invalid"));
        let err2 = validate_font_family("Ari\u{2029}al").unwrap_err();
        assert!(err2.contains("invalid"));
    }

    #[test]
    fn validate_font_family_rejects_arabic_letter_mark() {
        // U+061C — Cf bidi format char; same parity rationale as
        // above. The TS unicode-controls set has had it since Round 5
        // Wave 5.1.
        let err = validate_font_family("Ari\u{061C}al").unwrap_err();
        assert!(err.contains("invalid"));
    }
}
