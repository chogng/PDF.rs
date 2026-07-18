use pdf_rs_font::{
    CffParseOutcome, FontErrorCode, FontLimits, FontProfile, NeverCancelled, OutlineSegment,
    parse_cff,
};

fn index(items: &[Vec<u8>]) -> Vec<u8> {
    let mut bytes = Vec::new();
    bytes.extend_from_slice(&(items.len() as u16).to_be_bytes());
    if items.is_empty() {
        return bytes;
    }
    let data_len: usize = items.iter().map(Vec::len).sum();
    assert!(data_len < 255);
    bytes.push(1);
    let mut offset = 1_u8;
    bytes.push(offset);
    for item in items {
        offset = offset.checked_add(item.len() as u8).unwrap();
        bytes.push(offset);
    }
    for item in items {
        bytes.extend_from_slice(item);
    }
    bytes
}

fn dict_i32(bytes: &mut Vec<u8>, value: usize) {
    bytes.push(29);
    bytes.extend_from_slice(&(value as i32).to_be_bytes());
}

fn type2_integer(bytes: &mut Vec<u8>, value: i16) {
    match value {
        -107..=107 => bytes.push((value + 139) as u8),
        108..=1_131 => {
            let adjusted = value - 108;
            bytes.push((247 + adjusted / 256) as u8);
            bytes.push((adjusted % 256) as u8);
        }
        -1_131..=-108 => {
            let adjusted = -value - 108;
            bytes.push((251 + adjusted / 256) as u8);
            bytes.push((adjusted % 256) as u8);
        }
        _ => {
            bytes.push(28);
            bytes.extend_from_slice(&value.to_be_bytes());
        }
    }
}

fn fixture_with_encoding(encoding: Option<&[u8]>) -> Vec<u8> {
    let name = index(&[b"TestCff".to_vec()]);
    let strings = index(&[]);

    let mut global_subr = Vec::new();
    type2_integer(&mut global_subr, 100);
    type2_integer(&mut global_subr, 0);
    global_subr.extend_from_slice(&[5, 11]);
    let global_subrs = index(&[global_subr]);

    let charset = vec![0, 0, 34, 0, 200];

    let notdef = vec![14];
    let mut letter_a = Vec::new();
    type2_integer(&mut letter_a, 100);
    type2_integer(&mut letter_a, 100);
    letter_a.push(21);
    type2_integer(&mut letter_a, -107);
    letter_a.push(29);
    type2_integer(&mut letter_a, 0);
    type2_integer(&mut letter_a, 100);
    letter_a.extend_from_slice(&[5, 14]);
    let mut aacute = Vec::new();
    type2_integer(&mut aacute, 0);
    type2_integer(&mut aacute, 0);
    aacute.push(21);
    type2_integer(&mut aacute, -107);
    aacute.push(10);
    for value in [10, 5, 10, 5, 10, 10, 10, 5, 10] {
        type2_integer(&mut aacute, value);
    }
    aacute.extend_from_slice(&[12, 36, 14]);
    let charstrings = index(&[notdef, letter_a, aacute]);

    let mut local_subr = Vec::new();
    for value in [50, 0, 50, 100, 50, 0] {
        type2_integer(&mut local_subr, value);
    }
    local_subr.extend_from_slice(&[8, 11]);
    let local_subrs = index(&[local_subr]);

    let mut private = Vec::new();
    type2_integer(&mut private, 500);
    private.push(20);
    dict_i32(&mut private, 9);
    private.push(19);
    assert_eq!(private.len(), 9);

    let top_len = if encoding.is_some() { 29 } else { 23 };
    let top_index_len = 2 + 1 + 2 + top_len;
    let prefix = 4 + name.len() + top_index_len + strings.len() + global_subrs.len();
    let charset_offset = prefix;
    let charstrings_offset = charset_offset + charset.len();
    let private_offset = charstrings_offset + charstrings.len();
    let encoding_offset = private_offset + private.len() + local_subrs.len();

    let mut top = Vec::new();
    dict_i32(&mut top, charset_offset);
    top.push(15);
    dict_i32(&mut top, charstrings_offset);
    top.push(17);
    dict_i32(&mut top, private.len());
    dict_i32(&mut top, private_offset);
    top.push(18);
    if encoding.is_some() {
        dict_i32(&mut top, encoding_offset);
        top.push(16);
    }
    assert_eq!(top.len(), top_len);

    let mut bytes = vec![1, 0, 4, 4];
    bytes.extend_from_slice(&name);
    bytes.extend_from_slice(&index(&[top]));
    bytes.extend_from_slice(&strings);
    bytes.extend_from_slice(&global_subrs);
    bytes.extend_from_slice(&charset);
    bytes.extend_from_slice(&charstrings);
    bytes.extend_from_slice(&private);
    bytes.extend_from_slice(&local_subrs);
    if let Some(encoding) = encoding {
        bytes.extend_from_slice(encoding);
    }
    bytes
}

fn fixture() -> Vec<u8> {
    fixture_with_encoding(None)
}

#[test]
fn standalone_cff_publishes_named_line_and_cubic_glyphs() {
    let bytes = fixture();
    let report = parse_cff(
        &bytes,
        FontProfile::SimpleType1CStandardV1,
        FontLimits::default(),
        &NeverCancelled,
    );
    let outcome = report.into_outcome();
    let CffParseOutcome::Ready(font) = outcome else {
        panic!("fixture must publish, got {outcome:?}");
    };

    assert_eq!(font.glyph_count(), 3);
    assert_eq!(font.units_per_em(), 1_000);
    assert_eq!(font.glyph_id_for_name("A").unwrap().get(), 1);
    assert_eq!(font.glyph_id_for_standard_code(b'A').unwrap().get(), 1);
    assert_eq!(font.glyph_id_for_winansi_code(b'A').unwrap().get(), 1);
    assert_eq!(font.glyph_id_for_name("aacute").unwrap().get(), 2);
    assert_eq!(font.glyph_id_for_winansi_code(0xe1).unwrap().get(), 2);
    assert_eq!(
        font.advance_width(font.glyph_id_for_name("A").unwrap()),
        Some(500)
    );

    let a = font
        .glyph_outline(font.glyph_id_for_name("A").unwrap())
        .unwrap();
    assert!(matches!(a.segments()[0], OutlineSegment::MoveTo(_)));
    assert!(
        a.segments()
            .iter()
            .any(|segment| matches!(segment, OutlineSegment::LineTo(_)))
    );
    assert!(matches!(
        a.segments().last(),
        Some(OutlineSegment::CloseContour)
    ));

    let accented = font
        .glyph_outline(font.glyph_id_for_name("aacute").unwrap())
        .unwrap();
    assert!(
        accented
            .segments()
            .iter()
            .any(|segment| matches!(segment, OutlineSegment::CubicTo { .. }))
    );
}

#[test]
fn malformed_index_and_profile_mismatch_are_typed() {
    let bytes = fixture();
    let mismatch = parse_cff(
        &bytes,
        FontProfile::SimpleTrueTypeWinAnsiV1,
        FontLimits::default(),
        &NeverCancelled,
    );
    assert!(matches!(
        mismatch.into_outcome(),
        CffParseOutcome::Unsupported(_)
    ));

    let malformed = parse_cff(
        &bytes[..8],
        FontProfile::SimpleType1CStandardV1,
        FontLimits::default(),
        &NeverCancelled,
    );
    match malformed.into_outcome() {
        CffParseOutcome::Failed(error) => assert_eq!(error.code(), FontErrorCode::InvalidCff),
        outcome => panic!("truncated INDEX must fail, got {outcome:?}"),
    }
}

#[test]
fn bounded_custom_encoding_formats_are_validated_without_overriding_pdf_name_mapping() {
    for encoding in [
        vec![0, 2, b'A', 0xe1],
        vec![1, 2, b'A', 0, 0xe1, 0],
        vec![0x80, 1, b'A', 1, 0xe1, 0, 200],
    ] {
        let bytes = fixture_with_encoding(Some(&encoding));
        let outcome = parse_cff(
            &bytes,
            FontProfile::SimpleType1CStandardV1,
            FontLimits::default(),
            &NeverCancelled,
        )
        .into_outcome();
        let CffParseOutcome::Ready(font) = outcome else {
            panic!("bounded custom Encoding must publish: {outcome:?}");
        };
        assert_eq!(font.glyph_id_for_standard_code(b'A').unwrap().get(), 1);
        assert_eq!(font.glyph_id_for_winansi_code(0xe1).unwrap().get(), 2);
    }

    for malformed in [
        vec![0, 3, b'A', b'B'],
        vec![0, 2, b'A', b'A'],
        vec![1, 1, 0xff, 1],
        vec![0x80, 1, b'A', 1, b'A', 0, 34],
        vec![0x80, 1, b'A', 1, b'B', 0xff, 0xff],
    ] {
        let bytes = fixture_with_encoding(Some(&malformed));
        match parse_cff(
            &bytes,
            FontProfile::SimpleType1CStandardV1,
            FontLimits::default(),
            &NeverCancelled,
        )
        .into_outcome()
        {
            CffParseOutcome::Failed(error) => assert_eq!(error.code(), FontErrorCode::InvalidCff),
            outcome => panic!("malformed custom Encoding must fail: {outcome:?}"),
        }
    }
}
