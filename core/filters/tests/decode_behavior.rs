use std::sync::atomic::{AtomicUsize, Ordering};

use pdf_rs_bytes::{
    ByteRange, ByteSlice, ByteSource, JobId, RangeResponse, RangeStore, ReadPoll, ReadRequest,
    RequestPriority, ResumeCheckpoint, SourceIdentity, SourceRevision, SourceSnapshot,
    SourceStableId, SourceValidator, SourceValidatorKind,
};
use pdf_rs_filters::{
    DecodeCancellation, DecodeErrorCategory, DecodeErrorCode, DecodeFuelScheduleVersion,
    DecodeLimitConfig, DecodeLimitKind, DecodeLimits, DecodeProfile, DecodeRecoverability,
    DecodeRequest, DecodedOffset, DecodedRange, FilterPlan, NeverCancelled, StreamFilter,
    decode_stream,
};
use pdf_rs_syntax::{ByteSpan, ObjectRef};

const ENCODED_START: u64 = 64;

struct Fixture {
    snapshot: SourceSnapshot,
    encoded_span: ByteSpan,
    slice: ByteSlice,
}

impl Fixture {
    fn new(bytes: &[u8]) -> Self {
        Self::with_identity(bytes, identity(1))
    }

    fn with_identity(bytes: &[u8], source_identity: SourceIdentity) -> Self {
        let len = u64::try_from(bytes.len()).expect("test bytes fit u64");
        let snapshot = SourceSnapshot::new(
            source_identity,
            Some(ENCODED_START + len),
            SourceValidator::new(SourceValidatorKind::FrozenResponse, [0x35; 32]),
        );
        let range = ByteRange::new(ENCODED_START, len).expect("fixtures are non-empty");
        let store = RangeStore::new(snapshot, Default::default()).expect("store limits validate");
        store
            .supply(
                RangeResponse::new(snapshot, range, bytes.to_vec())
                    .expect("response has exact physical geometry"),
            )
            .expect("fixture fits store limits");
        let slice = match store.poll(ReadRequest::new(
            range,
            RequestPriority::Metadata,
            JobId::new(71),
            ResumeCheckpoint::new(72),
        )) {
            ReadPoll::Ready(slice) => slice,
            _ => panic!("supplied fixture must be immediately readable"),
        };
        Self {
            snapshot,
            encoded_span: ByteSpan::new(ENCODED_START, len).unwrap(),
            slice,
        }
    }

    fn request(
        self,
        plan: FilterPlan,
        limits: DecodeLimits,
    ) -> Result<DecodeRequest, pdf_rs_filters::DecodeError> {
        DecodeRequest::new(
            self.snapshot,
            object_ref(9),
            ByteSpan::new(8, 24).unwrap(),
            self.encoded_span,
            self.slice,
            plan,
            DecodeProfile::M1StrictV1,
            limits,
        )
    }
}

fn identity(revision: u64) -> SourceIdentity {
    SourceIdentity::new(
        SourceStableId::new([0x24; 32]),
        SourceRevision::new(revision),
    )
}

fn object_ref(number: u32) -> ObjectRef {
    ObjectRef::new(number, 0).unwrap()
}

fn plan(filters: &[StreamFilter]) -> FilterPlan {
    FilterPlan::new(filters).unwrap()
}

fn configured(update: impl FnOnce(&mut DecodeLimitConfig)) -> DecodeLimits {
    let mut config = DecodeLimitConfig::default();
    update(&mut config);
    DecodeLimits::validate(config).expect("test limit profile is consistent")
}

fn decode(bytes: &[u8], filters: &[StreamFilter]) -> pdf_rs_filters::DecodedStream {
    let request = Fixture::new(bytes)
        .request(plan(filters), DecodeLimits::default())
        .unwrap();
    decode_stream(request, &NeverCancelled).unwrap()
}

#[test]
fn internal_identity_keeps_exact_physical_evidence_and_decoded_coordinates() {
    let fixture = Fixture::new(b"secret");
    let expected_slice = fixture.slice.clone();
    let snapshot = fixture.snapshot;
    let encoded_span = fixture.encoded_span;
    let decoded = decode_stream(
        fixture.request(plan(&[]), DecodeLimits::default()).unwrap(),
        &NeverCancelled,
    )
    .unwrap();

    assert_eq!(decoded.bytes(), b"secret");
    assert_eq!(decoded.len(), 6);
    assert_eq!(decoded.decoded_range().start(), DecodedOffset::new(0));
    assert_eq!(
        decoded.decoded_range().end_exclusive(),
        DecodedOffset::new(6)
    );
    assert_eq!(
        decoded.slice(DecodedRange::new(DecodedOffset::new(1), 3).unwrap()),
        Some(b"ecr".as_slice())
    );
    assert_eq!(
        decoded.slice(DecodedRange::new(DecodedOffset::new(5), 2).unwrap()),
        None
    );

    let evidence = decoded.attestation();
    assert_eq!(evidence.snapshot(), snapshot);
    assert_eq!(evidence.source_identity(), snapshot.identity());
    assert_eq!(evidence.owner(), object_ref(9));
    assert_eq!(evidence.dictionary_span(), ByteSpan::new(8, 24).unwrap());
    assert_eq!(evidence.encoded_span(), encoded_span);
    assert_eq!(evidence.encoded(), &expected_slice);
    assert!(evidence.filter_plan().is_empty());
    assert_eq!(evidence.profile(), DecodeProfile::M1StrictV1);
    assert_eq!(evidence.fuel_schedule(), DecodeFuelScheduleVersion::M1V1);
    assert_eq!(evidence.fuel_consumed(), 13);
    assert_eq!(evidence.cumulative_output_bytes(), 6);
    assert_eq!(evidence.decoded_length(), 6);
    assert!(evidence.peak_retained_capacity_bytes() >= 6);
    assert!(!format!("{decoded:?}").contains("secret"));
}

#[test]
fn canonical_name_plans_reject_unknown_alias_and_pdf_identity() {
    let canonical = FilterPlan::from_pdf_names(&[
        b"ASCIIHexDecode".as_slice(),
        b"ASCII85Decode".as_slice(),
        b"RunLengthDecode".as_slice(),
    ])
    .unwrap();
    assert_eq!(
        canonical.filters(),
        &[
            StreamFilter::AsciiHexDecode,
            StreamFilter::Ascii85Decode,
            StreamFilter::RunLengthDecode,
        ]
    );
    assert_eq!(
        FilterPlan::from_pdf_names(&[b"Identity".as_slice()])
            .unwrap_err()
            .code(),
        DecodeErrorCode::UnsupportedFilter
    );
    let alias = FilterPlan::from_pdf_names(&[b"AHx".as_slice()]).unwrap_err();
    assert_eq!(alias.code(), DecodeErrorCode::UnsupportedFilter);
    assert_eq!(alias.filter_index(), Some(0));
    let unknown =
        FilterPlan::from_pdf_names(&[b"ASCIIHexDecode".as_slice(), b"FlateDecode".as_slice()])
            .unwrap_err();
    assert_eq!(unknown.filter_index(), Some(1));
    assert_eq!(unknown.category(), DecodeErrorCategory::Unsupported);
    assert_eq!(
        unknown.recoverability(),
        DecodeRecoverability::ReportUnsupported
    );
}

#[test]
fn ascii_hex_handles_whitespace_odd_nibbles_and_strict_termination() {
    let decoded = decode(b"61\t6> \r\n", &[StreamFilter::AsciiHexDecode]);
    assert_eq!(decoded.bytes(), &[0x61, 0x60]);

    for (bytes, code) in [
        (b"61".as_slice(), DecodeErrorCode::MissingEndMarker),
        (b"6g>".as_slice(), DecodeErrorCode::InvalidAsciiHex),
        (b"61>x".as_slice(), DecodeErrorCode::TrailingData),
    ] {
        let error = decode_stream(
            Fixture::new(bytes)
                .request(
                    plan(&[StreamFilter::AsciiHexDecode]),
                    DecodeLimits::default(),
                )
                .unwrap(),
            &NeverCancelled,
        )
        .unwrap_err();
        assert_eq!(error.code(), code);
        assert_eq!(error.filter_index(), Some(0));
    }
}

#[test]
fn ascii85_handles_full_partial_and_zero_groups_strictly() {
    assert_eq!(
        decode(b"9jqo^~>", &[StreamFilter::Ascii85Decode]).bytes(),
        b"Man "
    );
    assert_eq!(
        decode(b"9jqo~>", &[StreamFilter::Ascii85Decode]).bytes(),
        b"Man"
    );
    assert_eq!(
        decode(b"z !!!!!~> \n", &[StreamFilter::Ascii85Decode]).bytes(),
        &[0; 8]
    );

    for (bytes, code) in [
        (b"9jqo^".as_slice(), DecodeErrorCode::MissingEndMarker),
        (b"!~>".as_slice(), DecodeErrorCode::InvalidAscii85),
        (b"!z~>".as_slice(), DecodeErrorCode::InvalidAscii85),
        (b"uuuuu~>".as_slice(), DecodeErrorCode::InvalidAscii85),
        (b"z~>x".as_slice(), DecodeErrorCode::TrailingData),
        (b"z~ >".as_slice(), DecodeErrorCode::InvalidAscii85),
    ] {
        let error = decode_stream(
            Fixture::new(bytes)
                .request(
                    plan(&[StreamFilter::Ascii85Decode]),
                    DecodeLimits::default(),
                )
                .unwrap(),
            &NeverCancelled,
        )
        .unwrap_err();
        assert_eq!(error.code(), code, "input={bytes:?}");
    }
}

#[test]
fn run_length_decodes_literal_and_repeat_runs_and_rejects_bad_framing() {
    assert_eq!(
        decode(
            &[2, b'A', b'B', b'C', 254, b'Z', 128],
            &[StreamFilter::RunLengthDecode]
        )
        .bytes(),
        b"ABCZZZ"
    );

    for (bytes, code) in [
        (&[2, b'A', b'B'][..], DecodeErrorCode::InvalidRunLength),
        (&[255][..], DecodeErrorCode::InvalidRunLength),
        (&[0, b'A'][..], DecodeErrorCode::MissingEndMarker),
        (&[128, 0][..], DecodeErrorCode::TrailingData),
    ] {
        let error = decode_stream(
            Fixture::new(bytes)
                .request(
                    plan(&[StreamFilter::RunLengthDecode]),
                    DecodeLimits::default(),
                )
                .unwrap(),
            &NeverCancelled,
        )
        .unwrap_err();
        assert_eq!(error.code(), code, "input={bytes:?}");
    }
}

#[test]
fn filter_chains_run_in_source_order_and_attest_cumulative_output() {
    let filters = [StreamFilter::AsciiHexDecode, StreamFilter::RunLengthDecode];
    let decoded = decode(b"0241424380>", &filters);
    assert_eq!(decoded.bytes(), b"ABC");
    assert_eq!(decoded.attestation().filter_plan().filters(), &filters);
    assert_eq!(decoded.attestation().cumulative_output_bytes(), 8);
    assert_eq!(decoded.attestation().decoded_length(), 3);
}

#[test]
fn request_rejects_source_change_and_non_exact_physical_geometry() {
    let fixture = Fixture::new(b"abc");
    let changed = SourceSnapshot::new(
        identity(2),
        fixture.snapshot.len(),
        SourceValidator::new(SourceValidatorKind::FrozenResponse, [0x77; 32]),
    );
    let source_error = DecodeRequest::new(
        changed,
        object_ref(1),
        ByteSpan::new(0, 2).unwrap(),
        fixture.encoded_span,
        fixture.slice,
        plan(&[]),
        DecodeProfile::M1StrictV1,
        DecodeLimits::default(),
    )
    .unwrap_err();
    assert_eq!(source_error.code(), DecodeErrorCode::SourceChanged);
    assert_eq!(source_error.category(), DecodeErrorCategory::Integrity);
    assert_eq!(
        source_error.recoverability(),
        DecodeRecoverability::ReopenSource
    );

    let fixture = Fixture::new(b"abc");
    let geometry_error = DecodeRequest::new(
        fixture.snapshot,
        object_ref(1),
        ByteSpan::new(0, 2).unwrap(),
        ByteSpan::new(ENCODED_START + 1, 3).unwrap(),
        fixture.slice,
        plan(&[]),
        DecodeProfile::M1StrictV1,
        DecodeLimits::default(),
    )
    .unwrap_err();
    assert_eq!(geometry_error.code(), DecodeErrorCode::InvalidRequest);

    let fixture = Fixture::new(b"abc");
    let outside_dictionary = DecodeRequest::new(
        fixture.snapshot,
        object_ref(1),
        ByteSpan::new(fixture.snapshot.len().unwrap(), 1).unwrap(),
        fixture.encoded_span,
        fixture.slice,
        plan(&[]),
        DecodeProfile::M1StrictV1,
        DecodeLimits::default(),
    )
    .unwrap_err();
    assert_eq!(outside_dictionary.code(), DecodeErrorCode::InvalidRequest);
}

#[test]
fn all_decode_budget_dimensions_fail_with_structured_context() {
    let cases = [
        (
            b"abc".as_slice(),
            plan(&[]),
            configured(|config| config.max_input_bytes = 2),
            DecodeLimitKind::InputBytes,
        ),
        (
            b"00>".as_slice(),
            plan(&[StreamFilter::AsciiHexDecode, StreamFilter::RunLengthDecode]),
            configured(|config| config.max_filters = 1),
            DecodeLimitKind::FilterCount,
        ),
        (
            &[254, b'A', 128][..],
            plan(&[StreamFilter::RunLengthDecode]),
            configured(|config| {
                config.max_layer_output_bytes = 2;
                config.max_final_output_bytes = 2;
            }),
            DecodeLimitKind::LayerOutputBytes,
        ),
        (
            b"0241424380>".as_slice(),
            plan(&[StreamFilter::AsciiHexDecode, StreamFilter::RunLengthDecode]),
            configured(|config| {
                config.max_total_output_bytes = 7;
                config.max_final_output_bytes = 3;
            }),
            DecodeLimitKind::TotalOutputBytes,
        ),
        (
            b"abc".as_slice(),
            plan(&[]),
            configured(|config| config.max_final_output_bytes = 2),
            DecodeLimitKind::FinalOutputBytes,
        ),
        (
            b"0241424380>".as_slice(),
            plan(&[StreamFilter::AsciiHexDecode, StreamFilter::RunLengthDecode]),
            configured(|config| {
                config.max_final_output_bytes = 3;
                config.max_retained_capacity_bytes = 8;
            }),
            DecodeLimitKind::RetainedCapacityBytes,
        ),
        (
            b"abc".as_slice(),
            plan(&[]),
            configured(|config| {
                config.max_fuel = 2;
                config.cancellation_check_interval_fuel = 1;
            }),
            DecodeLimitKind::Fuel,
        ),
    ];

    for (bytes, plan, limits, kind) in cases {
        let error = decode_stream(
            Fixture::new(bytes).request(plan, limits).unwrap(),
            &NeverCancelled,
        )
        .unwrap_err();
        assert_eq!(error.code(), DecodeErrorCode::ResourceLimit);
        let context = error.limit().expect("resource errors carry context");
        assert_eq!(context.kind(), kind);
        assert!(context.attempted() > context.limit());
        assert!(context.consumed() <= context.limit());
    }
}

#[test]
fn invalid_limit_profiles_are_rejected_before_decode() {
    let mut config = DecodeLimitConfig::default();
    config.max_retained_capacity_bytes = config.max_final_output_bytes - 1;
    let error = DecodeLimits::validate(config).unwrap_err();
    assert_eq!(error.code(), DecodeErrorCode::InvalidLimits);
    assert_eq!(error.diagnostic_id(), "RPE-FILTERS-0001");
}

struct CancelAfter {
    calls: AtomicUsize,
    allowed_false_calls: usize,
}

impl DecodeCancellation for CancelAfter {
    fn is_cancelled(&self) -> bool {
        self.calls.fetch_add(1, Ordering::SeqCst) >= self.allowed_false_calls
    }
}

#[test]
fn cancellation_is_distinct_and_probed_at_the_fuel_interval() {
    let cancellation = CancelAfter {
        calls: AtomicUsize::new(0),
        allowed_false_calls: 2,
    };
    let limits = configured(|config| config.cancellation_check_interval_fuel = 1);
    let error = decode_stream(
        Fixture::new(b"abcdef").request(plan(&[]), limits).unwrap(),
        &cancellation,
    )
    .unwrap_err();
    assert_eq!(error.code(), DecodeErrorCode::Cancelled);
    assert_eq!(error.category(), DecodeErrorCategory::Cancellation);
    assert_eq!(
        error.recoverability(),
        DecodeRecoverability::AbandonOperation
    );
    assert_eq!(cancellation.calls.load(Ordering::SeqCst), 3);

    let immediate = CancelAfter {
        calls: AtomicUsize::new(0),
        allowed_false_calls: 0,
    };
    let error = decode_stream(
        Fixture::new(b"x")
            .request(plan(&[]), DecodeLimits::default())
            .unwrap(),
        &immediate,
    )
    .unwrap_err();
    assert_eq!(error.code(), DecodeErrorCode::Cancelled);
    assert_eq!(immediate.calls.load(Ordering::SeqCst), 1);
}
