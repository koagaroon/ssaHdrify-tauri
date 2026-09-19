use super::subset_with_index;
use fontcull_read_fonts::{FontRef, TableProvider};
use fontcull_skrifa::MetadataProvider;
use fontcull_write_fonts::{
    from_obj::ToOwnedTable,
    tables::{
        cmap::Cmap,
        gpos::{
            Gpos, PairPos, PairPosFormat1, PairSet, PairValueRecord, PositionLookup, ValueRecord,
        },
        gsub::{
            Gsub, Ligature, LigatureSet, LigatureSubstFormat1, SingleSubst, SingleSubstFormat2,
            SubstitutionLookup, SubstitutionSequenceContext,
        },
        head::Head,
        hhea::Hhea,
        layout::{
            CoverageFormat1, CoverageTable, Feature, FeatureList, FeatureRecord, LangSys, Lookup,
            LookupFlag, LookupList, Script, ScriptList, ScriptRecord, SequenceContext,
            SequenceContextFormat3, SequenceLookupRecord,
        },
    },
    types::{GlyphId, GlyphId16, Tag},
    FontBuilder,
};

const CODEPOINTS: &[u32] = &[0x66, 0x69, 0x3001, 0x41];
const GLYPH_COUNT: u16 = 11;

fn coverage(glyph: u16) -> CoverageTable {
    CoverageTable::Format1(CoverageFormat1::new(vec![GlyphId16::new(glyph)]))
}

fn single_substitution(input: u16, output: u16) -> SubstitutionLookup {
    SubstitutionLookup::Single(Lookup::new(
        LookupFlag::empty(),
        vec![SingleSubst::Format2(SingleSubstFormat2::new(
            coverage(input),
            vec![GlyphId16::new(output)],
        ))],
    ))
}

fn scripts(required_feature_index: u16, features: Vec<u16>) -> ScriptList {
    ScriptList::new(vec![ScriptRecord::new(
        Tag::new(b"DFLT"),
        Script::new(
            Some(LangSys {
                required_feature_index,
                feature_indices: features,
            }),
            Vec::new(),
        ),
    )])
}

fn substitution_table() -> Gsub {
    let features = [
        (b"calt", 4),
        (b"cust", 3),
        (b"liga", 0),
        (b"vert", 1),
        (b"vrt2", 2),
    ]
    .into_iter()
    .map(|(tag, lookup)| FeatureRecord::new(Tag::new(tag), Feature::new(None, vec![lookup])))
    .collect();
    let ligature = SubstitutionLookup::Ligature(Lookup::new(
        LookupFlag::empty(),
        vec![LigatureSubstFormat1::new(
            coverage(1),
            vec![LigatureSet::new(vec![Ligature::new(
                GlyphId16::new(3),
                vec![GlyphId16::new(2)],
            )])],
        )],
    ));
    let contextual = SubstitutionLookup::Contextual(Lookup::new(
        LookupFlag::empty(),
        vec![SubstitutionSequenceContext::from(SequenceContext::Format3(
            SequenceContextFormat3::new(
                vec![coverage(1), coverage(2)],
                vec![SequenceLookupRecord::new(0, 5)],
            ),
        ))],
    ));
    Gsub::new(
        // The custom required feature deliberately lies outside the library defaults.
        scripts(1, vec![0, 2, 3, 4]),
        FeatureList::new(features),
        LookupList::new(vec![
            ligature,
            single_substitution(4, 5),
            single_substitution(5, 6),
            single_substitution(7, 8),
            contextual,
            single_substitution(1, 9),
        ]),
    )
}

fn positioning_table() -> Gpos {
    Gpos::new(
        scripts(0xffff, vec![0]),
        FeatureList::new(vec![FeatureRecord::new(
            Tag::new(b"kern"),
            Feature::new(None, vec![0]),
        )]),
        LookupList::new(vec![PositionLookup::Pair(Lookup::new(
            LookupFlag::empty(),
            vec![PairPos::Format1(PairPosFormat1::new(
                coverage(1),
                vec![PairSet::new(vec![PairValueRecord::new(
                    GlyphId16::new(2),
                    ValueRecord::new().with_x_advance(-75),
                    ValueRecord::new(),
                )])],
            ))],
        ))]),
    )
}

// Generated original fixture: four encoded glyphs, five layout-only glyphs,
// one unused glyph and .notdef. Every glyph has a distinct triangle and advance.
fn synthetic_font(with_layout: bool) -> Vec<u8> {
    let mut builder = FontBuilder::new();
    builder
        .add_table(&Head {
            units_per_em: 1000,
            index_to_loc_format: 1,
            ..Default::default()
        })
        .unwrap();
    builder
        .add_table(&Hhea {
            number_of_h_metrics: GLYPH_COUNT,
            ..Default::default()
        })
        .unwrap();
    builder
        .add_table(
            &Cmap::from_mappings([
                ('f', GlyphId::new(1)),
                ('i', GlyphId::new(2)),
                ('\u{3001}', GlyphId::new(4)),
                ('A', GlyphId::new(7)),
            ])
            .unwrap(),
        )
        .unwrap();

    let mut maxp = 0x0001_0000u32.to_be_bytes().to_vec();
    for value in [GLYPH_COUNT, 3, 1, 0, 0, 1, 0, 0, 0, 0, 0, 0, 0, 0] {
        maxp.extend(value.to_be_bytes());
    }
    builder.add_raw(Tag::new(b"maxp"), maxp);
    let mut glyf = Vec::new();
    let mut loca = Vec::new();
    let mut hmtx = Vec::new();
    for glyph in 0..GLYPH_COUNT {
        loca.extend((glyf.len() as u32).to_be_bytes());
        let width = 10 + glyph as i16;
        for value in [1i16, 0, 0, width, 20] {
            glyf.extend(value.to_be_bytes());
        }
        glyf.extend(2u16.to_be_bytes()); // Last point in the triangle.
        glyf.extend(0u16.to_be_bytes()); // No instructions.
        glyf.extend([1, 1, 1]); // On-curve points; signed 16-bit coordinate deltas.
        for delta in [0i16, width, -width, 0, 0, 20] {
            glyf.extend(delta.to_be_bytes());
        }
        glyf.push(0); // Pad to an even glyph length.
        hmtx.extend((500 + glyph * 10).to_be_bytes());
        hmtx.extend(0i16.to_be_bytes());
    }
    loca.extend((glyf.len() as u32).to_be_bytes());
    builder.add_raw(Tag::new(b"glyf"), glyf);
    builder.add_raw(Tag::new(b"loca"), loca);
    builder.add_raw(Tag::new(b"hmtx"), hmtx);
    if with_layout {
        builder.add_table(&substitution_table()).unwrap();
        builder.add_table(&positioning_table()).unwrap();
    }
    builder.build()
}

fn feature_lookup<'a>(gsub: &'a Gsub, tag: &[u8; 4]) -> &'a SubstitutionLookup {
    let feature = gsub
        .feature_list
        .feature_records
        .iter()
        .find(|record| record.feature_tag == Tag::new(tag))
        .expect("requested layout feature must survive");
    &gsub.lookup_list.lookups[feature.feature.lookup_list_indices[0] as usize]
}

fn substitute_single(lookup: &SubstitutionLookup, glyph: u16) -> u16 {
    let SubstitutionLookup::Single(lookup) = lookup else {
        panic!("expected a single substitution");
    };
    match &*lookup.subtables[0] {
        SingleSubst::Format1(table) => {
            assert!(table
                .coverage
                .iter()
                .any(|covered| covered.to_u16() == glyph));
            glyph.wrapping_add_signed(table.delta_glyph_id)
        }
        SingleSubst::Format2(table) => {
            let index = table
                .coverage
                .iter()
                .position(|covered| covered.to_u16() == glyph)
                .expect("input glyph must remain covered");
            table.substitute_glyph_ids[index].to_u16()
        }
    }
}

#[test]
fn subset_preserves_required_ligature_contextual_and_vertical_glyph_closure() {
    let bytes = subset_with_index(&synthetic_font(true), 0, CODEPOINTS).unwrap();
    let font = FontRef::new(&bytes).unwrap();
    let gsub: Gsub = font.gsub().unwrap().to_owned_table();
    assert_eq!(font.maxp().unwrap().num_glyphs(), 10);
    for glyph in 0..10 {
        assert_eq!(
            font.hmtx().unwrap().advance(GlyphId::new(glyph)),
            Some(500 + glyph as u16 * 10)
        );
    }

    let language = gsub.script_list.script_records[0]
        .script
        .default_lang_sys
        .as_ref()
        .unwrap();
    let required = &gsub.feature_list.feature_records[language.required_feature_index as usize];
    assert_eq!(required.feature_tag, Tag::new(b"cust"));
    assert_eq!(substitute_single(feature_lookup(&gsub, b"cust"), 7), 8);

    let SubstitutionLookup::Ligature(lookup) = feature_lookup(&gsub, b"liga") else {
        panic!("ligature lookup must survive");
    };
    let table = &lookup.subtables[0];
    assert_eq!(
        table.coverage.iter().collect::<Vec<_>>(),
        vec![GlyphId16::new(1)]
    );
    let ligature = &table.ligature_sets[0].ligatures[0];
    assert_eq!(ligature.component_glyph_ids, vec![GlyphId16::new(2)]);
    assert_eq!(ligature.ligature_glyph, GlyphId16::new(3));

    assert_eq!(substitute_single(feature_lookup(&gsub, b"vert"), 4), 5);
    assert_eq!(substitute_single(feature_lookup(&gsub, b"vrt2"), 5), 6);

    let SubstitutionLookup::Contextual(lookup) = feature_lookup(&gsub, b"calt") else {
        panic!("contextual lookup must survive");
    };
    let SequenceContext::Format3(context) = lookup.subtables[0].as_inner() else {
        panic!("the two-glyph context must survive");
    };
    assert_eq!(
        context.coverages[0].iter().collect::<Vec<_>>(),
        vec![GlyphId16::new(1)]
    );
    assert_eq!(
        context.coverages[1].iter().collect::<Vec<_>>(),
        vec![GlyphId16::new(2)]
    );
    let record = &context.seq_lookup_records[0];
    assert_eq!(record.sequence_index, 0);
    assert_eq!(
        substitute_single(
            &gsub.lookup_list.lookups[record.lookup_list_index as usize],
            1
        ),
        9
    );
}

#[test]
fn subset_preserves_gpos_pair_adjustment_on_the_normal_attempt() {
    let bytes = subset_with_index(&synthetic_font(true), 0, CODEPOINTS).unwrap();
    let font = FontRef::new(&bytes).unwrap();
    let gpos: Gpos = font
        .gpos()
        .expect("valid GPOS must not be dropped")
        .to_owned_table();
    assert_eq!(
        gpos.feature_list.feature_records[0].feature_tag,
        Tag::new(b"kern")
    );
    let PositionLookup::Pair(lookup) = &*gpos.lookup_list.lookups[0] else {
        panic!("pair positioning must survive");
    };
    let PairPos::Format1(table) = &*lookup.subtables[0] else {
        panic!("pair positioning must remain readable");
    };
    assert_eq!(
        table.coverage.iter().collect::<Vec<_>>(),
        vec![GlyphId16::new(1)]
    );
    let pair = &table.pair_sets[0].pair_value_records[0];
    assert_eq!(pair.second_glyph, GlyphId16::new(2));
    assert_eq!(pair.value_record1.x_advance, Some(-75));
}

#[test]
fn font_without_layout_subsets_to_only_requested_glyphs() {
    let bytes = subset_with_index(&synthetic_font(false), 0, CODEPOINTS).unwrap();
    let font = FontRef::new(&bytes).unwrap();
    assert_eq!(font.maxp().unwrap().num_glyphs(), 5);
    assert!(font.gsub().is_err());
    assert!(font.gpos().is_err());
    for (character, expected_advance) in [('f', 510), ('i', 520), ('\u{3001}', 540), ('A', 570)] {
        let glyph = font
            .charmap()
            .map(character)
            .expect("requested character must survive");
        assert_eq!(font.hmtx().unwrap().advance(glyph), Some(expected_advance));
    }
}
