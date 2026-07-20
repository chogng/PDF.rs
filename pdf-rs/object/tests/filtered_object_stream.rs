use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use pdf_rs_bytes::{
    ByteRange, ByteSlice, ByteSource, JobId, RangeResponse, RangeStore, ReadPoll, ReadRequest,
    RequestPriority, ResumeCheckpoint, SourceIdentity, SourceRevision, SourceSnapshot,
    SourceStableId, SourceValidator, SourceValidatorKind,
};
use pdf_rs_filters::{
    DecodeLimitConfig, DecodeLimits, DecodeProfile, DecodeRequest, DecodedStream,
    FilterDecodeParameters, FilterPlan, FilterStage, NeverCancelled as NeverDecodeCancelled,
    PredictorParameters, StreamFilter, decode_stream,
};
use pdf_rs_object::{
    IndirectObject, IndirectObjectTarget, IndirectObjectValue,
    NeverCancelled as NeverObjectCancelled, ObjectJobContext, ObjectLimits, ObjectPoll,
    ObjectRecoverability, ObjectStreamError, ObjectStreamErrorCategory, ObjectStreamErrorCode,
    ObjectStreamLimitConfig, ObjectStreamLimitKind, ObjectStreamLimits, OpenObjectJob,
    parse_filtered_object_stream,
};
use pdf_rs_syntax::{ByteSpan, ObjectRef, SyntaxLimits};

const CONTAINER_NUMBER: u32 = 5;

fn snapshot(len: u64, marker: u8) -> SourceSnapshot {
    SourceSnapshot::new(
        SourceIdentity::new(
            SourceStableId::new([marker; 32]),
            SourceRevision::new(u64::from(marker)),
        ),
        Some(len),
        SourceValidator::new(SourceValidatorKind::FrozenResponse, [marker ^ 0x5a; 32]),
    )
}

fn reference(number: u32, generation: u16) -> ObjectRef {
    ObjectRef::new(number, generation).unwrap()
}

fn context() -> ObjectJobContext {
    ObjectJobContext::new(
        JobId::new(801),
        ResumeCheckpoint::new(802),
        ResumeCheckpoint::new(803),
        RequestPriority::VisiblePage,
    )
}

struct Fixture {
    bytes: Vec<u8>,
    decoded: Vec<u8>,
    encoded_range: ByteRange,
    duplicate_encoded_range: ByteRange,
    object_upper_bound: u64,
    startxref: u64,
    container: ObjectRef,
    predictor: bool,
}

fn fixture(
    first_object: &[u8],
    second_object: &[u8],
    include_filter: bool,
    predictor: bool,
    generation: u16,
) -> Fixture {
    assemble_fixture(
        first_object,
        second_object,
        predictor,
        generation,
        |decoded| {
            if include_filter {
                if predictor {
                    format!(
                        " /Filter /FlateDecode /DecodeParms << /Predictor 12 /Colors 1 /BitsPerComponent 8 /Columns {} >>",
                        decoded.len()
                    )
                } else {
                    " /Filter /FlateDecode".to_owned()
                }
            } else {
                String::new()
            }
        },
        |decoded| {
            if predictor {
                let mut bytes = Vec::with_capacity(decoded.len() + 1);
                bytes.push(0);
                bytes.extend_from_slice(decoded);
                zlib_stored(&bytes)
            } else {
                zlib_stored(decoded)
            }
        },
    )
}

fn fixture_with_metadata(filter_metadata: &str, encoder: impl FnOnce(&[u8]) -> Vec<u8>) -> Fixture {
    assemble_fixture(
        b"<< /A [1 2] /Ref 12 0 R >>",
        b"(second)",
        false,
        0,
        |_| filter_metadata.to_owned(),
        encoder,
    )
}

fn assemble_fixture(
    first_object: &[u8],
    second_object: &[u8],
    predictor: bool,
    generation: u16,
    metadata: impl FnOnce(&[u8]) -> String,
    encoder: impl FnOnce(&[u8]) -> Vec<u8>,
) -> Fixture {
    let header = format!("10 0 11 {} ", first_object.len() + 1);
    let mut decoded = header.as_bytes().to_vec();
    decoded.extend_from_slice(first_object);
    decoded.push(b' ');
    decoded.extend_from_slice(second_object);
    let first = header.len();
    let encoded = encoder(&decoded);
    let filter = metadata(&decoded);
    let mut bytes = format!(
        "{CONTAINER_NUMBER} {generation} obj\n<< /Type /ObjStm /N 2 /First {first} /Length {}{filter} >>\nstream\n",
        encoded.len()
    )
    .into_bytes();
    let encoded_start = u64::try_from(bytes.len()).unwrap();
    bytes.extend_from_slice(&encoded);
    bytes.extend_from_slice(b"\nendstream\nendobj\n");
    let object_upper_bound = u64::try_from(bytes.len()).unwrap();
    let startxref = object_upper_bound;
    bytes.extend_from_slice(b"xref\n");
    let duplicate_start = u64::try_from(bytes.len()).unwrap();
    bytes.extend_from_slice(&encoded);
    Fixture {
        bytes,
        decoded,
        encoded_range: ByteRange::new(encoded_start, u64::try_from(encoded.len()).unwrap())
            .unwrap(),
        duplicate_encoded_range: ByteRange::new(
            duplicate_start,
            u64::try_from(encoded.len()).unwrap(),
        )
        .unwrap(),
        object_upper_bound,
        startxref,
        container: reference(CONTAINER_NUMBER, generation),
        predictor,
    }
}

fn valid_fixture(predictor: bool) -> Fixture {
    fixture(
        b"<< /A [1 2] /Ref 12 0 R >>",
        b"(second)",
        true,
        predictor,
        0,
    )
}

fn supplied_store(fixture: &Fixture, marker: u8) -> RangeStore {
    let source = snapshot(u64::try_from(fixture.bytes.len()).unwrap(), marker);
    let store = RangeStore::new(source, Default::default()).unwrap();
    let range = ByteRange::new(0, u64::try_from(fixture.bytes.len()).unwrap()).unwrap();
    store
        .supply(RangeResponse::new(source, range, fixture.bytes.clone()).unwrap())
        .unwrap();
    store
}

fn open_container(store: &RangeStore, fixture: &Fixture) -> IndirectObject {
    let target = IndirectObjectTarget::new(
        store.snapshot(),
        fixture.container,
        0,
        fixture.object_upper_bound,
        fixture.startxref,
    )
    .unwrap();
    let mut job = OpenObjectJob::new(
        target,
        context(),
        ObjectLimits::default(),
        SyntaxLimits::default(),
    )
    .unwrap();
    match job.poll(store, &NeverObjectCancelled) {
        ObjectPoll::Ready(object) => object,
        other => panic!("filtered fixture container must frame: {other:?}"),
    }
}

fn source_slice(store: &RangeStore, range: ByteRange) -> ByteSlice {
    match store.poll(ReadRequest::new(
        range,
        RequestPriority::VisiblePage,
        JobId::new(804),
        ResumeCheckpoint::new(805),
    )) {
        ReadPoll::Ready(bytes) => bytes,
        other => panic!("supplied encoded bytes must be ready: {other:?}"),
    }
}

fn stream(container: &IndirectObject) -> &pdf_rs_object::FramedStream {
    let IndirectObjectValue::Stream(stream) = container.value() else {
        panic!("fixture container must be a stream")
    };
    stream
}

fn plan(fixture: &Fixture) -> FilterPlan {
    if fixture.predictor {
        let parameters =
            PredictorParameters::new(12, 1, 8, i64::try_from(fixture.decoded.len()).unwrap())
                .unwrap();
        FilterPlan::from_stages(&[FilterStage::new(
            StreamFilter::FlateDecode,
            FilterDecodeParameters::Predictor(parameters),
        )
        .unwrap()])
        .unwrap()
    } else {
        FilterPlan::new(&[StreamFilter::FlateDecode]).unwrap()
    }
}

fn decode_proof(
    container: &IndirectObject,
    encoded: ByteSlice,
    owner: ObjectRef,
    dictionary_span: ByteSpan,
    plan: FilterPlan,
) -> DecodedStream {
    decode_proof_with_limits(
        container,
        encoded,
        owner,
        dictionary_span,
        plan,
        DecodeLimits::default(),
    )
}

fn decode_proof_with_limits(
    container: &IndirectObject,
    encoded: ByteSlice,
    owner: ObjectRef,
    dictionary_span: ByteSpan,
    plan: FilterPlan,
    limits: DecodeLimits,
) -> DecodedStream {
    let encoded_span = ByteSpan::new(encoded.range().start(), encoded.range().len()).unwrap();
    let request = DecodeRequest::new(
        container.snapshot(),
        owner,
        dictionary_span,
        encoded_span,
        encoded,
        plan,
        DecodeProfile::M1StrictV1,
        limits,
    )
    .unwrap();
    decode_stream(request, &NeverDecodeCancelled).unwrap()
}

fn valid_proof(store: &RangeStore, fixture: &Fixture, container: &IndirectObject) -> DecodedStream {
    decode_proof(
        container,
        source_slice(store, fixture.encoded_range),
        container.reference(),
        stream(container).dictionary().span(),
        plan(fixture),
    )
}

fn assert_decode_proof_mismatch(error: ObjectStreamError) {
    assert_eq!(error.code(), ObjectStreamErrorCode::DecodeProofMismatch);
    assert_eq!(error.category(), ObjectStreamErrorCategory::Internal);
    assert_eq!(error.recoverability(), ObjectRecoverability::DoNotRetry);
    assert_eq!(error.diagnostic_id(), "RPE-OBJECT-0112");
    assert!(error.source_offset().is_some());
    assert!(error.decoded_offset().is_none());
}

fn reject_with_plan(fixture: &Fixture, marker: u8, plan: FilterPlan) -> ObjectStreamError {
    let store = supplied_store(fixture, marker);
    let container = open_container(&store, fixture);
    let proof = decode_proof(
        &container,
        source_slice(&store, fixture.encoded_range),
        container.reference(),
        stream(&container).dictionary().span(),
        plan,
    );
    parse_filtered_object_stream(
        container,
        proof,
        ObjectStreamLimits::default(),
        &NeverObjectCancelled,
    )
    .unwrap_err()
}

#[test]
fn flate_result_retains_all_proofs_and_decoded_coordinates() {
    let fixture = valid_fixture(false);
    let store = supplied_store(&fixture, 0x71);
    let container = open_container(&store, &fixture);
    let proof = valid_proof(&store, &fixture, &container);
    let result = parse_filtered_object_stream(
        container,
        proof,
        ObjectStreamLimits::default(),
        &NeverObjectCancelled,
    )
    .unwrap();

    assert_eq!(result.framed_container().snapshot(), store.snapshot());
    assert_eq!(result.framed_container().reference(), fixture.container);
    assert_eq!(result.decoded_proof().bytes(), fixture.decoded);
    assert_eq!(
        result.decoded_proof().attestation().encoded_span(),
        stream(result.framed_container()).data_span()
    );
    assert_eq!(result.object_stream().entries().len(), 2);
    assert_eq!(result.object_stream().entry(0).unwrap().object_number(), 10);
    assert_eq!(result.object_stream().entry(1).unwrap().object_number(), 11);
    assert_eq!(
        result
            .object_stream()
            .entry(0)
            .unwrap()
            .value()
            .span()
            .start(),
        result.object_stream().first_object_offset()
    );
    assert!(
        result
            .object_stream()
            .entry(1)
            .unwrap()
            .value()
            .span()
            .start()
            > result
                .object_stream()
                .entry(0)
                .unwrap()
                .value()
                .span()
                .start()
    );
    let attestation = result.decoded_proof().attestation();
    let semantic = result.object_stream().stats();
    assert_eq!(
        result.retained_proof_bytes(),
        result.framed_container().retained_heap_bytes()
            + attestation.peak_retained_capacity_bytes()
            + attestation.plan_retained_heap_bytes()
            + semantic.retained_entry_bytes()
            + semantic.retained_value_bytes()
    );
    assert!(!format!("{result:?}").contains("second"));
}

#[test]
fn flate_png_predictor_proof_is_preserved_without_object_layer_recanonicalization() {
    let fixture = valid_fixture(true);
    let store = supplied_store(&fixture, 0x72);
    let container = open_container(&store, &fixture);
    let proof = valid_proof(&store, &fixture, &container);
    let result = parse_filtered_object_stream(
        container,
        proof,
        ObjectStreamLimits::default(),
        &NeverObjectCancelled,
    )
    .unwrap();

    assert_eq!(result.decoded_proof().bytes(), fixture.decoded);
    let FilterDecodeParameters::Predictor(parameters) =
        result.decoded_proof().attestation().filter_plan().stages()[0].decode_parameters()
    else {
        panic!("predictor parameters must remain in the sealed proof")
    };
    assert_eq!(parameters.predictor(), 12);
    assert_eq!(parameters.columns(), fixture.decoded.len() as u32);
    assert_eq!(result.object_stream().entry(1).unwrap().object_number(), 11);
}

#[test]
fn null_and_empty_direct_decode_parameters_preserve_canonical_none() {
    for (index, metadata) in [
        " /Filter [/FlateDecode] /DecodeParms null",
        " /Filter /FlateDecode /DecodeParms <<>>",
    ]
    .into_iter()
    .enumerate()
    {
        let fixture = fixture_with_metadata(metadata, zlib_stored);
        let store = supplied_store(&fixture, 0x7a + u8::try_from(index).unwrap());
        let container = open_container(&store, &fixture);
        let proof = valid_proof(&store, &fixture, &container);
        let result = parse_filtered_object_stream(
            container,
            proof,
            ObjectStreamLimits::default(),
            &NeverObjectCancelled,
        )
        .unwrap();
        assert_eq!(result.decoded_proof().bytes(), fixture.decoded);
        assert_eq!(
            result.decoded_proof().attestation().filter_plan().stages()[0].decode_parameters(),
            FilterDecodeParameters::None
        );
    }
}

#[test]
fn foreign_owner_dictionary_encoded_slice_and_empty_plan_never_authorize_publication() {
    let fixture = valid_fixture(false);
    let store = supplied_store(&fixture, 0x73);

    let container = open_container(&store, &fixture);
    let proof = decode_proof(
        &container,
        source_slice(&store, fixture.encoded_range),
        reference(6, 0),
        stream(&container).dictionary().span(),
        plan(&fixture),
    );
    assert_decode_proof_mismatch(
        parse_filtered_object_stream(
            container,
            proof,
            ObjectStreamLimits::default(),
            &NeverObjectCancelled,
        )
        .unwrap_err(),
    );

    let container = open_container(&store, &fixture);
    let proof = decode_proof(
        &container,
        source_slice(&store, fixture.encoded_range),
        container.reference(),
        ByteSpan::new(0, 1).unwrap(),
        plan(&fixture),
    );
    assert_decode_proof_mismatch(
        parse_filtered_object_stream(
            container,
            proof,
            ObjectStreamLimits::default(),
            &NeverObjectCancelled,
        )
        .unwrap_err(),
    );

    let container = open_container(&store, &fixture);
    let proof = decode_proof(
        &container,
        source_slice(&store, fixture.duplicate_encoded_range),
        container.reference(),
        stream(&container).dictionary().span(),
        plan(&fixture),
    );
    assert_decode_proof_mismatch(
        parse_filtered_object_stream(
            container,
            proof,
            ObjectStreamLimits::default(),
            &NeverObjectCancelled,
        )
        .unwrap_err(),
    );

    let container = open_container(&store, &fixture);
    let proof = decode_proof(
        &container,
        source_slice(&store, fixture.encoded_range),
        container.reference(),
        stream(&container).dictionary().span(),
        FilterPlan::new(&[]).unwrap(),
    );
    assert_decode_proof_mismatch(
        parse_filtered_object_stream(
            container,
            proof,
            ObjectStreamLimits::default(),
            &NeverObjectCancelled,
        )
        .unwrap_err(),
    );
}

#[test]
fn snapshot_filter_presence_and_generation_zero_are_authority_gates() {
    let canonical = valid_fixture(false);
    let container_store = supplied_store(&canonical, 0x74);
    let proof_store = supplied_store(&canonical, 0x75);
    let container = open_container(&container_store, &canonical);
    let proof_container = open_container(&proof_store, &canonical);
    let proof = valid_proof(&proof_store, &canonical, &proof_container);
    assert_decode_proof_mismatch(
        parse_filtered_object_stream(
            container,
            proof,
            ObjectStreamLimits::default(),
            &NeverObjectCancelled,
        )
        .unwrap_err(),
    );

    let absent = fixture(b"42", b"(second)", false, false, 0);
    let store = supplied_store(&absent, 0x76);
    let container = open_container(&store, &absent);
    let proof = valid_proof(&store, &absent, &container);
    assert_decode_proof_mismatch(
        parse_filtered_object_stream(
            container,
            proof,
            ObjectStreamLimits::default(),
            &NeverObjectCancelled,
        )
        .unwrap_err(),
    );

    let generated = fixture(b"42", b"(second)", true, false, 1);
    let store = supplied_store(&generated, 0x77);
    let container = open_container(&store, &generated);
    let proof = valid_proof(&store, &generated, &container);
    let error = parse_filtered_object_stream(
        container,
        proof,
        ObjectStreamLimits::default(),
        &NeverObjectCancelled,
    )
    .unwrap_err();
    assert_eq!(error.code(), ObjectStreamErrorCode::InvalidDictionary);
}

#[test]
fn dictionary_filter_names_shapes_and_order_must_match_the_attested_plan() {
    let flate = || FilterPlan::new(&[StreamFilter::FlateDecode]).unwrap();

    let wrong_supported = fixture_with_metadata(" /Filter /ASCIIHexDecode", zlib_stored);
    assert_decode_proof_mismatch(reject_with_plan(&wrong_supported, 0x81, flate()));

    let wrong_order = fixture_with_metadata(" /Filter [/ASCIIHexDecode /FlateDecode]", |decoded| {
        zlib_stored(&ascii_hex(decoded))
    });
    assert_decode_proof_mismatch(reject_with_plan(
        &wrong_order,
        0x82,
        FilterPlan::new(&[StreamFilter::FlateDecode, StreamFilter::AsciiHexDecode]).unwrap(),
    ));

    let wrong_arity = fixture_with_metadata(" /Filter [/ASCIIHexDecode /FlateDecode]", zlib_stored);
    assert_decode_proof_mismatch(reject_with_plan(&wrong_arity, 0x83, flate()));

    for (index, metadata) in [
        " /Filter 7",
        " /Filter []",
        " /Filter [/FlateDecode 7]",
        " /Filter /FlateDecode /Filter /FlateDecode",
        " /Filter /FlateDecode /DecodeParms [null]",
        " /Filter /FlateDecode /DecodeParms null /DecodeParms null",
        " /Filter /FlateDecode /DecodeParms << /Predictor 2 /Predictor 12 >>",
    ]
    .into_iter()
    .enumerate()
    {
        let fixture = fixture_with_metadata(metadata, zlib_stored);
        let error = reject_with_plan(&fixture, 0x84 + u8::try_from(index).unwrap(), flate());
        assert_eq!(error.code(), ObjectStreamErrorCode::InvalidDictionary);
        assert_eq!(error.category(), ObjectStreamErrorCategory::Syntax);
    }

    let unsupported = fixture_with_metadata(" /Filter /LZWDecode", zlib_stored);
    let error = reject_with_plan(&unsupported, 0x8b, flate());
    assert_eq!(error.code(), ObjectStreamErrorCode::UnsupportedFilter);
    assert_eq!(error.category(), ObjectStreamErrorCategory::Unsupported);
}

#[test]
fn decode_parameters_defaults_and_unfiltered_identity_cannot_forge_authority() {
    let predictor = valid_fixture(true);
    assert_decode_proof_mismatch(reject_with_plan(
        &predictor,
        0x8c,
        FilterPlan::new(&[StreamFilter::FlateDecode]).unwrap(),
    ));

    let explicit_defaults = fixture_with_metadata(
        " /Filter /FlateDecode /DecodeParms << /Predictor 1 >>",
        zlib_stored,
    );
    assert_decode_proof_mismatch(reject_with_plan(
        &explicit_defaults,
        0x8d,
        FilterPlan::new(&[StreamFilter::FlateDecode]).unwrap(),
    ));

    let unfiltered = fixture_with_metadata("", zlib_stored);
    assert_decode_proof_mismatch(reject_with_plan(
        &unfiltered,
        0x8e,
        FilterPlan::new(&[]).unwrap(),
    ));
}

#[test]
fn filtered_semantic_budgets_cancellation_and_failures_remain_decoded_relative() {
    let canonical = valid_fixture(false);
    let store = supplied_store(&canonical, 0x78);
    let container = open_container(&store, &canonical);
    let proof = valid_proof(&store, &canonical, &container);
    let decoded_limit = u64::try_from(canonical.decoded.len()).unwrap() - 1;
    let limits = ObjectStreamLimits::validate(ObjectStreamLimitConfig {
        max_decoded_bytes: decoded_limit,
        max_header_bytes: decoded_limit,
        ..ObjectStreamLimitConfig::default()
    })
    .unwrap();
    let error =
        parse_filtered_object_stream(container, proof, limits, &NeverObjectCancelled).unwrap_err();
    assert_eq!(error.code(), ObjectStreamErrorCode::ResourceLimit);
    assert_eq!(
        error.limit().unwrap().kind(),
        ObjectStreamLimitKind::DecodedBytes
    );
    assert_eq!(error.decoded_offset(), Some(0));
    assert!(error.source_offset().is_none());

    let filter_limited =
        fixture_with_metadata(" /Filter [/ASCIIHexDecode /FlateDecode]", zlib_stored);
    let filter_store = supplied_store(&filter_limited, 0x8f);
    let container = open_container(&filter_store, &filter_limited);
    let decode_limits = DecodeLimits::validate(DecodeLimitConfig {
        max_filters: 1,
        ..DecodeLimitConfig::default()
    })
    .unwrap();
    let proof = decode_proof_with_limits(
        &container,
        source_slice(&filter_store, filter_limited.encoded_range),
        container.reference(),
        stream(&container).dictionary().span(),
        FilterPlan::new(&[StreamFilter::FlateDecode]).unwrap(),
        decode_limits,
    );
    let error = parse_filtered_object_stream(
        container,
        proof,
        ObjectStreamLimits::default(),
        &NeverObjectCancelled,
    )
    .unwrap_err();
    assert_eq!(error.code(), ObjectStreamErrorCode::ResourceLimit);
    let filter_limit = error.limit().unwrap();
    assert_eq!(filter_limit.kind(), ObjectStreamLimitKind::FilterCount);
    assert_eq!(filter_limit.limit(), 1);
    assert_eq!(filter_limit.attempted(), 2);

    let container = open_container(&store, &canonical);
    let proof = valid_proof(&store, &canonical, &container);
    let cancelled = AtomicBool::new(true);
    let error =
        parse_filtered_object_stream(container, proof, ObjectStreamLimits::default(), &cancelled)
            .unwrap_err();
    assert_eq!(error.code(), ObjectStreamErrorCode::Cancelled);

    struct CancelDuringMetadata(AtomicUsize);

    impl pdf_rs_object::ObjectCancellation for CancelDuringMetadata {
        fn is_cancelled(&self) -> bool {
            self.0.fetch_add(1, Ordering::Relaxed) >= 2
        }
    }

    let container = open_container(&store, &canonical);
    let proof = valid_proof(&store, &canonical, &container);
    let cancellation = CancelDuringMetadata(AtomicUsize::new(0));
    let error = parse_filtered_object_stream(
        container,
        proof,
        ObjectStreamLimits::default(),
        &cancellation,
    )
    .unwrap_err();
    assert_eq!(error.code(), ObjectStreamErrorCode::Cancelled);
    assert!(cancellation.0.load(Ordering::Relaxed) >= 3);

    let malformed = fixture(b"[  ", b"(second)", true, false, 0);
    let malformed_store = supplied_store(&malformed, 0x79);
    let container = open_container(&malformed_store, &malformed);
    let proof = valid_proof(&malformed_store, &malformed, &container);
    let error = parse_filtered_object_stream(
        container,
        proof,
        ObjectStreamLimits::default(),
        &NeverObjectCancelled,
    )
    .unwrap_err();
    assert!(matches!(
        error.code(),
        ObjectStreamErrorCode::SyntaxFailure | ObjectStreamErrorCode::InvalidEntryBoundary
    ));
    assert!(error.decoded_offset().is_some());
    assert!(error.source_offset().is_none());
}

fn zlib_stored(payload: &[u8]) -> Vec<u8> {
    assert!(!payload.is_empty());
    assert!(payload.len() <= usize::from(u16::MAX));
    let length = payload.len() as u16;
    let mut output = vec![0x78, 0x01, 0x01];
    output.extend_from_slice(&length.to_le_bytes());
    output.extend_from_slice(&(!length).to_le_bytes());
    output.extend_from_slice(payload);
    output.extend_from_slice(&adler32(payload).to_be_bytes());
    output
}

fn ascii_hex(bytes: &[u8]) -> Vec<u8> {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    let mut output = Vec::with_capacity(bytes.len() * 2 + 1);
    for byte in bytes {
        output.push(HEX[usize::from(byte >> 4)]);
        output.push(HEX[usize::from(byte & 0x0f)]);
    }
    output.push(b'>');
    output
}

fn adler32(bytes: &[u8]) -> u32 {
    let mut first = 1_u32;
    let mut second = 0_u32;
    for byte in bytes {
        first = (first + u32::from(*byte)) % 65_521;
        second = (second + first) % 65_521;
    }
    (second << 16) | first
}
