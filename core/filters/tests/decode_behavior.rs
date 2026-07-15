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
        b"FlateDecode".as_slice(),
        b"ASCIIHexDecode".as_slice(),
        b"ASCII85Decode".as_slice(),
        b"RunLengthDecode".as_slice(),
    ])
    .unwrap();
    assert_eq!(
        canonical.filters(),
        &[
            StreamFilter::FlateDecode,
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
        FilterPlan::from_pdf_names(&[b"ASCIIHexDecode".as_slice(), b"LZWDecode".as_slice()])
            .unwrap_err();
    assert_eq!(unknown.filter_index(), Some(1));
    assert_eq!(unknown.category(), DecodeErrorCategory::Unsupported);
    assert_eq!(
        unknown.recoverability(),
        DecodeRecoverability::ReportUnsupported
    );
}

#[test]
fn flate_decodes_stored_fixed_and_dynamic_huffman_blocks() {
    let stored = hex_bytes("7801010c00f3ff73746f72656420626c6f636b1f8004bd");
    assert_eq!(
        decode(&stored, &[StreamFilter::FlateDecode]).bytes(),
        b"stored block"
    );

    let fixed = hex_bytes(
        "78014bcbac484d51f0284d4bcb4dcc53484e2c2aca4c2d56284a2d484d2c014a6032922a4b528b012fc11484",
    );
    assert_eq!(
        decode(&fixed, &[StreamFilter::FlateDecode]).bytes(),
        b"fixed Huffman carries repeated repeated repeated bytes"
    );

    let dynamic = hex_bytes(
        "78daedcbc111c0101000c05a2f1139fa2f00430b7ebbff8d389ee3ddcaf62d75f9a7cc6cad87a2288aa2288aa2288aa2288aa2288aa2288aa2288aa2288aa2288aa22877ca009ff2952a",
    );
    let expected = b"aaaaaaaaabbbbbbbbcccccccdddddddeeeeefffffgggghhhiij".repeat(200);
    assert_eq!(
        decode(&dynamic, &[StreamFilter::FlateDecode]).bytes(),
        expected
    );
}

#[test]
fn flate_supports_the_full_32k_window_and_enforces_header_window_size() {
    let full_window = fixed_distance_stream(32_768, 29, 8_191, 7);
    let decoded = decode(&full_window, &[StreamFilter::FlateDecode]);
    assert_eq!(decoded.len(), 32_771);
    assert!(decoded.bytes().iter().all(|byte| *byte == b'A'));

    let too_far_for_header = fixed_distance_stream(257, 16, 0, 0);
    let error = decode_stream(
        Fixture::new(&too_far_for_header)
            .request(plan(&[StreamFilter::FlateDecode]), DecodeLimits::default())
            .unwrap(),
        &NeverCancelled,
    )
    .unwrap_err();
    assert_eq!(error.code(), DecodeErrorCode::InvalidFlate);
}

#[test]
fn flate_rejects_bad_headers_dictionary_adler_trailing_and_framing() {
    let valid = hex_bytes("7801010c00f3ff73746f72656420626c6f636b1f8004bd");
    let mut bad_check = valid.clone();
    bad_check[1] ^= 1;
    let mut preset_dictionary = valid.clone();
    preset_dictionary[..2].copy_from_slice(&[0x78, 0x20]);
    let mut bad_adler = valid.clone();
    *bad_adler.last_mut().unwrap() ^= 1;
    let mut trailing = valid.clone();
    trailing.push(0);
    let mut bad_stored_complement = valid.clone();
    bad_stored_complement[5] ^= 1;
    let mut truncated = valid.clone();
    truncated.pop();
    let reserved_block_type = [0x78, 0x01, 0x07];

    for bytes in [
        bad_check.as_slice(),
        bad_adler.as_slice(),
        bad_stored_complement.as_slice(),
        truncated.as_slice(),
        reserved_block_type.as_slice(),
    ] {
        let error = decode_stream(
            Fixture::new(bytes)
                .request(plan(&[StreamFilter::FlateDecode]), DecodeLimits::default())
                .unwrap(),
            &NeverCancelled,
        )
        .unwrap_err();
        assert_eq!(error.code(), DecodeErrorCode::InvalidFlate, "{bytes:02x?}");
        assert_eq!(error.filter_index(), Some(0));
    }

    let error = decode_stream(
        Fixture::new(&preset_dictionary)
            .request(plan(&[StreamFilter::FlateDecode]), DecodeLimits::default())
            .unwrap(),
        &NeverCancelled,
    )
    .unwrap_err();
    assert_eq!(error.code(), DecodeErrorCode::UnsupportedFlateDictionary);
    assert_eq!(error.category(), DecodeErrorCategory::Unsupported);
    assert_eq!(
        error.recoverability(),
        DecodeRecoverability::ReportUnsupported
    );

    let error = decode_stream(
        Fixture::new(&trailing)
            .request(plan(&[StreamFilter::FlateDecode]), DecodeLimits::default())
            .unwrap(),
        &NeverCancelled,
    )
    .unwrap_err();
    assert_eq!(error.code(), DecodeErrorCode::TrailingData);
}

#[test]
fn flate_expansion_and_huffman_work_obey_limits_and_cancellation() {
    let compressed = hex_bytes(
        "78daedcbc111c0101000c05a2f1139fa2f00430b7ebbff8d389ee3ddcaf62d75f9a7cc6cad87a2288aa2288aa2288aa2288aa2288aa2288aa2288aa2288aa2288aa22877ca009ff2952a",
    );
    let limits = configured(|config| {
        config.max_layer_output_bytes = 64;
        config.max_final_output_bytes = 64;
    });
    let error = decode_stream(
        Fixture::new(&compressed)
            .request(plan(&[StreamFilter::FlateDecode]), limits)
            .unwrap(),
        &NeverCancelled,
    )
    .unwrap_err();
    assert_eq!(error.code(), DecodeErrorCode::ResourceLimit);
    assert_eq!(
        error.limit().unwrap().kind(),
        DecodeLimitKind::LayerOutputBytes
    );

    let fixed = hex_bytes(
        "78014bcbac484d51f0284d4bcb4dcc53484e2c2aca4c2d56284a2d484d2c014a6032922a4b528b012fc11484",
    );
    let limits = configured(|config| {
        config.max_fuel = 10;
        config.cancellation_check_interval_fuel = 1;
    });
    let error = decode_stream(
        Fixture::new(&fixed)
            .request(plan(&[StreamFilter::FlateDecode]), limits)
            .unwrap(),
        &NeverCancelled,
    )
    .unwrap_err();
    assert_eq!(error.code(), DecodeErrorCode::ResourceLimit);
    let fuel = error.limit().unwrap();
    assert_eq!(fuel.kind(), DecodeLimitKind::Fuel);
    assert_eq!(fuel.attempted() - fuel.consumed(), 1);
    assert_eq!(error.filter_index(), Some(0));

    let cancellation = CancelAfter {
        calls: AtomicUsize::new(0),
        allowed_false_calls: 328,
    };
    let limits = configured(|config| config.cancellation_check_interval_fuel = 1);
    let error = decode_stream(
        Fixture::new(&fixed)
            .request(plan(&[StreamFilter::FlateDecode]), limits)
            .unwrap(),
        &cancellation,
    )
    .unwrap_err();
    assert_eq!(error.code(), DecodeErrorCode::Cancelled);
    assert_eq!(cancellation.calls.load(Ordering::SeqCst), 329);
}

fn hex_bytes(hex: &str) -> Vec<u8> {
    assert_eq!(hex.len() % 2, 0);
    hex.as_bytes()
        .chunks_exact(2)
        .map(|pair| {
            let text = std::str::from_utf8(pair).unwrap();
            u8::from_str_radix(text, 16).unwrap()
        })
        .collect()
}

fn fixed_distance_stream(
    literal_count: usize,
    distance_symbol: u16,
    distance_extra: u32,
    window_code: u8,
) -> Vec<u8> {
    let mut bits = DeflateBits::default();
    bits.write_bits(1, 1);
    bits.write_bits(1, 2);
    for _ in 0..literal_count {
        bits.write_fixed_symbol(u16::from(b'A'));
    }
    bits.write_fixed_symbol(257);
    bits.write_huffman(distance_symbol, 5);
    let extra_bits = if distance_symbol < 4 {
        0
    } else {
        u8::try_from((distance_symbol / 2) - 1).unwrap()
    };
    bits.write_bits(distance_extra, extra_bits);
    bits.write_fixed_symbol(256);

    let output = vec![b'A'; literal_count + 3];
    let mut encoded = zlib_header(window_code).to_vec();
    encoded.extend(bits.finish());
    encoded.extend(adler32(&output).to_be_bytes());
    encoded
}

fn zlib_header(window_code: u8) -> [u8; 2] {
    let cmf = (window_code << 4) | 8;
    let flg = (0_u8..=31)
        .find(|candidate| ((u16::from(cmf) << 8) | u16::from(*candidate)) % 31 == 0)
        .unwrap();
    [cmf, flg]
}

fn adler32(bytes: &[u8]) -> u32 {
    let mut a = 1_u32;
    let mut b = 0_u32;
    for byte in bytes {
        a = (a + u32::from(*byte)) % 65_521;
        b = (b + a) % 65_521;
    }
    (b << 16) | a
}

#[derive(Default)]
struct DeflateBits {
    bytes: Vec<u8>,
    current: u8,
    used: u8,
}

impl DeflateBits {
    fn write_bits(&mut self, value: u32, count: u8) {
        for bit in 0..count {
            self.current |= u8::try_from((value >> bit) & 1).unwrap() << self.used;
            self.used += 1;
            if self.used == 8 {
                self.bytes.push(self.current);
                self.current = 0;
                self.used = 0;
            }
        }
    }

    fn write_huffman(&mut self, canonical: u16, length: u8) {
        let mut reversed = 0_u32;
        for bit in 0..length {
            reversed |= u32::from((canonical >> (length - bit - 1)) & 1) << bit;
        }
        self.write_bits(reversed, length);
    }

    fn write_fixed_symbol(&mut self, symbol: u16) {
        let (canonical, length) = match symbol {
            0..=143 => (symbol + 0x30, 8),
            144..=255 => (symbol - 144 + 0x190, 9),
            256..=279 => (symbol - 256, 7),
            280..=287 => (symbol - 280 + 0xc0, 8),
            _ => panic!("not a fixed literal/length symbol"),
        };
        self.write_huffman(canonical, length);
    }

    fn finish(mut self) -> Vec<u8> {
        if self.used != 0 {
            self.bytes.push(self.current);
        }
        self.bytes
    }
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
