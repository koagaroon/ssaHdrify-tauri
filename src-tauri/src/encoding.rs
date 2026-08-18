//! Encoding detection and decoding for subtitle files.
//!
//! Strategy: BOM detection first, then conservative BOM-less UTF-16
//! inference, then chardetng for the remaining encodings.
//! Returns UTF-8 text + detected encoding name so the frontend always gets
//! clean Unicode regardless of the original file encoding.

use crate::util::is_reparse_point;
use chardetng::EncodingDetector;
use sha2::{Digest, Sha256};
use std::fmt::Write as FmtWrite;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};

/// Verify a path's extension is in `ALLOWED_TEXT_EXTENSIONS`. Case-folded.
fn ext_is_allowed(path: &Path) -> bool {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        // ASCII-only — every entry in ALLOWED_TEXT_EXTENSIONS is ASCII,
        // so to_ascii_lowercase is correct AND avoids the locale-aware
        // allocations to_lowercase performs.
        .map(|e| e.to_ascii_lowercase())
        .unwrap_or_default();
    ALLOWED_TEXT_EXTENSIONS.contains(&ext.as_str())
}

pub(crate) const MAX_TEXT_SIZE: u64 = 50 * 1024 * 1024; // 50 MB

fn source_revision(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let mut revision = String::with_capacity(digest.len() * 2);
    for byte in digest {
        write!(&mut revision, "{byte:02x}").expect("writing to String cannot fail");
    }
    revision
}

fn decoded_result(
    text: String,
    encoding: String,
    encoding_id: &str,
    had_bom: bool,
    lossy: bool,
    inferred_without_bom: bool,
    source_bytes: &[u8],
) -> ReadTextResult {
    ReadTextResult {
        text,
        encoding,
        encoding_id: encoding_id.to_string(),
        had_bom,
        lossy,
        inferred_without_bom,
        source_revision: source_revision(source_bytes),
        source_byte_length: source_bytes.len() as u64,
    }
}

/// Allowed subtitle file extensions for `read_text_detect_encoding` AND
/// the safe_io write/copy/rename commands — read and write sides must
/// agree on what counts as a subtitle file, so this is the single
/// source of truth for both. Defense-in-depth: the frontend only sends
/// paths from file dialogs, but this prevents the IPC commands from
/// being repurposed as a generic file reader / writer. `.txt` is
/// intentionally excluded — the frontend dialogs never offer it, and
/// keeping it in the allow-list would widen arbitrary-read via any JS
/// bug.
///
/// Kept in lockstep with TS `SUBTITLE_EXTS` in
/// `src/lib/rename-extensions.ts`. The TS set was narrowed to what
/// `subtitle-parser.ts::detectFormat` actually handles (dropping
/// `sbv` and `lrc`); this Rust set was missed in that earlier
/// alignment. The two sides agreed on a 7-entry superset, then
/// drifted to TS 5 / Rust 7. Now realigned at 5: read and write
/// IPC commands refuse `.sbv` / `.lrc` destinations the same way
/// the TS folder-drop filter routes those extensions to the
/// "unknown" bucket.
pub(crate) const ALLOWED_TEXT_EXTENSIONS: &[&str] = &["ass", "ssa", "srt", "vtt", "sub"];

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
pub(crate) fn decode_bytes(bytes: &[u8]) -> Result<ReadTextResult, String> {
    // 1. BOM detection
    if let Some(result) = detect_bom(bytes) {
        return Ok(result);
    }

    // 2. Conservative BOM-less UTF-16 inference. chardetng intentionally
    // does not consider UTF-16, and NUL bytes are valid UTF-8, so an
    // ASCII-heavy UTF-16 subtitle would otherwise be returned as NUL-filled
    // mojibake. Only accept a strong byte-lane pattern plus recognizable
    // subtitle structure; ambiguous data fails instead of being rewritten.
    if let Some(result) = detect_bomless_utf16(bytes)? {
        return Ok(result);
    }

    // 3. chardetng heuristic
    //
    // chardetng 1.0 broke two API points compared with 0.1:
    //   - `EncodingDetector::new()` now takes an `Iso2022JpDetection`
    //     argument controlling whether ISO-2022-JP is even considered.
    //     `Allow` reproduces 0.1's always-on behavior (subtitle files
    //     occasionally land in this encoding for older Japanese sources).
    //   - `guess()`'s second arg switched from `bool` to a two-variant
    //     `Utf8Detection` enum. `Allow` matches the old `true` (UTF-8 is
    //     a permissible guess result).
    let mut detector = EncodingDetector::new(chardetng::Iso2022JpDetection::Allow);
    detector.feed(bytes, true);
    // First arg = top-level domain hint (None = no hint). chardetng can
    // bias detection toward the script associated with a given TLD
    // (e.g., `.cn` → CJK preference); we have no domain context for a
    // local file path, so pass None and let the byte-distribution
    // heuristic stand on its own.
    let encoding = detector.guess(None, chardetng::Utf8Detection::Allow);

    let (cow, _, had_errors) = encoding.decode(bytes);
    if had_errors {
        // chardetng picked an encoding but decoding hit invalid sequences.
        // Record the attempted encoding in the label so callers can see what
        // was tried — plain "UTF-8 (lossy)" masked whether the file was
        // actually UTF-8 or some other guess that failed.
        //
        // Use `cow` (the chardetng-decoded text with U+FFFD on bad bytes
        // in the chosen encoding) — NOT a fresh UTF-8-lossy decode of the
        // original bytes. For e.g. a GBK file with a few bad bytes, the
        // UTF-8-lossy fallback would label "GBK (lossy)" but actually
        // return UTF-8-lossy mojibake of GBK bytes — content and label
        // disagree, and the content is much worse than `cow` already
        // contained.
        return Ok(decoded_result(
            cow.into_owned(),
            format!("{} (lossy)", encoding.name()),
            encoding.name(),
            false,
            true,
            false,
            bytes,
        ));
    }

    Ok(decoded_result(
        cow.into_owned(),
        encoding.name().to_string(),
        encoding.name(),
        false,
        false,
        false,
        bytes,
    ))
}

/// Result of reading a text file with encoding detection.
#[derive(Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReadTextResult {
    /// File content decoded to UTF-8
    pub text: String,
    /// Detected encoding name (e.g. "UTF-8", "GBK", "Big5", "Shift_JIS")
    pub encoding: String,
    /// Stable encoding_rs identifier used by the style editor's strict writer.
    pub encoding_id: String,
    /// Whether the original byte stream began with a supported BOM.
    pub had_bom: bool,
    /// Whether decoding replaced malformed source bytes.
    pub lossy: bool,
    /// True only when UTF-16LE/BE was inferred conservatively without a BOM.
    pub inferred_without_bom: bool,
    /// SHA-256 of the exact source bytes used for this decoded result.
    pub source_revision: String,
    /// Exact byte length used for bounded batch planning in the GUI.
    pub source_byte_length: u64,
}

/// Read a file, detect its encoding, and return UTF-8 text + encoding
/// name. Inner implementation parameterized over the fs:scope policy.
///
/// Detection order:
/// 1. BOM (UTF-8, UTF-16 LE/BE) — deterministic, highest priority
/// 2. Conservative BOM-less UTF-16 inference from byte-lane structure
/// 3. chardetng heuristic — handles GBK, Big5, Shift_JIS, EUC-KR, etc.
///
/// The Tauri command (`read_text_detect_encoding`) wraps this with the
/// app-owned fs scope; the CLI binary (which has no Tauri app
/// handle and treats argv-provided paths as the trust surface) passes
/// an allow-all closure. Same shape as `safe_io`'s `*_inner` helpers —
/// keeps the policy gate testable without an `AppHandle` mock.
pub fn read_text_detect_encoding_inner(
    path: &str,
    is_allowed: impl Fn(&Path) -> bool,
) -> Result<ReadTextResult, String> {
    // Length and content guards on the IPC-supplied path itself. Reject
    // obviously-hostile or pathological shapes BEFORE touching the
    // filesystem. Control chars / NUL in a path on Windows can truncate
    // the access target at the null byte; zero-width and bidi-control
    // characters are blocked here too. `..` segments are rejected
    // upstream by `validate_ipc_path` (see
    // util.rs § "Reject path-traversal segments"), so the canonicalize-
    // fails fallback discussion further down doesn't apply to `..` —
    // that branch only covers symlink-redirect cases that survive the
    // ipc-path validator. The OS-level `..`-resolution notes below
    // (CreateFileW on Windows / openat on Unix) are kept for general
    // operational reference, but they aren't a live defense gap here.
    crate::util::validate_ipc_path(path, "Subtitle")?;

    // Extension validation: only allow subtitle/text file types
    let path_ref = Path::new(path);
    if !ext_is_allowed(path_ref) {
        let ext = path_ref
            .extension()
            .and_then(|e| e.to_str())
            .map(|e| e.to_ascii_lowercase())
            .unwrap_or_default();
        // Phrase the error so an empty extension reads naturally
        // ("Unsupported file type: (no extension)") rather than the
        // bare ". " trailing-dot artifact.
        let label = if ext.is_empty() {
            "(no extension)".to_string()
        } else {
            format!(".{ext}")
        };
        return Err(format!("Unsupported file type: {label}"));
    }

    // fs_scope consultation: mirror safe_io's policy on the READ side so
    // read + write enforce the same allow/deny set. Without this, a
    // misbehaving frontend or compromised WebView could read files in
    // scope-denied locations (e.g. `$HOME/.ssh/id_rsa`) through the
    // subtitle reader even though the symmetric write path would refuse.
    // First pass checks the RAW path so a directly-typed deny-listed
    // path fails fast. A second pass after canonicalize (below) catches
    // symlink redirection that resolves into a deny-listed location.
    //
    // A previously-leaked path was `C:\Allowed\..\Denied\file.ass`:
    // `is_allowed` string-matched the `..`-bearing form (passed
    // allow=**), then the canonicalize-fails fallback at the bottom of
    // the match below read via the raw path, so the OS resolved the
    // `..` into a deny-listed target. Closed at the IPC entry by
    // `validate_ipc_path` (called above), which now rejects `..` as a
    // path component. The fs_scope re-check on the canonical form
    // below remains as defense-in-depth for symlink-redirect cases
    // that don't involve `..`.
    if !is_allowed(path_ref) {
        return Err(format!(
            "Subtitle path is denied by the application's filesystem scope \
             policy: {}",
            path_ref.display()
        ));
    }

    // Resolve symlinks / reparse points. Two attack surfaces drive the
    // checks below:
    //   (1) A plain symlink `foo.ass` → `C:/Users/<u>/.ssh/id_rsa`. The
    //       extension allow-list above only sees the symlink's own name,
    //       so without a second check we'd silently read the target file.
    //   (2) An NTFS junction with the same shape (OneDrive-redirected
    //       Documents, or a deliberate `mklink /J`). Rust's `is_symlink()`
    //       returns FALSE for junctions (IO_REPARSE_TAG_MOUNT_POINT), so a
    //       junction-based bypass slips past a naive symlink check.
    //
    // Defense: re-validate the CANONICAL path's extension after canonicalize
    // succeeds — a malicious symlink to `SAM` resolves to a non-subtitle
    // path that fails the allow-list. Legitimate OneDrive placeholders
    // resolve to same-named subtitle files and still pass.
    //
    // When canonicalize FAILS (some OneDrive cloud-only placeholders, some
    // network shares), fall back to the raw path ONLY if the raw path is
    // not itself a reparse point — the `is_reparse_point` helper uses the
    // raw `FILE_ATTRIBUTE_REPARSE_POINT` bit on Windows to catch junctions
    // that `is_symlink()` misses.
    let read_path: PathBuf = match path_ref.canonicalize() {
        Ok(canonical) => {
            if !ext_is_allowed(&canonical) {
                return Err(
                    "Resolved path is not a subtitle file (symlink to disallowed target?)"
                        .to_string(),
                );
            }
            // Post-canonicalize scope re-check: a same-name reparse point
            // can pass the raw `is_allowed` (e.g. user-picked `foo.ass`
            // in a scope-allowed dir) and resolve into a deny-listed
            // target (`~/.ssh/id_rsa.ass`). Catch that path here.
            if !is_allowed(&canonical) {
                return Err(format!(
                    "Resolved subtitle path is denied by the application's \
                     filesystem scope policy: {}",
                    canonical.display()
                ));
            }
            canonical
        }
        Err(e) => {
            // Log-level discrimination: warn only when the read is refused.
            // Reparse-point + canonicalize-failure is the path that
            // genuinely refuses the read; WARN names the user-visible
            // action. The non-reparse fallback succeeds with the raw
            // path (common on network-mapped Z: drives, SUBST, OneDrive
            // cloud-only — Rust's GetFinalPathNameByHandle returns
            // ERROR_INVALID_PARAMETER on those filesystems, an upstream
            // Windows-API limitation, not our bug). DEBUG keeps it
            // available with `RUST_LOG=debug` without alarming default
            // users every invocation.
            if is_reparse_point(path_ref) {
                log::warn!(
                    "Refusing to read possible symlink / junction (canonicalize failed: {e})"
                );
                return Err(
                    "Refusing to read symlink / junction when canonicalize fails".to_string(),
                );
            }
            // no second `is_allowed` re-check on
            // `path_ref` here, even though the Ok-arm above does
            // double-check (pre-canonicalize at line 188, post-
            // canonicalize on `read_path` below). The pre-canonicalize
            // `is_allowed(path_ref)` at the function entry already
            // gated this exact path; `validate_ipc_path` (line 149)
            // rejected the BiDi / control / DOS-device shapes that
            // would let a crafted argv bypass the scope policy. The
            // canonicalize-fail branch ends with the SAME path the
            // pre-check accepted, so a third re-check would test the
            // identical predicate twice without closing any window.
            log::debug!("canonicalize failed; falling back to raw path: {e}");
            path_ref.to_path_buf()
        }
    };

    // Re-check `is_reparse_point` on the resolved `read_path` right
    // before the stat. The arms above checked the pre-canonicalize
    // `path_ref`; on attacker-controlled local filesystems a swap
    // between that check and the stat below would slip a reparse
    // point past the upstream gate . Bounded by the
    // same single-user trust model as the size-check TOCTOU note
    // below, but the re-check is cheap (one syscall) and parallels
    // the symmetric scrub `lib.rs::one_line` already does for the
    // rfd dialog path.
    if is_reparse_point(&read_path) {
        log::warn!(
            "Refusing to read possible symlink / junction at stat-time: {}",
            read_path.display()
        );
        return Err("Refusing to read symlink / junction (race-time detection)".to_string());
    }

    // Size check.
    //
    // TOCTOU note: there's a small window between this stat and the
    // `std::fs::read` below where the file could be swapped for a
    // larger one, defeating the size cap. We accept the race because
    // (a) the threat model is "user picked the file, no concurrent
    // attacker on this local machine," (b) `std::fs::read` itself
    // would still cap at the OS's per-syscall read limits, and (c)
    // Rust's `Vec::reserve` plus the read loop would surface OOM as
    // a normal Err instead of a crash. The pre-read `is_file()` check
    // immediately below covers the race-target being a non-file at
    // stat time; a race-replaced target between stat and read still
    // produces a normal `std::fs::read` error on a directory / pipe.
    let metadata = std::fs::metadata(&read_path).map_err(|e| sanitize_io_error(&e, "stat"))?;
    // Must be a regular file — directories, FIFOs, and device files
    // (Unix /dev/urandom, raw devices the FS reports as non-file)
    // would otherwise produce crashes or unbounded reads. Windows DOS
    // device namespaces (`\\.\PhysicalDrive0` and `\\?\GLOBALROOT\…`)
    // are already rejected upstream by `validate_ipc_path`; this
    // defense-in-depth check covers everything else.
    if !metadata.file_type().is_file() {
        return Err("Path does not point to a regular file".to_string());
    }
    if metadata.len() > MAX_TEXT_SIZE {
        let size_mb = metadata.len() as f64 / (1024.0 * 1024.0);
        return Err(format!(
            "File too large: {size_mb:.1} MB exceeds the 50 MB limit"
        ));
    }

    let bytes = std::fs::read(&read_path).map_err(|e| sanitize_io_error(&e, "read"))?;

    // Post-read size check (TOCTOU mitigation — file could grow between stat and read)
    if bytes.len() as u64 > MAX_TEXT_SIZE {
        let size_mb = bytes.len() as f64 / (1024.0 * 1024.0);
        return Err(format!(
            "File too large after read: {size_mb:.1} MB exceeds the 50 MB limit"
        ));
    }

    if bytes.starts_with(&[0xFF, 0xFE, 0x00, 0x00]) || bytes.starts_with(&[0x00, 0x00, 0xFE, 0xFF])
    {
        return Err("UTF-32 subtitle encoding is not supported".to_string());
    }

    decode_bytes(&bytes)
}

/// Blocking command implementation. The async Tauri boundary in
/// `ipc_commands` moves scope resolution, file reading, and decoding to the
/// blocking pool before calling this function.
pub fn read_text_detect_encoding(
    app: tauri::AppHandle,
    path: String,
) -> Result<ReadTextResult, String> {
    let scope = crate::fs_policy::app_fs_scope(&app)?;
    read_text_detect_encoding_inner(&path, move |p| scope.is_allowed(p))
}

fn looks_like_subtitle_text(text: &str) -> bool {
    if text
        .chars()
        .any(|ch| ch.is_control() && !matches!(ch, '\t' | '\r' | '\n'))
        || text
            .chars()
            .filter(|ch| !ch.is_whitespace())
            .take(4)
            .count()
            < 4
    {
        return false;
    }

    text.lines().any(|line| {
        let trimmed = line.trim_start_matches('\u{feff}').trim();
        trimmed.starts_with("WEBVTT")
            || trimmed.eq_ignore_ascii_case("[Script Info]")
            || trimmed.eq_ignore_ascii_case("[Events]")
            || trimmed.eq_ignore_ascii_case("[V4+ Styles]")
            || trimmed.eq_ignore_ascii_case("[V4 Styles]")
            || trimmed
                .get(..9)
                .is_some_and(|prefix| prefix.eq_ignore_ascii_case("Dialogue:"))
            || (trimmed.contains("-->")
                && trimmed.contains(':')
                && (trimmed.contains('.') || trimmed.contains(',')))
            || (trimmed.starts_with('{') && trimmed.contains("}{"))
    })
}

const UTF32_PROBE_BUDGET_BYTES: usize = 64 * 1024;
const UTF32_PROBE_WINDOW_COUNT: usize = 3;

fn utf32_probe_windows(bytes: &[u8]) -> Vec<&[u8]> {
    if bytes.len() <= UTF32_PROBE_BUDGET_BYTES {
        return vec![bytes];
    }

    // Spread the existing 64 KiB probe budget across the beginning, middle,
    // and end. Subtitle files can have a long CJK preamble before the first
    // timing/header marker; probing only the prefix misses UTF-32 in that
    // case because BMP CJK text naturally has just two NUL-heavy byte lanes.
    // Every boundary remains four-byte aligned so decoding never starts in
    // the middle of a UTF-32 scalar.
    let window_len = (UTF32_PROBE_BUDGET_BYTES / UTF32_PROBE_WINDOW_COUNT) & !3;
    let middle_start = ((bytes.len() - window_len) / 2) & !3;
    let suffix_start = bytes.len() - window_len;

    vec![
        &bytes[..window_len],
        &bytes[middle_start..middle_start + window_len],
        &bytes[suffix_start..],
    ]
}

fn utf32_lane_meets_nul_ratio(
    probes: &[&[u8]],
    lane: usize,
    units: usize,
    minimum_tenths: usize,
) -> bool {
    probes
        .iter()
        .map(|probe| {
            probe
                .iter()
                .skip(lane)
                .step_by(4)
                .filter(|byte| **byte == 0)
                .count()
        })
        .sum::<usize>()
        * 10
        >= units * minimum_tenths
}

fn is_cjk_scalar(ch: char) -> bool {
    matches!(
        ch as u32,
        0x1100..=0x11FF
            | 0x2E80..=0x2FFF
            | 0x3040..=0x30FF
            | 0x31F0..=0x31FF
            | 0x3400..=0x4DBF
            | 0x4E00..=0x9FFF
            | 0xAC00..=0xD7AF
            | 0xF900..=0xFAFF
    )
}

fn is_plausible_text_scalar(ch: char) -> bool {
    ch.is_alphanumeric()
        || ch.is_whitespace()
        || ch.is_ascii_graphic()
        || matches!(
            ch as u32,
            0x0300..=0x036F | 0x2000..=0x206F | 0x3000..=0x303F | 0xFF01..=0xFF65
        )
}

fn looks_like_cjk_utf32_scalar_text(probes: &[&[u8]], little_endian: bool) -> bool {
    let units = probes.iter().map(|probe| probe.len() / 4).sum::<usize>();
    if units < 32 {
        return false;
    }

    // Normal UTF-16 has alternating NUL lanes for ASCII and no NUL-heavy
    // lanes for CJK. It can satisfy this adjacent high-lane pair only when
    // almost every other UTF-16 code unit is NUL; that byte stream is also
    // the exact BMP UTF-32 representation, while its UTF-16 interpretation
    // contains control characters and is not acceptable subtitle text.
    let high_lanes = if little_endian { [2, 3] } else { [0, 1] };
    if !high_lanes
        .into_iter()
        .all(|lane| utf32_lane_meets_nul_ratio(probes, lane, units, 9))
    {
        return false;
    }

    let mut plausible = 0usize;
    let mut visible = 0usize;
    let mut cjk = 0usize;
    let mut line_breaks = 0usize;

    for unit in probes.iter().flat_map(|probe| probe.chunks_exact(4)) {
        let value = if little_endian {
            u32::from_le_bytes([unit[0], unit[1], unit[2], unit[3]])
        } else {
            u32::from_be_bytes([unit[0], unit[1], unit[2], unit[3]])
        };
        let Some(ch) = char::from_u32(value) else {
            return false;
        };
        if ch.is_control() && !matches!(ch, '\t' | '\r' | '\n') {
            return false;
        }

        plausible += usize::from(is_plausible_text_scalar(ch));
        if !ch.is_whitespace() {
            visible += 1;
            cjk += usize::from(is_cjk_scalar(ch));
        }
        line_breaks += usize::from(matches!(ch, '\r' | '\n'));
    }

    // This fallback is intentionally narrower than generic Unicode-text
    // detection. It covers the reported two-NUL-lane, CJK-heavy UTF-32 case
    // while leaving arbitrary four-byte binary and ordinary UTF-16 to their
    // existing paths. ASCII-heavy UTF-32 is handled by the three-lane check.
    line_breaks > 0 && visible >= 16 && plausible * 100 >= units * 95 && cjk * 4 >= visible
}

fn looks_like_bomless_utf32(bytes: &[u8]) -> bool {
    if bytes.len() < 16 || !bytes.len().is_multiple_of(4) {
        return false;
    }

    let probes = utf32_probe_windows(bytes);
    if [true, false].into_iter().any(|little_endian| {
        probes.iter().any(|probe| {
            decode_utf32_probe(probe, little_endian)
                .as_deref()
                .is_some_and(looks_like_subtitle_text)
        }) || looks_like_cjk_utf32_scalar_text(&probes, little_endian)
    }) {
        return true;
    }

    let units = probes.iter().map(|probe| probe.len() / 4).sum::<usize>();
    let high_nul_lanes = (0..4)
        .filter(|lane| utf32_lane_meets_nul_ratio(&probes, *lane, units, 8))
        .count();
    // ASCII-heavy UTF-32 has at least three nearly-empty byte lanes. UTF-16
    // has two, so requiring three avoids classifying ordinary UTF-16 text as
    // UTF-32 while the structural probe above still catches CJK-heavy UTF-32.
    high_nul_lanes >= 3
}

fn decode_utf32_probe(bytes: &[u8], little_endian: bool) -> Option<String> {
    bytes
        .chunks_exact(4)
        .map(|unit| {
            let value = if little_endian {
                u32::from_le_bytes([unit[0], unit[1], unit[2], unit[3]])
            } else {
                u32::from_be_bytes([unit[0], unit[1], unit[2], unit[3]])
            };
            char::from_u32(value)
        })
        .collect()
}

fn detect_bomless_utf16(bytes: &[u8]) -> Result<Option<ReadTextResult>, String> {
    if bytes.len() < 8 {
        return Ok(None);
    }

    if looks_like_bomless_utf32(bytes) {
        return Err("UTF-32 subtitle encoding is not supported".to_string());
    }

    let even_len = bytes.len() & !1;
    let even_bytes = &bytes[..even_len];

    const SAMPLE_BYTES: usize = 8 * 1024;
    let sample_len = even_bytes.len().min(SAMPLE_BYTES);
    let sample = &even_bytes[..sample_len];
    let even_nuls = sample.iter().step_by(2).filter(|byte| **byte == 0).count();
    let odd_nuls = sample
        .iter()
        .skip(1)
        .step_by(2)
        .filter(|byte| **byte == 0)
        .count();

    // NUL-lane evidence catches ordinary ASCII-heavy UTF-16. A fixed density
    // threshold is insufficient for Chinese-heavy captions, so candidate
    // selection also decodes a bounded prefix in both byte orders and looks
    // for actual subtitle structure. The wrong byte order turns ASCII syntax
    // into unrelated code points and therefore cannot satisfy that check.
    let likely_le = odd_nuls >= 4 && odd_nuls >= even_nuls.saturating_mul(4);
    let likely_be = even_nuls >= 4 && even_nuls >= odd_nuls.saturating_mul(4);

    const STRUCTURE_PROBE_BYTES: usize = 64 * 1024;
    let probe_len = even_bytes.len().min(STRUCTURE_PROBE_BYTES);
    let probe = &even_bytes[..probe_len];
    let le_structure = decode_utf16_probe(probe, true)
        .as_deref()
        .is_some_and(looks_like_subtitle_text);
    let be_structure = decode_utf16_probe(probe, false)
        .as_deref()
        .is_some_and(looks_like_subtitle_text);

    let little_endian = match (le_structure, be_structure, likely_le, likely_be) {
        (true, false, _, _) => Some(true),
        (false, true, _, _) => Some(false),
        (true, true, _, _) => {
            return Err("BOM-less UTF-16 byte order is ambiguous; add a BOM and retry".to_string());
        }
        (false, false, true, false) => Some(true),
        (false, false, false, true) => Some(false),
        _ => None,
    };

    let Some(little_endian) = little_endian else {
        return Ok(None);
    };
    let encoding_id = if little_endian {
        "UTF-16LE"
    } else {
        "UTF-16BE"
    };
    if !bytes.len().is_multiple_of(2) {
        return Err(format!(
            "File resembles BOM-less {encoding_id} but has an odd byte length (truncated UTF-16 data)"
        ));
    }

    let (cow, had_errors) = if little_endian {
        let (cow, had_errors) = encoding_rs::UTF_16LE.decode_without_bom_handling(bytes);
        (cow, had_errors)
    } else {
        let (cow, had_errors) = encoding_rs::UTF_16BE.decode_without_bom_handling(bytes);
        (cow, had_errors)
    };
    if had_errors {
        return Err(format!(
            "File resembles BOM-less {encoding_id} but contains invalid UTF-16 data"
        ));
    }

    let text = cow.into_owned();
    if !looks_like_subtitle_text(&text) {
        return Err(format!(
            "File resembles BOM-less {encoding_id}, but its subtitle structure could not be verified"
        ));
    }

    Ok(Some(decoded_result(
        text,
        format!("{encoding_id} (inferred, no BOM)"),
        encoding_id,
        false,
        false,
        true,
        bytes,
    )))
}

fn decode_utf16_probe(bytes: &[u8], little_endian: bool) -> Option<String> {
    let mut probe = bytes;
    if probe.len() >= 2 {
        let last = if little_endian {
            u16::from_le_bytes([probe[probe.len() - 2], probe[probe.len() - 1]])
        } else {
            u16::from_be_bytes([probe[probe.len() - 2], probe[probe.len() - 1]])
        };
        if (0xD800..=0xDBFF).contains(&last) {
            probe = &probe[..probe.len() - 2];
        }
    }

    let (decoded, had_errors) = if little_endian {
        encoding_rs::UTF_16LE.decode_without_bom_handling(probe)
    } else {
        encoding_rs::UTF_16BE.decode_without_bom_handling(probe)
    };
    (!had_errors).then(|| decoded.into_owned())
}

/// Check for Byte Order Mark and decode accordingly. When the decoded text
/// contained invalid sequences, the encoding label is suffixed with "(lossy)"
/// so the frontend can distinguish clean decodes from ones with U+FFFD
/// replacements.
fn detect_bom(bytes: &[u8]) -> Option<ReadTextResult> {
    // UTF-8 BOM (EF BB BF) — strip BOM, decode as UTF-8.
    // The is_err()-then-from_utf8_lossy pair walks the bytes twice: once
    // to validate, once to lossy-decode. Acceptable cost for the typical
    // path (small subtitle files, success branch is single-walk via
    // from_utf8_lossy's own validity check). If from_utf8_lossy ever
    // grew an "encountered errors" return signal, we could collapse to
    // a single walk.
    if bytes.starts_with(&[0xEF, 0xBB, 0xBF]) {
        let payload = &bytes[3..];
        let lossy = std::str::from_utf8(payload).is_err();
        let text = String::from_utf8_lossy(payload).into_owned();
        return Some(decoded_result(
            text,
            if lossy {
                "UTF-8 (BOM, lossy)".to_string()
            } else {
                "UTF-8 (BOM)".to_string()
            },
            "UTF-8",
            true,
            lossy,
            false,
            bytes,
        ));
    }

    // UTF-16 LE BOM (FF FE).
    //
    // The file-reading boundary rejects UTF-32LE's longer BOM before this
    // helper is called. Decode the remaining payload without another BOM
    // sniff so a legitimate leading U+FEFF character is not stripped or
    // interpreted as an encoding switch.
    if bytes.starts_with(&[0xFF, 0xFE]) {
        let (cow, had_errors) = encoding_rs::UTF_16LE.decode_without_bom_handling(&bytes[2..]);
        return Some(decoded_result(
            cow.into_owned(),
            if had_errors {
                "UTF-16LE (lossy)".to_string()
            } else {
                "UTF-16LE".to_string()
            },
            "UTF-16LE",
            true,
            had_errors,
            false,
            bytes,
        ));
    }

    // UTF-16 BE BOM (FE FF)
    if bytes.starts_with(&[0xFE, 0xFF]) {
        let (cow, had_errors) = encoding_rs::UTF_16BE.decode_without_bom_handling(&bytes[2..]);
        return Some(decoded_result(
            cow.into_owned(),
            if had_errors {
                "UTF-16BE (lossy)".to_string()
            } else {
                "UTF-16BE".to_string()
            },
            "UTF-16BE",
            true,
            had_errors,
            false,
            bytes,
        ));
    }

    None
}

// ── Tests ────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    /// Helper: read fixture file and decode
    fn decode_fixture(name: &str) -> ReadTextResult {
        let path = format!("{}/tests/fixtures/{}", env!("CARGO_MANIFEST_DIR"), name);
        let bytes =
            std::fs::read(&path).unwrap_or_else(|e| panic!("Cannot read fixture {name}: {e}"));
        decode_bytes(&bytes).unwrap_or_else(|e| panic!("Cannot decode fixture {name}: {e}"))
    }

    fn temp_file_path(name: &str, ext: &str) -> std::path::PathBuf {
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let mut path = std::env::temp_dir();
        path.push(format!(
            "ssahdrify_encoding_{name}_{}_{}.{}",
            std::process::id(),
            stamp,
            ext
        ));
        path
    }

    fn encode_utf16_without_bom(text: &str, little_endian: bool) -> Vec<u8> {
        text.encode_utf16()
            .flat_map(|unit| {
                if little_endian {
                    unit.to_le_bytes()
                } else {
                    unit.to_be_bytes()
                }
            })
            .collect()
    }

    fn encode_utf32_without_bom(text: &str, little_endian: bool) -> Vec<u8> {
        text.chars()
            .flat_map(|ch| {
                if little_endian {
                    (ch as u32).to_le_bytes()
                } else {
                    (ch as u32).to_be_bytes()
                }
            })
            .collect()
    }

    #[test]
    fn utf8_no_bom() {
        let result = decode_fixture("utf8.ass");
        assert_eq!(result.encoding, "UTF-8");
        // Pin that the encoding label does NOT mention BOM — the no-BOM
        // fixture must not be mis-classified as `UTF-8 (BOM)`. Bare
        // eq=="UTF-8" technically catches that already, but an explicit
        // not-contains is harder to break by accident if the label
        // string ever grows variants like "UTF-8 (UTF-8 BOM stripped)".
        assert!(!result.encoding.contains("BOM"));
        assert!(result.text.contains("中文字幕测试"));
        assert!(result.text.contains("[Script Info]"));
    }

    #[test]
    fn read_result_exposes_exact_source_revision_and_camel_case_wire_fields() {
        let result = decode_bytes(b"abc").unwrap();
        assert_eq!(result.source_byte_length, 3);
        assert_eq!(
            result.source_revision,
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );

        let json = serde_json::to_value(&result).unwrap();
        assert_eq!(json["encodingId"], "UTF-8");
        assert_eq!(json["hadBom"], false);
        assert_eq!(json["lossy"], false);
        assert_eq!(json["inferredWithoutBom"], false);
        assert_eq!(json["sourceByteLength"], 3);
        assert_eq!(json["sourceRevision"], result.source_revision);
        assert!(json.get("source_revision").is_none());
    }

    #[test]
    fn empty_after_bom_strip() {
        // A file containing ONLY a UTF-8 BOM and nothing else should
        // decode cleanly to an empty string with the BOM-stripped
        // label — not panic, not mis-detect as another encoding.
        let result = decode_bytes(&[0xEF, 0xBB, 0xBF]).unwrap();
        assert_eq!(result.encoding, "UTF-8 (BOM)");
        assert_eq!(result.text, "");
    }

    #[test]
    fn utf16be_with_bom() {
        // FE FF BOM + a few BE-encoded characters. Tests the UTF-16BE
        // branch which has no fixture file; the inline byte sequence
        // covers it without needing a new test asset.
        let mut bytes = vec![0xFE, 0xFF];
        // "AB" in UTF-16BE: 0x00 0x41, 0x00 0x42
        bytes.extend_from_slice(&[0x00, 0x41, 0x00, 0x42]);
        let result = decode_bytes(&bytes).unwrap();
        assert_eq!(result.encoding, "UTF-16BE");
        assert_eq!(result.text, "AB");
    }

    #[test]
    fn utf16_payload_leading_bom_character_is_preserved() {
        let le = decode_bytes(&[0xFF, 0xFE, 0xFF, 0xFE, 0x41, 0x00]).unwrap();
        assert_eq!(le.encoding, "UTF-16LE");
        assert_eq!(le.text, "\u{feff}A");

        let be = decode_bytes(&[0xFE, 0xFF, 0xFE, 0xFF, 0x00, 0x41]).unwrap();
        assert_eq!(be.encoding, "UTF-16BE");
        assert_eq!(be.text, "\u{feff}A");
    }

    #[test]
    fn infers_bomless_utf16le_only_after_subtitle_structure_check() {
        let source =
            "[Script Info]\r\n\r\n[Events]\r\nDialogue: 0,0:00:01.00,0:00:02.00,Default,Hello\r\n";
        let bytes = encode_utf16_without_bom(source, true);

        let result = decode_bytes(&bytes).unwrap();

        assert_eq!(result.text, source);
        assert_eq!(result.encoding, "UTF-16LE (inferred, no BOM)");
        assert_eq!(result.encoding_id, "UTF-16LE");
        assert!(!result.had_bom);
        assert!(!result.lossy);
        assert!(result.inferred_without_bom);
    }

    #[test]
    fn infers_bomless_utf16be_only_after_subtitle_structure_check() {
        let source = "WEBVTT\n\n00:00:01.000 --> 00:00:02.000\nHello\n";
        let bytes = encode_utf16_without_bom(source, false);

        let result = decode_bytes(&bytes).unwrap();

        assert_eq!(result.text, source);
        assert_eq!(result.encoding, "UTF-16BE (inferred, no BOM)");
        assert_eq!(result.encoding_id, "UTF-16BE");
        assert!(!result.had_bom);
        assert!(!result.lossy);
        assert!(result.inferred_without_bom);
    }

    #[test]
    fn infers_cjk_heavy_bomless_utf16_srt_and_microdvd() {
        let cjk = "中".repeat(100);
        let sources = [
            format!("1\n00:00:01,000 --> 00:00:02,000\n{cjk}\n"),
            format!("{{24}}{{48}}{cjk}\n"),
        ];

        for source in sources {
            for little_endian in [true, false] {
                let bytes = encode_utf16_without_bom(&source, little_endian);
                let result = decode_bytes(&bytes).unwrap();
                let expected_id = if little_endian {
                    "UTF-16LE"
                } else {
                    "UTF-16BE"
                };

                assert_eq!(result.text, source);
                assert_eq!(result.encoding_id, expected_id);
                assert!(result.inferred_without_bom);
            }
        }
    }

    #[test]
    fn infers_bomless_utf16_style_only_ass_and_ssa_documents() {
        let sources = [
            "[V4+ Styles]\nFormat: Name, Fontname\nStyle: Default,Arial\n",
            "[V4 Styles]\nFormat: Name, Fontname\nStyle: Default,Arial\n",
        ];

        for source in sources {
            for little_endian in [true, false] {
                let bytes = encode_utf16_without_bom(source, little_endian);
                let result = decode_bytes(&bytes).unwrap();

                assert_eq!(result.text, source);
                assert!(result.inferred_without_bom);
            }
        }
    }

    #[test]
    fn truncated_bomless_utf16_returns_a_targeted_error() {
        let source = "1\n00:00:01,000 --> 00:00:02,000\nHello\n";

        for little_endian in [true, false] {
            let mut bytes = encode_utf16_without_bom(source, little_endian);
            bytes.pop();
            let error = decode_bytes(&bytes).unwrap_err();

            assert!(error.contains("odd byte length"), "got: {error}");
            assert!(error.contains("truncated UTF-16"), "got: {error}");
        }
    }

    #[test]
    fn bomless_utf32_is_not_misclassified_as_utf16() {
        let source = "WEBVTT\n\n00:00:01.000 --> 00:00:02.000\nHello\n";
        for little_endian in [true, false] {
            let bytes = encode_utf32_without_bom(source, little_endian);
            let decoded = decode_bytes(&bytes);
            assert!(
                decoded
                    .as_ref()
                    .is_err_and(|error| error.contains("UTF-32")),
                "unexpected UTF-32 result: {decoded:?}"
            );
        }
    }

    #[test]
    fn rejects_bomless_utf32_when_structure_starts_after_prefix_probe_budget() {
        // The first subtitle marker starts immediately after the old 64 KiB
        // prefix-only probe. BMP CJK scalars have only two NUL-heavy lanes,
        // so the legacy three-NUL-lane fallback cannot identify it by itself.
        let source = format!(
            "{}\nWEBVTT\n\n00:00:01.000 --> 00:00:02.000\n正文\n",
            "中".repeat(UTF32_PROBE_BUDGET_BYTES / 4)
        );

        for little_endian in [true, false] {
            let bytes = encode_utf32_without_bom(&source, little_endian);
            assert!(looks_like_bomless_utf32(&bytes));

            let error = decode_bytes(&bytes).unwrap_err();
            assert!(error.contains("UTF-32"), "got: {error}");
        }
    }

    #[test]
    fn rejects_cjk_utf32_when_the_only_structure_is_between_probe_windows() {
        // About 1 MiB after UTF-32 encoding, with WEBVTT around byte 256 KiB.
        // None of the bounded head/middle/tail windows contains the marker;
        // the conservative scalar/lane check must identify the surrounding
        // CJK text without expanding the probe budget.
        let source = format!(
            "{}WEBVTT\n{}",
            "中\n".repeat(32 * 1024),
            "文\n".repeat(96 * 1024)
        );

        for little_endian in [true, false] {
            let bytes = encode_utf32_without_bom(&source, little_endian);
            let probes = utf32_probe_windows(&bytes);
            assert!(probes.iter().all(|probe| {
                !decode_utf32_probe(probe, little_endian)
                    .as_deref()
                    .is_some_and(looks_like_subtitle_text)
            }));
            assert!(looks_like_bomless_utf32(&bytes));

            let error = decode_bytes(&bytes).unwrap_err();
            assert!(error.contains("UTF-32"), "got: {error}");
        }
    }

    #[test]
    fn utf32_probe_windows_are_aligned_and_stay_within_the_existing_budget() {
        for len in [
            UTF32_PROBE_BUDGET_BYTES,
            UTF32_PROBE_BUDGET_BYTES + 4,
            UTF32_PROBE_BUDGET_BYTES * 4,
        ] {
            let bytes = vec![0; len];
            let probes = utf32_probe_windows(&bytes);

            assert!(probes.len() <= UTF32_PROBE_WINDOW_COUNT);
            assert!(
                probes.iter().map(|probe| probe.len()).sum::<usize>() <= UTF32_PROBE_BUDGET_BYTES
            );
            assert!(probe_alignment_is_valid(&bytes, &probes));
        }
    }

    fn probe_alignment_is_valid(source: &[u8], probes: &[&[u8]]) -> bool {
        probes.iter().all(|probe| {
            let offset = probe.as_ptr() as usize - source.as_ptr() as usize;
            offset.is_multiple_of(4) && probe.len().is_multiple_of(4)
        })
    }

    #[test]
    fn utf32_probe_rejects_utf8_utf16_binary_and_truncated_counterexamples() {
        let mut utf8_source = "WEBVTT\n\n00:00:01.000 --> 00:00:02.000\nText".to_string();
        while !utf8_source.len().is_multiple_of(4) {
            utf8_source.push(' ');
        }
        assert!(utf8_source.len().is_multiple_of(4));
        assert!(!looks_like_bomless_utf32(utf8_source.as_bytes()));
        assert_eq!(
            decode_bytes(utf8_source.as_bytes()).unwrap().encoding_id,
            "UTF-8"
        );

        let utf16_source = format!(
            "WEBVTT\n\n00:00:01.000 --> 00:00:02.000\n{}",
            "正文\n".repeat(12 * 1024)
        );
        for little_endian in [true, false] {
            let bytes = encode_utf16_without_bom(&utf16_source, little_endian);
            let probes = utf32_probe_windows(&bytes);
            assert!(!looks_like_cjk_utf32_scalar_text(&probes, true));
            assert!(!looks_like_cjk_utf32_scalar_text(&probes, false));
            assert!(!looks_like_bomless_utf32(&bytes));
            let result = decode_bytes(&bytes).unwrap();
            assert_eq!(result.text, utf16_source);
            assert!(result.inferred_without_bom);
        }

        // Four-byte records with two adjacent zero lanes can resemble BMP
        // UTF-32 at the byte level. Without subtitle structure, they remain
        // an ordinary binary counterexample and must not be rejected as
        // unsupported UTF-32.
        let binary: Vec<u8> = [[0xB1, 0x03, 0, 0], [0xB2, 0x03, 0, 0], [b'\n', 0, 0, 0]]
            .into_iter()
            .cycle()
            .take(1024)
            .flatten()
            .collect();
        assert!(!looks_like_bomless_utf32(&binary));

        let mut truncated =
            encode_utf32_without_bom("WEBVTT\n\n00:00:01.000 --> 00:00:02.000\nText\n", true);
        truncated.pop();
        assert!(!looks_like_bomless_utf32(&truncated));
    }

    #[test]
    fn alternating_zero_binary_is_not_accepted_as_utf16_subtitle_text() {
        let bytes = [b'A', 0, 1, 0, b'B', 0, 2, 0, b'C', 0, 3, 0];

        let error = decode_bytes(&bytes).unwrap_err();

        assert!(error.contains("subtitle structure"), "got: {error}");
    }

    #[test]
    fn reader_refuses_utf32_bom_instead_of_misdecoding_as_utf16() {
        let path = temp_file_path("utf32", "ass");
        std::fs::write(&path, [0xFF, 0xFE, 0x00, 0x00, b'A', 0x00, 0x00, 0x00]).unwrap();

        let err = read_text_detect_encoding_inner(&path.to_string_lossy(), |_| true).unwrap_err();

        assert!(err.contains("UTF-32"), "got: {err}");
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn read_text_refuses_sup_sidecar_extension() {
        let path = temp_file_path("read_sup", "sup");
        std::fs::write(&path, b"pgs-binary").unwrap();

        let err = match read_text_detect_encoding_inner(&path.to_string_lossy(), |_| true) {
            Ok(_) => panic!("expected .sup read to fail"),
            Err(err) => err,
        };

        assert!(err.contains("Unsupported file type: .sup"), "got: {err}");
        let _ = std::fs::remove_file(path);
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
