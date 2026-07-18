mod support;

use std::sync::atomic::{AtomicUsize, Ordering};

use pdf_rs_font::{
    FontCancellation, FontErrorCode, FontLimitConfig, FontLimitKind, FontLimits, FontParseOutcome,
    FontProfile, FontUnsupportedKind, GlyphId, NeverCancelled, OutlineSegment, TrueTypeFont,
    parse_truetype,
};

use support::{
    build_font, byte_compound_glyph, compound_glyph, contour_glyph, foundational_font, glyph_range,
    quadratic_glyph, set_i16, set_u16, set_u32, table_range, triangle_glyph,
};

fn parse(bytes: &[u8], limits: FontLimits) -> pdf_rs_font::FontParseReport {
    parse_truetype(
        bytes,
        FontProfile::SimpleTrueTypeWinAnsiAsciiV1,
        limits,
        &NeverCancelled,
    )
}

fn ready(bytes: &[u8], limits: FontLimits) -> TrueTypeFont {
    match parse(bytes, limits).into_outcome() {
        FontParseOutcome::Ready(font) => font,
        outcome => panic!("expected ready font, got {outcome:?}"),
    }
}

fn ready_with_profile(bytes: &[u8], profile: FontProfile, limits: FontLimits) -> TrueTypeFont {
    match parse_truetype(bytes, profile, limits, &NeverCancelled).into_outcome() {
        FontParseOutcome::Ready(font) => font,
        outcome => panic!("expected ready font, got {outcome:?}"),
    }
}

fn limit_kind(bytes: &[u8], config: FontLimitConfig) -> FontLimitKind {
    let limits = FontLimits::validate(config).expect("test limits are valid");
    match parse(bytes, limits).into_outcome() {
        FontParseOutcome::Failed(error) => {
            assert_eq!(error.code(), FontErrorCode::ResourceLimit);
            error.limit().expect("resource error carries limit").kind()
        }
        outcome => panic!("expected resource limit, got {outcome:?}"),
    }
}

#[test]
fn registered_profile_publishes_ascii_metrics_and_exact_outlines() {
    let bytes = foundational_font();
    let font = ready(&bytes, FontLimits::default());
    assert_eq!(
        font.profile().identifier(),
        "m3.simple-truetype-winansi-ascii.v1"
    );
    assert_eq!(font.units_per_em(), 1_000);
    assert_eq!(font.glyph_count(), 4);
    assert_eq!(font.glyph_id_for_winansi(b'A'), Some(GlyphId::new(1)));
    assert_eq!(font.glyph_id_for_winansi(0x1f), None);
    assert_eq!(font.advance_width(GlyphId::new(2)), Some(502));
    assert_eq!(font.advance_width(GlyphId::new(9)), None);

    let simple = font.glyph_outline(GlyphId::new(1)).unwrap();
    assert_eq!(simple.advance_width(), 501);
    assert_eq!(simple.bounds().unwrap().x_max(), 100);
    assert_eq!(simple.segments().len(), 4);
    assert_eq!(
        simple.segments()[0],
        OutlineSegment::MoveTo(pdf_rs_font::FontPoint::new(
            pdf_rs_font::FontCoordinate::from_half_units(0),
            pdf_rs_font::FontCoordinate::from_half_units(0),
        ))
    );

    let compound = font.glyph_outline(GlyphId::new(3)).unwrap();
    assert_eq!(compound.segments().len(), 8);
    match compound.segments()[0] {
        OutlineSegment::MoveTo(point) => {
            assert_eq!(point.x().half_units(), 20);
            assert_eq!(point.y().half_units(), 40);
        }
        segment => panic!("expected first translated move, got {segment:?}"),
    }
    match compound.segments()[4] {
        OutlineSegment::MoveTo(point) => {
            assert_eq!(point.x().half_units(), -20);
            assert_eq!(point.y().half_units(), -40);
        }
        segment => panic!("expected second translated move, got {segment:?}"),
    }
    assert_eq!(font.stats().source_points(), 6);
    assert_eq!(font.stats().components(), 2);
    assert_eq!(font.stats().path_segments(), 16);
    assert!(font.stats().retained_bytes() > 0);
    assert!(font.stats().peak_retained_bytes() >= font.stats().retained_bytes());
}

#[test]
fn complete_winansi_profile_admits_extended_codes_without_changing_legacy_ascii() {
    let bytes = foundational_font();
    let full = ready_with_profile(
        &bytes,
        FontProfile::SimpleTrueTypeWinAnsiV1,
        FontLimits::default(),
    );
    assert_eq!(full.profile().identifier(), "m4.simple-truetype-winansi.v1");
    assert_eq!(full.glyph_id_for_winansi(0x80), Some(GlyphId::new(0)));
    assert_eq!(full.glyph_id_for_winansi(0xff), Some(GlyphId::new(0)));
    assert_eq!(full.glyph_id_for_winansi(0x1f), None);

    let legacy = ready(&bytes, FontLimits::default());
    assert_eq!(legacy.glyph_id_for_winansi(0x80), None);
}

#[test]
fn identity_cid_profile_does_not_require_an_unused_cmap_table() {
    let mut bytes = foundational_font();
    let table_count = usize::from(u16::from_be_bytes([bytes[4], bytes[5]]));
    let cmap_record = (0..table_count)
        .map(|index| 12 + index * 16)
        .find(|record| bytes[*record..*record + 4] == *b"cmap")
        .expect("fixture contains cmap");
    bytes[cmap_record..cmap_record + 4].copy_from_slice(b"name");

    let cid = ready_with_profile(
        &bytes,
        FontProfile::CidFontType2IdentityV1,
        FontLimits::default(),
    );
    assert_eq!(cid.glyph_count(), 4);
    assert_eq!(cid.stats().cmap_segments(), 0);
    assert!(cid.glyph_outline(GlyphId::new(1)).is_some());

    match parse_truetype(
        &bytes,
        FontProfile::SimpleTrueTypeWinAnsiV1,
        FontLimits::default(),
        &NeverCancelled,
    )
    .into_outcome()
    {
        FontParseOutcome::Failed(error) => {
            assert_eq!(error.code(), FontErrorCode::MissingRequiredTable)
        }
        outcome => panic!("simple WinAnsi profile must still require cmap, got {outcome:?}"),
    }
}

#[test]
fn default_profile_admits_large_bounded_cid_font_subsets() {
    const GLYPH_COUNT: usize = 50_377;
    let bytes = build_font((0..GLYPH_COUNT).map(|_| Vec::new()).collect());
    let font = ready_with_profile(
        &bytes,
        FontProfile::CidFontType2IdentityV1,
        FontLimits::default(),
    );

    assert_eq!(font.glyph_count(), GLYPH_COUNT as u16);
    assert_eq!(font.stats().glyphs(), GLYPH_COUNT as u64);
    assert_eq!(font.stats().path_segments(), 0);
    assert!(font.stats().retained_bytes() < FontLimits::default().max_retained_bytes());
}

#[test]
fn compound_use_my_metrics_is_last_wins_and_inherits_nested_metrics() {
    let mut selected_first = compound_glyph(&[(1, 0, 0), (2, 0, 0)]);
    set_u16(&mut selected_first, 10, 0x0001 | 0x0002 | 0x0020 | 0x0200);
    let bytes = build_font(vec![
        Vec::new(),
        triangle_glyph(),
        triangle_glyph(),
        selected_first,
    ]);
    let font = ready(&bytes, FontLimits::default());
    assert_eq!(font.advance_width(GlyphId::new(3)), Some(501));

    let mut selected_second = compound_glyph(&[(1, 0, 0), (2, 0, 0)]);
    set_u16(&mut selected_second, 18, 0x0001 | 0x0002 | 0x0200);
    let bytes = build_font(vec![
        Vec::new(),
        triangle_glyph(),
        triangle_glyph(),
        selected_second,
    ]);
    let font = ready(&bytes, FontLimits::default());
    assert_eq!(font.advance_width(GlyphId::new(3)), Some(502));

    let mut inner = compound_glyph(&[(1, 0, 0)]);
    set_u16(&mut inner, 10, 0x0001 | 0x0002 | 0x0200);
    let mut outer = compound_glyph(&[(2, 0, 0)]);
    set_u16(&mut outer, 10, 0x0001 | 0x0002 | 0x0200);
    let bytes = build_font(vec![Vec::new(), triangle_glyph(), inner, outer]);
    let font = ready(&bytes, FontLimits::default());
    assert_eq!(font.advance_width(GlyphId::new(2)), Some(501));
    assert_eq!(font.advance_width(GlyphId::new(3)), Some(501));

    let mut repeated = compound_glyph(&[(1, 0, 0), (2, 0, 0)]);
    set_u16(&mut repeated, 10, 0x0001 | 0x0002 | 0x0020 | 0x0200);
    set_u16(&mut repeated, 18, 0x0001 | 0x0002 | 0x0200);
    let bytes = build_font(vec![
        Vec::new(),
        triangle_glyph(),
        triangle_glyph(),
        repeated,
    ]);
    let font = ready(&bytes, FontLimits::default());
    assert_eq!(font.advance_width(GlyphId::new(3)), Some(502));

    let mut nested_inner = compound_glyph(&[(1, 0, 0)]);
    set_u16(&mut nested_inner, 10, 0x0001 | 0x0002 | 0x0200);
    let mut nested_last = compound_glyph(&[(2, 0, 0), (3, 0, 0)]);
    set_u16(&mut nested_last, 10, 0x0001 | 0x0002 | 0x0020 | 0x0200);
    set_u16(&mut nested_last, 18, 0x0001 | 0x0002 | 0x0200);
    let bytes = build_font(vec![
        Vec::new(),
        triangle_glyph(),
        triangle_glyph(),
        nested_inner,
        nested_last,
    ]);
    let font = ready(&bytes, FontLimits::default());
    assert_eq!(font.advance_width(GlyphId::new(3)), Some(501));
    assert_eq!(font.advance_width(GlyphId::new(4)), Some(501));
}

#[test]
fn cmap_format4_range_offsets_stay_inside_the_glyph_id_array() {
    let exact = foundational_font();
    let font = ready(&exact, FontLimits::default());
    assert_eq!(font.glyph_id_for_winansi(b' '), Some(GlyphId::new(1)));
    assert_eq!(font.glyph_id_for_winansi(b'~'), Some(GlyphId::new(1)));

    let mut before_lower_bound = foundational_font();
    let cmap = table_range(&before_lower_bound, b"cmap");
    let format = cmap.start + 12;
    set_u16(&mut before_lower_bound, format + 28, 2);
    match parse(&before_lower_bound, FontLimits::default()).into_outcome() {
        FontParseOutcome::Failed(error) => assert_eq!(error.code(), FontErrorCode::InvalidCmap),
        outcome => panic!("expected glyphIdArray lower-bound failure, got {outcome:?}"),
    }

    let mut beyond_upper_bound = foundational_font();
    let cmap = table_range(&beyond_upper_bound, b"cmap");
    let format = cmap.start + 12;
    set_u16(&mut beyond_upper_bound, format + 14, 0x007f);
    match parse(&beyond_upper_bound, FontLimits::default()).into_outcome() {
        FontParseOutcome::Failed(error) => assert_eq!(error.code(), FontErrorCode::InvalidCmap),
        outcome => panic!("expected glyphIdArray upper-bound failure, got {outcome:?}"),
    }

    let mut zero_glyph_with_delta = foundational_font();
    let cmap = table_range(&zero_glyph_with_delta, b"cmap");
    let format = cmap.start + 12;
    set_i16(&mut zero_glyph_with_delta, format + 24, 1);
    let a_entry = format + 32 + usize::from(b'A' - b' ') * 2;
    set_u16(&mut zero_glyph_with_delta, a_entry, 0);
    let font = ready(&zero_glyph_with_delta, FontLimits::default());
    assert_eq!(font.glyph_id_for_winansi(b'A'), Some(GlyphId::new(0)));
    assert_eq!(font.glyph_id_for_winansi(b'B'), Some(GlyphId::new(2)));
}

#[test]
fn simple_quadratic_outlines_preserve_exact_implied_half_unit_midpoints() {
    let bytes = build_font(vec![Vec::new(), quadratic_glyph()]);
    let font = ready(&bytes, FontLimits::default());
    let outline = font.glyph_outline(GlyphId::new(1)).unwrap();
    assert_eq!(outline.segments().len(), 4);
    match outline.segments()[1] {
        OutlineSegment::QuadTo { control, end } => {
            assert_eq!(control.x().half_units(), 0);
            assert_eq!(control.y().half_units(), 0);
            assert_eq!(end.x().half_units(), 100);
            assert_eq!(end.y().half_units(), 0);
        }
        segment => panic!("expected implied-midpoint quadratic, got {segment:?}"),
    }
    match outline.segments()[2] {
        OutlineSegment::QuadTo { control, end } => {
            assert_eq!(control.x().half_units(), 200);
            assert_eq!(control.y().half_units(), 0);
            assert_eq!(end.x().half_units(), 200);
            assert_eq!(end.y().half_units(), 200);
        }
        segment => panic!("expected closing quadratic, got {segment:?}"),
    }
}

#[test]
fn every_small_on_off_curve_pattern_measurement_matches_atomic_build() {
    let mut glyphs = vec![Vec::new()];
    for point_count in 1..=6 {
        for pattern in 0_u64..(1_u64 << point_count) {
            let flags = (0..point_count)
                .map(|point| pattern & (1_u64 << point) != 0)
                .collect::<Vec<_>>();
            glyphs.push(contour_glyph(&flags));
        }
    }
    let expected_glyphs = glyphs.len() as u16;
    let bytes = build_font(glyphs);
    let font = ready(&bytes, FontLimits::default());
    assert_eq!(font.glyph_count(), expected_glyphs);
    for glyph_id in 1..expected_glyphs {
        let outline = font.glyph_outline(GlyphId::new(glyph_id)).unwrap();
        assert!(matches!(
            outline.segments().first(),
            Some(OutlineSegment::MoveTo(_))
        ));
        assert_eq!(
            outline.segments().last(),
            Some(&OutlineSegment::CloseContour)
        );
    }
}

#[test]
fn short_loca_reused_hmetric_and_byte_component_offsets_are_supported() {
    let mut triangle = triangle_glyph();
    if !triangle.len().is_multiple_of(2) {
        triangle.push(0);
    }
    let mut bytes = build_font(vec![Vec::new(), triangle, byte_compound_glyph(1, -7, 9)]);
    let head = table_range(&bytes, b"head");
    set_i16(&mut bytes, head.start + 50, 0);
    let loca = table_range(&bytes, b"loca");
    let long_offsets = (0..=3)
        .map(|index| {
            u32::from_be_bytes(
                bytes[loca.start + index * 4..loca.start + index * 4 + 4]
                    .try_into()
                    .unwrap(),
            )
        })
        .collect::<Vec<_>>();
    for (index, offset) in long_offsets.into_iter().enumerate() {
        assert!(offset.is_multiple_of(2));
        set_u16(&mut bytes, loca.start + index * 2, (offset / 2) as u16);
    }
    let hhea = table_range(&bytes, b"hhea");
    set_u16(&mut bytes, hhea.start + 34, 1);

    let font = ready(&bytes, FontLimits::default());
    assert_eq!(font.advance_width(GlyphId::new(2)), Some(500));
    let compound = font.glyph_outline(GlyphId::new(2)).unwrap();
    match compound.segments()[0] {
        OutlineSegment::MoveTo(point) => {
            assert_eq!(point.x().half_units(), -14);
            assert_eq!(point.y().half_units(), 18);
        }
        segment => panic!("expected byte-offset component move, got {segment:?}"),
    }
}

#[test]
fn compound_bounds_conservatively_contain_nested_negative_outlines() {
    let inner = compound_glyph(&[(1, -10, -20)]);
    let mut outer = compound_glyph(&[(2, -5, -7)]);
    set_i16(&mut outer, 2, -15);
    set_i16(&mut outer, 4, -27);
    set_i16(&mut outer, 6, 85);
    set_i16(&mut outer, 8, 73);
    let bytes = build_font(vec![
        Vec::new(),
        triangle_glyph(),
        inner.clone(),
        outer.clone(),
    ]);
    let font = ready(&bytes, FontLimits::default());
    let outline = font.glyph_outline(GlyphId::new(3)).unwrap();
    assert_eq!(outline.bounds().unwrap().x_min(), -15);
    match outline.segments()[0] {
        OutlineSegment::MoveTo(point) => {
            assert_eq!(point.x().half_units(), -30);
            assert_eq!(point.y().half_units(), -54);
        }
        segment => panic!("expected nested negative move, got {segment:?}"),
    }

    let mut conservative = outer.clone();
    set_i16(&mut conservative, 2, -16);
    set_i16(&mut conservative, 4, -28);
    set_i16(&mut conservative, 6, 86);
    set_i16(&mut conservative, 8, 74);
    let bytes = build_font(vec![
        Vec::new(),
        triangle_glyph(),
        inner.clone(),
        conservative,
    ]);
    let _ = ready(&bytes, FontLimits::default());

    for malformed in [
        {
            let mut glyph = outer.clone();
            set_i16(&mut glyph, 6, 84);
            glyph
        },
        {
            let mut glyph = outer.clone();
            set_i16(&mut glyph, 2, -14);
            set_i16(&mut glyph, 4, -26);
            set_i16(&mut glyph, 6, 86);
            set_i16(&mut glyph, 8, 74);
            glyph
        },
        {
            let mut glyph = outer.clone();
            for offset in [2, 4, 6, 8] {
                set_i16(&mut glyph, offset, 0);
            }
            glyph
        },
    ] {
        let bytes = build_font(vec![Vec::new(), triangle_glyph(), inner.clone(), malformed]);
        match parse(&bytes, FontLimits::default()).into_outcome() {
            FontParseOutcome::Failed(error) => {
                assert_eq!(error.code(), FontErrorCode::InvalidGlyph);
                assert_eq!(error.glyph_id(), Some(3));
            }
            outcome => panic!("expected compound bounds failure, got {outcome:?}"),
        }
    }
}

#[test]
fn empty_and_spec_classified_glyph_descriptions_remain_valid() {
    let zero_contour_header = vec![0_u8; 10];
    let bytes = build_font(vec![Vec::new(), zero_contour_header]);
    let font = ready(&bytes, FontLimits::default());
    assert!(
        font.glyph_outline(GlyphId::new(1))
            .unwrap()
            .segments()
            .is_empty()
    );

    let mut empty_compound = compound_glyph(&[(0, 0, 0)]);
    set_i16(&mut empty_compound, 0, -2);
    set_i16(&mut empty_compound, 6, 0);
    set_i16(&mut empty_compound, 8, 0);
    let bytes = build_font(vec![Vec::new(), empty_compound]);
    let font = ready(&bytes, FontLimits::default());
    assert!(
        font.glyph_outline(GlyphId::new(1))
            .unwrap()
            .segments()
            .is_empty()
    );

    let mut overlap = triangle_glyph();
    overlap[14] |= 0x40;
    let bytes = build_font(vec![Vec::new(), overlap]);
    let font = ready(&bytes, FontLimits::default());
    assert_eq!(
        font.glyph_outline(GlyphId::new(1))
            .unwrap()
            .segments()
            .len(),
        4
    );
}

#[test]
fn unsupported_capabilities_are_typed_and_not_malformed() {
    let mut sfnt = foundational_font();
    set_u32(&mut sfnt, 0, u32::from_be_bytes(*b"OTTO"));
    match parse(&sfnt, FontLimits::default()).into_outcome() {
        FontParseOutcome::Unsupported(value) => {
            assert_eq!(value.kind(), FontUnsupportedKind::SfntFlavor)
        }
        outcome => panic!("expected sfnt flavor outcome, got {outcome:?}"),
    }

    let mut cmap = foundational_font();
    let cmap_range = table_range(&cmap, b"cmap");
    set_u16(&mut cmap, cmap_range.start + 12, 6);
    match parse(&cmap, FontLimits::default()).into_outcome() {
        FontParseOutcome::Unsupported(value) => {
            assert_eq!(value.kind(), FontUnsupportedKind::CmapFormat)
        }
        outcome => panic!("expected cmap format outcome, got {outcome:?}"),
    }

    let mut transform = foundational_font();
    let compound = glyph_range(&transform, 3);
    set_u16(
        &mut transform,
        compound.start + 10,
        0x0001 | 0x0002 | 0x0008,
    );
    match parse(&transform, FontLimits::default()).into_outcome() {
        FontParseOutcome::Unsupported(value) => {
            assert_eq!(value.kind(), FontUnsupportedKind::CompoundTransform);
            assert_eq!(value.glyph_id(), Some(3));
        }
        outcome => panic!("expected compound transform outcome, got {outcome:?}"),
    }
}

#[test]
fn compound_transform_records_are_structurally_validated_before_capability_policy() {
    for (transform_flag, transform_bytes) in [(0x0008, 2), (0x0040, 4), (0x0080, 8)] {
        let mut truncated = compound_glyph(&[(1, 0, 0)]);
        set_u16(&mut truncated, 10, 0x0001 | 0x0002 | transform_flag);
        truncated.extend(std::iter::repeat_n(0, transform_bytes - 1));
        let bytes = build_font(vec![Vec::new(), triangle_glyph(), truncated]);
        match parse(&bytes, FontLimits::default()).into_outcome() {
            FontParseOutcome::Failed(error) => {
                assert_eq!(error.code(), FontErrorCode::InvalidGlyph);
                assert_eq!(error.glyph_id(), Some(2));
            }
            outcome => panic!("expected truncated transform failure, got {outcome:?}"),
        }

        let mut complete = compound_glyph(&[(1, 0, 0)]);
        set_u16(&mut complete, 10, 0x0001 | 0x0002 | transform_flag);
        complete.extend(std::iter::repeat_n(0, transform_bytes));
        let bytes = build_font(vec![Vec::new(), triangle_glyph(), complete]);
        match parse(&bytes, FontLimits::default()).into_outcome() {
            FontParseOutcome::Unsupported(value) => {
                assert_eq!(value.kind(), FontUnsupportedKind::CompoundTransform);
                assert_eq!(value.glyph_id(), Some(2));
            }
            outcome => panic!("expected complete transform capability outcome, got {outcome:?}"),
        }
    }

    for flags in [0x0001 | 0x0002 | 0x0008 | 0x0040, 0x0001 | 0x0002 | 0x2000] {
        let mut malformed = compound_glyph(&[(1, 0, 0)]);
        set_u16(&mut malformed, 10, flags);
        let bytes = build_font(vec![Vec::new(), triangle_glyph(), malformed]);
        match parse(&bytes, FontLimits::default()).into_outcome() {
            FontParseOutcome::Failed(error) => {
                assert_eq!(error.code(), FontErrorCode::InvalidGlyph)
            }
            outcome => panic!("expected malformed compound flags, got {outcome:?}"),
        }
    }
}

#[test]
fn compound_capability_outcome_requires_a_fully_valid_record_list_and_tail() {
    let mut truncated_after_point_attachment = compound_glyph(&[(1, 0, 0), (1, 0, 0)]);
    set_u16(&mut truncated_after_point_attachment, 10, 0x0001 | 0x0020);
    truncated_after_point_attachment.truncate(20);
    let bytes = build_font(vec![
        Vec::new(),
        triangle_glyph(),
        truncated_after_point_attachment,
    ]);
    match parse(&bytes, FontLimits::default()).into_outcome() {
        FontParseOutcome::Failed(error) => {
            assert_eq!(error.code(), FontErrorCode::InvalidGlyph);
            assert_eq!(error.glyph_id(), Some(2));
        }
        outcome => panic!("expected truncated later component, got {outcome:?}"),
    }

    let mut malformed_after_point_attachment = compound_glyph(&[(1, 0, 0), (1, 0, 0)]);
    set_u16(&mut malformed_after_point_attachment, 10, 0x0001 | 0x0020);
    set_u16(
        &mut malformed_after_point_attachment,
        18,
        0x0001 | 0x0002 | 0x2000,
    );
    let bytes = build_font(vec![
        Vec::new(),
        triangle_glyph(),
        malformed_after_point_attachment,
    ]);
    match parse(&bytes, FontLimits::default()).into_outcome() {
        FontParseOutcome::Failed(error) => assert_eq!(error.code(), FontErrorCode::InvalidGlyph),
        outcome => panic!("expected malformed later component, got {outcome:?}"),
    }

    let mut truncated_tail_after_middle_transform =
        compound_glyph(&[(1, 0, 0), (1, 0, 0), (1, 0, 0)]);
    set_u16(
        &mut truncated_tail_after_middle_transform,
        18,
        0x0001 | 0x0002 | 0x0008 | 0x0020 | 0x0100,
    );
    truncated_tail_after_middle_transform.splice(26..26, [0, 0]);
    truncated_tail_after_middle_transform.extend_from_slice(&2_u16.to_be_bytes());
    truncated_tail_after_middle_transform.push(0x2f);
    let bytes = build_font(vec![
        Vec::new(),
        triangle_glyph(),
        truncated_tail_after_middle_transform,
    ]);
    match parse(&bytes, FontLimits::default()).into_outcome() {
        FontParseOutcome::Failed(error) => {
            assert_eq!(error.code(), FontErrorCode::InvalidGlyph);
            assert_eq!(error.glyph_id(), Some(2));
        }
        outcome => panic!("expected truncated instruction tail, got {outcome:?}"),
    }

    let mut complete_point_attachment = compound_glyph(&[(1, 0, 0), (1, 0, 0)]);
    set_u16(&mut complete_point_attachment, 10, 0x0001 | 0x0020);
    let bytes = build_font(vec![
        Vec::new(),
        triangle_glyph(),
        complete_point_attachment,
    ]);
    match parse(&bytes, FontLimits::default()).into_outcome() {
        FontParseOutcome::Unsupported(value) => {
            assert_eq!(value.kind(), FontUnsupportedKind::CompoundPointAttachment);
            assert_eq!(value.glyph_id(), Some(2));
        }
        outcome => panic!("expected structurally complete point attachment, got {outcome:?}"),
    }

    let mut complete_middle_transform = compound_glyph(&[(1, 0, 0), (1, 0, 0), (1, 0, 0)]);
    set_u16(
        &mut complete_middle_transform,
        18,
        0x0001 | 0x0002 | 0x0008 | 0x0020,
    );
    complete_middle_transform.splice(26..26, [0, 0]);
    let bytes = build_font(vec![
        Vec::new(),
        triangle_glyph(),
        complete_middle_transform,
    ]);
    match parse(&bytes, FontLimits::default()).into_outcome() {
        FontParseOutcome::Unsupported(value) => {
            assert_eq!(value.kind(), FontUnsupportedKind::CompoundTransform);
            assert_eq!(value.glyph_id(), Some(2));
        }
        outcome => panic!("expected structurally complete middle transform, got {outcome:?}"),
    }

    let mut first_kind_wins = compound_glyph(&[(1, 0, 0), (1, 0, 0), (1, 0, 0)]);
    set_u16(&mut first_kind_wins, 10, 0x0001 | 0x0020);
    set_u16(&mut first_kind_wins, 18, 0x0001 | 0x0002 | 0x0008 | 0x0020);
    first_kind_wins.splice(26..26, [0, 0]);
    let bytes = build_font(vec![Vec::new(), triangle_glyph(), first_kind_wins]);
    match parse(&bytes, FontLimits::default()).into_outcome() {
        FontParseOutcome::Unsupported(value) => {
            assert_eq!(value.kind(), FontUnsupportedKind::CompoundPointAttachment)
        }
        outcome => panic!("expected first deterministic capability, got {outcome:?}"),
    }
}

#[test]
fn invalid_index_to_loca_format_is_malformed_head_data() {
    let mut bytes = foundational_font();
    let head = table_range(&bytes, b"head");
    set_i16(&mut bytes, head.start + 50, 2);
    match parse(&bytes, FontLimits::default()).into_outcome() {
        FontParseOutcome::Failed(error) => assert_eq!(error.code(), FontErrorCode::InvalidHead),
        outcome => panic!("expected invalid head outcome, got {outcome:?}"),
    }
}

#[test]
fn malformed_tables_loca_and_glyphs_fail_without_publication() {
    let truncated = &foundational_font()[..10];
    match parse(truncated, FontLimits::default()).into_outcome() {
        FontParseOutcome::Failed(error) => assert_eq!(error.code(), FontErrorCode::InvalidRequest),
        outcome => panic!("expected invalid request, got {outcome:?}"),
    }

    let mut loca = foundational_font();
    let loca_range = table_range(&loca, b"loca");
    set_u32(&mut loca, loca_range.start + 4, u32::MAX);
    match parse(&loca, FontLimits::default()).into_outcome() {
        FontParseOutcome::Failed(error) => assert_eq!(error.code(), FontErrorCode::InvalidLoca),
        outcome => panic!("expected invalid loca, got {outcome:?}"),
    }

    let mut glyph = foundational_font();
    let simple = glyph_range(&glyph, 1);
    set_u16(&mut glyph, simple.start + 10, 100);
    match parse(&glyph, FontLimits::default()).into_outcome() {
        FontParseOutcome::Failed(error) => {
            assert_eq!(error.code(), FontErrorCode::InvalidGlyph);
            assert_eq!(error.glyph_id(), Some(1));
        }
        outcome => panic!("expected invalid glyph, got {outcome:?}"),
    }

    let mut repeated_flags = foundational_font();
    let simple = glyph_range(&repeated_flags, 1);
    repeated_flags[simple.start + 14] = 0x09;
    repeated_flags[simple.start + 15] = 3;
    match parse(&repeated_flags, FontLimits::default()).into_outcome() {
        FontParseOutcome::Failed(error) => assert_eq!(error.code(), FontErrorCode::InvalidGlyph),
        outcome => panic!("expected invalid repeated flags, got {outcome:?}"),
    }
}

#[test]
fn truncations_and_single_byte_corruptions_always_reach_a_typed_terminal_outcome() {
    let bytes = foundational_font();
    for length in 0..bytes.len() {
        let _ = parse(&bytes[..length], FontLimits::default()).into_outcome();
    }
    for index in 0..bytes.len() {
        let mut corrupted = bytes.clone();
        corrupted[index] ^= 0xff;
        let _ = parse(&corrupted, FontLimits::default()).into_outcome();
    }
}

#[test]
fn exact_limits_pass_and_every_one_less_boundary_is_typed() {
    let bytes = foundational_font();
    let baseline = ready(&bytes, FontLimits::default());
    let stats = baseline.stats();
    let largest_glyph = (0..baseline.glyph_count())
        .map(|glyph| glyph_range(&bytes, glyph).len() as u64)
        .max()
        .unwrap();
    let exact = FontLimitConfig {
        max_input_bytes: bytes.len() as u64,
        max_tables: 7,
        max_glyphs: u32::from(baseline.glyph_count()),
        max_cmap_segments: stats.cmap_segments() as u32,
        max_glyph_data_bytes: stats.glyph_data_bytes(),
        max_glyph_bytes: largest_glyph,
        max_glyph_contours: 1,
        max_total_contours: stats.source_contours(),
        max_glyph_points: 3,
        max_total_points: stats.source_points(),
        max_components: stats.components(),
        max_component_depth: 1,
        max_path_segments: stats.path_segments(),
        max_retained_bytes: stats.peak_retained_bytes(),
        max_fuel: stats.fuel(),
        cancellation_check_interval_fuel: 1,
    };
    let exact_font = ready(&bytes, FontLimits::validate(exact).unwrap());
    assert_eq!(exact_font.stats().path_segments(), stats.path_segments());
    assert_eq!(exact_font.stats().fuel(), stats.fuel());

    let cases = [
        (
            FontLimitKind::InputBytes,
            FontLimitConfig {
                max_input_bytes: exact.max_input_bytes - 1,
                ..exact
            },
        ),
        (
            FontLimitKind::Tables,
            FontLimitConfig {
                max_tables: exact.max_tables - 1,
                ..exact
            },
        ),
        (
            FontLimitKind::Glyphs,
            FontLimitConfig {
                max_glyphs: exact.max_glyphs - 1,
                ..exact
            },
        ),
        (
            FontLimitKind::CmapSegments,
            FontLimitConfig {
                max_cmap_segments: exact.max_cmap_segments - 1,
                ..exact
            },
        ),
        (
            FontLimitKind::GlyphDataBytes,
            FontLimitConfig {
                max_glyph_data_bytes: exact.max_glyph_data_bytes - 1,
                ..exact
            },
        ),
        (
            FontLimitKind::GlyphBytes,
            FontLimitConfig {
                max_glyph_bytes: exact.max_glyph_bytes - 1,
                ..exact
            },
        ),
        (
            FontLimitKind::TotalContours,
            FontLimitConfig {
                max_total_contours: exact.max_total_contours - 1,
                ..exact
            },
        ),
        (
            FontLimitKind::GlyphPoints,
            FontLimitConfig {
                max_glyph_points: exact.max_glyph_points - 1,
                ..exact
            },
        ),
        (
            FontLimitKind::TotalPoints,
            FontLimitConfig {
                max_total_points: exact.max_total_points - 1,
                ..exact
            },
        ),
        (
            FontLimitKind::Components,
            FontLimitConfig {
                max_components: exact.max_components - 1,
                ..exact
            },
        ),
        (
            FontLimitKind::PathSegments,
            FontLimitConfig {
                max_path_segments: exact.max_path_segments - 1,
                ..exact
            },
        ),
        (
            FontLimitKind::RetainedBytes,
            FontLimitConfig {
                max_retained_bytes: exact.max_retained_bytes - 1,
                ..exact
            },
        ),
        (
            FontLimitKind::Fuel,
            FontLimitConfig {
                max_fuel: exact.max_fuel - 1,
                ..exact
            },
        ),
    ];
    for (expected, config) in cases {
        assert_eq!(
            limit_kind(&bytes, config),
            expected,
            "one-less {expected:?}"
        );
    }
}

#[test]
fn invalid_limit_profiles_are_rejected_before_parsing() {
    let error = FontLimits::validate(FontLimitConfig {
        max_component_depth: 0,
        ..FontLimitConfig::default()
    })
    .unwrap_err();
    assert_eq!(error.code(), FontErrorCode::InvalidLimits);

    let error = FontLimits::validate(FontLimitConfig {
        max_total_points: 2,
        max_glyph_points: 3,
        ..FontLimitConfig::default()
    })
    .unwrap_err();
    assert_eq!(error.code(), FontErrorCode::InvalidLimits);
}

#[test]
fn compound_depth_cycle_and_component_placement_are_bounded() {
    let nested = build_font(vec![
        Vec::new(),
        triangle_glyph(),
        compound_glyph(&[(1, 0, 0)]),
        compound_glyph(&[(2, 0, 0)]),
    ]);
    assert_eq!(
        limit_kind(
            &nested,
            FontLimitConfig {
                max_component_depth: 1,
                ..FontLimitConfig::default()
            }
        ),
        FontLimitKind::ComponentDepth
    );

    let cycle = build_font(vec![
        Vec::new(),
        triangle_glyph(),
        compound_glyph(&[(2, 0, 0)]),
    ]);
    match parse(&cycle, FontLimits::default()).into_outcome() {
        FontParseOutcome::Failed(error) => {
            assert_eq!(error.code(), FontErrorCode::CompoundCycle);
            assert_eq!(error.glyph_id(), Some(2));
        }
        outcome => panic!("expected compound cycle, got {outcome:?}"),
    }

    let mut point_attachment = foundational_font();
    let compound = glyph_range(&point_attachment, 3);
    set_u16(&mut point_attachment, compound.start + 10, 0x0001 | 0x0020);
    match parse(&point_attachment, FontLimits::default()).into_outcome() {
        FontParseOutcome::Unsupported(value) => {
            assert_eq!(value.kind(), FontUnsupportedKind::CompoundPointAttachment)
        }
        outcome => panic!("expected point attachment outcome, got {outcome:?}"),
    }
}

struct CancelAfter {
    probes: AtomicUsize,
    permitted: usize,
}

impl FontCancellation for CancelAfter {
    fn is_cancelled(&self) -> bool {
        self.probes.fetch_add(1, Ordering::SeqCst) >= self.permitted
    }
}

#[test]
fn cancellation_is_terminal_typed_and_preserves_partial_stats() {
    let bytes = foundational_font();
    let cancellation = CancelAfter {
        probes: AtomicUsize::new(0),
        permitted: 20,
    };
    let limits = FontLimits::validate(FontLimitConfig {
        cancellation_check_interval_fuel: 1,
        ..FontLimitConfig::default()
    })
    .unwrap();
    let report = parse_truetype(&bytes, FontProfile::default(), limits, &cancellation);
    assert!(report.stats().fuel() > 0);
    match report.into_outcome() {
        FontParseOutcome::Cancelled(error) => {
            assert_eq!(error.code(), FontErrorCode::Cancelled)
        }
        outcome => panic!("expected cancellation, got {outcome:?}"),
    }
}

#[test]
fn instructions_are_skipped_but_never_executed() {
    let mut glyph = triangle_glyph();
    set_u16(&mut glyph, 12, 3);
    glyph.splice(14..14, [0xb0, 0x01, 0x2f]);
    let font = build_font(vec![Vec::new(), glyph]);
    let parsed = ready(&font, FontLimits::default());
    assert_eq!(
        parsed
            .glyph_outline(GlyphId::new(1))
            .unwrap()
            .segments()
            .len(),
        4
    );

    let mut compound = compound_glyph(&[(1, 0, 0)]);
    set_u16(&mut compound, 10, 0x0001 | 0x0002 | 0x0100);
    compound.extend_from_slice(&2_u16.to_be_bytes());
    compound.extend_from_slice(&[0xb0, 0x2f]);
    let font = build_font(vec![Vec::new(), triangle_glyph(), compound]);
    let parsed = ready(&font, FontLimits::default());
    assert_eq!(
        parsed
            .glyph_outline(GlyphId::new(2))
            .unwrap()
            .segments()
            .len(),
        4
    );
}

#[test]
fn compound_instruction_flag_is_accumulated_until_the_unique_tail() {
    for flag_position in [10, 18] {
        let mut complete = compound_glyph(&[(1, 0, 0), (1, 0, 0), (1, 0, 0)]);
        set_u16(
            &mut complete,
            flag_position,
            0x0001 | 0x0002 | 0x0020 | 0x0100,
        );
        complete.extend_from_slice(&2_u16.to_be_bytes());
        complete.extend_from_slice(&[0xb0, 0x2f]);
        let bytes = build_font(vec![Vec::new(), triangle_glyph(), complete]);
        let font = ready(&bytes, FontLimits::default());
        assert_eq!(
            font.glyph_outline(GlyphId::new(2))
                .unwrap()
                .segments()
                .len(),
            12
        );

        let mut truncated = compound_glyph(&[(1, 0, 0), (1, 0, 0), (1, 0, 0)]);
        set_u16(
            &mut truncated,
            flag_position,
            0x0001 | 0x0002 | 0x0020 | 0x0100,
        );
        truncated.extend_from_slice(&2_u16.to_be_bytes());
        truncated.push(0xb0);
        let bytes = build_font(vec![Vec::new(), triangle_glyph(), truncated]);
        match parse(&bytes, FontLimits::default()).into_outcome() {
            FontParseOutcome::Failed(error) => {
                assert_eq!(error.code(), FontErrorCode::InvalidGlyph);
                assert_eq!(error.glyph_id(), Some(2));
            }
            outcome => panic!("expected truncated compound instructions, got {outcome:?}"),
        }
    }

    let mut repeated_flag = compound_glyph(&[(1, 0, 0), (1, 0, 0), (1, 0, 0)]);
    set_u16(&mut repeated_flag, 10, 0x0001 | 0x0002 | 0x0020 | 0x0100);
    set_u16(&mut repeated_flag, 18, 0x0001 | 0x0002 | 0x0020 | 0x0100);
    repeated_flag.extend_from_slice(&1_u16.to_be_bytes());
    repeated_flag.push(0x2f);
    let bytes = build_font(vec![Vec::new(), triangle_glyph(), repeated_flag]);
    let font = ready(&bytes, FontLimits::default());
    assert_eq!(
        font.glyph_outline(GlyphId::new(2))
            .unwrap()
            .segments()
            .len(),
        12
    );
}
