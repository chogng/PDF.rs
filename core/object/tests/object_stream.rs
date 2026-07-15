use std::sync::atomic::{AtomicBool, Ordering};

use pdf_rs_bytes::{
    ByteRange, ByteSlice, ByteSource, JobId, RangeResponse, RangeStore, ReadPoll, ReadRequest,
    RequestPriority, ResumeCheckpoint, SourceIdentity, SourceRevision, SourceSnapshot,
    SourceStableId, SourceValidator, SourceValidatorKind,
};
use pdf_rs_object::{
    DecodedObject, IndirectObject, IndirectObjectTarget, NeverCancelled, ObjectJobContext,
    ObjectLimits, ObjectPoll, ObjectStreamErrorCode, ObjectStreamLimitConfig,
    ObjectStreamLimitKind, ObjectStreamLimits, OpenObjectJob, parse_unfiltered_object_stream,
};
use pdf_rs_syntax::{ObjectRef, SyntaxLimitConfig, SyntaxLimitKind, SyntaxLimits};

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

fn reference(number: u32) -> ObjectRef {
    ObjectRef::new(number, 0).unwrap()
}

fn context() -> ObjectJobContext {
    ObjectJobContext::new(
        JobId::new(701),
        ResumeCheckpoint::new(702),
        ResumeCheckpoint::new(703),
        RequestPriority::VisiblePage,
    )
}

struct Fixture {
    bytes: Vec<u8>,
    payload: Vec<u8>,
    object_upper_bound: u64,
    startxref: u64,
}

fn fixture_with_dictionary(dictionary_suffix: &str, header: &[u8], body: &[u8]) -> Fixture {
    let mut payload = header.to_vec();
    payload.extend_from_slice(body);
    let first = header.len();
    let mut bytes = format!(
        "5 0 obj\n<< /Type /ObjStm /N 2 /First {first} /Length {}{dictionary_suffix} >>\nstream\n",
        payload.len()
    )
    .into_bytes();
    bytes.extend_from_slice(&payload);
    bytes.extend_from_slice(b"\nendstream\nendobj\n");
    let object_upper_bound = u64::try_from(bytes.len()).unwrap();
    let startxref = object_upper_bound;
    bytes.extend_from_slice(b"xref\n");
    Fixture {
        bytes,
        payload,
        object_upper_bound,
        startxref,
    }
}

fn valid_fixture() -> Fixture {
    let first_object = b"<< /A [1 2] /Ref 12 0 R >>";
    let second_object = b"(second)";
    let header = format!("10 0 11 {} ", first_object.len() + 1);
    let mut body = first_object.to_vec();
    body.push(b' ');
    body.extend_from_slice(second_object);
    fixture_with_dictionary("", header.as_bytes(), &body)
}

fn supplied_store(bytes: &[u8], marker: u8) -> RangeStore {
    let source = snapshot(u64::try_from(bytes.len()).unwrap(), marker);
    let store = RangeStore::new(source, Default::default()).unwrap();
    let range = ByteRange::new(0, u64::try_from(bytes.len()).unwrap()).unwrap();
    store
        .supply(RangeResponse::new(source, range, bytes.to_vec()).unwrap())
        .unwrap();
    store
}

fn open_container(store: &RangeStore, fixture: &Fixture) -> IndirectObject {
    let target = IndirectObjectTarget::new(
        store.snapshot(),
        reference(5),
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
    match job.poll(store, &NeverCancelled) {
        ObjectPoll::Ready(object) => object,
        other => panic!("fixture container must frame: {other:?}"),
    }
}

fn payload_slice(store: &RangeStore, container: &IndirectObject) -> ByteSlice {
    let pdf_rs_object::IndirectObjectValue::Stream(stream) = container.value() else {
        panic!("fixture must frame as a stream")
    };
    let request = ReadRequest::new(
        ByteRange::new(stream.data_span().start(), stream.data_span().len()).unwrap(),
        RequestPriority::VisiblePage,
        JobId::new(704),
        ResumeCheckpoint::new(705),
    );
    match store.poll(request) {
        ReadPoll::Ready(bytes) => bytes,
        other => panic!("supplied payload must be ready: {other:?}"),
    }
}

fn minimum_passing_limit(
    container: &IndirectObject,
    payload: &ByteSlice,
    base: ObjectStreamLimitConfig,
    upper: u64,
    set_limit: fn(&mut ObjectStreamLimitConfig, u64),
) -> u64 {
    let mut low = 1_u64;
    let mut high = upper;
    while low < high {
        let middle = low + (high - low) / 2;
        let mut config = base;
        set_limit(&mut config, middle);
        let limits = ObjectStreamLimits::validate(config).unwrap();
        if parse_unfiltered_object_stream(container, payload, limits, &NeverCancelled).is_ok() {
            high = middle;
        } else {
            low = middle + 1;
        }
    }
    low
}

fn assert_one_less_limit(
    container: &IndirectObject,
    payload: &ByteSlice,
    base: ObjectStreamLimitConfig,
    minimum: u64,
    kind: ObjectStreamLimitKind,
    set_limit: fn(&mut ObjectStreamLimitConfig, u64),
) {
    assert!(minimum > 1);
    let mut exact_config = base;
    set_limit(&mut exact_config, minimum);
    let exact = ObjectStreamLimits::validate(exact_config).unwrap();
    let exact_result = parse_unfiltered_object_stream(container, payload, exact, &NeverCancelled);
    assert!(
        exact_result.is_ok(),
        "{kind:?} exact limit {minimum} failed: {exact_result:?}"
    );

    let mut one_less_config = base;
    set_limit(&mut one_less_config, minimum - 1);
    let one_less = ObjectStreamLimits::validate(one_less_config).unwrap();
    let error = parse_unfiltered_object_stream(container, payload, one_less, &NeverCancelled)
        .expect_err("one-less limit must not publish a partial object stream");
    assert_eq!(error.code(), ObjectStreamErrorCode::ResourceLimit);
    assert_eq!(error.limit().unwrap().kind(), kind);
}

#[test]
fn parses_source_bound_entries_without_fabricating_physical_spans() {
    let fixture = valid_fixture();
    let store = supplied_store(&fixture.bytes, 0x51);
    let container = open_container(&store, &fixture);
    let payload = payload_slice(&store, &container);

    let stream = parse_unfiltered_object_stream(
        &container,
        &payload,
        ObjectStreamLimits::default(),
        &NeverCancelled,
    )
    .unwrap();

    assert_eq!(stream.snapshot(), store.snapshot());
    assert_eq!(stream.container(), reference(5));
    assert_eq!(
        stream.encoded_payload_span().len(),
        fixture.payload.len() as u64
    );
    assert_eq!(stream.entries().len(), 2);
    assert_eq!(stream.entry(0).unwrap().object_number(), 10);
    assert_eq!(stream.entry(1).unwrap().object_number(), 11);
    assert_eq!(
        stream.entry(0).unwrap().value().span().start(),
        stream.first_object_offset()
    );
    assert_eq!(
        stream.entry(1).unwrap().value().span().start(),
        u64::try_from(
            fixture
                .payload
                .windows(b"(second)".len())
                .position(|window| window == b"(second)")
                .unwrap()
        )
        .unwrap()
    );
    let DecodedObject::Dictionary(dictionary) = stream.entry(0).unwrap().value().value() else {
        panic!("first embedded value must be a decoded-coordinate dictionary")
    };
    let DecodedObject::Array(array) = dictionary.get(b"A").unwrap().value() else {
        panic!("A must remain an array")
    };
    assert_eq!(array.values().len(), 2);
    assert_eq!(array.values()[0].value().as_integer(), Some(1));
    assert_eq!(
        dictionary.get(b"Ref").unwrap().value().as_reference(),
        Some(reference(12))
    );
    assert_eq!(stream.stats().objects(), 2);
    assert_eq!(stream.stats().decoded_bytes(), fixture.payload.len() as u64);
    assert!(stream.stats().retained_entry_bytes() > 0);
    assert!(stream.stats().retained_value_bytes() > 0);
}

#[test]
fn duplicate_numbers_and_nonincreasing_offsets_are_rejected() {
    let duplicate = fixture_with_dictionary("", b"10 0 10 3 ", b"42 (x)");
    let store = supplied_store(&duplicate.bytes, 0x52);
    let container = open_container(&store, &duplicate);
    let payload = payload_slice(&store, &container);
    assert_eq!(
        parse_unfiltered_object_stream(
            &container,
            &payload,
            ObjectStreamLimits::default(),
            &NeverCancelled,
        )
        .unwrap_err()
        .code(),
        ObjectStreamErrorCode::DuplicateObjectNumber
    );

    let reversed = fixture_with_dictionary("", b"10 3 11 0 ", b"42 (x)");
    let store = supplied_store(&reversed.bytes, 0x53);
    let container = open_container(&store, &reversed);
    let payload = payload_slice(&store, &container);
    assert_eq!(
        parse_unfiltered_object_stream(
            &container,
            &payload,
            ObjectStreamLimits::default(),
            &NeverCancelled,
        )
        .unwrap_err()
        .code(),
        ObjectStreamErrorCode::InvalidHeader
    );

    let nonzero_first = fixture_with_dictionary("", b"10 1 11 3 ", b" 42(x)");
    let store = supplied_store(&nonzero_first.bytes, 0x5a);
    let container = open_container(&store, &nonzero_first);
    let payload = payload_slice(&store, &container);
    assert_eq!(
        parse_unfiltered_object_stream(
            &container,
            &payload,
            ObjectStreamLimits::default(),
            &NeverCancelled,
        )
        .unwrap_err()
        .code(),
        ObjectStreamErrorCode::InvalidHeader
    );
}

#[test]
fn extends_is_validated_and_retained_without_affecting_lookup_order() {
    let fixture = fixture_with_dictionary(" /Extends 7 0 R", b"10 0 11 3 ", b"42 (x)");
    let store = supplied_store(&fixture.bytes, 0x5b);
    let container = open_container(&store, &fixture);
    let payload = payload_slice(&store, &container);
    let stream = parse_unfiltered_object_stream(
        &container,
        &payload,
        ObjectStreamLimits::default(),
        &NeverCancelled,
    )
    .unwrap();
    assert_eq!(stream.extends(), Some(reference(7)));

    let invalid = fixture_with_dictionary(" /Extends 7 1 R", b"10 0 11 3 ", b"42 (x)");
    let store = supplied_store(&invalid.bytes, 0x5c);
    let container = open_container(&store, &invalid);
    let payload = payload_slice(&store, &container);
    assert_eq!(
        parse_unfiltered_object_stream(
            &container,
            &payload,
            ObjectStreamLimits::default(),
            &NeverCancelled,
        )
        .unwrap_err()
        .code(),
        ObjectStreamErrorCode::InvalidDictionary
    );

    let self_extends = fixture_with_dictionary(" /Extends 5 0 R", b"10 0 11 3 ", b"42 (x)");
    let store = supplied_store(&self_extends.bytes, 0x5d);
    let container = open_container(&store, &self_extends);
    let payload = payload_slice(&store, &container);
    assert_eq!(
        parse_unfiltered_object_stream(
            &container,
            &payload,
            ObjectStreamLimits::default(),
            &NeverCancelled,
        )
        .unwrap_err()
        .code(),
        ObjectStreamErrorCode::InvalidDictionary
    );
}

#[test]
fn future_header_extension_bytes_are_retained_but_not_interpreted() {
    let fixture = fixture_with_dictionary("", b"10 0 11 3 \x01EXT", b"42 (x)");
    let store = supplied_store(&fixture.bytes, 0x5e);
    let container = open_container(&store, &fixture);
    let payload = payload_slice(&store, &container);
    let stream = parse_unfiltered_object_stream(
        &container,
        &payload,
        ObjectStreamLimits::default(),
        &NeverCancelled,
    )
    .unwrap();
    assert!(!stream.header_extension_span().is_empty());
    assert!(stream.header_extension_span().end_exclusive() <= stream.first_object_offset());
    assert_eq!(stream.entry(0).unwrap().object_number(), 10);
    assert_eq!(stream.entry(1).unwrap().object_number(), 11);
}

#[test]
fn top_level_reference_member_and_decoded_syntax_failure_are_rejected() {
    let reference_member = fixture_with_dictionary("", b"10 0 11 7 ", b"12 0 R (x)");
    let store = supplied_store(&reference_member.bytes, 0x5f);
    let container = open_container(&store, &reference_member);
    let payload = payload_slice(&store, &container);
    assert_eq!(
        parse_unfiltered_object_stream(
            &container,
            &payload,
            ObjectStreamLimits::default(),
            &NeverCancelled,
        )
        .unwrap_err()
        .code(),
        ObjectStreamErrorCode::SyntaxFailure
    );

    let malformed = fixture_with_dictionary("", b"10 0 11 6 ", b"[  (x)");
    let store = supplied_store(&malformed.bytes, 0x60);
    let container = open_container(&store, &malformed);
    let payload = payload_slice(&store, &container);
    let error = parse_unfiltered_object_stream(
        &container,
        &payload,
        ObjectStreamLimits::default(),
        &NeverCancelled,
    )
    .unwrap_err();
    assert_eq!(error.code(), ObjectStreamErrorCode::SyntaxFailure);
    assert!(error.decoded_offset().is_some());
    assert!(error.source_offset().is_none());
    assert!(error.syntax_code().is_some());
}

#[test]
fn entry_slots_must_contain_exactly_one_direct_object_plus_trivia() {
    let extra = fixture_with_dictionary("", b"10 0 11 5 ", b"1 2  (x)");
    let store = supplied_store(&extra.bytes, 0x54);
    let container = open_container(&store, &extra);
    let payload = payload_slice(&store, &container);
    assert_eq!(
        parse_unfiltered_object_stream(
            &container,
            &payload,
            ObjectStreamLimits::default(),
            &NeverCancelled,
        )
        .unwrap_err()
        .code(),
        ObjectStreamErrorCode::InvalidEntryBoundary
    );

    let stream_like = fixture_with_dictionary("", b"10 0 11 21 ", b"<< /Length 0 >>stream (x)");
    let store = supplied_store(&stream_like.bytes, 0x55);
    let container = open_container(&store, &stream_like);
    let payload = payload_slice(&store, &container);
    assert_eq!(
        parse_unfiltered_object_stream(
            &container,
            &payload,
            ObjectStreamLimits::default(),
            &NeverCancelled,
        )
        .unwrap_err()
        .code(),
        ObjectStreamErrorCode::InvalidEntryBoundary
    );
}

#[test]
fn filtered_dictionary_and_foreign_source_slice_are_rejected() {
    let filtered = fixture_with_dictionary(" /Filter /FlateDecode", b"10 0 11 3 ", b"42 (x)");
    let store = supplied_store(&filtered.bytes, 0x56);
    let container = open_container(&store, &filtered);
    let payload = payload_slice(&store, &container);
    assert_eq!(
        parse_unfiltered_object_stream(
            &container,
            &payload,
            ObjectStreamLimits::default(),
            &NeverCancelled,
        )
        .unwrap_err()
        .code(),
        ObjectStreamErrorCode::UnsupportedFilter
    );

    let valid = valid_fixture();
    let store = supplied_store(&valid.bytes, 0x57);
    let container = open_container(&store, &valid);
    let foreign_store = supplied_store(&valid.bytes, 0x58);
    let foreign = payload_slice(&foreign_store, &container);
    assert_eq!(
        parse_unfiltered_object_stream(
            &container,
            &foreign,
            ObjectStreamLimits::default(),
            &NeverCancelled,
        )
        .unwrap_err()
        .code(),
        ObjectStreamErrorCode::SourceMismatch
    );
}

#[test]
fn work_limits_and_cancellation_are_terminal_before_publication() {
    let fixture = valid_fixture();
    let store = supplied_store(&fixture.bytes, 0x59);
    let container = open_container(&store, &fixture);
    let payload = payload_slice(&store, &container);
    let limits = ObjectStreamLimits::validate(ObjectStreamLimitConfig {
        max_objects: 1,
        ..ObjectStreamLimitConfig::default()
    })
    .unwrap();
    let error =
        parse_unfiltered_object_stream(&container, &payload, limits, &NeverCancelled).unwrap_err();
    assert_eq!(error.code(), ObjectStreamErrorCode::ResourceLimit);
    assert_eq!(
        error.limit().unwrap().kind(),
        ObjectStreamLimitKind::Objects
    );

    let cancelled = AtomicBool::new(true);
    let error = parse_unfiltered_object_stream(
        &container,
        &payload,
        ObjectStreamLimits::default(),
        &cancelled,
    )
    .unwrap_err();
    assert_eq!(error.code(), ObjectStreamErrorCode::Cancelled);
    cancelled.store(false, Ordering::Release);

    let syntax = SyntaxLimits::validate(SyntaxLimitConfig {
        max_container_bytes: 1,
        ..SyntaxLimitConfig::default()
    })
    .unwrap();
    let child_limits = ObjectStreamLimits::validate(ObjectStreamLimitConfig {
        syntax,
        ..ObjectStreamLimitConfig::default()
    })
    .unwrap();
    let error = parse_unfiltered_object_stream(&container, &payload, child_limits, &NeverCancelled)
        .expect_err("child syntax exhaustion must not publish an object stream");
    assert_eq!(error.code(), ObjectStreamErrorCode::ResourceLimit);
    assert_eq!(
        error.syntax_code(),
        Some(pdf_rs_syntax::SyntaxErrorCode::ResourceLimit)
    );
    let syntax_limit = error
        .syntax_limit()
        .expect("lower syntax resource evidence must survive mapping");
    assert_eq!(syntax_limit.kind(), SyntaxLimitKind::ContainerBytes);
    assert_eq!(syntax_limit.limit(), 1);
    assert_eq!(syntax_limit.consumed(), 0);
    assert!(syntax_limit.attempted() > syntax_limit.limit());
    assert!(error.decoded_offset().is_some());
    assert!(error.source_offset().is_none());
}

#[test]
fn working_retained_and_total_syntax_limits_have_exact_one_less_boundaries() {
    let fixture = valid_fixture();
    let store = supplied_store(&fixture.bytes, 0x67);
    let container = open_container(&store, &fixture);
    let payload = payload_slice(&store, &container);
    let default = ObjectStreamLimitConfig::default();

    let working = minimum_passing_limit(
        &container,
        &payload,
        default,
        default.max_working_bytes,
        |config, value| config.max_working_bytes = value,
    );
    assert_one_less_limit(
        &container,
        &payload,
        default,
        working,
        ObjectStreamLimitKind::WorkingBytes,
        |config, value| config.max_working_bytes = value,
    );

    let baseline = parse_unfiltered_object_stream(
        &container,
        &payload,
        ObjectStreamLimits::default(),
        &NeverCancelled,
    )
    .unwrap();
    assert_one_less_limit(
        &container,
        &payload,
        default,
        baseline.stats().retained_entry_bytes(),
        ObjectStreamLimitKind::RetainedEntries,
        |config, value| config.max_retained_entry_bytes = value,
    );
    assert_one_less_limit(
        &container,
        &payload,
        default,
        baseline.stats().syntax_input_bytes(),
        ObjectStreamLimitKind::TotalSyntaxBytes,
        |config, value| config.max_total_syntax_bytes = value,
    );

    let syntax = SyntaxLimits::validate(SyntaxLimitConfig {
        max_name_bytes: 64,
        max_string_decoded_bytes: 64,
        max_owned_bytes: 64,
        max_container_bytes: 16 * 1_024,
        ..SyntaxLimitConfig::default()
    })
    .unwrap();
    let retained_config = ObjectStreamLimitConfig { syntax, ..default };
    let retained = minimum_passing_limit(
        &container,
        &payload,
        retained_config,
        retained_config.max_retained_value_bytes,
        |config, value| config.max_retained_value_bytes = value,
    );
    assert_one_less_limit(
        &container,
        &payload,
        retained_config,
        retained,
        ObjectStreamLimitKind::RetainedValues,
        |config, value| config.max_retained_value_bytes = value,
    );
}
