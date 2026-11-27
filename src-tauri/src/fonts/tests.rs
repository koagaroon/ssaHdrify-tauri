use super::*;
use rusqlite::{params, Connection, OptionalExtension};
use std::collections::HashSet;
use std::fs;
use std::path::PathBuf;
use std::sync::atomic::Ordering;

#[test]
fn explicit_font_path_revalidates_a_canonical_target_with_bidi_control() {
    let selected = r"C:\selected\font.ttf";
    // A filesystem alias can legitimately resolve a visually safe picker
    // path to an existing filename containing U+202E. Unlike a literal
    // `.` component, canonicalization does not normalize this issue shape
    // away, so the injected target models a reachable post-resolution
    // rejection without requiring symlink privileges in CI.
    let resolved_with_bidi = PathBuf::from("C:\\resolved\\font\u{202e}ttf.ttf");
    let mut canonicalizer_called = false;

    let error = canonicalize_and_validate_explicit_font_path_with(selected, |received| {
        canonicalizer_called = true;
        assert_eq!(received, Path::new(selected));
        Ok(resolved_with_bidi)
    })
    .expect_err("an unsafe canonical target must be rejected");

    assert!(canonicalizer_called);
    assert!(matches!(&error, ExplicitFontPathError::Rejected(_)));
    assert!(error.to_string().to_lowercase().contains("invalid"));
}

#[test]
fn explicit_font_path_returns_only_a_validated_normalized_target() {
    let selected = r"C:\selected\font.ttf";
    let canonical = PathBuf::from(r"\\?\C:\resolved\font.ttf");

    let (resolved, normalized) =
        canonicalize_and_validate_explicit_font_path_with(selected, |_| Ok(canonical.clone()))
            .expect("a safe canonical target should validate");

    assert_eq!(resolved, canonical);
    assert_eq!(normalized, r"C:\resolved\font.ttf");
}

#[test]
fn explicit_font_path_rejects_ads_introduced_by_resolution() {
    let error = canonicalize_and_validate_explicit_font_path_with(r"C:\selected\font.ttf", |_| {
        Ok(PathBuf::from(r"C:\resolved\font.ttf:payload"))
    })
    .expect_err("an ADS canonical target must be rejected");

    assert!(matches!(&error, ExplicitFontPathError::Rejected(_)));
    assert!(error.to_string().to_lowercase().contains("colon"));
}

fn cache_candidate(
    reason: ScanStopReason,
    added: usize,
    cache_truncated: bool,
) -> StreamingScanCommandResult {
    StreamingScanCommandResult {
        outcome: ScanOutcome {
            total: added,
            reason,
        },
        import: ImportOutcome {
            added,
            duplicated: 0,
        },
        cache_truncated,
    }
}

#[test]
fn persistent_cache_publication_requires_a_natural_complete_nonempty_scan() {
    assert!(is_cache_publication_eligible(&cache_candidate(
        ScanStopReason::Natural,
        1,
        false,
    )));
    for candidate in [
        cache_candidate(ScanStopReason::Natural, 0, false),
        cache_candidate(ScanStopReason::Natural, 1, true),
        cache_candidate(ScanStopReason::UserCancel, 1, false),
        cache_candidate(ScanStopReason::CeilingHit, 1, false),
        cache_candidate(ScanStopReason::IncompleteIo, 1, false),
    ] {
        assert!(!is_cache_publication_eligible(&candidate));
    }
}

/// Tripwire: a behavioral test for `MAX_FAMILY_VARIANTS_PER_FACE`
/// needs a real font with > 8
/// localized name-table entries, which the repo doesn't ship (CJK
/// font licensing — see `tests/test_subset.rs` for the same
/// constraint). Pin the constant value so an accidental raise is
/// noticed; the math behind the cap (bounded names × persisted key
/// rows × MAX_CACHE_POPULATE_FACES = OOM-on-crafted-pack ceiling) is
/// in the constant's doc comment above.
#[test]
fn max_family_variants_per_face_cap_value() {
    assert_eq!(MAX_FAMILY_VARIANTS_PER_FACE, 8);
}

#[test]
fn max_face_name_variants_per_face_cap_value() {
    assert_eq!(MAX_FACE_NAME_VARIANTS_PER_FACE, 8);
}

#[test]
fn skrifa_name_decode_cap_accepts_valid_record_at_limit() {
    let mut records: Vec<(u16, u16, Vec<u8>)> = (0..255).map(|_| (1, 0x0409, vec![0])).collect();
    records.push((16, 0x0409, utf16be_test_bytes("Boundary Family")));
    let font = font_with_name_records(&records);

    let collected = collected_skrifa_names(&font);
    assert!(collected.family_variants.contains("Boundary Family"));
}

#[test]
fn skrifa_name_decode_cap_rejects_valid_record_over_limit() {
    let mut records: Vec<(u16, u16, Vec<u8>)> = (0..256).map(|_| (1, 0x0409, vec![0])).collect();
    records.push((16, 0x0409, utf16be_test_bytes("Over Budget Family")));
    let font = font_with_name_records(&records);

    let collected = collected_skrifa_names(&font);
    assert!(!collected.family_variants.contains("Over Budget Family"));
    assert!(collected.family_variants.is_empty());
}

#[test]
fn skrifa_name_collection_keeps_preferred_name_inside_eight_variant_cap() {
    let mut records: Vec<(u16, u16, Vec<u8>)> = (0..8)
        .map(|index| (1, 0x0411, utf16be_test_bytes(&format!("Variant {index}"))))
        .collect();
    records.push((1, 0x0409, utf16be_test_bytes("English Preferred")));
    let font = font_with_name_records(&records);

    let collected = collected_skrifa_names(&font);
    assert_eq!(collected.family_variants.len(), 8);
    assert!(collected.family_variants.contains("English Preferred"));
    assert_eq!(collected.primary_hint.as_deref(), Some("English Preferred"));
}

#[test]
fn skrifa_name_collection_polls_cancel_during_raw_record_walk() {
    let mut records: Vec<(u16, u16, Vec<u8>)> = (0..100).map(|_| (2, 0x0409, vec![0])).collect();
    records.push((1, 0x0409, utf16be_test_bytes("Too Late")));
    let font = font_with_name_records(&records);
    let font_ref = fontcull_skrifa::FontRef::from_index(&font, 0).unwrap();
    let mut polls = 0usize;

    let outcome = collect_skrifa_face_names(&font_ref, || {
        polls += 1;
        polls > 10
    });
    assert!(matches!(outcome, SkrifaNameWalkOutcome::Cancelled));
    assert!(polls <= 11, "raw record walk ignored cancellation: {polls}");
}

#[test]
fn is_ttc_data_recognizes_ttcf_magic() {
    // Minimal valid TTC header: `ttcf` magic + version + numFonts.
    let data = b"ttcf\x00\x01\x00\x00\x00\x00\x00\x02";
    assert!(is_ttc_data(data));
}

#[test]
fn is_ttc_data_rejects_single_face_signatures() {
    // Three SFNT magics that the recognized-signature gate accepts
    // upstream of subset_font but that MUST route to the single-face
    // path, not the from_index collection path.
    assert!(!is_ttc_data(&[0x00, 0x01, 0x00, 0x00, 0xff, 0xff])); // sfnt-flavored TrueType
    assert!(!is_ttc_data(b"OTTOpadding")); // OpenType CFF
    assert!(!is_ttc_data(b"truepadding")); // Apple TrueType
}

#[test]
fn is_ttc_data_rejects_under_four_bytes() {
    // Boundary-pin: TTC magic is 4 bytes, so anything shorter must
    // return false (defends against a 0-byte read sneaking past the
    // upstream stat as if it were a single-face SFNT).
    assert!(!is_ttc_data(b""));
    assert!(!is_ttc_data(b"t"));
    assert!(!is_ttc_data(b"ttc"));
}

fn font_with_single_table(tag: &[u8; 4], table_data: &[u8]) -> Vec<u8> {
    let table_offset = 28u32;
    let table_len = u32::try_from(table_data.len()).expect("test table fits u32");

    let mut font = Vec::new();
    font.extend_from_slice(b"\0\x01\0\0");
    font.extend_from_slice(&1u16.to_be_bytes());
    font.extend_from_slice(&[0; 6]);
    font.extend_from_slice(tag);
    font.extend_from_slice(&0u32.to_be_bytes());
    font.extend_from_slice(&table_offset.to_be_bytes());
    font.extend_from_slice(&table_len.to_be_bytes());
    font.extend_from_slice(table_data);
    font
}

fn font_with_name_records(records: &[(u16, u16, Vec<u8>)]) -> Vec<u8> {
    let record_bytes = records
        .len()
        .checked_mul(12)
        .and_then(|bytes| bytes.checked_add(6))
        .expect("test name table size fits usize");
    let storage_offset = u16::try_from(record_bytes).expect("test records fit name offset");
    let mut string_offset = 0usize;
    let mut table = Vec::new();
    table.extend_from_slice(&0u16.to_be_bytes());
    table.extend_from_slice(
        &u16::try_from(records.len())
            .expect("test record count fits u16")
            .to_be_bytes(),
    );
    table.extend_from_slice(&storage_offset.to_be_bytes());
    for (name_id, language_id, bytes) in records {
        table.extend_from_slice(&3u16.to_be_bytes());
        table.extend_from_slice(&1u16.to_be_bytes());
        table.extend_from_slice(&language_id.to_be_bytes());
        table.extend_from_slice(&name_id.to_be_bytes());
        table.extend_from_slice(
            &u16::try_from(bytes.len())
                .expect("test name length fits u16")
                .to_be_bytes(),
        );
        table.extend_from_slice(
            &u16::try_from(string_offset)
                .expect("test string offset fits u16")
                .to_be_bytes(),
        );
        string_offset += bytes.len();
    }
    for (_, _, bytes) in records {
        table.extend_from_slice(bytes);
    }
    font_with_single_table(b"name", &table)
}

fn collected_skrifa_names(font: &[u8]) -> CollectedSkrifaFaceNames {
    let font_ref = fontcull_skrifa::FontRef::from_index(font, 0)
        .expect("synthetic name-table font should parse");
    match collect_skrifa_face_names(&font_ref, || false) {
        SkrifaNameWalkOutcome::Completed(collected) => collected,
        SkrifaNameWalkOutcome::Cancelled => panic!("unexpected cancellation"),
    }
}

fn cmap12_subtable_with_group_count(group_count: usize) -> Vec<u8> {
    let subtable_length = 16 + group_count as u32 * 12;
    let mut subtable = Vec::new();
    subtable.extend_from_slice(&12u16.to_be_bytes());
    subtable.extend_from_slice(&0u16.to_be_bytes());
    subtable.extend_from_slice(&subtable_length.to_be_bytes());
    subtable.extend_from_slice(&0u32.to_be_bytes());
    subtable.extend_from_slice(&(group_count as u32).to_be_bytes());
    for index in 0..group_count {
        let codepoint = 0x20 + u32::try_from(index % 64).expect("test index fits");
        subtable.extend_from_slice(&codepoint.to_be_bytes());
        subtable.extend_from_slice(&codepoint.to_be_bytes());
        subtable.extend_from_slice(&1u32.to_be_bytes());
    }
    subtable
}

fn cmap_with_format12_subtables(group_counts: &[usize]) -> Vec<u8> {
    let record_count = group_counts.len();
    let records_len = 4 + record_count * 8;
    let mut subtables = Vec::new();
    let mut subtable_offsets = Vec::new();
    for group_count in group_counts {
        subtable_offsets.push(records_len + subtables.len());
        subtables.extend_from_slice(&cmap12_subtable_with_group_count(*group_count));
    }

    let mut cmap = Vec::new();
    cmap.extend_from_slice(&0u16.to_be_bytes());
    cmap.extend_from_slice(
        &(u16::try_from(record_count).expect("test record count fits")).to_be_bytes(),
    );
    for offset in subtable_offsets {
        cmap.extend_from_slice(&0u16.to_be_bytes());
        cmap.extend_from_slice(&4u16.to_be_bytes());
        cmap.extend_from_slice(&(u32::try_from(offset).expect("test offset fits")).to_be_bytes());
    }
    cmap.extend_from_slice(&subtables);
    cmap
}

fn utf16be_test_bytes(value: &str) -> Vec<u8> {
    value
        .encode_utf16()
        .flat_map(|unit| unit.to_be_bytes())
        .collect()
}

#[test]
fn sanitize_cmap12_invalid_groups_makes_non_unicode_ranges_unselectable() {
    let cmap_offset = 28u32;
    let subtable_offset = 12u32;
    let groups = [
        (0x20u32, 0x7Eu32, 1u32),
        (0x110001u32, 0x110002u32, 7u32),
        (0x500u32, 0x4FFu32, 9u32),
    ];
    let subtable_length = 16 + groups.len() as u32 * 12;
    let cmap_length = subtable_offset + subtable_length;

    let mut font = Vec::new();
    font.extend_from_slice(b"\0\x01\0\0");
    font.extend_from_slice(&1u16.to_be_bytes());
    font.extend_from_slice(&[0; 6]);
    font.extend_from_slice(b"cmap");
    font.extend_from_slice(&0u32.to_be_bytes());
    font.extend_from_slice(&cmap_offset.to_be_bytes());
    font.extend_from_slice(&cmap_length.to_be_bytes());
    font.extend_from_slice(&0u16.to_be_bytes());
    font.extend_from_slice(&1u16.to_be_bytes());
    font.extend_from_slice(&0u16.to_be_bytes());
    font.extend_from_slice(&4u16.to_be_bytes());
    font.extend_from_slice(&subtable_offset.to_be_bytes());
    font.extend_from_slice(&12u16.to_be_bytes());
    font.extend_from_slice(&0u16.to_be_bytes());
    font.extend_from_slice(&subtable_length.to_be_bytes());
    font.extend_from_slice(&0u32.to_be_bytes());
    font.extend_from_slice(&(groups.len() as u32).to_be_bytes());
    for (start, end, gid) in groups {
        font.extend_from_slice(&start.to_be_bytes());
        font.extend_from_slice(&end.to_be_bytes());
        font.extend_from_slice(&gid.to_be_bytes());
    }

    let (sanitized, count) =
        sanitize_cmap12_invalid_groups(&font, 0).expect("test font should sanitize");
    assert_eq!(count, 2);
    let sanitized = sanitized.as_ref();
    let subtable_abs = cmap_offset as usize + subtable_offset as usize;
    assert_eq!(read_be_u32(sanitized, subtable_abs + 4), Some(16 + 12));
    assert_eq!(read_be_u32(sanitized, subtable_abs + 12), Some(1));
    let valid_group = subtable_abs + 16;
    assert_eq!(read_be_u32(sanitized, valid_group), Some(0x20));
    assert_eq!(read_be_u32(sanitized, valid_group + 4), Some(0x7E));
    assert_eq!(read_be_u32(sanitized, valid_group + 8), Some(1));

    let font_ref = fontcull_skrifa::FontRef::from_index(sanitized, 0).unwrap();
    let charmap = fontcull_skrifa::MetadataProvider::charmap(&font_ref);
    assert_eq!(charmap.map(0x20u32), Some(fontcull_skrifa::GlyphId::new(1)));
    assert_eq!(charmap.map(UNICODE_SCALAR_MAX), None);
}

#[test]
fn sanitize_cmap12_compacts_mid_table_invalid_group_preserving_binary_search_for_later_cjk() {
    let cmap_offset = 28u32;
    let subtable_offset = 12u32;
    let groups = [
        (0x20u32, 0x7Eu32, 1u32),
        (0x500u32, 0x4FFu32, 9u32),
        (0x4E00u32, 0x9FFFu32, 100u32),
    ];
    let subtable_length = 16 + groups.len() as u32 * 12;
    let cmap_length = subtable_offset + subtable_length;

    let mut font = Vec::new();
    font.extend_from_slice(b"\0\x01\0\0");
    font.extend_from_slice(&1u16.to_be_bytes());
    font.extend_from_slice(&[0; 6]);
    font.extend_from_slice(b"cmap");
    font.extend_from_slice(&0u32.to_be_bytes());
    font.extend_from_slice(&cmap_offset.to_be_bytes());
    font.extend_from_slice(&cmap_length.to_be_bytes());
    font.extend_from_slice(&0u16.to_be_bytes());
    font.extend_from_slice(&1u16.to_be_bytes());
    font.extend_from_slice(&0u16.to_be_bytes());
    font.extend_from_slice(&4u16.to_be_bytes());
    font.extend_from_slice(&subtable_offset.to_be_bytes());
    font.extend_from_slice(&12u16.to_be_bytes());
    font.extend_from_slice(&0u16.to_be_bytes());
    font.extend_from_slice(&subtable_length.to_be_bytes());
    font.extend_from_slice(&0u32.to_be_bytes());
    font.extend_from_slice(&(groups.len() as u32).to_be_bytes());
    for (start, end, gid) in groups {
        font.extend_from_slice(&start.to_be_bytes());
        font.extend_from_slice(&end.to_be_bytes());
        font.extend_from_slice(&gid.to_be_bytes());
    }

    let (sanitized, count) =
        sanitize_cmap12_invalid_groups(&font, 0).expect("test font should sanitize");
    assert_eq!(count, 1);
    let font_ref = fontcull_skrifa::FontRef::from_index(sanitized.as_ref(), 0).unwrap();
    let charmap = fontcull_skrifa::MetadataProvider::charmap(&font_ref);
    assert_eq!(
        charmap.map(0x4E2Du32),
        Some(fontcull_skrifa::GlyphId::new(100 + (0x4E2D - 0x4E00))),
        "valid CJK group after a compacted invalid group must remain reachable via binary search"
    );
    assert_eq!(charmap.map(UNICODE_SCALAR_MAX), None);
}

#[test]
fn sanitize_cmap12_invalid_groups_clamps_ranges_ending_past_unicode() {
    let cmap_offset = 28u32;
    let subtable_offset = 12u32;
    let groups = [
        (0x20u32, UNICODE_SCALAR_MAX + 1, 1u32),
        (0x30u32, 0x31u32, 2u32),
    ];
    let subtable_length = 16 + groups.len() as u32 * 12;
    let cmap_length = subtable_offset + subtable_length;

    let mut font = Vec::new();
    font.extend_from_slice(b"\0\x01\0\0");
    font.extend_from_slice(&1u16.to_be_bytes());
    font.extend_from_slice(&[0; 6]);
    font.extend_from_slice(b"cmap");
    font.extend_from_slice(&0u32.to_be_bytes());
    font.extend_from_slice(&cmap_offset.to_be_bytes());
    font.extend_from_slice(&cmap_length.to_be_bytes());
    font.extend_from_slice(&0u16.to_be_bytes());
    font.extend_from_slice(&1u16.to_be_bytes());
    font.extend_from_slice(&0u16.to_be_bytes());
    font.extend_from_slice(&4u16.to_be_bytes());
    font.extend_from_slice(&subtable_offset.to_be_bytes());
    font.extend_from_slice(&12u16.to_be_bytes());
    font.extend_from_slice(&0u16.to_be_bytes());
    font.extend_from_slice(&subtable_length.to_be_bytes());
    font.extend_from_slice(&0u32.to_be_bytes());
    font.extend_from_slice(&(groups.len() as u32).to_be_bytes());
    for (start, end, glyph) in groups {
        font.extend_from_slice(&start.to_be_bytes());
        font.extend_from_slice(&end.to_be_bytes());
        font.extend_from_slice(&glyph.to_be_bytes());
    }

    let (sanitized, count) =
        sanitize_cmap12_invalid_groups(&font, 0).expect("test font should sanitize");
    assert_eq!(count, 1);
    let sanitized = sanitized.as_ref();
    let clamped_group = cmap_offset as usize + subtable_offset as usize + 16;
    assert_eq!(read_be_u32(sanitized, clamped_group), Some(0x20));
    assert_eq!(
        read_be_u32(sanitized, clamped_group + 4),
        Some(UNICODE_SCALAR_MAX)
    );
    assert_eq!(read_be_u32(sanitized, clamped_group + 8), Some(1));

    let valid_group = clamped_group + 12;
    assert_eq!(read_be_u32(sanitized, valid_group), Some(0x30));
    assert_eq!(read_be_u32(sanitized, valid_group + 4), Some(0x31));
    assert_eq!(read_be_u32(sanitized, valid_group + 8), Some(2));
}

#[test]
fn sanitize_cmap12_invalid_groups_rejects_excessive_group_count() {
    let group_count = MAX_CMAP12_GROUPS_PER_FACE + 1;
    let cmap = cmap_with_format12_subtables(&[group_count]);
    let font = font_with_single_table(b"cmap", &cmap);
    let err = sanitize_cmap12_invalid_groups(&font, 0)
        .expect_err("excessive cmap format-12 groups must be rejected");
    assert!(
        err.contains("group count"),
        "error should identify cmap group-count cap: {err}"
    );
}

#[test]
fn sanitize_cmap12_invalid_groups_rejects_cumulative_group_count() {
    let first_count = MAX_CMAP12_GROUPS_PER_FACE / 2 + 1;
    let second_count = MAX_CMAP12_GROUPS_PER_FACE - first_count + 1;
    let cmap = cmap_with_format12_subtables(&[first_count, second_count]);
    let font = font_with_single_table(b"cmap", &cmap);

    let err = sanitize_cmap12_invalid_groups(&font, 0)
        .expect_err("cumulative cmap format-12 groups must be rejected");

    assert!(
        err.contains("group count"),
        "error should identify cumulative cmap group-count cap: {err}"
    );
}

#[test]
fn sanitize_cmap12_invalid_groups_rejects_excessive_encoding_records() {
    let record_count = MAX_CMAP12_ENCODING_RECORDS_PER_FACE + 1;
    let first_subtable_rel = 4 + record_count * 8;
    let mut cmap = Vec::new();
    cmap.extend_from_slice(&0u16.to_be_bytes());
    cmap.extend_from_slice(
        &(u16::try_from(record_count).expect("test record count fits")).to_be_bytes(),
    );
    for _ in 0..record_count {
        cmap.extend_from_slice(&0u16.to_be_bytes());
        cmap.extend_from_slice(&4u16.to_be_bytes());
        cmap.extend_from_slice(
            &(u32::try_from(first_subtable_rel).expect("test offset fits")).to_be_bytes(),
        );
    }
    let font = font_with_single_table(b"cmap", &cmap);

    let err = sanitize_cmap12_invalid_groups(&font, 0)
        .expect_err("excessive cmap encoding records must be rejected");

    assert!(
        err.contains("encoding record count"),
        "error should identify cmap encoding-record cap: {err}"
    );
}

#[test]
fn sanitize_cmap12_invalid_groups_rejects_excessive_unique_subtables() {
    let group_counts = vec![0usize; MAX_CMAP12_SUBTABLES_PER_FACE + 1];
    let cmap = cmap_with_format12_subtables(&group_counts);
    let font = font_with_single_table(b"cmap", &cmap);

    let err = sanitize_cmap12_invalid_groups(&font, 0)
        .expect_err("excessive cmap subtables must be rejected");

    assert!(
        err.contains("subtable count"),
        "error should identify cmap subtable-count cap: {err}"
    );
}

#[test]
fn sanitize_cmap12_invalid_groups_ignores_bogus_early_offset_before_later_record() {
    let real_subtable_rel = 20u32;
    let invalid_group = [UNICODE_SCALAR_MAX + 1, UNICODE_SCALAR_MAX + 1, 7u32];

    let mut cmap = Vec::new();
    cmap.extend_from_slice(&0u16.to_be_bytes());
    cmap.extend_from_slice(&2u16.to_be_bytes());
    cmap.extend_from_slice(&0u16.to_be_bytes());
    cmap.extend_from_slice(&4u16.to_be_bytes());
    cmap.extend_from_slice(&12u32.to_be_bytes());
    assert_eq!(cmap.len(), 12);
    cmap.extend_from_slice(&0u16.to_be_bytes());
    cmap.extend_from_slice(&4u16.to_be_bytes());
    cmap.extend_from_slice(&real_subtable_rel.to_be_bytes());
    assert_eq!(cmap.len(), real_subtable_rel as usize);
    cmap.extend_from_slice(&12u16.to_be_bytes());
    cmap.extend_from_slice(&0u16.to_be_bytes());
    cmap.extend_from_slice(&28u32.to_be_bytes());
    cmap.extend_from_slice(&0u32.to_be_bytes());
    cmap.extend_from_slice(&1u32.to_be_bytes());
    for value in invalid_group {
        cmap.extend_from_slice(&value.to_be_bytes());
    }

    let font = font_with_single_table(b"cmap", &cmap);
    let (sanitized, count) =
        sanitize_cmap12_invalid_groups(&font, 0).expect("later record should be scanned");

    assert_eq!(count, 1);
    let sanitized = sanitized.as_ref();
    let subtable_abs = 28usize + real_subtable_rel as usize;
    assert_eq!(read_be_u32(sanitized, subtable_abs + 4), Some(16));
    assert_eq!(read_be_u32(sanitized, subtable_abs + 12), Some(0));
}

#[test]
fn sanitize_cmap12_invalid_groups_rejects_excessive_rewrites() {
    let subtable_offset = 12u32;
    let group_count = MAX_CMAP12_REWRITE_OFFSETS_PER_FACE + 1;
    let subtable_length = 16 + group_count as u32 * 12;

    let mut cmap = Vec::new();
    cmap.extend_from_slice(&0u16.to_be_bytes());
    cmap.extend_from_slice(&1u16.to_be_bytes());
    cmap.extend_from_slice(&0u16.to_be_bytes());
    cmap.extend_from_slice(&4u16.to_be_bytes());
    cmap.extend_from_slice(&subtable_offset.to_be_bytes());
    assert_eq!(cmap.len(), subtable_offset as usize);
    cmap.extend_from_slice(&12u16.to_be_bytes());
    cmap.extend_from_slice(&0u16.to_be_bytes());
    cmap.extend_from_slice(&subtable_length.to_be_bytes());
    cmap.extend_from_slice(&0u32.to_be_bytes());
    cmap.extend_from_slice(&(group_count as u32).to_be_bytes());
    for _ in 0..group_count {
        cmap.extend_from_slice(&(UNICODE_SCALAR_MAX + 1).to_be_bytes());
        cmap.extend_from_slice(&(UNICODE_SCALAR_MAX + 1).to_be_bytes());
        cmap.extend_from_slice(&1u32.to_be_bytes());
    }

    let font = font_with_single_table(b"cmap", &cmap);
    let err = sanitize_cmap12_invalid_groups(&font, 0)
        .expect_err("excessive cmap format-12 rewrites must be rejected");
    assert!(
        err.contains("rewrite count"),
        "error should identify cmap rewrite cap: {err}"
    );
}

#[test]
fn sanitize_cmap12_invalid_groups_ignores_records_overlapping_first_subtable() {
    let first_subtable_rel = 12u32;
    let ghost_subtable_rel = 40u32;
    let first_invalid = [0x110001u32, 0x110002u32, 7u32];
    let ghost_invalid = [0x110010u32, 0x110011u32, 9u32];

    let mut cmap = Vec::new();
    cmap.extend_from_slice(&0u16.to_be_bytes());
    cmap.extend_from_slice(&2u16.to_be_bytes());
    cmap.extend_from_slice(&0u16.to_be_bytes());
    cmap.extend_from_slice(&4u16.to_be_bytes());
    cmap.extend_from_slice(&first_subtable_rel.to_be_bytes());
    assert_eq!(cmap.len(), first_subtable_rel as usize);

    // The declared format-12 length is also what a bogus second encoding
    // record would expose as its subtable offset if parser code kept
    // walking records after the first subtable begins.
    cmap.extend_from_slice(&12u16.to_be_bytes());
    cmap.extend_from_slice(&0u16.to_be_bytes());
    cmap.extend_from_slice(&ghost_subtable_rel.to_be_bytes());
    cmap.extend_from_slice(&0u32.to_be_bytes());
    cmap.extend_from_slice(&1u32.to_be_bytes());
    for value in first_invalid {
        cmap.extend_from_slice(&value.to_be_bytes());
    }
    assert_eq!(cmap.len(), ghost_subtable_rel as usize);

    cmap.extend_from_slice(&12u16.to_be_bytes());
    cmap.extend_from_slice(&0u16.to_be_bytes());
    cmap.extend_from_slice(&28u32.to_be_bytes());
    cmap.extend_from_slice(&0u32.to_be_bytes());
    cmap.extend_from_slice(&1u32.to_be_bytes());
    for value in ghost_invalid {
        cmap.extend_from_slice(&value.to_be_bytes());
    }

    let font = font_with_single_table(b"cmap", &cmap);
    let (sanitized, count) =
        sanitize_cmap12_invalid_groups(&font, 0).expect("test font should sanitize");
    assert_eq!(count, 1);
    let sanitized = sanitized.as_ref();

    let table_offset = 28usize;
    let first_subtable = table_offset + first_subtable_rel as usize;
    assert_eq!(read_be_u32(sanitized, first_subtable + 4), Some(16));
    assert_eq!(read_be_u32(sanitized, first_subtable + 12), Some(0));

    let ghost_group = table_offset + ghost_subtable_rel as usize + 16;
    assert_eq!(read_be_u32(sanitized, ghost_group), Some(ghost_invalid[0]));
    assert_eq!(
        read_be_u32(sanitized, ghost_group + 4),
        Some(ghost_invalid[1])
    );
    assert_eq!(
        read_be_u32(sanitized, ghost_group + 8),
        Some(ghost_invalid[2])
    );
}

#[test]
fn parse_legacy_name_table_entries_rejects_strings_outside_declared_name_table() {
    let family = utf16be_test_bytes("Escaped Family");
    let mut name_table = Vec::new();
    name_table.extend_from_slice(&0u16.to_be_bytes());
    name_table.extend_from_slice(&1u16.to_be_bytes());
    name_table.extend_from_slice(&18u16.to_be_bytes());
    name_table.extend_from_slice(&3u16.to_be_bytes());
    name_table.extend_from_slice(&1u16.to_be_bytes());
    name_table.extend_from_slice(&0x0409u16.to_be_bytes());
    name_table.extend_from_slice(&1u16.to_be_bytes());
    name_table.extend_from_slice(&(family.len() as u16).to_be_bytes());
    name_table.extend_from_slice(&0u16.to_be_bytes());
    assert_eq!(name_table.len(), 18);

    let mut font = font_with_single_table(b"name", &name_table);
    font.extend_from_slice(&family);

    let entries = parse_legacy_name_table_entries(
        &font,
        "C:/fonts/malformed.ttf",
        font.len() as u64,
        false,
        1,
        NO_SCAN_ID,
    );
    assert!(
        entries.is_empty(),
        "name strings beyond the declared table length must be ignored"
    );
}

#[test]
fn decode_legacy_name_record_rejects_oversized_raw_records() {
    let mut raw = Vec::with_capacity(MAX_NAME_RECORD_BYTES + 2);
    while raw.len() <= MAX_NAME_RECORD_BYTES {
        raw.extend_from_slice(&[0, b'A']);
    }

    assert!(decode_legacy_name_record(3, 1, 0x0409, &raw).is_none());
}

fn next_fonts_test_suffix() -> String {
    static NEXT_ID: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let seq = NEXT_ID.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    format!("{}-{stamp}-{seq}", std::process::id())
}

fn make_fonts_test_dir(label: &str) -> crate::test_support::TestTempDir {
    crate::test_support::TestTempDir::new("ssahdrify-fonts-test", label)
}

#[test]
fn legacy_name_merge_preserves_skrifa_english_primary_family_at_index_zero() {
    let mut entries = vec![LocalFontEntry {
        path: "C:\\Fonts\\Zeta.ttf".to_string(),
        index: 0,
        families: vec!["Zeta Sans".to_string(), "Alpha Sans".to_string()],
        face_names: Vec::new(),
        bold: false,
        italic: false,
        size_bytes: 100,
    }];
    // Case 1: legacy duplicates already-decoded names -> order unchanged.
    merge_legacy_name_table_entries(
        &mut entries,
        vec![LocalFontEntry {
            path: "C:\\Fonts\\Zeta.ttf".to_string(),
            index: 0,
            families: vec!["Alpha Sans".to_string(), "Zeta Sans".to_string()],
            face_names: Vec::new(),
            bold: false,
            italic: false,
            size_bytes: 100,
        }],
    );
    assert_eq!(entries[0].families, vec!["Zeta Sans", "Alpha Sans"]);

    // Case 2: legacy adds a DBCS name that sorts before "Zeta Sans" -> "Zeta Sans" stays at [0].
    merge_legacy_name_table_entries(
        &mut entries,
        vec![LocalFontEntry {
            path: "C:\\Fonts\\Zeta.ttf".to_string(),
            index: 0,
            families: vec!["00 Legacy CJK".to_string()],
            face_names: Vec::new(),
            bold: false,
            italic: false,
            size_bytes: 100,
        }],
    );
    assert_eq!(
        entries[0].families,
        vec!["Zeta Sans", "00 Legacy CJK", "Alpha Sans"]
    );
}

#[test]
fn system_font_dir_normalizer_handles_trailing_slash_and_unicode_case() {
    assert_eq!(
        normalize_system_font_dir_path(Path::new("C:\\Windows\\"), &["Fonts"]),
        normalize_system_font_dir_path(Path::new("C:/WINDOWS"), &["fonts"])
    );
    let non_ascii = normalize_system_font_dir_path(
        Path::new("C:\\Users\\Ünsal\\AppData\\Local\\"),
        &["Microsoft", "Windows", "Fonts"],
    );
    assert!(
        non_ascii.contains("ünsal") && !non_ascii.contains("\\\\fonts"),
        "expected Unicode case-folding and clean path joining, got: {non_ascii}"
    );
}

#[test]
fn font_file_skip_reason_distinguishes_missing_over_limit_and_malformed_files() {
    let root = make_fonts_test_dir("skip-reasons");

    let missing = root.join("missing.ttf");
    assert_eq!(
        try_parse_local_font_file(&missing, NO_SCAN_ID)
            .err()
            .expect("missing file should be skipped"),
        FontFileSkipReason::MetadataUnreadable
    );

    let oversized = root.join("oversized.ttf");
    fs::File::create(&oversized)
        .unwrap()
        .set_len(MAX_FONT_DATA_SIZE + 1)
        .unwrap();
    assert_eq!(
        try_parse_local_font_file(&oversized, NO_SCAN_ID)
            .err()
            .expect("oversized file should be skipped"),
        FontFileSkipReason::TooLargeBeforeRead
    );

    let malformed = root.join("malformed.ttf");
    fs::write(&malformed, b"not an OpenType font").unwrap();
    assert_eq!(
        try_parse_local_font_file(&malformed, NO_SCAN_ID)
            .err()
            .expect("malformed file should be skipped"),
        FontFileSkipReason::NoUsableFaces
    );

    let unreadable = root.join("directory.ttf");
    fs::create_dir_all(&unreadable).unwrap();
    assert!(matches!(
        try_parse_local_font_file(&unreadable, NO_SCAN_ID),
        Err(FontFileSkipReason::OpenFailed | FontFileSkipReason::ReadFailed)
    ));

    let unsupported = root.join("not-a-candidate.txt");
    assert!(
        try_parse_local_font_file(&unsupported, NO_SCAN_ID)
            .unwrap()
            .is_empty(),
        "unsupported extensions should remain a quiet non-candidate"
    );

    let _ = fs::remove_dir_all(&root);
}

#[test]
fn font_file_skip_warning_uses_only_a_validated_basename() {
    let safe = Path::new("/private/library/Broken Font.ttf");
    let warning = format_font_file_skip_warning(safe, FontFileSkipReason::NoUsableFaces);
    assert!(warning.contains("Broken Font.ttf"));
    assert!(!warning.contains("private"));
    assert!(!warning.contains("library"));

    let unsafe_name = Path::new("/private/library/evil\u{202e}.ttf");
    let redacted = format_font_file_skip_warning(unsafe_name, FontFileSkipReason::NoUsableFaces);
    assert!(redacted.contains("<unsafe font filename>"));
    assert!(!redacted.contains("evil"));
    assert!(!redacted.contains("private"));
}

/// Boundary tests for the TTC numFonts peek inside
/// `subset_with_index`. A crafted TTC whose declared `numFonts`
/// does not match the actual offset table length must produce a
/// clean Err with attribution naming the requested index and the
/// declared count, BEFORE fontcull's parser allocates any
/// intermediate offset-table buffer.
///
/// The peek lives at TTCHeader offset 8..=11 (`numFonts`, ULONG
/// big-endian). Anything shorter than 12 bytes that still starts
/// with `ttcf` magic must be rejected as a truncated header.
#[test]
fn subset_with_index_rejects_truncated_ttc_header() {
    // 4 bytes of `ttcf` magic + 4 bytes version, but `numFonts`
    // field absent. is_ttc_data passes; the peek must reject.
    let truncated = b"ttcf\x00\x01\x00\x00\x00\x00\x00";
    assert_eq!(truncated.len(), 11);
    let err = subset_with_index(truncated, 0, &[0x41])
        .expect_err("11-byte TTC header (numFonts missing) must Err");
    assert!(
        err.contains("Truncated TTC header"),
        "Err should name the truncation explicitly: {err}"
    );
}

#[test]
fn subset_with_index_rejects_index_at_or_above_declared_num_fonts() {
    // Declared numFonts = 2 (big-endian at offset 8..=11).
    // Request face index 2 (out of range) and 99 (well out of range).
    let header = b"ttcf\x00\x01\x00\x00\x00\x00\x00\x02";
    assert_eq!(header.len(), 12);

    let err_at_cap = subset_with_index(header, 2, &[0x41])
        .expect_err("index == numFonts must Err (zero-indexed)");
    assert!(
        err_at_cap.contains("out of range") && err_at_cap.contains("requested 2"),
        "Err should attribute index + declared count: {err_at_cap}"
    );

    let err_above_cap =
        subset_with_index(header, 99, &[0x41]).expect_err("index >> numFonts must Err");
    assert!(
        err_above_cap.contains("requested 99") && err_above_cap.contains("2 face"),
        "Err should attribute the requested + declared values: {err_above_cap}"
    );
}

/// Exercises `subset_with_index` against face 0 AND face 1 of a real
/// TTC, pinning the contract that both dispatch arms produce distinct,
/// non-empty, smaller-than-original subset bytes. Without an env-var-
/// supplied fixture the test skips (mirrors `tests/test_subset.rs`'s
/// CJK-font convention — multi-face CJK fonts are licensing-encumbered
/// and cannot be checked into the repo).
///
/// Usage on Windows:
///   SSAHDRIFY_TEST_TTC_FONT=C:/Windows/Fonts/msyh.ttc
///   cargo test --lib -- --ignored subset_with_index_handles_ttc
#[test]
#[ignore = "requires SSAHDRIFY_TEST_TTC_FONT env var pointing to a multi-face .ttc"]
fn subset_with_index_handles_ttc_face_zero_and_one() {
    let Ok(font_path) = std::env::var("SSAHDRIFY_TEST_TTC_FONT") else {
        eprintln!("SSAHDRIFY_TEST_TTC_FONT not set — skipping");
        return;
    };
    let font_data = match std::fs::read(&font_path) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("Cannot read {font_path}: {e} — skipping");
            return;
        }
    };
    assert!(
        is_ttc_data(&font_data),
        "fixture at {font_path} must be a TTC (starts with 'ttcf' magic)"
    );

    let codepoints: Vec<u32> = "你好世界Hello".chars().map(|c| c as u32).collect();

    let face0 = subset_with_index(&font_data, 0, &codepoints)
        .expect("face 0 of a TTC should subset cleanly");
    assert!(!face0.is_empty(), "face 0 subset must not be empty");
    assert!(
        face0.len() < font_data.len(),
        "face 0 subset ({} bytes) should be smaller than original ({} bytes)",
        face0.len(),
        font_data.len()
    );

    let face1 = subset_with_index(&font_data, 1, &codepoints)
        .expect("face 1 of a TTC should subset cleanly");
    assert!(!face1.is_empty(), "face 1 subset must not be empty");
    assert!(
        face1.len() < font_data.len(),
        "face 1 subset ({} bytes) should be smaller than original ({} bytes)",
        face1.len(),
        font_data.len()
    );

    // Different faces with different name tables / glyph counts must
    // produce different output bytes. If the from_index path silently
    // collapsed to face 0, this assertion would catch it.
    assert_ne!(
        face0, face1,
        "face 0 and face 1 should produce different subset bytes"
    );
}

struct UserFontDbTestGuard {
    _dir: crate::test_support::TestTempDir,
}

impl Drop for UserFontDbTestGuard {
    fn drop(&mut self) {
        clear_user_font_db_path();
    }
}

fn init_test_user_font_db(name: &str) -> UserFontDbTestGuard {
    let dir = make_fonts_test_dir(&format!("user-font-db-{name}"));
    init_user_font_db(&dir).expect("test DB should initialize");
    clear_font_sources().expect("test DB should clear");
    UserFontDbTestGuard { _dir: dir }
}

/// replaces the previous `tests/test_subset.rs` integration
/// test which called `fontcull::subset_font_data_unicode` directly —
/// bypassing the project's tuned Plan in `subset_with_index` (drop
/// vmtx/LTSH/kern, preserve full name table — the 4.6× size win
/// from forum-feedback (2a), commit `00f44ac`). Unit test inside
/// `mod tests` so `subset_with_index` stays private; outside-crate
/// callers go through the public `subset_font` (which requires
/// provenance setup), and integration tests have no need to expose
/// the internal subset path as public API.
///
/// Usage (any single-face CJK .ttf works):
///   SSAHDRIFY_TEST_CJK_FONT=C:/Windows/Fonts/simsun.ttc
///   cargo test --lib -- --ignored subset_with_index_yields_size_reduction
#[test]
#[ignore = "requires SSAHDRIFY_TEST_CJK_FONT env var pointing to a CJK font file"]
fn subset_with_index_yields_significant_size_reduction_on_cjk() {
    let Ok(font_path) = std::env::var("SSAHDRIFY_TEST_CJK_FONT") else {
        eprintln!("SSAHDRIFY_TEST_CJK_FONT not set — skipping");
        return;
    };
    let font_data = match std::fs::read(&font_path) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("Cannot read {font_path}: {e} — skipping");
            return;
        }
    };
    let original_size = font_data.len();

    // ~50 CJK chars + ASCII / fullwidth padding (mirrors the
    // padding subset_font itself applies in production).
    let subtitle_text = "你好世界，这是一个字幕测试。中文字体子集化可以显著减小文件体积！";
    let mut codepoints: Vec<u32> = subtitle_text.chars().map(|c| c as u32).collect();
    codepoints.extend(0x0020u32..=0x007Eu32);
    codepoints.extend(0xFF01u32..=0xFF5Eu32);
    codepoints.sort();
    codepoints.dedup();

    // Pick face 0 (works for both single-face .ttf and TTC). If the
    // fixture is a TTC, the numFonts peek bounds the index too.
    let subsetted = subset_with_index(&font_data, 0, &codepoints)
        .expect("subset_with_index should succeed on a real CJK font");
    let subset_size = subsetted.len();

    let ratio = (subset_size as f64 / original_size as f64) * 100.0;
    eprintln!(
        "fontset → {} bytes → {} bytes ({:.1}% of original, {} codepoints)",
        original_size,
        subset_size,
        ratio,
        codepoints.len()
    );

    assert!(
        subset_size > 0 && subset_size < original_size,
        "Subset must be non-empty and smaller than original"
    );
    // 70% reduction floor on CJK fonts. The tuned Plan (drop
    // vmtx/LTSH/kern) gives this comfortably; a regression that
    // re-introduced any of those tables would push the ratio
    // above 30%.
    assert!(
        ratio < 30.0,
        "Expected ratio < 30% with tuned Plan, got {:.1}%",
        ratio
    );
}

#[test]
fn gpos_retry_classification_uses_the_structured_table_tag() {
    use fontcull_klippa::SubsetError;
    use fontcull_skrifa::Tag;

    assert!(is_gpos_subset_error(&SubsetError::SubsetTableError(
        Tag::new(b"GPOS")
    )));
    assert!(!is_gpos_subset_error(&SubsetError::SubsetTableError(
        Tag::new(b"GSUB")
    )));
    assert!(!is_gpos_subset_error(&SubsetError::InvalidTag(
        "message happens to mention table 'GPOS'".to_string()
    )));
}

#[test]
#[ignore = "requires SSAHDRIFY_TEST_GPOS_FALLBACK_FONT env var pointing to a local fixture"]
fn subset_with_index_handles_legacy_gpos_failure_font() {
    let Ok(font_path) = std::env::var("SSAHDRIFY_TEST_GPOS_FALLBACK_FONT") else {
        eprintln!("SSAHDRIFY_TEST_GPOS_FALLBACK_FONT not set — skipping");
        return;
    };
    let font_data = match std::fs::read(&font_path) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("Cannot read {font_path}: {e} — skipping");
            return;
        }
    };

    let subsetted = subset_with_index(&font_data, 0, &[0x41])
        .expect("legacy GPOS fallback fixture should subset cleanly");
    assert!(!subsetted.is_empty(), "subset must not be empty");
}

#[test]
#[ignore = "requires SSAHDRIFY_TEST_CMAP12_SANITIZE_FONT env var pointing to a local fixture"]
fn subset_with_index_handles_out_of_unicode_cmap12_groups() {
    let Ok(font_path) = std::env::var("SSAHDRIFY_TEST_CMAP12_SANITIZE_FONT") else {
        eprintln!("SSAHDRIFY_TEST_CMAP12_SANITIZE_FONT not set — skipping");
        return;
    };
    let font_data = match std::fs::read(&font_path) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("Cannot read {font_path}: {e} — skipping");
            return;
        }
    };

    let subsetted = subset_with_index(&font_data, 0, &[0x41, 0x4E2D])
        .expect("format-12 cmap groups beyond Unicode should be ignored");
    assert!(!subsetted.is_empty(), "subset must not be empty");
}

#[test]
#[ignore = "requires SSAHDRIFY_TEST_ALL_FONTS_CACHE env var pointing to a local font cache"]
fn subset_cached_font_faces_in_batch() {
    let Ok(cache_path) = std::env::var("SSAHDRIFY_TEST_ALL_FONTS_CACHE") else {
        eprintln!("SSAHDRIFY_TEST_ALL_FONTS_CACHE not set — skipping");
        return;
    };
    let offset = std::env::var("SSAHDRIFY_TEST_ALL_FONTS_OFFSET")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(0);
    let limit = std::env::var("SSAHDRIFY_TEST_ALL_FONTS_LIMIT")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(usize::MAX);
    let sqlite_limit = i64::try_from(limit).unwrap_or(-1);
    let sqlite_offset = i64::try_from(offset).unwrap_or(i64::MAX);

    let conn = Connection::open_with_flags(cache_path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
        .expect("open local font cache read-only");
    let mut stmt = conn
        .prepare(
            "SELECT font_path, face_index \
             FROM cached_fonts \
             ORDER BY font_path, face_index \
             LIMIT ?1 OFFSET ?2",
        )
        .expect("prepare cached font query");
    let rows = stmt
        .query_map(params![sqlite_limit, sqlite_offset], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, i32>(1)?))
        })
        .expect("query cached fonts");

    let mut checked = 0usize;
    let mut failures = Vec::new();
    for row in rows {
        let (path, face_index) = row.expect("read cached font row");
        checked += 1;
        let Ok(face_index) = u32::try_from(face_index) else {
            failures.push(format!("{path}#invalid-index"));
            continue;
        };
        let font_data = match fs::read(&path) {
            Ok(data) => data,
            Err(error) => {
                failures.push(format!("{path}#{face_index}: read failed: {error}"));
                continue;
            }
        };
        let codepoints = [0x20, 0x41, 0x4E2D, 0x3042, 0xAC00];
        if let Err(error) = subset_with_index(&font_data, face_index, &codepoints) {
            failures.push(format!("{path}#{face_index}: {error}"));
        }
        if checked.is_multiple_of(100) {
            eprintln!("subset cache batch progress: {checked} checked from offset {offset}");
        }
    }

    assert!(
        checked > 0,
        "cache batch at offset {offset} selected no font faces"
    );
    assert!(
        failures.is_empty(),
        "{} subset failure(s) in cache batch at offset {}:\n{}",
        failures.len(),
        offset,
        failures
            .iter()
            .take(20)
            .cloned()
            .collect::<Vec<_>>()
            .join("\n")
    );
}

fn sample_entry(path: &str, family: &str, index: u32) -> LocalFontEntry {
    LocalFontEntry {
        path: path.to_string(),
        index,
        families: vec![family.to_string()],
        face_names: Vec::new(),
        bold: false,
        italic: false,
        size_bytes: 123,
    }
}

#[test]
fn subset_with_index_display_error_carries_diagnostic_context() {
    // A truncated TTF header (4 magic bytes + 3 noise bytes) fails the
    // skrifa parser inside fontcull immediately. The IPC return path
    // formats fontcull errors via `Display` rather than `Debug`
    // because Debug leaks internal struct fields, table tags, and
    // byte offsets into the frontend; this test pins the contract
    // that Display still carries enough context for support
    // diagnostics — i.e., it isn't a bare "subset failed" string.
    //
    // Wrapper prefixes ("Cannot parse font face N:" / "Subset failed
    // for face N:") guarantee a baseline of context regardless of
    // fontcull's Display verbosity, so the assertion below also
    // accepts the prefix text. If a future fontcull upgrade renames
    // the parse error or strips its Display, this test still pins
    // that the wrapper-supplied context is intact.
    let truncated: Vec<u8> = vec![0x00, 0x01, 0x00, 0x00, b'A', b'B', b'C'];
    let err = subset_with_index(&truncated, 0, &[0x41])
        .expect_err("malformed TTF should fail subset_with_index at parse time");
    assert!(!err.is_empty(), "subset error must be non-empty");
    // Either the wrapper prefix OR a recognizable font-format term
    // surfaces — both are acceptable diagnostic signals.
    let lower = err.to_lowercase();
    let has_context = lower.contains("face")
        || lower.contains("font")
        || lower.contains("table")
        || lower.contains("parse")
        || lower.contains("read")
        || lower.contains("invalid")
        || lower.contains("magic")
        || lower.contains("collection");
    assert!(
        has_context,
        "subset error should carry diagnostic context — got: {err}"
    );
    // fontcull's unicode-subset error contract — parse
    // failures must produce a non-empty Display string so downstream
    // wrappers (subset_with_index, subset_font) can interpolate them
    // cleanly into the IPC error path. Pre-`00f44ac` this branch
    // referenced a "subset_font index==0 branch" that no longer
    // exists (dispatch now keys on `is_ttc_data` magic peek, not
    // on `font_index == 0`); the comment was archaeology pointing
    // at a removed code path.
    let unicode_err = fontcull::subset_font_data_unicode(&truncated, &[0x41], &[])
        .expect_err("malformed TTF should fail unicode subset")
        .to_string();
    assert!(
        !unicode_err.is_empty(),
        "fontcull Display must be non-empty"
    );
}

#[test]
fn user_font_key_lowercases_nfc_normalizes_and_separates_with_us() {
    // Case-insensitive — mirrors TS userFontKey case-fold contract.
    assert_eq!(
        user_font_key("Arial", false, false),
        user_font_key("ARIAL", false, false)
    );
    // NFC normalization — precomposed `é` and decomposed `e + ´` key
    // identically. Without NFC, a font name table storing `café` would
    // miss an ASS \fn `café` lookup (and vice versa).
    let precomposed = user_font_key("caf\u{00e9}", false, false);
    let decomposed = user_font_key("cafe\u{0301}", false, false);
    assert_eq!(precomposed, decomposed);
    // Bold and italic flags carry distinctly.
    let plain = user_font_key("Arial", false, false);
    let bold = user_font_key("Arial", true, false);
    let italic = user_font_key("Arial", false, true);
    let both = user_font_key("Arial", true, true);
    assert_eq!(
        [&plain, &bold, &italic, &both]
            .iter()
            .copied()
            .collect::<HashSet<_>>()
            .len(),
        4
    );
    // Separator is U+001F (Unit Separator); pin the exact byte shape so
    // future cross-layer audits land on the same encoding the TS layer
    // produces.
    assert_eq!(plain, "arial\u{001F}0\u{001F}0");
}

#[test]
fn bounded_font_family_name_trims_and_rejects_overlong_values() {
    assert_eq!(
        bounded_font_family_name("  Demo Sans  ".chars()),
        Some("Demo Sans".to_string())
    );
    assert_eq!(
        bounded_font_family_name("x".repeat(256).chars()),
        Some("x".repeat(256))
    );
    assert!(bounded_font_family_name("x".repeat(257).chars()).is_none());
    assert!(bounded_font_family_name("   ".chars()).is_none());
}

#[test]
fn bounded_font_family_name_keeps_full_name_past_leading_whitespace() {
    // Leading whitespace must NOT consume the materialize window and
    // truncate a legitimate 256-codepoint name — that would store a
    // different family string than the font declares and miss at lookup.
    let padded = format!("{}{}", " ".repeat(5), "x".repeat(256));
    assert_eq!(
        bounded_font_family_name(padded.chars()),
        Some("x".repeat(256))
    );
}

#[test]
fn bounded_font_family_name_rejects_overpadded_truncation_alias() {
    let padded_attack = format!("{}{}{}", " ".repeat(1019), "Arial", "X".repeat(252));
    assert!(
        bounded_font_family_name(padded_attack.chars()).is_none(),
        "must validate the full trimmed name, not truncate to the fake alias Arial"
    );
}

#[test]
fn bounded_font_family_name_ignores_only_true_trailing_whitespace() {
    let padded = format!("{}{}", "x".repeat(256), " ".repeat(4096));
    assert_eq!(
        bounded_font_family_name(padded.chars()),
        Some("x".repeat(256))
    );

    let internal_over_cap = format!("{}{}y", "x".repeat(255), " ".repeat(2));
    assert!(
        bounded_font_family_name(internal_over_cap.chars()).is_none(),
        "internal whitespace before a later non-space character counts toward the cap"
    );
}

#[test]
fn bounded_font_family_name_rejects_bidi_and_zero_width() {
    // bounded_font_family_name now delegates to
    // validate_font_family for the BiDi / zero-width / line-sep /
    // U+061C rejection set. Without this pin, a future refactor
    // that drops the validate call (or replaces it with a partial
    // re-check) would let a U+202E-bearing name-table entry land
    // in the session DB and propagate into the UI / log layer.
    // Codepoints chosen to cover the major rejection classes:
    // bidi override, zero-width, line separator, Arabic Letter Mark.
    assert!(bounded_font_family_name("Ari\u{202E}al".chars()).is_none());
    assert!(bounded_font_family_name("Ari\u{200B}al".chars()).is_none());
    assert!(bounded_font_family_name("Ari\u{2028}al".chars()).is_none());
    assert!(bounded_font_family_name("Ari\u{061C}al".chars()).is_none());
    // Counter-assertion: ordinary Unicode (CJK, accented Latin)
    // continues to pass — the rejection is targeted, not over-broad.
    assert_eq!(
        bounded_font_family_name("微软雅黑".chars()),
        Some("微软雅黑".to_string())
    );
    assert_eq!(
        bounded_font_family_name("Demo Sans Pro".chars()),
        Some("Demo Sans Pro".to_string())
    );
}

#[test]
fn face_name_aliases_keep_full_name_that_duplicates_family_variant() {
    let duplicated_family_name = "Dream Han Serif SC W22".to_string();
    let family_variants = [duplicated_family_name.clone()];
    let mut face_name_variants = HashSet::new();
    face_name_variants.insert(duplicated_family_name.clone());
    face_name_variants.insert("DreamHanSerifSC-W22".to_string());

    let aliases = materialize_face_name_aliases(face_name_variants);

    assert!(
        family_variants.contains(&duplicated_family_name),
        "test fixture should model a font that repeats FULL_NAME in FAMILY_NAME"
    );
    assert_eq!(
        aliases,
        vec![
            "Dream Han Serif SC W22".to_string(),
            "DreamHanSerifSC-W22".to_string()
        ]
    );
}

/// Usage on Windows:
///   SSAHDRIFY_TEST_DREAMHAN_SERIF_TTC=<your-font-folder>/DreamHanSerif-W22.ttc
///   cargo test --lib -- --ignored parse_local_dream_han_serif_w22_keeps_duplicate_full_name_alias
#[test]
#[ignore = "requires SSAHDRIFY_TEST_DREAMHAN_SERIF_TTC pointing to a Dream Han Serif W22 TTC"]
fn parse_local_dream_han_serif_w22_keeps_duplicate_full_name_alias() {
    let Some(fixture) = std::env::var_os("SSAHDRIFY_TEST_DREAMHAN_SERIF_TTC").map(PathBuf::from)
    else {
        eprintln!("SSAHDRIFY_TEST_DREAMHAN_SERIF_TTC not set; skipping");
        return;
    };
    if !fixture.is_file() {
        eprintln!(
            "Dream Han fixture not found at {}; skipping",
            fixture.display()
        );
        return;
    }
    let canonical = fixture
        .canonicalize()
        .expect("Dream Han fixture should canonicalize");
    let entries = parse_local_font_file(&canonical, NO_SCAN_ID)
        .expect("the supplied font should be readable and parseable");
    let sc_face = entries
        .iter()
        .find(|entry| {
            entry
                .families
                .iter()
                .any(|name| name == "Dream Han Serif SC W22")
        })
        .unwrap_or_else(|| {
            panic!(
                "fixture did not expose Dream Han Serif SC W22 as a family variant; parsed {} entries",
                entries.len()
            )
        });

    assert!(sc_face.bold, "W22 should be classified as bold");
    assert!(
        sc_face
            .face_names
            .iter()
            .any(|name| name == "Dream Han Serif SC W22"),
        "duplicated FULL_NAME must remain a concrete face alias: {:?}",
        sc_face.face_names
    );
    assert!(
        sc_face
            .face_names
            .iter()
            .any(|name| name == "DreamHanSerifSC-W22"),
        "PostScript name must remain a concrete face alias: {:?}",
        sc_face.face_names
    );
}

fn commit_entries(source_id: &str, entries: Vec<LocalFontEntry>) -> ImportOutcome {
    let mut conn = open_user_font_db().expect("test DB should open");
    let tx = conn
        .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
        .expect("transaction should start");
    create_user_font_source_tx(&tx, source_id, FontSourceKind::Files, None)
        .expect("source should insert");
    let outcome = import_user_font_batch_tx(&tx, source_id, entries).expect("batch should import");
    finish_user_font_source_tx(&tx, source_id, ScanStopReason::Natural)
        .expect("source should finish");
    remove_empty_user_font_source_tx(&tx, source_id, outcome.added)
        .expect("empty source cleanup should work");
    tx.commit().expect("transaction should commit");
    outcome
}

#[test]
fn user_font_lookup_returns_expected_rows_without_repeated_repreparing() {
    let conn = Connection::open_in_memory().expect("test database should open");
    conn.pragma_update(None, "foreign_keys", "ON").unwrap();
    init_user_font_schema(&conn).expect("font schema should initialize");
    conn.execute_batch(
        "INSERT INTO font_sources VALUES('source', 1, 2, NULL, 0);
         INSERT INTO font_faces VALUES
             (1, 'alpha-regular.ttf', 0, 0, 0, 1),
             (2, 'alpha-bold.ttf', 1, 1, 0, 1),
             (3, 'alpha-italic.ttf', 2, 0, 1, 1),
             (4, 'beta-bold-italic.ttf', 3, 1, 1, 1);
         INSERT INTO font_source_faces VALUES
             ('source', 1), ('source', 2), ('source', 3), ('source', 4);
         INSERT INTO font_family_keys VALUES
             ('alpha', 0, 0, 0, 1), ('alpha', 1, 0, 0, 2),
             ('alpha', 0, 1, 0, 3), ('beta', 1, 1, 0, 4),
             ('beta alias', 0, 0, 1, 4);",
    )
    .expect("font lookup fixtures should insert");

    for (family, bold, italic, expected) in [
        ("alpha", false, false, Some(("alpha-regular.ttf", 0))),
        ("alpha", true, false, Some(("alpha-bold.ttf", 1))),
        ("alpha", false, true, Some(("alpha-italic.ttf", 2))),
        ("beta", true, true, Some(("beta-bold-italic.ttf", 3))),
        (
            "beta alias",
            false,
            false,
            Some(("beta-bold-italic.ttf", 3)),
        ),
        ("beta alias", true, true, Some(("beta-bold-italic.ttf", 3))),
        ("alpha", true, true, None),
        ("missing", false, false, None),
    ] {
        // Start with resolve_user_font's fresh lookup, then probe reuse for regression.
        let mut statement = conn
            .prepare(USER_FONT_LOOKUP_SQL)
            .expect("font lookup should prepare");
        let mut initial_reprepares = None;
        for execution in 0..3 {
            let actual = statement
                .query_row(
                    params![
                        crate::font_cache::family_lookup_key(family),
                        USER_FONT_KEY_KIND_FAMILY,
                        i32::from(bold),
                        i32::from(italic),
                        USER_FONT_KEY_KIND_FACE_ALIAS,
                    ],
                    |row| Ok((row.get::<_, String>(0)?, row.get::<_, u32>(1)?)),
                )
                .optional()
                .expect("font lookup should execute");
            assert_eq!(
                actual,
                expected.map(|(path, index)| (path.to_string(), index)),
                "family={family}, bold={bold}, italic={italic}, execution={execution}"
            );
            let reprepares = statement.get_status(rusqlite::StatementStatus::RePrepare);
            if let Some(initial) = initial_reprepares {
                assert_eq!(
                    reprepares, initial,
                    "unchanged bindings must not repeatedly recompile the lookup: family={family}, bold={bold}, italic={italic}, execution={execution}"
                );
            } else {
                // SQLite may optimize once after the initial 0/1 parameter binding.
                assert!(
                    (0..=1).contains(&reprepares),
                    "fresh lookup preparation must stay bounded: family={family}, bold={bold}, italic={italic}"
                );
                initial_reprepares = Some(reprepares);
            }
        }
    }
}

#[test]
fn db_schema_indexes_family_keys_for_cascade_delete() {
    let _guard = lock_font_state_for_test();
    let _db = init_test_user_font_db("schema-index");
    let conn = open_user_font_db().expect("test DB should open");
    let index_count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type = 'index' AND name = 'idx_font_family_face'",
            [],
            |row| row.get(0),
        )
        .expect("schema index query should work");
    assert_eq!(index_count, 1);
}

#[test]
fn db_import_dedupes_faces_and_resolves_family() {
    let _guard = lock_font_state_for_test();
    let _db = init_test_user_font_db("dedupe");
    let outcome = commit_entries(
        "source-a",
        vec![
            sample_entry("C:\\Fonts\\A.ttf", "Demo Sans", 0),
            sample_entry("C:\\Fonts\\A.ttf", "Demo Sans Duplicate", 0),
        ],
    );

    assert_eq!(outcome.added, 1);
    assert_eq!(outcome.duplicated, 1);
    let resolved = resolve_user_font("Demo Sans".to_string(), false, false)
        .unwrap()
        .expect("family should resolve");
    assert_eq!(resolved.path, "C:\\Fonts\\A.ttf");
    assert!(is_user_font_face_registered("C:\\Fonts\\A.ttf", 0).unwrap());
    assert!(!is_user_font_face_registered("C:\\Fonts\\A.ttf", 1).unwrap());
}

#[test]
fn cache_metadata_conversion_fails_if_a_scanned_face_disappears() {
    let _guard = lock_font_state_for_test();
    let missing_path = std::env::temp_dir().join(format!(
        "ssahdrify-missing-cache-face-{}.ttf",
        next_fonts_test_suffix()
    ));
    let _ = fs::remove_file(&missing_path);
    let missing_path_text = missing_path.to_string_lossy().into_owned();
    let error =
        entries_to_cache_metadata(&[sample_entry(&missing_path_text, "Missing Cache Face", 0)])
            .expect_err("a missing scanned face must abort cache publication");
    assert!(error.contains(&missing_path_text));
}

#[test]
fn full_face_names_match_without_weakening_family_style_matching() {
    let _guard = lock_font_state_for_test();
    let _db = init_test_user_font_db("face-name-alias");
    let temp_font_dir = make_fonts_test_dir("face-name-alias");
    let temp_font_path = temp_font_dir.join("DreamHanSerif-W22.ttc");
    fs::write(&temp_font_path, b"fake Dream Han cache metadata fixture")
        .expect("test font file should be written");
    let temp_font_path_str = temp_font_path.to_string_lossy().into_owned();
    let temp_font_size = fs::metadata(&temp_font_path)
        .expect("test font metadata should be readable")
        .len();
    let entry = LocalFontEntry {
        path: temp_font_path_str.clone(),
        index: 3,
        families: vec![
            "Dream Han Serif SC".to_string(),
            "Dream Han Serif SC W22".to_string(),
            "梦源宋体 SC".to_string(),
        ],
        face_names: vec![
            "Dream Han Serif SC W22".to_string(),
            "DreamHanSerifSC-W22".to_string(),
        ],
        bold: true,
        italic: false,
        size_bytes: temp_font_size,
    };
    let metadata = entries_to_cache_metadata(std::slice::from_ref(&entry)).unwrap();
    assert_eq!(metadata.len(), 1);
    let keys: HashSet<String> = metadata[0]
        .family_keys
        .iter()
        .map(|key| user_font_key(&key.family_name, key.bold, key.italic))
        .collect();

    assert!(keys.contains(&user_font_key("Dream Han Serif SC", true, false)));
    assert!(keys.contains(&user_font_key("Dream Han Serif SC W22", true, false)));
    assert!(!keys.contains(&user_font_key("Dream Han Serif SC", false, false)));
    assert!(!keys.contains(&user_font_key("Dream Han Serif SC W22", false, false)));
    assert_eq!(
        metadata[0].face_name_aliases,
        vec![
            "Dream Han Serif SC W22".to_string(),
            "DreamHanSerifSC-W22".to_string()
        ]
    );

    commit_entries("source-dream-han", vec![entry]);

    let direct_face_name = resolve_user_font("Dream Han Serif SC W22".to_string(), false, false)
        .unwrap()
        .expect("full face name should resolve regardless of ASS bold flag");
    assert_eq!(direct_face_name.path.as_str(), temp_font_path_str.as_str());
    assert_eq!(direct_face_name.index, 3);

    let postscript_name = resolve_user_font("DreamHanSerifSC-W22".to_string(), false, true)
        .unwrap()
        .expect("PostScript face alias should resolve regardless of ASS italic flag");
    assert_eq!(postscript_name.path.as_str(), temp_font_path_str.as_str());
    assert_eq!(postscript_name.index, 3);

    assert!(
        resolve_user_font("Dream Han Serif SC".to_string(), true, false)
            .unwrap()
            .is_some()
    );
    assert!(
        resolve_user_font("Dream Han Serif SC".to_string(), false, false)
            .unwrap()
            .is_none()
    );

    let _ = fs::remove_dir_all(&temp_font_dir);
}

#[test]
fn full_face_aliases_do_not_override_exact_family_matches() {
    let _guard = lock_font_state_for_test();
    let _db = init_test_user_font_db("face-name-priority");
    let exact = LocalFontEntry {
        path: "C:\\Fonts\\ExactSharedSans-Regular.otf".to_string(),
        index: 0,
        families: vec!["Shared Sans".to_string()],
        face_names: Vec::new(),
        bold: false,
        italic: false,
        size_bytes: 1_000_000,
    };
    let alias = LocalFontEntry {
        path: "C:\\Fonts\\AliasFace-Bold.otf".to_string(),
        index: 0,
        families: vec!["Other Sans".to_string()],
        face_names: vec!["Shared Sans".to_string()],
        bold: true,
        italic: false,
        size_bytes: 1_000_000,
    };

    commit_entries("source-exact", vec![exact]);
    commit_entries("source-alias-newer", vec![alias]);

    let exact_result = resolve_user_font("Shared Sans".to_string(), false, false)
        .unwrap()
        .expect("exact family should resolve");
    assert_eq!(exact_result.path, "C:\\Fonts\\ExactSharedSans-Regular.otf");

    let alias_result = resolve_user_font("Shared Sans".to_string(), true, true)
        .unwrap()
        .expect("face alias should resolve when exact style is absent");
    assert_eq!(alias_result.path, "C:\\Fonts\\AliasFace-Bold.otf");
}

// ── cache provenance gate pins ──
//
// ALLOWED_CACHE_FONT_PATHS is a SECOND trusted set (alongside
// ALLOWED_FONT_PATHS for system fonts) so cache lookup hits can
// pass subset_font's gate. These three tests pin the gate's three
// documented states: rejects unregistered, accepts registered, and
// refuses to grow past MAX_PROVENANCE_CACHE_SIZE. Without these
// pins, a refactor that drops the (path, face_index) pair-keying
// or accidentally widens the gate to trust EVERY path that ever
// entered the SQLite cache (the anti-pattern the design explicitly
// rejects) would still compile and pass higher-level integration
// tests.
//
// Tests acquire SCAN_TEST_LOCK because they mutate
// ALLOWED_CACHE_FONT_PATHS, a process-global mutex. Clean up
// after themselves so the assertion order doesn't matter and
// the suite stays parallel-safe.

#[test]
fn provenance_overflow_messages_match_each_set_recovery_path() {
    assert_eq!(
        provenance_overflow_message(ProvenanceSetKind::Cache),
        format!(
            "Too many registered font paths in cache set (> {MAX_PROVENANCE_CACHE_SIZE}). \
             Clear font sources or clear the font cache, then retry."
        )
    );
    assert_eq!(
        provenance_overflow_message(ProvenanceSetKind::System),
        format!(
            "Too many registered font paths in system set (> {MAX_PROVENANCE_CACHE_SIZE}). \
             Restart the app to clear the system-font registration set, then retry."
        )
    );
}

struct CacheProvenanceSnapshotGuard {
    snapshot: HashSet<(String, u32)>,
}

impl CacheProvenanceSnapshotGuard {
    fn acquire() -> Self {
        let mut set = ALLOWED_CACHE_FONT_PATHS
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let snapshot = set.clone();
        set.clear();
        Self { snapshot }
    }
}

impl Drop for CacheProvenanceSnapshotGuard {
    fn drop(&mut self) {
        *ALLOWED_CACHE_FONT_PATHS
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = std::mem::take(&mut self.snapshot);
    }
}

#[test]
fn cache_provenance_gate_rejects_unregistered_path() {
    let _guard = lock_font_state_for_test();
    let _prov = CacheProvenanceSnapshotGuard::acquire();

    let path = "C:\\Fonts\\NotRegistered.ttf".to_string();
    let key = (path, 0);
    assert!(
        !ALLOWED_CACHE_FONT_PATHS.lock().unwrap().contains(&key),
        "unregistered cache path must NOT be in the provenance set"
    );
}

#[test]
fn cache_provenance_gate_accepts_registered_path() {
    let _guard = lock_font_state_for_test();
    let _prov = CacheProvenanceSnapshotGuard::acquire();

    let path = "C:\\Fonts\\Registered.ttf";
    let hit = crate::font_cache::FontLookupResult {
        font_path: path.to_string(),
        face_index: 2,
    };
    register_cache_provenance(&hit).expect("register should succeed under cap");
    let key = (path.to_string(), 2);
    assert!(
        ALLOWED_CACHE_FONT_PATHS.lock().unwrap().contains(&key),
        "registered (path, face_index) must be in the provenance set"
    );
    let wrong_face = (path.to_string(), 5);
    assert!(
        !ALLOWED_CACHE_FONT_PATHS
            .lock()
            .unwrap()
            .contains(&wrong_face),
        "different face_index on same path must not slip through pair-keyed gate"
    );
}

#[test]
fn cache_mutation_invalidation_clears_registered_lookup_provenance() {
    let _guard = lock_font_state_for_test();
    let _prov = CacheProvenanceSnapshotGuard::acquire();
    let path = "C:\\Fonts\\ReplacedAfterLookup.ttf";
    let hit = crate::font_cache::FontLookupResult {
        font_path: path.to_string(),
        face_index: 0,
    };
    register_cache_provenance(&hit).unwrap();
    assert!(cache_provenance_contains(path, 0));

    clear_cache_provenance();
    assert!(
        !cache_provenance_contains(path, 0),
        "a cache replacement or eviction must revoke earlier lookup trust"
    );
}

#[test]
fn cache_provenance_gate_is_capped_at_max_provenance() {
    let _guard = lock_font_state_for_test();
    let _prov = CacheProvenanceSnapshotGuard::acquire();

    for i in 0..MAX_PROVENANCE_CACHE_SIZE {
        let hit = crate::font_cache::FontLookupResult {
            font_path: format!("C:\\Fonts\\Cap{i}.ttf"),
            face_index: 0,
        };
        register_cache_provenance(&hit).expect("pre-cap insert should succeed");
    }
    assert_eq!(
        ALLOWED_CACHE_FONT_PATHS.lock().unwrap().len(),
        MAX_PROVENANCE_CACHE_SIZE,
        "set should sit exactly at the cap"
    );
    let overflow = register_cache_provenance(&crate::font_cache::FontLookupResult {
        font_path: "C:\\Fonts\\Overflow.ttf".to_string(),
        face_index: 0,
    });
    assert!(
        overflow.is_err(),
        "past-cap insert must return Err (rolled back)"
    );
    let err_msg = overflow.unwrap_err();
    assert!(
        err_msg.contains("Clear font sources or clear the font cache, then retry."),
        "cache-set overflow must name a reachable recovery action, got: {err_msg}"
    );
    assert_eq!(
        ALLOWED_CACHE_FONT_PATHS.lock().unwrap().len(),
        MAX_PROVENANCE_CACHE_SIZE,
        "rollback should leave the set at exactly the cap, not cap+1"
    );
}

#[test]
fn db_lookup_prefers_newer_source_for_same_family_key() {
    let _guard = lock_font_state_for_test();
    let _db = init_test_user_font_db("source-order");
    commit_entries(
        "source-a",
        vec![sample_entry("C:\\Fonts\\Old.ttf", "Demo Sans", 0)],
    );
    commit_entries(
        "source-b",
        vec![sample_entry("C:\\Fonts\\New.ttf", "Demo Sans", 0)],
    );

    let resolved = resolve_user_font("Demo Sans".to_string(), false, false)
        .unwrap()
        .expect("family should resolve");
    assert_eq!(resolved.path, "C:\\Fonts\\New.ttf");
}

#[test]
fn db_remove_and_clear_update_lookup_and_provenance() {
    let _guard = lock_font_state_for_test();
    let _db = init_test_user_font_db("remove-clear");
    commit_entries(
        "source-a",
        vec![sample_entry("C:\\Fonts\\A.ttf", "Demo Sans", 0)],
    );
    commit_entries(
        "source-b",
        vec![sample_entry("C:\\Fonts\\B.ttf", "Demo Sans", 0)],
    );

    remove_font_source("source-b".to_string(), None).unwrap();
    let resolved = resolve_user_font("Demo Sans".to_string(), false, false)
        .unwrap()
        .expect("older source should become visible again");
    assert_eq!(resolved.path, "C:\\Fonts\\A.ttf");
    assert!(!is_user_font_face_registered("C:\\Fonts\\B.ttf", 0).unwrap());

    clear_font_sources().unwrap();
    assert!(resolve_user_font("Demo Sans".to_string(), false, false)
        .unwrap()
        .is_none());
    assert!(!is_user_font_face_registered("C:\\Fonts\\A.ttf", 0).unwrap());
}

#[test]
fn db_overlapping_sources_keep_independent_face_ownership() {
    let _guard = lock_font_state_for_test();
    let _db = init_test_user_font_db("overlapping-ownership");
    let shared = sample_entry("C:\\Fonts\\Shared.ttf", "Shared Sans", 0);
    commit_entries("source-parent", vec![shared.clone()]);
    commit_entries("source-child", vec![shared]);

    let conn = open_user_font_db().unwrap();
    let memberships: i64 = conn
        .query_row("SELECT COUNT(*) FROM font_source_faces", [], |row| {
            row.get(0)
        })
        .unwrap();
    let faces: i64 = conn
        .query_row("SELECT COUNT(*) FROM font_faces", [], |row| row.get(0))
        .unwrap();
    assert_eq!(memberships, 2);
    assert_eq!(faces, 1);
    drop(conn);

    remove_font_source("source-parent".to_string(), None).unwrap();
    let resolved = resolve_user_font("Shared Sans".to_string(), false, false)
        .unwrap()
        .expect("remaining owner should keep the shared face available");
    assert_eq!(resolved.path, "C:\\Fonts\\Shared.ttf");

    remove_font_source("source-child".to_string(), None).unwrap();
    assert!(resolve_user_font("Shared Sans".to_string(), false, false)
        .unwrap()
        .is_none());
    assert!(!is_user_font_face_registered("C:\\Fonts\\Shared.ttf", 0).unwrap());
}

#[test]
fn overlapping_rescan_refreshes_shared_face_metadata_and_keys() {
    let _guard = lock_font_state_for_test();
    let _db = init_test_user_font_db("overlapping-refresh");
    let path = "C:\\Fonts\\Replaced.ttf";
    commit_entries("source-parent", vec![sample_entry(path, "Old Family", 0)]);

    let mut replacement = sample_entry(path, "New Family", 0);
    replacement.bold = true;
    replacement.size_bytes = 456;
    commit_entries("source-child", vec![replacement]);

    assert!(resolve_user_font("Old Family".to_string(), false, false)
        .unwrap()
        .is_none());
    let refreshed = resolve_user_font("New Family".to_string(), true, false)
        .unwrap()
        .expect("new metadata from the overlapping rescan should replace stale keys");
    assert_eq!(refreshed.path, path);

    remove_font_source("source-parent".to_string(), None).unwrap();
    assert!(resolve_user_font("New Family".to_string(), true, false)
        .unwrap()
        .is_some());
}

#[test]
fn recursive_discovery_is_opt_in_and_deterministic() {
    use std::io::Write as _;
    let root = make_fonts_test_dir("recursive-discovery");
    let nested = root.join("b").join("deeper");
    fs::create_dir_all(&nested).unwrap();
    fs::create_dir_all(root.join("a")).unwrap();
    for relative in ["top.ttf", "a/first.otf", "b/deeper/last.ttc"] {
        let path = root.join(relative);
        fs::File::create(path)
            .unwrap()
            .write_all(b"not-a-real-font")
            .unwrap();
    }
    let canonical = root.canonicalize().unwrap();

    let shallow = discover_font_files(&canonical, FontDirectoryScope::Shallow, NO_SCAN_ID)
        .expect("shallow discovery should succeed");
    assert_eq!(shallow.reason, ScanStopReason::Natural);
    assert_eq!(shallow.files.len(), 1);
    assert_eq!(shallow.directories.len(), 1);

    let recursive = discover_font_files(&canonical, FontDirectoryScope::Recursive, NO_SCAN_ID)
        .expect("recursive discovery should succeed");
    assert_eq!(recursive.reason, ScanStopReason::Natural);
    assert_eq!(recursive.files.len(), 3);
    assert_eq!(recursive.directories.len(), 4);
    let ordered: Vec<_> = recursive
        .files
        .iter()
        .map(|file| {
            file.path
                .file_name()
                .unwrap()
                .to_string_lossy()
                .into_owned()
        })
        .collect();
    assert_eq!(ordered, vec!["first.otf", "last.ttc", "top.ttf"]);

    let _ = fs::remove_dir_all(&root);
}

#[test]
fn collecting_scan_returns_its_full_recursive_snapshot_when_no_face_parses() {
    let root = make_fonts_test_dir("collecting-snapshot");
    let nested = root.join("nested");
    fs::create_dir_all(&nested).unwrap();
    fs::write(nested.join("broken.ttf"), b"not a font").unwrap();

    let (entries, snapshot) =
        scan_directory_collecting_with_snapshot(&root, FontDirectoryScope::Recursive)
            .expect("complete scan should return its discovery snapshot");
    assert!(entries.is_empty(), "malformed font must not produce faces");
    assert_eq!(snapshot.directories.len(), 2);
    assert_eq!(snapshot.files.len(), 1);
    assert!(snapshot.files[0].file_path.ends_with("broken.ttf"));

    let _ = fs::remove_dir_all(&root);
}

struct ReadFailureFixtures(
    PathBuf,
    #[allow(dead_code)] crate::test_support::TestTempDir,
);

impl ReadFailureFixtures {
    fn new(label: &str) -> Self {
        let root = make_fonts_test_dir(&format!("font-read-{label}"));
        for basename in ["a.ttf", "b.ttf", "c.ttf"] {
            let font = font_with_name_records(&[(
                1,
                0x0409,
                utf16be_test_bytes(&format!("Test {basename}")),
            )]);
            fs::write(root.join(basename), font).unwrap();
        }
        let canonical = root.canonicalize().unwrap();
        Self(canonical, root)
    }

    fn paths(&self) -> Vec<String> {
        ["a.ttf", "b.ttf", "c.ttf"]
            .map(|name| normalize_canonical_path(&self.0.join(name).to_string_lossy()))
            .to_vec()
    }
}

fn scan_with_file_result(
    fixtures: &ReadFailureFixtures,
    scope: Option<FontDirectoryScope>,
    failure: Option<FontFileSkipReason>,
) -> (ScanOutcome, Vec<LocalFontEntry>, Vec<String>) {
    let mut parsed_names = Vec::new();
    let mut emitted = Vec::new();
    let parser = |path: &Path, scan_id| {
        let name = path.file_name().unwrap().to_str().unwrap();
        parsed_names.push(name.to_string());
        if name == "b.ttf" {
            if let Some(reason) = failure {
                return Err(reason);
            }
        }
        parse_local_font_file(path, scan_id)
    };
    let emit = |batch| {
        emitted.extend(batch);
        Ok(())
    };
    let outcome = match scope {
        Some(scope) => scan_directory_with_scope_inner_with_parser(
            &fixtures.0,
            scope,
            NO_SCAN_ID,
            None,
            parser,
            emit,
        ),
        None => scan_files_inner_with_operations(
            fixtures.paths(),
            NO_SCAN_ID,
            resolve_explicit_font_file,
            parser,
            emit,
        ),
    }
    .expect("scan outcome should preserve partial results");
    (outcome, emitted, parsed_names)
}

#[test]
fn font_read_failures_preserve_partial_results_and_refuse_cache_publication() {
    let fixtures = ReadFailureFixtures::new("incomplete");
    for scope in [
        Some(FontDirectoryScope::Shallow),
        Some(FontDirectoryScope::Recursive),
        None,
    ] {
        let (control, entries, parsed) = scan_with_file_result(&fixtures, scope, None);
        assert_eq!(control.reason, ScanStopReason::Natural);
        assert_eq!(control.total, 3);
        assert_eq!(entries.len(), 3);
        assert_eq!(parsed, ["a.ttf", "b.ttf", "c.ttf"]);

        for failure in [
            FontFileSkipReason::MetadataUnreadable,
            FontFileSkipReason::OpenFailed,
            FontFileSkipReason::ReadFailed,
            FontFileSkipReason::GrewPastReadLimit,
        ] {
            let (outcome, entries, parsed) = scan_with_file_result(&fixtures, scope, Some(failure));
            assert_eq!(outcome.reason, ScanStopReason::IncompleteIo, "{failure:?}");
            assert_eq!(outcome.total, 1);
            assert_eq!(entries.len(), 1, "valid earlier faces remain available");
            assert!(entries[0].path.ends_with("a.ttf"));
            assert_eq!(parsed, ["a.ttf", "b.ttf"], "stop after incomplete I/O");
            assert!(!is_cache_publication_eligible(&cache_candidate(
                outcome.reason,
                entries.len(),
                false,
            )));
        }
    }
}

#[test]
fn malformed_and_preexisting_oversize_fonts_remain_complete_scan_skips() {
    let fixtures = ReadFailureFixtures::new("intentional-skip");
    for scope in [
        Some(FontDirectoryScope::Shallow),
        Some(FontDirectoryScope::Recursive),
        None,
    ] {
        for failure in [
            FontFileSkipReason::NoUsableFaces,
            FontFileSkipReason::TooLargeBeforeRead,
        ] {
            let (outcome, entries, parsed) = scan_with_file_result(&fixtures, scope, Some(failure));
            assert_eq!(outcome.reason, ScanStopReason::Natural, "{failure:?}");
            assert_eq!(outcome.total, 2);
            assert_eq!(entries.len(), 2);
            assert!(entries[0].path.ends_with("a.ttf"));
            assert!(entries[1].path.ends_with("c.ttf"));
            assert_eq!(parsed, ["a.ttf", "b.ttf", "c.ttf"]);
        }
    }
}

#[test]
fn collecting_scan_rejects_incomplete_reads_instead_of_returning_cache_rows() {
    let fixtures = ReadFailureFixtures::new("collecting");
    for scope in [FontDirectoryScope::Shallow, FontDirectoryScope::Recursive] {
        let mut snapshot = None;
        let result = scan_directory_collecting_impl(
            &fixtures.0,
            scope,
            Some(&mut snapshot),
            |path, scan_id| {
                if path.file_name().unwrap() == "b.ttf" {
                    Err(FontFileSkipReason::OpenFailed)
                } else {
                    parse_local_font_file(path, scan_id)
                }
            },
        );
        let error = result
            .err()
            .expect("incomplete scan cannot return publishable rows");
        assert!(error.contains("IncompleteIo"));
        assert!(error.contains("persistent cache was not changed"));
        assert_eq!(snapshot.unwrap().files.len(), 3);
    }
}

#[test]
fn explicit_file_resolution_failures_preserve_partial_results_as_incomplete() {
    let fixtures = ReadFailureFixtures::new("resolution");
    for fail_metadata in [false, true] {
        for kind in [
            std::io::ErrorKind::PermissionDenied,
            std::io::ErrorKind::NotFound,
        ] {
            let mut emitted = Vec::new();
            let mut parsed = Vec::new();
            let outcome = scan_files_inner_with_operations(
                fixtures.paths(),
                NO_SCAN_ID,
                |selected| {
                    resolve_explicit_font_file_with(
                        selected,
                        |path| {
                            if !fail_metadata && path.file_name().unwrap() == "b.ttf" {
                                Err(std::io::Error::new(kind, "injected resolution failure"))
                            } else {
                                path.canonicalize()
                            }
                        },
                        |path| {
                            if fail_metadata && path.file_name().unwrap() == "b.ttf" {
                                Err(std::io::Error::new(kind, "injected metadata failure"))
                            } else {
                                fs::metadata(path)
                            }
                        },
                    )
                },
                |path, scan_id| {
                    parsed.push(path.file_name().unwrap().to_string_lossy().into_owned());
                    parse_local_font_file(path, scan_id)
                },
                |batch| {
                    emitted.extend(batch);
                    Ok(())
                },
            )
            .unwrap();
            assert_eq!(outcome.reason, ScanStopReason::IncompleteIo);
            assert_eq!(outcome.total, 1);
            assert_eq!(emitted.len(), 1);
            assert!(emitted[0].path.ends_with("a.ttf"));
            assert_eq!(parsed, ["a.ttf"], "the unresolved file must not be parsed");
        }
    }
}

#[test]
fn explicit_file_scan_keeps_unsupported_and_non_file_targets_as_normal_skips() {
    let fixtures = ReadFailureFixtures::new("non-font-targets");
    let unsupported = fixtures.0.join("notes.txt");
    fs::write(&unsupported, "not a font").unwrap();
    let directory = fixtures.0.join("directory.ttf");
    fs::create_dir(&directory).unwrap();
    let mut paths = vec![
        String::new(),
        "has\u{0000}control.ttf".to_string(),
        normalize_canonical_path(&unsupported.to_string_lossy()),
        normalize_canonical_path(&directory.to_string_lossy()),
    ];
    paths.extend(fixtures.paths());
    let mut emitted = Vec::new();
    let outcome = scan_files_inner(paths, NO_SCAN_ID, |batch| {
        emitted.extend(batch);
        Ok(())
    })
    .unwrap();
    assert_eq!(outcome.reason, ScanStopReason::Natural);
    assert_eq!(outcome.total, 3);
    assert_eq!(emitted.len(), 3);
}

#[test]
fn explicit_file_scan_reports_a_disappeared_font_as_incomplete() {
    let fixtures = ReadFailureFixtures::new("missing-target");
    let missing = normalize_canonical_path(&fixtures.0.join("missing.ttf").to_string_lossy());
    let mut emitted = Vec::new();
    let outcome = scan_files_inner(vec![missing], NO_SCAN_ID, |batch| {
        emitted.extend(batch);
        Ok(())
    })
    .unwrap();
    assert_eq!(outcome.reason, ScanStopReason::IncompleteIo);
    assert_eq!(outcome.total, 0);
    assert!(emitted.is_empty());
}

/// `scan_directory_inner` on a non-existent path surfaces the read_dir
/// error as the user-facing string. The closure is never called.
#[test]
fn directory_inner_rejects_missing_dir() {
    let mut emitted: Vec<Vec<LocalFontEntry>> = Vec::new();
    let bogus = Path::new("Z:\\absolutely-not-a-real-directory\\for-testing");
    let result =
        scan_directory_with_scope_inner(bogus, FontDirectoryScope::Shallow, 1, None, |batch| {
            emitted.push(batch);
            Ok(())
        });
    assert!(result.is_err());
    assert!(emitted.is_empty());
}

/// `scan_directory_inner` walks a directory of non-font files to
/// natural completion without emitting batches or producing faces.
/// Pins two related contracts: `has_allowed_font_extension` filters
/// non-font files, and the visited-entry cap does not false-fire on
/// normal-size directories (CeilingHit only on real overflow). A
/// future refactor that drops or shifts either guard would either
/// eat budget on dirs full of `.txt` / `.png` / `.log` files
/// (the original gap this test guards against) or false-report
/// `CeilingHit` here.
#[test]
fn directory_inner_skips_non_font_files_without_emitting() {
    use std::io::Write as _;
    let dir = make_fonts_test_dir("non-font-skip");
    for n in [
        "a.txt",
        "b.png",
        "c.log",
        "d.json",
        "e.md",
        "f.csv",
        "g.bin",
        "h.gitignore",
    ] {
        let p = dir.join(n);
        std::fs::File::create(&p)
            .unwrap()
            .write_all(b"not-a-font")
            .unwrap();
    }

    let mut emitted: Vec<Vec<LocalFontEntry>> = Vec::new();
    let outcome = scan_directory_with_scope_inner(
        &dir,
        FontDirectoryScope::Shallow,
        NO_SCAN_ID,
        None,
        |batch| {
            emitted.push(batch);
            Ok(())
        },
    )
    .expect("non-font directory should complete naturally");
    assert_eq!(outcome.total, 0);
    assert_eq!(outcome.reason, ScanStopReason::Natural);
    assert!(
        emitted.is_empty(),
        "no batches expected; got {} batch(es)",
        emitted.len()
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// `scan_files_inner` skips invalid entries (empty / oversized / control
/// chars) silently and emits nothing when none of the inputs resolve to
/// a real font file. The streaming contract holds for the empty case:
/// the closure receives zero batches.
#[test]
fn files_inner_skips_invalid_paths_without_emitting() {
    let mut emitted: Vec<Vec<LocalFontEntry>> = Vec::new();
    let bad_paths = vec![
        String::new(),                    // empty
        "x".repeat(5000),                 // oversized
        "has\u{0000}control".to_string(), // control char
    ];
    let outcome = scan_files_inner(bad_paths, 2, |batch| {
        emitted.push(batch);
        Ok(())
    })
    .expect("invalid paths should be skipped, not error");
    assert_eq!(outcome.total, 0);
    assert_eq!(outcome.reason, ScanStopReason::Natural);
    assert!(emitted.is_empty());
}

/// A targeted cancel before the first file causes an immediate return on
/// the first iteration. Validates the cancel-poll path without depending
/// on real font files. Buffer is empty so no batch is emitted.
#[test]
fn files_inner_honors_targeted_cancel_before_first_file() {
    let _guard = lock_font_state_for_test();
    let scan_id = 42u64;
    let _active = begin_font_scan(scan_id).expect("begin scan");
    cancel_font_scan(scan_id);
    let mut emitted: Vec<Vec<LocalFontEntry>> = Vec::new();
    let outcome = scan_files_inner(vec!["irrelevant.ttf".to_string()], scan_id, |batch| {
        emitted.push(batch);
        Ok(())
    })
    .expect("cancel returns Ok with partial results");
    assert_eq!(outcome.total, 0);
    assert_eq!(outcome.reason, ScanStopReason::UserCancel);
    assert!(emitted.is_empty());
}

#[test]
fn cancel_command_records_scan_id_monotonically() {
    let _lock = lock_font_state_for_test();
    let scan_id = 500u64;
    let _guard = begin_font_scan(scan_id).expect("begin scan");
    cancel_font_scan(scan_id);
    assert!(font_scan_cancelled(scan_id));
    cancel_font_scan(scan_id - 1);
    assert!(font_scan_cancelled(scan_id));
}

#[test]
fn stale_lower_id_cancel_during_uncancelled_scan_does_not_poison_future_scan() {
    let _lock = lock_font_state_for_test();
    {
        let _active_200 = begin_font_scan(200).expect("begin scan 200");
        // Stale lower-id cancel arrives while scan 200 is running uncancelled.
        cancel_font_scan(100);
        assert!(!font_scan_cancelled(200));
        assert!(!font_scan_cancelled(100));
    }
    assert_eq!(SCAN_STATE.load(Ordering::SeqCst), NO_SCAN_ID);
    // Starting scan 100 afterward must NOT see a poisoned stale cancel!
    let _active_100 = begin_font_scan(100).expect("scan 100 should start cleanly");
    assert!(!font_scan_cancelled(100));
}

#[test]
fn cancel_command_rejects_future_id_when_no_active_scan() {
    let _guard = lock_font_state_for_test();
    assert_eq!(SCAN_STATE.load(Ordering::SeqCst), NO_SCAN_ID);
    cancel_font_scan(u64::MAX);
    cancel_font_scan(999);
    assert_eq!(SCAN_STATE.load(Ordering::SeqCst), NO_SCAN_ID);
    assert!(begin_font_scan(CANCELLED_BIT).is_err());
}

#[test]
fn subset_font_enforces_ipc_bounds_provenance_magic_and_system_dir_gates() {
    let _guard = lock_font_state_for_test();
    let _db = init_test_user_font_db("subset-gates");
    let _cache_prov = CacheProvenanceSnapshotGuard::acquire();
    let dir = make_fonts_test_dir("subset-gates");

    let valid_path = dir.join("sample.ttf");
    fs::write(&valid_path, b"not-font-magic").unwrap();
    let valid_path_str =
        normalize_canonical_path(&valid_path.canonicalize().unwrap().to_string_lossy());

    // 1. Face-index cap
    let err = subset_font(
        valid_path_str.clone(),
        MAX_SUBSET_FONT_INDEX + 1,
        vec![0x41],
    )
    .unwrap_err();
    assert!(err.contains("Invalid font face index"), "got: {err}");

    // 2. Codepoint count cap
    let too_many = vec![0x41u32; MAX_SUBSET_CODEPOINTS + 1];
    let err = subset_font(valid_path_str.clone(), 0, too_many).unwrap_err();
    assert!(err.contains("Too many codepoints"), "got: {err}");

    // 3. Out-of-range Unicode scalar
    let err = subset_font(valid_path_str.clone(), 0, vec![0x11_0000]).unwrap_err();
    assert!(err.contains("Invalid codepoint"), "got: {err}");

    // 4. Disallowed extension
    let bad_ext = dir.join("sample.txt");
    fs::write(&bad_ext, b"text").unwrap();
    let err = subset_font(bad_ext.to_string_lossy().into_owned(), 0, vec![0x41]).unwrap_err();
    assert!(err.contains("Invalid font file type"), "got: {err}");

    // 5. Unregistered provenance rejection
    let err = subset_font(valid_path_str.clone(), 0, vec![0x41]).unwrap_err();
    assert!(
        err.contains("not discovered by a scan command"),
        "got: {err}"
    );

    // 6. Registered user font with non-OpenType header fails magic-byte sniff
    commit_entries(
        "source-magic",
        vec![sample_entry(&valid_path_str, "Magic Sans", 0)],
    );
    let err = subset_font(valid_path_str.clone(), 0, vec![0x41]).unwrap_err();
    assert!(err.contains("no recognized font signature"), "got: {err}");

    // 7. System-font provenance outside WINDOWS_SYSTEM_FONTS_DIR is rejected
    let sys_snapshot = ALLOWED_FONT_PATHS.lock().unwrap().clone();
    ALLOWED_FONT_PATHS
        .lock()
        .unwrap()
        .insert((valid_path_str.clone(), 0));
    let sys_err = subset_font(valid_path_str, 0, vec![0x41]).unwrap_err();
    *ALLOWED_FONT_PATHS.lock().unwrap() = sys_snapshot;
    assert!(
        sys_err.contains("not in a system fonts directory"),
        "got: {sys_err}"
    );
}
