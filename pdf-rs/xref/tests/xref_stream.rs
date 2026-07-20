use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use pdf_rs_bytes::{
    SourceIdentity, SourceRevision, SourceSnapshot, SourceStableId, SourceValidator,
    SourceValidatorKind,
};
use pdf_rs_syntax::{
    ByteSpan, InputExtent, ObjectRef, PdfDictionary, SyntaxInput, SyntaxLimits, SyntaxObject,
    SyntaxParser, SyntaxPoll,
};
use pdf_rs_xref::{
    NeverCancelled, RevisionEntry, RevisionEntryKind, XrefCancellation, XrefRecoverability,
    XrefStream, XrefStreamEntryKind, XrefStreamError, XrefStreamErrorCategory, XrefStreamErrorCode,
    XrefStreamLimitConfig, XrefStreamLimitKind, XrefStreamLimits, parse_decoded_xref_stream,
    parse_unfiltered_xref_stream,
};

fn identity(byte: u8) -> SourceIdentity {
    SourceIdentity::new(SourceStableId::new([byte; 32]), SourceRevision::new(13))
}

fn snapshot(source: SourceIdentity) -> SourceSnapshot {
    snapshot_with_len(source, 1024)
}

fn snapshot_with_len(source: SourceIdentity, len: u64) -> SourceSnapshot {
    SourceSnapshot::new(
        source,
        Some(len),
        SourceValidator::new(SourceValidatorKind::FrozenResponse, [0x44; 32]),
    )
}

#[derive(Default)]
struct CancelOnFirstProbe {
    probes: AtomicUsize,
}

impl CancelOnFirstProbe {
    fn probes(&self) -> usize {
        self.probes.load(Ordering::SeqCst)
    }
}

impl XrefCancellation for CancelOnFirstProbe {
    fn is_cancelled(&self) -> bool {
        self.probes.fetch_add(1, Ordering::SeqCst) == 0
    }
}

fn parse_dictionary(source: SourceIdentity, input: &[u8]) -> pdf_rs_syntax::Located<SyntaxObject> {
    let input = SyntaxInput::new(source, 32, input, InputExtent::KnownSourceEnd).unwrap();
    let mut parser = SyntaxParser::new(input, SyntaxLimits::default()).unwrap();
    match parser.parse_object() {
        SyntaxPoll::Ready(value) => value,
        other => panic!("expected dictionary, got {other:?}"),
    }
}

fn dictionary(value: &pdf_rs_syntax::Located<SyntaxObject>) -> &PdfDictionary {
    value.value().as_dictionary().unwrap()
}

fn canonical_payload() -> Vec<u8> {
    vec![
        0, 0, 0, 255, // object 0: free, next 0, generation 255
        1, 0, 64, 0, // object 1: uncompressed at 64, generation 0
        2, 0, 5, 7, // object 2: object stream 5, index 7
    ]
}

fn large_filtered_dictionary(extra_entries: usize) -> Vec<u8> {
    let mut bytes = b"<< /Type /XRef /Size 3 /W [1 2 1] /Filter /FlateDecode ".to_vec();
    for index in 0..extra_entries {
        bytes.extend_from_slice(format!("/K{index} {index} ").as_bytes());
    }
    bytes.extend_from_slice(b">>");
    bytes
}

fn parse(
    dictionary_bytes: &[u8],
    payload: &[u8],
    limits: XrefStreamLimits,
) -> Result<XrefStream, XrefStreamError> {
    let source = identity(0x91);
    let parsed = parse_dictionary(source, dictionary_bytes);
    let span = ByteSpan::new(400, u64::try_from(payload.len()).unwrap()).unwrap();
    parse_unfiltered_xref_stream(
        snapshot(source),
        ObjectRef::new(9, 0).unwrap(),
        dictionary(&parsed),
        span,
        payload,
        limits,
        &NeverCancelled,
    )
}

fn parse_filtered(
    dictionary_bytes: &[u8],
    encoded_len: u64,
    decoded_payload: &[u8],
    limits: XrefStreamLimits,
) -> Result<XrefStream, XrefStreamError> {
    let source = identity(0x91);
    let parsed = parse_dictionary(source, dictionary_bytes);
    parse_decoded_xref_stream(
        snapshot(source),
        ObjectRef::new(9, 0).unwrap(),
        dictionary(&parsed),
        ByteSpan::new(400, encoded_len).unwrap(),
        decoded_payload,
        limits,
        &NeverCancelled,
    )
}

fn canonical() -> XrefStream {
    parse(
        b"<< /Type /XRef /Size 3 /W [1 2 1] /Root 1 0 R /Prev 12 >>",
        &canonical_payload(),
        XrefStreamLimits::default(),
    )
    .unwrap()
}

#[test]
fn canonical_rows_keep_semantics_and_decoded_coordinates_separate() {
    let stream = canonical();
    assert_eq!(stream.container(), ObjectRef::new(9, 0).unwrap());
    assert_eq!(
        stream.encoded_payload_span(),
        ByteSpan::new(400, 12).unwrap()
    );
    assert_eq!(stream.declared_size(), 3);
    assert_eq!(stream.root(), Some(ObjectRef::new(1, 0).unwrap()));
    assert_eq!(stream.previous(), Some(12));
    assert_eq!(stream.widths(), [1, 2, 1]);
    assert_eq!(stream.entries().len(), 3);
    assert_eq!(stream.entries()[0].object_number(), 0);
    assert_eq!(stream.entries()[0].decoded_span().start(), 0);
    assert_eq!(stream.entries()[0].decoded_span().end_exclusive(), 4);
    assert_eq!(
        stream.entries()[0].kind(),
        XrefStreamEntryKind::Free {
            next_free: 0,
            generation: 255
        }
    );
    assert_eq!(
        stream.entries()[1].kind(),
        XrefStreamEntryKind::Uncompressed {
            offset: 64,
            generation: 0
        }
    );
    assert_eq!(
        stream.entries()[2].kind(),
        XrefStreamEntryKind::Compressed {
            object_stream: 5,
            index: 7
        }
    );
    assert_eq!(stream.stats().decoded_bytes(), 12);
    assert_eq!(stream.stats().entries(), 3);
    assert_eq!(stream.stats().index_pairs(), 1);
    assert!(stream.stats().retained_entry_bytes() >= 3 * 24);
    assert!(!format!("{stream:?}").contains("[0, 0, 0, 255]"));
}

#[test]
fn zero_type_width_defaults_to_uncompressed_and_index_controls_numbers() {
    let stream = parse(
        b"<< /Type /XRef /Size 8 /W [0 2 1] /Index [2 1 7 1] >>",
        &[0, 20, 0, 0, 90, 3],
        XrefStreamLimits::default(),
    )
    .unwrap();
    assert_eq!(stream.entries().len(), 2);
    assert_eq!(stream.entries()[0].object_number(), 2);
    assert_eq!(stream.entries()[1].object_number(), 7);
    assert_eq!(
        stream.entries()[1].kind(),
        XrefStreamEntryKind::Uncompressed {
            offset: 90,
            generation: 3
        }
    );
}

#[test]
fn malformed_width_index_and_payload_geometry_are_distinct() {
    for (dictionary, code) in [
        (
            b"<< /Type /XRef /Size 3 /W [1 2] >>".as_slice(),
            XrefStreamErrorCode::InvalidWidths,
        ),
        (
            b"<< /Type /XRef /Size 3 /W [1 2 1] /Index [0 2 1 1] >>".as_slice(),
            XrefStreamErrorCode::InvalidIndex,
        ),
        (
            b"<< /Type /XRef /Size 3 /W [1 2 1] /Index [0 4] >>".as_slice(),
            XrefStreamErrorCode::InvalidIndex,
        ),
    ] {
        assert_eq!(
            parse(
                dictionary,
                &canonical_payload(),
                XrefStreamLimits::default()
            )
            .unwrap_err()
            .code(),
            code
        );
    }
    let error = parse(
        b"<< /Type /XRef /Size 3 /W [1 2 1] >>",
        &canonical_payload()[..11],
        XrefStreamLimits::default(),
    )
    .unwrap_err();
    assert_eq!(error.code(), XrefStreamErrorCode::InvalidPayloadLength);
    assert_eq!(error.source_offset(), Some(400));
    assert_eq!(error.decoded_offset(), None);
}

#[test]
fn unknown_rows_become_null_and_out_of_range_rows_use_only_decoded_offsets() {
    let mut unknown = canonical_payload();
    unknown[4] = 3;
    let stream = parse(
        b"<< /Type /XRef /Size 3 /W [1 2 1] >>",
        &unknown,
        XrefStreamLimits::default(),
    )
    .unwrap();
    assert_eq!(
        stream.entries()[1].kind(),
        XrefStreamEntryKind::Null { encoded_type: 3 }
    );
    assert_eq!(stream.entries()[1].decoded_span().start(), 4);
    assert_eq!(
        RevisionEntry::from(stream.entries()[1]).kind(),
        RevisionEntryKind::Null { encoded_type: 3 }
    );

    let error = parse(
        b"<< /Type /XRef /Size 1 /W [1 8 1] >>",
        &[2, 0, 0, 0, 1, 0, 0, 0, 0, 0],
        XrefStreamLimits::default(),
    )
    .unwrap_err();
    assert_eq!(error.code(), XrefStreamErrorCode::InvalidEntry);
    assert_eq!(error.decoded_offset(), Some(0));
    assert!(!format!("{error:?}").contains("[2, 0, 0"));
}

#[test]
fn unfiltered_entry_rejects_filter_metadata_without_decoding() {
    for dictionary in [
        b"<< /Type /XRef /Size 3 /W [1 2 1] /Filter /FlateDecode >>".as_slice(),
        b"<< /Type /XRef /Size 3 /W [1 2 1] /DecodeParms << >> >>".as_slice(),
    ] {
        let error = parse(
            dictionary,
            &canonical_payload(),
            XrefStreamLimits::default(),
        )
        .unwrap_err();
        assert_eq!(error.code(), XrefStreamErrorCode::UnsupportedFilter);
        assert_eq!(error.category(), XrefStreamErrorCategory::Unsupported);
        assert_eq!(
            error.recoverability(),
            XrefRecoverability::UseSupportedFeature
        );
        assert_eq!(error.diagnostic_id(), "RPE-XREF-0104");
    }
}

#[test]
fn decoded_entry_accepts_filtered_metadata_and_distinct_physical_geometry() {
    let stream = parse_filtered(
        b"<< /Type /XRef /Size 3 /W [1 2 1] /Root 1 0 R /Prev 12 /Filter /FlateDecode /DecodeParms << /Predictor 12 >> >>",
        7,
        &canonical_payload(),
        XrefStreamLimits::default(),
    )
    .unwrap();

    assert_eq!(
        stream.encoded_payload_span(),
        ByteSpan::new(400, 7).unwrap()
    );
    assert_eq!(stream.stats().decoded_bytes(), 12);
    assert_eq!(stream.entries().len(), 3);
    assert_eq!(stream.entries()[1].decoded_span().start(), 4);
    assert_eq!(
        stream.entries()[1].kind(),
        XrefStreamEntryKind::Uncompressed {
            offset: 64,
            generation: 0
        }
    );
}

#[test]
fn decoded_entry_reuses_strict_dictionary_semantics() {
    for (dictionary, code) in [
        (
            b"<< /Type /Other /Size 3 /W [1 2 1] /Filter /FlateDecode >>".as_slice(),
            XrefStreamErrorCode::InvalidDictionary,
        ),
        (
            b"<< /Type /XRef /Size 0 /W [1 2 1] /Filter /FlateDecode >>".as_slice(),
            XrefStreamErrorCode::InvalidDictionary,
        ),
        (
            b"<< /Type /XRef /Size 3 /W [1 2] /Filter /FlateDecode >>".as_slice(),
            XrefStreamErrorCode::InvalidWidths,
        ),
        (
            b"<< /Type /XRef /Size 3 /W [1 2 1] /Index [0 4] /Filter /FlateDecode >>".as_slice(),
            XrefStreamErrorCode::InvalidIndex,
        ),
        (
            b"<< /Type /XRef /Size 3 /W [1 2 1] /Root 7 /Filter /FlateDecode >>".as_slice(),
            XrefStreamErrorCode::InvalidDictionary,
        ),
        (
            b"<< /Type /XRef /Size 3 /W [1 2 1] /Prev -1 /Filter /FlateDecode >>".as_slice(),
            XrefStreamErrorCode::InvalidDictionary,
        ),
    ] {
        assert_eq!(
            parse_filtered(
                dictionary,
                7,
                &canonical_payload(),
                XrefStreamLimits::default()
            )
            .unwrap_err()
            .code(),
            code
        );
    }
}

#[test]
fn decoded_entry_leaves_filter_plan_interpretation_to_the_proof_owner() {
    parse_filtered(
        b"<< /Type /XRef /Size 3 /W [1 2 1] /Filter 7 /DecodeParms true >>",
        7,
        &canonical_payload(),
        XrefStreamLimits::default(),
    )
    .unwrap();

    for dictionary in [
        b"<< /Type /XRef /Size 3 /W [1 2 1] /Filter /A /Filter /B >>".as_slice(),
        b"<< /Type /XRef /Size 3 /W [1 2 1] /Filter /A /DecodeParms null /DecodeParms null >>"
            .as_slice(),
    ] {
        let error = parse_filtered(
            dictionary,
            7,
            &canonical_payload(),
            XrefStreamLimits::default(),
        )
        .unwrap_err();
        assert_eq!(error.code(), XrefStreamErrorCode::InvalidDictionary);
    }
}

#[test]
fn payload_entry_modes_remain_disjoint() {
    let source = identity(0x91);
    let parsed = parse_dictionary(source, b"<< /Type /XRef /Size 3 /W [1 2 1] >>");
    let payload = canonical_payload();

    let error = parse_decoded_xref_stream(
        snapshot(source),
        ObjectRef::new(9, 0).unwrap(),
        dictionary(&parsed),
        ByteSpan::new(400, 7).unwrap(),
        &payload,
        XrefStreamLimits::default(),
        &NeverCancelled,
    )
    .unwrap_err();
    assert_eq!(error.code(), XrefStreamErrorCode::UnsupportedFilter);

    let error = parse_unfiltered_xref_stream(
        snapshot(source),
        ObjectRef::new(9, 0).unwrap(),
        dictionary(&parsed),
        ByteSpan::new(400, 7).unwrap(),
        &payload,
        XrefStreamLimits::default(),
        &NeverCancelled,
    )
    .unwrap_err();
    assert_eq!(error.code(), XrefStreamErrorCode::SourceMismatch);
    assert_eq!(error.source_offset(), Some(400));
}

#[test]
fn decoded_payload_failures_use_only_relative_decoded_coordinates() {
    let error = parse_filtered(
        b"<< /Type /XRef /Size 3 /W [1 2 1] /Filter /FlateDecode >>",
        7,
        &canonical_payload()[..11],
        XrefStreamLimits::default(),
    )
    .unwrap_err();
    assert_eq!(error.code(), XrefStreamErrorCode::InvalidPayloadLength);
    assert_eq!(error.source_offset(), None);
    assert_eq!(error.decoded_offset(), Some(11));

    let error = parse_filtered(
        b"<< /Type /XRef /Size 1 /W [1 2 1] /Filter /FlateDecode >>",
        2,
        &[2, 0, 0, 0],
        XrefStreamLimits::default(),
    )
    .unwrap_err();
    assert_eq!(error.code(), XrefStreamErrorCode::InvalidEntry);
    assert_eq!(error.source_offset(), None);
    assert_eq!(error.decoded_offset(), Some(0));
}

#[test]
fn decoded_entry_enforces_limits_cancellation_and_source_geometry() {
    let tight = XrefStreamLimits::validate(XrefStreamLimitConfig {
        max_decoded_bytes: 11,
        ..XrefStreamLimitConfig::default()
    })
    .unwrap();
    let error = parse_filtered(
        b"<< /Type /XRef /Size 3 /W [1 2 1] /Filter /FlateDecode >>",
        7,
        &canonical_payload(),
        tight,
    )
    .unwrap_err();
    assert_eq!(error.code(), XrefStreamErrorCode::ResourceLimit);
    assert_eq!(error.source_offset(), None);
    assert_eq!(error.decoded_offset(), Some(11));
    assert_eq!(
        error.limit(),
        Some((XrefStreamLimitKind::DecodedBytes, 11, 12))
    );

    let source = identity(0x91);
    let parsed = parse_dictionary(
        source,
        b"<< /Type /XRef /Size 3 /W [1 2 1] /Filter /FlateDecode >>",
    );
    let payload = canonical_payload();
    let cancelled = AtomicBool::new(true);
    let error = parse_decoded_xref_stream(
        snapshot(source),
        ObjectRef::new(9, 0).unwrap(),
        dictionary(&parsed),
        ByteSpan::new(400, 7).unwrap(),
        &payload,
        XrefStreamLimits::default(),
        &cancelled,
    )
    .unwrap_err();
    assert_eq!(error.code(), XrefStreamErrorCode::Cancelled);

    for (wrong_snapshot, span) in [
        (snapshot(identity(0x92)), ByteSpan::new(400, 7).unwrap()),
        (snapshot(source), ByteSpan::new(1018, 7).unwrap()),
    ] {
        let error = parse_decoded_xref_stream(
            wrong_snapshot,
            ObjectRef::new(9, 0).unwrap(),
            dictionary(&parsed),
            span,
            &payload,
            XrefStreamLimits::default(),
            &cancelled,
        )
        .unwrap_err();
        assert_eq!(error.code(), XrefStreamErrorCode::SourceMismatch);
        assert_eq!(error.decoded_offset(), None);
    }
}

#[test]
fn decoded_dictionary_geometry_cancellation_is_bounded_to_256_entries() {
    let source = identity(0x91);
    let dictionary_bytes = large_filtered_dictionary(300);
    let parsed = parse_dictionary(source, &dictionary_bytes);
    assert!(dictionary(&parsed).entries().len() > 256);
    let cancellation = CancelOnFirstProbe::default();

    let error = parse_decoded_xref_stream(
        snapshot_with_len(source, 30_000),
        ObjectRef::new(9, 0).unwrap(),
        dictionary(&parsed),
        ByteSpan::new(20_000, 7).unwrap(),
        &canonical_payload(),
        XrefStreamLimits::default(),
        &cancellation,
    )
    .unwrap_err();

    assert_eq!(error.code(), XrefStreamErrorCode::Cancelled);
    assert_eq!(error.category(), XrefStreamErrorCategory::Cancellation);
    assert_eq!(cancellation.probes(), 1);
}

#[test]
fn decoded_retained_capacity_accepts_exact_and_rejects_one_less() {
    let dictionary = b"<< /Type /XRef /Size 3 /W [1 2 1] /Filter /FlateDecode >>";
    let measured = parse_filtered(
        dictionary,
        7,
        &canonical_payload(),
        XrefStreamLimits::default(),
    )
    .unwrap();
    let stats = measured.stats();
    let exact = XrefStreamLimits::validate(XrefStreamLimitConfig {
        max_decoded_bytes: stats.decoded_bytes(),
        max_entries: stats.entries(),
        max_index_pairs: stats.index_pairs(),
        max_field_width: 2,
        max_retained_entry_bytes: stats.retained_entry_bytes(),
    })
    .unwrap();
    parse_filtered(dictionary, 7, &canonical_payload(), exact).unwrap();

    let tight = XrefStreamLimits::validate(XrefStreamLimitConfig {
        max_retained_entry_bytes: stats.retained_entry_bytes() - 1,
        max_decoded_bytes: stats.decoded_bytes(),
        max_entries: stats.entries(),
        max_index_pairs: stats.index_pairs(),
        max_field_width: 2,
    })
    .unwrap();
    let error = parse_filtered(dictionary, 7, &canonical_payload(), tight).unwrap_err();
    assert_eq!(error.code(), XrefStreamErrorCode::ResourceLimit);
    assert_eq!(
        error.limit(),
        Some((
            XrefStreamLimitKind::RetainedEntries,
            stats.retained_entry_bytes() - 1,
            stats.retained_entry_bytes()
        ))
    );
}

#[test]
fn unfiltered_entry_decoded_byte_and_retained_capacity_limits_are_exact() {
    let measured = canonical();
    let stats = measured.stats();
    let exact = XrefStreamLimits::validate(XrefStreamLimitConfig {
        max_decoded_bytes: stats.decoded_bytes(),
        max_entries: stats.entries(),
        max_index_pairs: stats.index_pairs(),
        max_field_width: 2,
        max_retained_entry_bytes: stats.retained_entry_bytes(),
    })
    .unwrap();
    parse(
        b"<< /Type /XRef /Size 3 /W [1 2 1] >>",
        &canonical_payload(),
        exact,
    )
    .unwrap();

    let decoded_tight = XrefStreamLimits::validate(XrefStreamLimitConfig {
        max_decoded_bytes: stats.decoded_bytes() - 1,
        ..XrefStreamLimitConfig {
            max_decoded_bytes: stats.decoded_bytes(),
            max_entries: stats.entries(),
            max_index_pairs: stats.index_pairs(),
            max_field_width: 2,
            max_retained_entry_bytes: stats.retained_entry_bytes(),
        }
    })
    .unwrap();
    let error = parse(
        b"<< /Type /XRef /Size 3 /W [1 2 1] >>",
        &canonical_payload(),
        decoded_tight,
    )
    .unwrap_err();
    assert_eq!(error.code(), XrefStreamErrorCode::ResourceLimit);
    assert_eq!(
        error.limit(),
        Some((
            XrefStreamLimitKind::DecodedBytes,
            stats.decoded_bytes() - 1,
            stats.decoded_bytes()
        ))
    );

    let retained_tight = XrefStreamLimits::validate(XrefStreamLimitConfig {
        max_retained_entry_bytes: stats.retained_entry_bytes() - 1,
        max_decoded_bytes: stats.decoded_bytes(),
        max_entries: stats.entries(),
        max_index_pairs: stats.index_pairs(),
        max_field_width: 2,
    })
    .unwrap();
    let error = parse(
        b"<< /Type /XRef /Size 3 /W [1 2 1] >>",
        &canonical_payload(),
        retained_tight,
    )
    .unwrap_err();
    assert_eq!(
        error.limit().map(|value| value.0),
        Some(XrefStreamLimitKind::RetainedEntries)
    );
}

#[test]
fn source_mismatch_and_cancellation_precede_row_parsing() {
    let dictionary_source = identity(0x91);
    let parsed = parse_dictionary(dictionary_source, b"<< /Type /XRef /Size 3 /W [1 2 1] >>");
    let payload = canonical_payload();
    let span = ByteSpan::new(400, 12).unwrap();
    let cancelled = AtomicBool::new(true);
    for (source_snapshot, encoded_span) in [
        (snapshot(identity(0x92)), span),
        (snapshot(dictionary_source), ByteSpan::new(400, 11).unwrap()),
    ] {
        let error = parse_unfiltered_xref_stream(
            source_snapshot,
            ObjectRef::new(9, 0).unwrap(),
            dictionary(&parsed),
            encoded_span,
            &payload,
            XrefStreamLimits::default(),
            &cancelled,
        )
        .unwrap_err();
        assert_eq!(error.code(), XrefStreamErrorCode::SourceMismatch);
        assert_eq!(error.category(), XrefStreamErrorCategory::Source);
    }

    let error = parse_unfiltered_xref_stream(
        snapshot(dictionary_source),
        ObjectRef::new(9, 0).unwrap(),
        dictionary(&parsed),
        span,
        &payload,
        XrefStreamLimits::default(),
        &cancelled,
    )
    .unwrap_err();
    assert_eq!(error.code(), XrefStreamErrorCode::Cancelled);
    assert_eq!(error.category(), XrefStreamErrorCategory::Cancellation);
    assert_eq!(error.recoverability(), XrefRecoverability::AbandonOperation);
}

#[test]
fn limit_profiles_reject_zero_and_hard_ceiling_overrides() {
    let defaults = XrefStreamLimitConfig::default();
    for invalid in [
        XrefStreamLimitConfig {
            max_decoded_bytes: 0,
            ..defaults
        },
        XrefStreamLimitConfig {
            max_entries: 0,
            ..defaults
        },
        XrefStreamLimitConfig {
            max_index_pairs: 0,
            ..defaults
        },
        XrefStreamLimitConfig {
            max_field_width: 9,
            ..defaults
        },
        XrefStreamLimitConfig {
            max_retained_entry_bytes: 0,
            ..defaults
        },
    ] {
        assert_eq!(
            XrefStreamLimits::validate(invalid).unwrap_err().code(),
            XrefStreamErrorCode::InvalidLimits
        );
    }
}
