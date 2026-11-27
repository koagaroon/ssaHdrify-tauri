//! OpenType/TTC table inspection, `cmap` format-12 sanitization, legacy CJK
//! `name`-table decoding, and single-file local font parsing.

use super::*;
use crate::util::validate_ipc_path;
use std::borrow::Cow;
use std::collections::HashSet;
use std::fs;
use std::path::Path;

/// Per-face cap on the number of localized family-name variants kept
/// from the OpenType `name` table. Worst-case real-world fonts ship at
/// most ~5 variants (English plus Simplified Chinese, Traditional
/// Chinese, Japanese, Korean — e.g. Source Han / Noto CJK families); 8
/// keeps 60% margin over that without giving attacker-influenced font
/// packs room to bloat per-entry memory.
///
/// Full-face aliases share the same per-face cap and are stored as one
/// style-insensitive key each in SQLite, bounding each face to at most
/// 16 lookup rows (8 family rows + 8 alias rows).
pub(super) const MAX_FAMILY_VARIANTS_PER_FACE: usize = 8;
pub(super) const MAX_FACE_NAME_VARIANTS_PER_FACE: usize = 8;
/// Shared bound for both the skrifa and legacy OpenType `name` decoders.
/// A family name accepted by this app is at most 256 Unicode scalars, so a
/// record larger than 4 KiB cannot add a useful name. Keeping one limit for
/// both paths prevents a crafted table from escaping through whichever
/// decoder happens to understand its platform/encoding tuple.
pub(super) const MAX_NAME_RECORD_BYTES: usize = 4096;

/// Maximum unique name records whose string payload may be decoded for one
/// face. The OpenType record count is a u16, but payload offsets may overlap,
/// so an attacker can otherwise make tens of thousands of records repeatedly
/// decode the same large invalid string.
pub(super) const MAX_NAME_DECODE_ATTEMPTS_PER_FACE: usize = 256;
pub(super) const MAX_CMAP12_ENCODING_RECORDS_PER_FACE: usize = 1024;
pub(super) const MAX_CMAP12_SUBTABLES_PER_FACE: usize = 128;
pub(super) const MAX_CMAP12_GROUPS_PER_FACE: usize = 65_536;
pub(super) const MAX_CMAP12_REWRITE_OFFSETS_PER_FACE: usize = 4_096;

pub(super) fn bounded_font_family_name(chars: impl Iterator<Item = char>) -> Option<String> {
    // Stream-trim instead of `take(N).collect().trim()`: truncating before
    // validation can turn an overpadded name like "   ArialMalicious..." into
    // the fake alias "Arial". Keep at most the 256 codepoints that could be
    // accepted, count internal whitespace toward that cap, and ignore only
    // true leading/trailing whitespace.
    let mut name = String::new();
    let mut name_count = 0usize;
    let mut pending_ws = String::new();
    let mut pending_ws_count = 0usize;
    let mut seen_non_ws = false;

    for ch in chars {
        if !seen_non_ws && ch.is_whitespace() {
            continue;
        }
        if ch.is_whitespace() {
            pending_ws_count += 1;
            if name_count + pending_ws_count <= 256 {
                pending_ws.push(ch);
            }
            continue;
        }

        seen_non_ws = true;
        if name_count + pending_ws_count + 1 > 256 {
            return None;
        }
        name.push_str(&pending_ws);
        pending_ws.clear();
        name.push(ch);
        name_count += pending_ws_count + 1;
        pending_ws_count = 0;
    }

    // Guard counts CODEPOINTS, not bytes — a 100-char CJK family name
    // (300+ UTF-8 bytes) is perfectly legitimate.
    if name.is_empty() {
        return None;
    }
    // fold validate_font_family into this helper so
    // every call from `parse_local_font_file` (3 sites: family /
    // typographic-family / full-name+postscript fallback) automatically
    // rejects BiDi / zero-width-bearing names. Without the fold, a
    // crafted font pack with U+202E in its name table could land a row
    // in the
    // session DB / persistent cache, then surface in detection-grid
    // labels and log lines with the reversal undisturbed (the
    // unicode-controls sweep covers TS-side ASS \fn references but
    // name-table entries come in via this Rust path). Single-source
    // semantics — `validate_font_family` is the canonical rejection
    // predicate used elsewhere, calling it here means callers don't
    // each have to remember to revalidate.
    if crate::util::validate_font_family(&name).is_err() {
        return None;
    }
    Some(name)
}

pub(super) fn materialize_face_name_aliases(face_name_variants: HashSet<String>) -> Vec<String> {
    // FULL_NAME / POSTSCRIPT_NAME values identify a concrete face. Keep them
    // even when a font also repeats the same string in FAMILY_NAME; Dream Han
    // Serif W15+ does exactly that, and filtering the duplicate leaves only a
    // bold style-sensitive family row that ASS `Bold=0` cannot match.
    let mut face_names: Vec<String> = face_name_variants.into_iter().collect();
    face_names.sort();
    face_names
}

pub(super) fn read_be_u16(data: &[u8], offset: usize) -> Option<u16> {
    data.get(offset..offset + 2)
        .map(|bytes| u16::from_be_bytes([bytes[0], bytes[1]]))
}

pub(super) fn read_be_u32(data: &[u8], offset: usize) -> Option<u32> {
    data.get(offset..offset + 4)
        .map(|bytes| u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
}

pub(super) fn write_be_u32(data: &mut [u8], offset: usize, value: u32) -> Option<()> {
    data.get_mut(offset..offset + 4)?
        .copy_from_slice(&value.to_be_bytes());
    Some(())
}

pub(super) fn table_range(
    data: &[u8],
    face_offset: usize,
    tag: &[u8; 4],
) -> Option<(usize, usize)> {
    let table_count = usize::from(read_be_u16(data, face_offset + 4)?);
    let table_records = face_offset.checked_add(12)?;
    for index in 0..table_count {
        let record = table_records.checked_add(index.checked_mul(16)?)?;
        let record_tag = data.get(record..record + 4)?;
        if record_tag != tag {
            continue;
        }
        let offset = usize::try_from(read_be_u32(data, record + 8)?).ok()?;
        let len = usize::try_from(read_be_u32(data, record + 12)?).ok()?;
        let end = offset.checked_add(len)?;
        if end <= data.len() {
            return Some((offset, len));
        }
        return None;
    }
    None
}

pub(super) fn sfnt_face_offsets(
    data: &[u8],
    is_collection: bool,
    max_faces: u32,
) -> Vec<(u32, usize)> {
    if !is_collection {
        return vec![(0, 0)];
    }
    if data.get(0..4) != Some(b"ttcf") {
        return Vec::new();
    }
    let Some(face_count) = read_be_u32(data, 8) else {
        return Vec::new();
    };
    let capped = face_count.min(max_faces);
    let mut offsets = Vec::new();
    for index in 0..capped {
        let Some(offset) = read_be_u32(data, 12 + usize::try_from(index).unwrap_or(0) * 4) else {
            break;
        };
        let Ok(offset) = usize::try_from(offset) else {
            continue;
        };
        if offset < data.len() {
            offsets.push((index, offset));
        }
    }
    offsets
}

struct Cmap12SubtablePlan {
    subtable_offset: usize,
    group_count: usize,
    skip_indices: Vec<usize>,
    clamp_indices: Vec<usize>,
}

pub(super) fn sanitize_cmap12_invalid_groups(
    font_data: &[u8],
    index: u32,
) -> Result<(Cow<'_, [u8]>, usize), String> {
    let face_offset = if is_ttc_data(font_data) {
        let Ok(index) = usize::try_from(index) else {
            return Ok((Cow::Borrowed(font_data), 0));
        };
        let Some(offset_slot) = 12usize.checked_add(index.saturating_mul(4)) else {
            return Ok((Cow::Borrowed(font_data), 0));
        };
        let Some(offset) = read_be_u32(font_data, offset_slot) else {
            return Ok((Cow::Borrowed(font_data), 0));
        };
        let Ok(offset) = usize::try_from(offset) else {
            return Ok((Cow::Borrowed(font_data), 0));
        };
        offset
    } else {
        0
    };

    let Some((cmap_offset, cmap_len)) = table_range(font_data, face_offset, b"cmap") else {
        return Ok((Cow::Borrowed(font_data), 0));
    };
    let Some(cmap_end) = cmap_offset.checked_add(cmap_len) else {
        return Ok((Cow::Borrowed(font_data), 0));
    };
    let Some(record_count) = read_be_u16(font_data, cmap_offset + 2).map(usize::from) else {
        return Ok((Cow::Borrowed(font_data), 0));
    };

    let mut earliest_subtable_rel: Option<usize> = None;
    let max_record_count = cmap_len.saturating_sub(4) / 8;
    let record_count = record_count.min(max_record_count);
    if record_count > MAX_CMAP12_ENCODING_RECORDS_PER_FACE {
        return Err(format!(
            "cmap encoding record count {record_count} exceeds max {MAX_CMAP12_ENCODING_RECORDS_PER_FACE}"
        ));
    }
    let mut scanned_subtables = HashSet::new();
    let mut scanned_cmap12_groups = 0usize;
    let mut total_rewrites = 0usize;
    let mut plans: Vec<Cmap12SubtablePlan> = Vec::new();
    for record_index in 0..record_count {
        let Some(record_rel) = 4usize.checked_add(record_index.saturating_mul(8)) else {
            break;
        };
        if earliest_subtable_rel.is_some_and(|first_subtable| record_rel >= first_subtable) {
            break;
        }
        let Some(record_offset) = cmap_offset.checked_add(record_rel) else {
            break;
        };
        if record_offset
            .checked_add(8)
            .is_none_or(|end| end > cmap_end)
        {
            break;
        }
        let Some(subtable_rel) = read_be_u32(font_data, record_offset + 4) else {
            break;
        };
        let Ok(subtable_rel) = usize::try_from(subtable_rel) else {
            continue;
        };
        if subtable_rel >= cmap_len {
            continue;
        }
        if subtable_rel < record_rel.saturating_add(8) {
            continue;
        }
        let Some(subtable_offset) = cmap_offset.checked_add(subtable_rel) else {
            continue;
        };
        if !scanned_subtables.insert(subtable_offset) {
            continue;
        }
        if scanned_subtables.len() > MAX_CMAP12_SUBTABLES_PER_FACE {
            return Err(format!(
                "cmap subtable count exceeds max {MAX_CMAP12_SUBTABLES_PER_FACE}"
            ));
        }
        if read_be_u16(font_data, subtable_offset) != Some(12) {
            continue;
        }
        let Some(length) = read_be_u32(font_data, subtable_offset + 4) else {
            continue;
        };
        let Ok(length) = usize::try_from(length) else {
            continue;
        };
        let Some(subtable_end) = subtable_offset.checked_add(length) else {
            continue;
        };
        if length < 16 || subtable_end > cmap_end || subtable_end > font_data.len() {
            continue;
        }
        earliest_subtable_rel =
            Some(earliest_subtable_rel.map_or(subtable_rel, |current| current.min(subtable_rel)));
        let Some(group_count) = read_be_u32(font_data, subtable_offset + 12) else {
            continue;
        };
        let Ok(group_count) = usize::try_from(group_count) else {
            continue;
        };
        let max_group_count = length.saturating_sub(16) / 12;
        let group_count = group_count.min(max_group_count);
        scanned_cmap12_groups = scanned_cmap12_groups
            .checked_add(group_count)
            .ok_or_else(|| "cmap format-12 group count overflowed".to_string())?;
        if scanned_cmap12_groups > MAX_CMAP12_GROUPS_PER_FACE {
            return Err(format!(
                "cmap format-12 group count {scanned_cmap12_groups} exceeds max {MAX_CMAP12_GROUPS_PER_FACE}"
            ));
        }
        let groups_offset = subtable_offset + 16;
        let mut skip_indices = Vec::new();
        let mut clamp_indices = Vec::new();
        for group_index in 0..group_count {
            let Some(group_offset) = groups_offset.checked_add(group_index.saturating_mul(12))
            else {
                break;
            };
            if group_offset + 12 > subtable_end {
                break;
            }
            let Some(start) = read_be_u32(font_data, group_offset) else {
                break;
            };
            let Some(end) = read_be_u32(font_data, group_offset + 4) else {
                break;
            };
            if start > end || start > UNICODE_SCALAR_MAX {
                skip_indices.push(group_index);
                total_rewrites += 1;
                if total_rewrites > MAX_CMAP12_REWRITE_OFFSETS_PER_FACE {
                    return Err(format!(
                        "cmap format-12 rewrite count exceeds max {MAX_CMAP12_REWRITE_OFFSETS_PER_FACE}"
                    ));
                }
            } else if end > UNICODE_SCALAR_MAX {
                clamp_indices.push(group_index);
                total_rewrites += 1;
                if total_rewrites > MAX_CMAP12_REWRITE_OFFSETS_PER_FACE {
                    return Err(format!(
                        "cmap format-12 rewrite count exceeds max {MAX_CMAP12_REWRITE_OFFSETS_PER_FACE}"
                    ));
                }
            }
        }
        if !skip_indices.is_empty() || !clamp_indices.is_empty() {
            plans.push(Cmap12SubtablePlan {
                subtable_offset,
                group_count,
                skip_indices,
                clamp_indices,
            });
        }
    }

    if total_rewrites == 0 {
        return Ok((Cow::Borrowed(font_data), 0));
    }

    let mut sanitized = font_data.to_vec();
    for plan in plans {
        let groups_base = plan.subtable_offset + 16;
        for &idx in &plan.clamp_indices {
            let end_offset = groups_base + idx * 12 + 4;
            let _ = write_be_u32(&mut sanitized, end_offset, UNICODE_SCALAR_MAX);
        }
        if !plan.skip_indices.is_empty() {
            let mut write_idx = 0usize;
            let mut skip_iter = plan.skip_indices.iter().copied().peekable();
            for read_idx in 0..plan.group_count {
                if skip_iter.peek() == Some(&read_idx) {
                    skip_iter.next();
                    continue;
                }
                if write_idx != read_idx {
                    let src = groups_base + read_idx * 12;
                    let dst = groups_base + write_idx * 12;
                    sanitized.copy_within(src..src + 12, dst);
                }
                write_idx += 1;
            }
            let vacated_start = groups_base + write_idx * 12;
            let vacated_end = groups_base + plan.group_count * 12;
            if let Some(tail) = sanitized.get_mut(vacated_start..vacated_end) {
                tail.fill(0);
            }
            let kept_u32 = u32::try_from(write_idx).unwrap_or(0);
            let new_length = 16u32.saturating_add(kept_u32.saturating_mul(12));
            let _ = write_be_u32(&mut sanitized, plan.subtable_offset + 4, new_length);
            let _ = write_be_u32(&mut sanitized, plan.subtable_offset + 12, kept_u32);
        }
    }

    Ok((Cow::Owned(sanitized), total_rewrites))
}

pub(super) fn decode_utf16be_name(raw: &[u8]) -> Option<String> {
    if !raw.len().is_multiple_of(2) {
        return None;
    }
    let code_units: Vec<u16> = raw
        .as_chunks::<2>()
        .0
        .iter()
        .map(|chunk| u16::from_be_bytes([chunk[0], chunk[1]]))
        .collect();
    String::from_utf16(&code_units).ok()
}

pub(super) fn raw_looks_utf16be_ascii(raw: &[u8]) -> bool {
    raw.len() >= 4
        && raw.len().is_multiple_of(2)
        && raw.as_chunks::<2>().0.iter().all(|chunk| chunk[0] == 0)
}

pub(super) fn clean_legacy_name(decoded: String) -> Option<String> {
    let cleaned = decoded.replace('\0', "");
    bounded_font_family_name(cleaned.chars())
}

pub(super) fn decode_legacy_name_record(
    platform: u16,
    encoding: u16,
    language: u16,
    raw: &[u8],
) -> Option<String> {
    if raw.len() > MAX_NAME_RECORD_BYTES {
        return None;
    }

    if matches!(platform, 0) || matches!((platform, encoding), (3, 0 | 1 | 10)) {
        return decode_utf16be_name(raw).and_then(clean_legacy_name);
    }

    // Some legacy CJK fonts use platform 3 encoding IDs 2/3/4/5 for
    // Shift-JIS / GBK / Big5 / Wansung family names, while still storing
    // ASCII PostScript names as UTF-16BE. Prefer UTF-16BE only for the
    // unambiguous ASCII-shaped records, then decode the legacy DBCS bytes.
    if raw_looks_utf16be_ascii(raw) {
        if let Some(name) = decode_utf16be_name(raw).and_then(clean_legacy_name) {
            return Some(name);
        }
    }

    let legacy_encoding = match (platform, encoding, language) {
        (3, 2, _) | (1, 1, _) => Some(encoding_rs::SHIFT_JIS),
        (3, 3, _) | (1, 25, _) => Some(encoding_rs::GBK),
        (3, 4, _) | (1, 2, _) => Some(encoding_rs::BIG5),
        (3, 5, _) | (1, 3, _) => Some(encoding_rs::EUC_KR),
        (1, 0, 11) => Some(encoding_rs::SHIFT_JIS),
        (1, 0, 19) => Some(encoding_rs::BIG5),
        (1, 0, 23) => Some(encoding_rs::EUC_KR),
        (1, 0, 33) => Some(encoding_rs::GBK),
        _ => None,
    };

    if let Some(encoding) = legacy_encoding {
        let (decoded, _, had_errors) = encoding.decode(raw);
        if !had_errors {
            return clean_legacy_name(decoded.into_owned());
        }
    }

    if raw.iter().all(|byte| byte.is_ascii()) {
        return String::from_utf8(raw.to_vec())
            .ok()
            .and_then(clean_legacy_name);
    }
    None
}

pub(super) fn legacy_face_style(data: &[u8], face_offset: usize) -> (bool, bool) {
    let weight_bold = table_range(data, face_offset, b"OS/2")
        .and_then(|(offset, len)| {
            if len >= 6 {
                read_be_u16(data, offset + 4)
            } else {
                None
            }
        })
        .is_some_and(|weight| weight >= 600);
    let (mac_bold, mac_italic) = table_range(data, face_offset, b"head")
        .and_then(|(offset, len)| {
            if len >= 46 {
                read_be_u16(data, offset + 44)
            } else {
                None
            }
        })
        .map(|style| ((style & 0x0001) != 0, (style & 0x0002) != 0))
        .unwrap_or((false, false));
    (weight_bold || mac_bold, mac_italic)
}

#[cfg(test)]
pub(super) fn parse_legacy_name_table_entries(
    data: &[u8],
    canonical_string: &str,
    size_bytes: u64,
    is_collection: bool,
    max_faces: u32,
    scan_id: u64,
) -> Vec<LocalFontEntry> {
    parse_legacy_name_table_entries_skipping_skrifa(
        data,
        canonical_string,
        size_bytes,
        is_collection,
        max_faces,
        scan_id,
        &HashSet::new(),
    )
}

pub(super) fn parse_legacy_name_table_entries_skipping_skrifa(
    data: &[u8],
    canonical_string: &str,
    size_bytes: u64,
    is_collection: bool,
    max_faces: u32,
    scan_id: u64,
    skrifa_parsed_faces: &HashSet<u32>,
) -> Vec<LocalFontEntry> {
    let mut entries = Vec::new();
    for (face_index, face_offset) in sfnt_face_offsets(data, is_collection, max_faces) {
        if font_scan_cancelled(scan_id) {
            break;
        }
        let already_decoded_unicode = skrifa_parsed_faces.contains(&face_index);
        let Some((name_offset, name_len)) = table_range(data, face_offset, b"name") else {
            continue;
        };
        if name_len < 6 {
            continue;
        }
        let Some(name_end) = name_offset.checked_add(name_len) else {
            continue;
        };
        let Some(record_count) = read_be_u16(data, name_offset + 2).map(usize::from) else {
            continue;
        };
        let Some(string_offset) = read_be_u16(data, name_offset + 4).map(usize::from) else {
            continue;
        };
        if string_offset < 6 || string_offset > name_len {
            continue;
        }
        let Some(string_base) = name_offset.checked_add(string_offset) else {
            continue;
        };
        if string_base > name_end {
            continue;
        }
        let max_record_count = string_offset.saturating_sub(6) / 12;
        let record_count = record_count.min(max_record_count);

        let mut families: HashSet<String> = HashSet::new();
        let mut face_name_variants: HashSet<String> = HashSet::new();
        let mut seen_records = HashSet::new();
        let mut decode_attempts = 0usize;
        for record_index in 0..record_count {
            if font_scan_cancelled(scan_id) {
                break;
            }
            let Some(record) = name_offset
                .checked_add(6)
                .and_then(|v| v.checked_add(record_index.checked_mul(12)?))
            else {
                break;
            };
            if record.checked_add(12).is_none_or(|end| end > name_end) {
                break;
            }
            let Some(platform) = read_be_u16(data, record) else {
                continue;
            };
            let Some(encoding) = read_be_u16(data, record + 2) else {
                continue;
            };
            let Some(language) = read_be_u16(data, record + 4) else {
                continue;
            };
            let Some(name_id) = read_be_u16(data, record + 6) else {
                continue;
            };
            if !matches!(name_id, 1 | 4 | 6 | 16) {
                continue;
            }
            if already_decoded_unicode
                && (platform == 0 || matches!((platform, encoding), (3, 0 | 1 | 10)))
            {
                continue;
            }
            let target_full = match name_id {
                1 | 16 => families.len() >= MAX_FAMILY_VARIANTS_PER_FACE,
                4 | 6 => face_name_variants.len() >= MAX_FACE_NAME_VARIANTS_PER_FACE,
                _ => false,
            };
            if target_full {
                continue;
            }
            let Some(len) = read_be_u16(data, record + 8).map(usize::from) else {
                continue;
            };
            if len == 0 || len > MAX_NAME_RECORD_BYTES {
                continue;
            }
            let Some(offset) = read_be_u16(data, record + 10).map(usize::from) else {
                continue;
            };
            let Some(start) = string_base.checked_add(offset) else {
                continue;
            };
            let Some(end) = start.checked_add(len) else {
                continue;
            };
            if end > name_end {
                continue;
            }
            if !seen_records.insert((platform, encoding, language, name_id, start, end)) {
                continue;
            }
            if decode_attempts >= MAX_NAME_DECODE_ATTEMPTS_PER_FACE {
                break;
            }
            decode_attempts += 1;

            let target_full = match name_id {
                1 | 16 => families.len() >= MAX_FAMILY_VARIANTS_PER_FACE,
                4 | 6 => face_name_variants.len() >= MAX_FACE_NAME_VARIANTS_PER_FACE,
                _ => false,
            };
            if target_full {
                continue;
            }
            let Some(name) =
                decode_legacy_name_record(platform, encoding, language, &data[start..end])
            else {
                continue;
            };
            match name_id {
                1 | 16 => {
                    if families.len() < MAX_FAMILY_VARIANTS_PER_FACE {
                        families.insert(name);
                    }
                }
                4 | 6 if face_name_variants.len() < MAX_FACE_NAME_VARIANTS_PER_FACE => {
                    face_name_variants.insert(name);
                }
                _ => {}
            }
        }

        if families.is_empty() {
            if let Some(name) = face_name_variants.iter().min().cloned() {
                families.insert(name);
            }
        }
        if families.is_empty() {
            continue;
        }
        let mut families: Vec<String> = families.into_iter().collect();
        families.sort();
        let face_names = materialize_face_name_aliases(face_name_variants);
        let (bold, italic) = legacy_face_style(data, face_offset);
        entries.push(LocalFontEntry {
            path: canonical_string.to_string(),
            index: face_index,
            families,
            face_names,
            bold,
            italic,
            size_bytes,
        });
    }
    entries
}

pub(super) fn merge_legacy_name_table_entries(
    entries: &mut Vec<LocalFontEntry>,
    legacy: Vec<LocalFontEntry>,
) {
    for legacy_entry in legacy {
        let Some(existing) = entries
            .iter_mut()
            .find(|entry| entry.index == legacy_entry.index)
        else {
            entries.push(legacy_entry);
            continue;
        };
        let primary = existing.families.first().cloned();
        let mut added_family = false;
        for family in legacy_entry.families {
            if existing.families.len() >= MAX_FAMILY_VARIANTS_PER_FACE {
                break;
            }
            if !existing.families.contains(&family) {
                existing.families.push(family);
                added_family = true;
            }
        }
        let mut added_face_name = false;
        for face_name in legacy_entry.face_names {
            if existing.face_names.len() >= MAX_FACE_NAME_VARIANTS_PER_FACE {
                break;
            }
            if !existing.face_names.contains(&face_name) {
                existing.face_names.push(face_name);
                added_face_name = true;
            }
        }
        if added_family {
            existing.families.sort();
            if let Some(ref preferred) = primary {
                if let Some(pos) = existing.families.iter().position(|v| v == preferred) {
                    existing.families[..=pos].rotate_right(1);
                }
            }
        }
        if added_face_name {
            existing.face_names.sort();
        }
    }
}

#[derive(Default)]
pub(super) struct CollectedSkrifaFaceNames {
    pub(super) family_variants: HashSet<String>,
    pub(super) face_name_variants: HashSet<String>,
    pub(super) primary_hint: Option<String>,
}

pub(super) enum SkrifaNameWalkOutcome {
    Completed(CollectedSkrifaFaceNames),
    Cancelled,
}

pub(super) fn decode_skrifa_name_bucket<'a, C>(
    name_table: &fontcull_skrifa::raw::tables::name::Name<'a>,
    records: &[&fontcull_skrifa::raw::tables::name::NameRecord],
    variants: &mut HashSet<String>,
    max_variants: usize,
    track_primary: bool,
    decode_attempts: &mut usize,
    is_cancelled: &mut C,
) -> Result<Option<String>, ()>
where
    C: FnMut() -> bool,
{
    use fontcull_skrifa::string::LocalizedString;

    let mut best_primary_rank = -1i8;
    let mut best_primary = None;
    for (record_index, record) in records.iter().enumerate() {
        if is_cancelled() {
            return Err(());
        }
        if *decode_attempts >= MAX_NAME_DECODE_ATTEMPTS_PER_FACE {
            break;
        }
        if !track_primary && variants.len() >= max_variants {
            break;
        }
        *decode_attempts += 1;

        let localized = LocalizedString::new(name_table, record);
        let primary_rank = if track_primary {
            match localized.language() {
                Some("en-US") => 3,
                Some("en") => 2,
                None => 1,
                Some(_) if record_index == 0 => 0,
                Some(_) => -1,
            }
        } else {
            -1
        };
        let decoded = bounded_font_family_name(localized.chars());

        // Match skrifa's `english_or_first` ranking. The selected record may
        // itself be malformed; retaining `None` in that case deliberately
        // permits the typographic-family ID to supply the primary hint.
        if primary_rank > best_primary_rank {
            best_primary_rank = primary_rank;
            best_primary = decoded.clone();
        }
        if let Some(value) = decoded {
            if variants.len() < max_variants || variants.contains(&value) {
                variants.insert(value);
            }
        }
    }
    Ok(best_primary)
}

pub(super) fn retain_preferred_name(
    variants: &mut HashSet<String>,
    preferred: &Option<String>,
    max_variants: usize,
) {
    let Some(preferred) = preferred else {
        return;
    };
    if variants.contains(preferred) {
        return;
    }
    if variants.len() >= max_variants {
        // Deterministic replacement keeps the bucket bounded while preserving
        // the preferred English name. Appending here used to produce a ninth
        // family despite the documented eight-variant ceiling.
        if let Some(to_remove) = variants.iter().max().cloned() {
            variants.remove(&to_remove);
        }
    }
    variants.insert(preferred.clone());
}

/// Collect the four name IDs used by the cache without calling
/// `MetadataProvider::localized_strings`. That high-level iterator starts at
/// the beginning of the full name-record slice for every ID and hides the
/// scan inside `next()`, so caller-side `.take()` cannot bound or cancel it.
///
/// This function performs one raw-record pass, polls cancellation at every
/// record, stores at most one decode budget per target ID, then decodes at
/// most `MAX_NAME_DECODE_ATTEMPTS_PER_FACE` unique records in the same ID
/// priority used by the previous implementation.
pub(super) fn collect_skrifa_face_names<C>(
    font_ref: &fontcull_skrifa::FontRef<'_>,
    mut is_cancelled: C,
) -> SkrifaNameWalkOutcome
where
    C: FnMut() -> bool,
{
    use fontcull_skrifa::raw::TableProvider;
    use fontcull_skrifa::string::StringId;

    if is_cancelled() {
        return SkrifaNameWalkOutcome::Cancelled;
    }
    let name_table = match font_ref.name() {
        Ok(name_table) => name_table,
        Err(_) => return SkrifaNameWalkOutcome::Completed(Default::default()),
    };

    let mut buckets: [Vec<&fontcull_skrifa::raw::tables::name::NameRecord>; 4] =
        std::array::from_fn(|_| Vec::with_capacity(MAX_NAME_DECODE_ATTEMPTS_PER_FACE));
    let mut seen_records = HashSet::new();
    for record in name_table.name_record() {
        if is_cancelled() {
            return SkrifaNameWalkOutcome::Cancelled;
        }
        let bucket_index = match record.name_id() {
            StringId::FAMILY_NAME => 0,
            StringId::TYPOGRAPHIC_FAMILY_NAME => 1,
            StringId::FULL_NAME => 2,
            StringId::POSTSCRIPT_NAME => 3,
            _ => continue,
        };
        let length = usize::from(record.length());
        if length == 0 || length > MAX_NAME_RECORD_BYTES {
            continue;
        }
        let dedup_key = (
            record.platform_id(),
            record.encoding_id(),
            record.language_id(),
            record.name_id(),
            record.length(),
            record.string_offset().to_u32(),
        );
        if !seen_records.insert(dedup_key) {
            continue;
        }
        if buckets[bucket_index].len() < MAX_NAME_DECODE_ATTEMPTS_PER_FACE {
            buckets[bucket_index].push(record);
        }
        if buckets
            .iter()
            .all(|bucket| bucket.len() >= MAX_NAME_DECODE_ATTEMPTS_PER_FACE)
        {
            break;
        }
    }

    let mut collected = CollectedSkrifaFaceNames::default();
    let mut decode_attempts = 0usize;
    let primary_family = match decode_skrifa_name_bucket(
        &name_table,
        &buckets[0],
        &mut collected.family_variants,
        MAX_FAMILY_VARIANTS_PER_FACE,
        true,
        &mut decode_attempts,
        &mut is_cancelled,
    ) {
        Ok(primary) => primary,
        Err(()) => return SkrifaNameWalkOutcome::Cancelled,
    };
    collected.primary_hint = primary_family;

    if collected.family_variants.len() < MAX_FAMILY_VARIANTS_PER_FACE {
        let track_primary = collected.primary_hint.is_none();
        let typographic_primary = match decode_skrifa_name_bucket(
            &name_table,
            &buckets[1],
            &mut collected.family_variants,
            MAX_FAMILY_VARIANTS_PER_FACE,
            track_primary,
            &mut decode_attempts,
            &mut is_cancelled,
        ) {
            Ok(primary) => primary,
            Err(()) => return SkrifaNameWalkOutcome::Cancelled,
        };
        if collected.primary_hint.is_none() {
            collected.primary_hint = typographic_primary;
        }
    }

    for bucket in [&buckets[2], &buckets[3]] {
        if decode_skrifa_name_bucket(
            &name_table,
            bucket,
            &mut collected.face_name_variants,
            MAX_FACE_NAME_VARIANTS_PER_FACE,
            false,
            &mut decode_attempts,
            &mut is_cancelled,
        )
        .is_err()
        {
            return SkrifaNameWalkOutcome::Cancelled;
        }
        if collected.face_name_variants.len() >= MAX_FACE_NAME_VARIANTS_PER_FACE {
            break;
        }
    }

    retain_preferred_name(
        &mut collected.family_variants,
        &collected.primary_hint,
        MAX_FAMILY_VARIANTS_PER_FACE,
    );
    SkrifaNameWalkOutcome::Completed(collected)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum FontFileSkipReason {
    MetadataUnreadable,
    TooLargeBeforeRead,
    OpenFailed,
    ReadFailed,
    GrewPastReadLimit,
    NoUsableFaces,
}

impl FontFileSkipReason {
    pub(super) fn is_incomplete_io(self) -> bool {
        matches!(
            self,
            Self::MetadataUnreadable
                | Self::OpenFailed
                | Self::ReadFailed
                | Self::GrewPastReadLimit
        )
    }
}

impl std::fmt::Display for FontFileSkipReason {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let message = match self {
            Self::MetadataUnreadable => "metadata could not be read",
            Self::TooLargeBeforeRead => "file exceeds the 64 MiB font-scan limit",
            Self::OpenFailed => "file could not be opened",
            Self::ReadFailed => "file could not be read",
            Self::GrewPastReadLimit => "file grew beyond the 64 MiB font-scan limit while reading",
            Self::NoUsableFaces => "no usable OpenType faces or family names were found",
        };
        formatter.write_str(message)
    }
}

pub(super) fn safe_font_file_log_label(path: &Path) -> String {
    path.file_name()
        .and_then(|name| name.to_str())
        .filter(|name| validate_ipc_path(name, "Font filename").is_ok())
        .map(str::to_owned)
        .unwrap_or_else(|| "<unsafe font filename>".to_string())
}

pub(super) fn format_font_file_skip_warning(path: &Path, reason: FontFileSkipReason) -> String {
    format!(
        "Skipping font file '{}': {reason}",
        safe_font_file_log_label(path)
    )
}

pub(super) fn parse_local_font_file(
    canonical: &Path,
    scan_id: u64,
) -> Result<Vec<LocalFontEntry>, FontFileSkipReason> {
    let result = try_parse_local_font_file(canonical, scan_id);
    if let Err(reason) = &result {
        // Use only the validated basename so library layout stays out of shared logs.
        log::warn!("{}", format_font_file_skip_warning(canonical, *reason));
    }
    result
}

/// Parse one font file (TTF/OTF/TTC/OTC) and return a `LocalFontEntry` per
/// face **and per distinct localized family name** in the face's name table.
///
/// A single TTF can declare its family under multiple languages (common with
/// CJK fonts that ship both an English and a Chinese name). We emit one entry
/// per variant so the frontend matcher finds the font no matter which name the
/// ASS script happens to reference. Earlier single-name lookup shadowed
/// English family names on zh-CN Windows when the OS preferred a localized
/// name, which caused "font not recognized" reports for valid local sources.
///
/// `canonical` must already be canonicalized by the caller. User provenance
/// is registered later when the emitted batch is committed to the session
/// SQLite index.
///
/// `scan_id` lets the parser poll cancellation between faces and during both
/// raw name-record walks. Without the in-table poll, a crafted face with the
/// u16 maximum record count could hide a long scan inside skrifa before the
/// outer file loop observed Cancel.
///
/// `NO_SCAN_ID` is the no-cancellation sentinel. `scan_directory_collecting`
/// passes it on the cache-populate path (CLI `refresh-fonts` + GUI
/// post-commit cache populate, neither of which participates in the
/// scan-cancel system). Interactive scan workers pass a positive id
/// minted by `begin_font_scan`.
pub(super) fn try_parse_local_font_file(
    canonical: &Path,
    scan_id: u64,
) -> Result<Vec<LocalFontEntry>, FontFileSkipReason> {
    use fontcull_skrifa::{FontRef, MetadataProvider};

    // Extension check is intentionally case-insensitive (.TTF vs .ttf are the
    // same file format). The ASCII-lowercase conversion is correct here — all
    // ALLOWED_FONT_EXTENSIONS entries are ASCII. Done inline (instead of the
    // `has_allowed_font_extension` helper) so `is_collection` below can reuse
    // the already-computed lowercase extension without a second extension
    // lookup; helper would lower-case once for the bool, then `is_collection`
    // would lower-case again for the TTC/OTC check.
    let ext = canonical
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase())
        .unwrap_or_default();
    if !ALLOWED_FONT_EXTENSIONS.contains(&ext.as_str()) {
        return Ok(Vec::new());
    }
    if font_scan_cancelled(scan_id) {
        return Ok(Vec::new());
    }

    // Stat + size-cap guard before fs::read — a malicious user-picked
    // directory could otherwise OOM the process by containing a .ttf
    // that's actually a hundred-gigabyte impostor file. Aligns with
    // subset_font's own MAX_FONT_DATA_SIZE cap.
    let size_bytes = match fs::metadata(canonical) {
        Ok(m) => {
            if m.len() > MAX_FONT_DATA_SIZE {
                return Err(FontFileSkipReason::TooLargeBeforeRead);
            }
            m.len()
        }
        Err(_) => return Err(FontFileSkipReason::MetadataUnreadable),
    };
    let is_collection = ext == "ttc" || ext == "otc";
    let max_faces = if is_collection { MAX_TTC_FACES } else { 1 };

    let canonical_string = normalize_canonical_path(&canonical.to_string_lossy());

    // Read the file once and parse it with fontcull/skrifa only. Avoid
    // font-kit here: on Windows its in-memory loader routes through
    // DirectWrite and can retain/copy whole font blobs across huge scans.
    //
    // bounded-read pattern closes the TOCTOU
    // window between the pre-read stat gate (`metadata.len()` check
    // above) and the unbounded `fs::read`. An attacker who can swap
    // the file between stat and read (rare on single-user but
    // non-zero on slow / network-mounted FS where the scan loop
    // walks for seconds) could force a full-size allocation of a
    // GB-scale impostor file before the post-read recheck fires.
    // `File::open + take(MAX_FONT_DATA_SIZE + 1) + read_to_end`
    // caps the buffer at the OS layer; the +1 byte lets us
    // distinguish "exactly at cap" from "over cap" — if the read
    // returns more than MAX_FONT_DATA_SIZE bytes, the file changed during
    // reading and the scan must remain incomplete.
    use std::io::Read;
    let mut data = Vec::new();
    {
        let file = match std::fs::File::open(canonical) {
            Ok(f) => f,
            Err(_) => return Err(FontFileSkipReason::OpenFailed),
        };
        let mut limited = file.take(MAX_FONT_DATA_SIZE + 1);
        if limited.read_to_end(&mut data).is_err() {
            return Err(FontFileSkipReason::ReadFailed);
        }
    }

    // Post-read size re-check (parity with
    // `encoding.rs::read_text_detect_encoding` and `subset_font` below).
    // Belt-and-suspenders with the bounded-read above: the +1 byte trick
    // means `data.len() > MAX_FONT_DATA_SIZE` cleanly identifies over-cap
    // reads even if the OS short-reads. Unlike a file already over the cap
    // before reading, this changed file cannot contribute to a complete scan.
    if data.len() as u64 > MAX_FONT_DATA_SIZE {
        return Err(FontFileSkipReason::GrewPastReadLimit);
    }

    let mut entries = Vec::new();
    // Permit a single consecutive parse failure before giving up. In practice
    // FontRef::from_index returns an error once `i` exceeds the real face
    // count, so one tolerance catches that natural end-of-collection while
    // keeping the per-file parse cost bounded at 2 × face_count rather than
    // 3 ×. Crafted TTCs cannot force us to parse all 64 slots just by salting
    // every other face with bad data.
    //
    // Single-face files (TTF/OTF) effectively use this as a one-shot:
    // `max_faces = 1`, the loop runs once, and the constant doesn't
    // matter. Only TTC/OTC iteration depends on it.
    const MAX_CONSECUTIVE_FAILURES: u32 = 1;
    let mut consecutive_failures: u32 = 0;
    for i in 0..max_faces {
        // Per-face cancel poll. The raw name-table collectors also poll within
        // each face; this outer check keeps the common small-table path cheap
        // and avoids opening the next TTC face after cancellation. NO_SCAN_ID
        // is the "no active scan" sentinel and must never trigger cancellation.
        //
        // Returning early with the already-parsed faces is safe — the
        // outer scan's cancel branch flushes the buffer (which now
        // includes our partial faces) before the cancelled outcome is
        // returned, so no parsed work is lost.
        if font_scan_cancelled(scan_id) {
            break;
        }
        let font_ref = match FontRef::from_index(&data, i) {
            Ok(f) => f,
            Err(_) => {
                consecutive_failures += 1;
                if consecutive_failures > MAX_CONSECUTIVE_FAILURES {
                    break;
                }
                continue;
            }
        };
        consecutive_failures = 0;

        // catch_unwind around the per-face
        // skrifa name-table walk + entry construction. Mirrors the
        // existing wrap around fontcull::subset_font_data_unicode
        // below: skrifa is in active development and crafted TTC /
        // OTC inputs can trigger panics on bad face counts, malformed
        // CFF, or out-of-range name-table records from attacker-influenced
        // content sources. Without catch_unwind
        // here, a single panicking face would unwind through the
        // whole `parse_local_font_file` and abort the surrounding
        // scan — instead of the documented "skip this face, continue
        // with the next" behavior the MAX_CONSECUTIVE_FAILURES tolerance
        // already provides for non-panic errors. AssertUnwindSafe is
        // sound: `data` is &Vec<u8> read-only, the inner mutations
        // are on local HashSet / Vec / Option that get dropped on
        // unwind; the only escape is `entries.push` which happens at
        // the tail, so a panic before push leaves entries unchanged.
        let face_result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let attrs = font_ref.attributes();
            let bold = attrs.weight.value() >= 600.0;
            let italic = !matches!(attrs.style, fontcull_skrifa::attribute::Style::Normal);
            let CollectedSkrifaFaceNames {
                mut family_variants,
                face_name_variants,
                mut primary_hint,
            } = match collect_skrifa_face_names(&font_ref, || font_scan_cancelled(scan_id)) {
                SkrifaNameWalkOutcome::Completed(collected) => collected,
                SkrifaNameWalkOutcome::Cancelled => return None,
            };

            // Last-resort fallback: malformed fonts may have no family IDs but
            // still have a full name. Indexing that is better than silently
            // dropping the face, and it avoids re-entering font-kit/DirectWrite.
            if family_variants.is_empty() {
                if let Some(name) = face_name_variants.iter().min().cloned() {
                    primary_hint = Some(name.clone());
                    family_variants.insert(name);
                }
            }

            Some((
                family_variants,
                face_name_variants,
                primary_hint,
                bold,
                italic,
            ))
        }));
        let (family_variants, face_name_variants, primary_hint, bold, italic) = match face_result {
            Ok(Some(values)) => values,
            Ok(None) => break,
            Err(_) => continue,
        };

        if family_variants.is_empty() {
            continue;
        }

        // Stabilize the primary-name pick: prefer the best available English
        // family name if it is among the variants, else fall back to sorted
        // order so UI listings stay deterministic across runs.
        //
        // WHY HashSet→sort here: `family_variants` is a
        // HashSet for cheap dedup during the per-name-record walk above
        // (a font's name table can list the same family string multiple
        // times across (platform, language) tuples). HashSet iteration
        // order is non-deterministic across runs, so directly using
        // `family_variants.into_iter().collect()` would surface different
        // primary names on different launches even for the same font
        // file. Sorting the Vec stabilizes the unwrap_or path before
        // the rotate_right() rotation places the English variant at
        // index 0. Test pin: `db_lookup_prefers_newer_source_for_same_family_key`.
        let mut families: Vec<String> = family_variants.into_iter().collect();
        families.sort();
        if let Some(ref primary) = primary_hint {
            if let Some(pos) = families.iter().position(|value| value == primary) {
                // rotate_right(1) moves families[pos] to index 0 while keeping
                // the elements before it in alphabetical order — swap(0, pos)
                // would displace the element at 0 to pos, breaking sort order.
                families[..=pos].rotate_right(1);
            } else {
                // `collect_skrifa_face_names` structurally retains a preferred
                // name inside the eight-entry bucket. Keep this loud tripwire
                // local to materialization in case a future collector changes
                // that contract.
                debug_assert!(false, "primary font name missing from bounded variants");
            }
        }

        let face_names = materialize_face_name_aliases(face_name_variants);

        entries.push(LocalFontEntry {
            path: canonical_string.clone(),
            index: i,
            families,
            face_names,
            bold,
            italic,
            size_bytes,
        });
    }
    if font_scan_cancelled(scan_id) {
        return Ok(entries);
    }
    let skrifa_parsed_faces: HashSet<u32> = entries.iter().map(|e| e.index).collect();
    let legacy_entries = parse_legacy_name_table_entries_skipping_skrifa(
        &data,
        &canonical_string,
        size_bytes,
        is_collection,
        max_faces,
        scan_id,
        &skrifa_parsed_faces,
    );
    if !legacy_entries.is_empty() {
        merge_legacy_name_table_entries(&mut entries, legacy_entries);
    }
    if entries.is_empty() && !font_scan_cancelled(scan_id) {
        Err(FontFileSkipReason::NoUsableFaces)
    } else {
        Ok(entries)
    }
}
