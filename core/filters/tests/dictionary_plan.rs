use std::sync::atomic::{AtomicUsize, Ordering};

use pdf_rs_bytes::{SourceIdentity, SourceRevision, SourceStableId};
use pdf_rs_filters::{
    DecodeCancellation, DecodeErrorCode, DecodeLimitConfig, DecodeLimitKind, DecodeLimits,
    FilterDecodeParameters, FilterPlan, NeverCancelled, StreamFilter,
};
use pdf_rs_syntax::{
    InputExtent, PdfDictionary, SyntaxInput, SyntaxLimits, SyntaxObject, SyntaxParser, SyntaxPoll,
};

fn dictionary(bytes: &[u8]) -> PdfDictionary {
    let identity = SourceIdentity::new(SourceStableId::new([0x64; 32]), SourceRevision::new(1));
    let input = SyntaxInput::new(identity, 0, bytes, InputExtent::KnownSourceEnd).unwrap();
    let mut parser = SyntaxParser::new(input, SyntaxLimits::default()).unwrap();
    let value = match parser.parse_object() {
        SyntaxPoll::Ready(value) => value.into_value(),
        other => panic!("dictionary fixture must parse: {other:?}"),
    };
    match value {
        SyntaxObject::Dictionary(dictionary) => dictionary,
        other => panic!("fixture must be a dictionary, got {other:?}"),
    }
}

fn plan(bytes: &[u8]) -> Result<FilterPlan, pdf_rs_filters::DecodeError> {
    FilterPlan::from_pdf_dictionary(&dictionary(bytes), DecodeLimits::default(), &NeverCancelled)
}

#[test]
fn dictionary_preflight_returns_the_exact_declared_filter_count_without_a_plan() {
    for (bytes, expected) in [
        (b"<< /Length 0 >>".as_slice(), 0),
        (b"<< /Filter /FlateDecode >>".as_slice(), 1),
        (
            b"<< /Filter [/ASCIIHexDecode /FlateDecode] /DecodeParms [null <<>>] >>".as_slice(),
            2,
        ),
    ] {
        assert_eq!(
            FilterPlan::preflight_pdf_dictionary(
                &dictionary(bytes),
                DecodeLimits::default(),
                &NeverCancelled,
            )
            .unwrap(),
            expected
        );
    }
}

#[test]
fn direct_dictionary_canonicalization_preserves_order_nulls_and_predictor_defaults() {
    assert!(plan(b"<< /Length 0 >>").unwrap().is_empty());
    let null_chain =
        plan(b"<< /Filter [/ASCIIHexDecode /FlateDecode] /DecodeParms null >>").unwrap();
    assert!(
        null_chain
            .stages()
            .iter()
            .all(|stage| stage.decode_parameters() == FilterDecodeParameters::None)
    );

    let canonical = plan(
        b"<< /Filter [/ASCIIHexDecode /FlateDecode] \
          /DecodeParms [null << /Predictor 12 /Columns 9 >>] >>",
    )
    .unwrap();
    assert_eq!(
        canonical.filters(),
        &[StreamFilter::AsciiHexDecode, StreamFilter::FlateDecode]
    );
    assert_eq!(
        canonical.stages()[0].decode_parameters(),
        FilterDecodeParameters::None
    );
    let FilterDecodeParameters::Predictor(parameters) = canonical.stages()[1].decode_parameters()
    else {
        panic!("Flate dictionary must retain explicit predictor parameters")
    };
    assert_eq!(parameters.predictor(), 12);
    assert_eq!(parameters.colors(), 1);
    assert_eq!(parameters.bits_per_component(), 8);
    assert_eq!(parameters.columns(), 9);
}

#[test]
fn single_filter_null_and_dictionary_forms_remain_distinct_canonical_evidence() {
    let absent = plan(b"<< /Filter /FlateDecode >>").unwrap();
    let null = plan(b"<< /Filter /FlateDecode /DecodeParms null >>").unwrap();
    assert_eq!(absent, null);
    assert_eq!(
        absent.stages()[0].decode_parameters(),
        FilterDecodeParameters::None
    );

    let defaulted = plan(b"<< /Filter /FlateDecode /DecodeParms <<>> >>").unwrap();
    let explicit = plan(
        b"<< /Filter /FlateDecode /DecodeParms \
          << /Predictor 1 /Colors 1 /BitsPerComponent 8 /Columns 1 >> >>",
    )
    .unwrap();
    assert_eq!(absent, defaulted);
    assert_eq!(
        plan(b"<< /Filter /ASCIIHexDecode /DecodeParms <<>> >>").unwrap(),
        FilterPlan::new(&[StreamFilter::AsciiHexDecode]).unwrap()
    );
    let FilterDecodeParameters::Predictor(parameters) = explicit.stages()[0].decode_parameters()
    else {
        panic!("a present Flate parameter dictionary must make defaults explicit")
    };
    assert_eq!(parameters, Default::default());
    assert_ne!(absent, explicit);
}

#[test]
fn dictionary_filter_count_is_rejected_before_plan_publication() {
    let config = DecodeLimitConfig {
        max_filters: 1,
        ..DecodeLimitConfig::default()
    };
    let limits = DecodeLimits::validate(config).unwrap();
    for error in [
        FilterPlan::preflight_pdf_dictionary(
            &dictionary(b"<< /Filter [/ASCIIHexDecode /FlateDecode] >>"),
            limits,
            &NeverCancelled,
        )
        .unwrap_err(),
        FilterPlan::from_pdf_dictionary(
            &dictionary(b"<< /Filter [/ASCIIHexDecode /FlateDecode] >>"),
            limits,
            &NeverCancelled,
        )
        .unwrap_err(),
    ] {
        assert_eq!(error.code(), DecodeErrorCode::ResourceLimit);
        assert_eq!(error.limit().unwrap().kind(), DecodeLimitKind::FilterCount);
        assert_eq!(error.limit().unwrap().limit(), 1);
        assert_eq!(error.limit().unwrap().consumed(), 0);
        assert_eq!(error.limit().unwrap().attempted(), 2);
    }
}

#[test]
fn dictionary_preflight_rejects_unsupported_metadata_before_plan_allocation() {
    let error = FilterPlan::preflight_pdf_dictionary(
        &dictionary(b"<< /Filter /UnknownDecode >>"),
        DecodeLimits::default(),
        &NeverCancelled,
    )
    .unwrap_err();
    assert_eq!(error.code(), DecodeErrorCode::UnsupportedFilter);
}

struct CancelAfter {
    probes: AtomicUsize,
    allowed: usize,
}

impl DecodeCancellation for CancelAfter {
    fn is_cancelled(&self) -> bool {
        self.probes.fetch_add(1, Ordering::Relaxed) >= self.allowed
    }
}

#[test]
fn metadata_walks_observe_cooperative_cancellation() {
    let cancellation = CancelAfter {
        probes: AtomicUsize::new(0),
        allowed: 2,
    };
    let error = FilterPlan::from_pdf_dictionary(
        &dictionary(
            b"<< /Type /ObjStm /N 2 /First 10 /Length 20 \
              /Filter [/ASCIIHexDecode /FlateDecode] /DecodeParms [null <<>>] >>",
        ),
        DecodeLimits::default(),
        &cancellation,
    )
    .unwrap_err();
    assert_eq!(error.code(), DecodeErrorCode::Cancelled);
    assert!(cancellation.probes.load(Ordering::Relaxed) >= 3);
}

#[test]
fn malformed_duplicate_indirect_and_unsupported_metadata_never_builds_a_plan() {
    for bytes in [
        b"<< /Filter /FlateDecode /Filter /ASCIIHexDecode >>".as_slice(),
        b"<< /DecodeParms null >>".as_slice(),
        b"<< /Filter 7 >>".as_slice(),
        b"<< /Filter [] >>".as_slice(),
        b"<< /Filter [/FlateDecode 7] >>".as_slice(),
        b"<< /Filter /Fl >>".as_slice(),
        b"<< /Filter /UnknownDecode >>".as_slice(),
        b"<< /Filter /FlateDecode /DecodeParms [null] >>".as_slice(),
        b"<< /Filter [/FlateDecode] /DecodeParms <<>> >>".as_slice(),
        b"<< /Filter [/FlateDecode /ASCIIHexDecode] /DecodeParms [null] >>".as_slice(),
        b"<< /Filter /FlateDecode /DecodeParms null /DecodeParms null >>".as_slice(),
        b"<< /Filter /FlateDecode /DecodeParms 8 0 R >>".as_slice(),
        b"<< /Filter /FlateDecode /DecodeParms << /Predictor 2 /Predictor 12 >> >>".as_slice(),
        b"<< /Filter /FlateDecode /DecodeParms << /Predictor /Twelve >> >>".as_slice(),
        b"<< /Filter /FlateDecode /DecodeParms << /Unknown 1 >> >>".as_slice(),
    ] {
        assert!(plan(bytes).is_err(), "metadata must be rejected: {bytes:?}");
    }
}
