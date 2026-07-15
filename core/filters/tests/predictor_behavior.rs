use std::sync::atomic::{AtomicUsize, Ordering};

use pdf_rs_bytes::{
    ByteRange, ByteSlice, ByteSource, JobId, RangeResponse, RangeStore, ReadPoll, ReadRequest,
    RequestPriority, ResumeCheckpoint, SourceIdentity, SourceRevision, SourceSnapshot,
    SourceStableId, SourceValidator, SourceValidatorKind,
};
use pdf_rs_filters::{
    DecodeCancellation, DecodeErrorCategory, DecodeErrorCode, DecodeLimitConfig, DecodeLimitKind,
    DecodeLimits, DecodeProfile, DecodeRecoverability, DecodeRequest, FilterDecodeParameters,
    FilterPlan, FilterStage, NeverCancelled, PredictorParameters, StreamFilter, decode_stream,
};
use pdf_rs_syntax::{ByteSpan, ObjectRef};

const ENCODED_START: u64 = 96;

struct Fixture {
    snapshot: SourceSnapshot,
    encoded_span: ByteSpan,
    slice: ByteSlice,
}

impl Fixture {
    fn new(bytes: &[u8]) -> Self {
        let len = u64::try_from(bytes.len()).unwrap();
        let snapshot = SourceSnapshot::new(
            SourceIdentity::new(SourceStableId::new([0x81; 32]), SourceRevision::new(7)),
            Some(ENCODED_START + len),
            SourceValidator::new(SourceValidatorKind::FrozenResponse, [0x18; 32]),
        );
        let range = ByteRange::new(ENCODED_START, len).unwrap();
        let store = RangeStore::new(snapshot, Default::default()).unwrap();
        store
            .supply(RangeResponse::new(snapshot, range, bytes.to_vec()).unwrap())
            .unwrap();
        let slice = match store.poll(ReadRequest::new(
            range,
            RequestPriority::Metadata,
            JobId::new(211),
            ResumeCheckpoint::new(212),
        )) {
            ReadPoll::Ready(slice) => slice,
            _ => panic!("supplied predictor fixture must be ready"),
        };
        Self {
            snapshot,
            encoded_span: ByteSpan::new(ENCODED_START, len).unwrap(),
            slice,
        }
    }

    fn request(self, plan: FilterPlan, limits: DecodeLimits) -> DecodeRequest {
        DecodeRequest::new(
            self.snapshot,
            ObjectRef::new(31, 0).unwrap(),
            ByteSpan::new(8, 40).unwrap(),
            self.encoded_span,
            self.slice,
            plan,
            DecodeProfile::M1StrictV1,
            limits,
        )
        .unwrap()
    }
}

fn predictor_plan(parameters: PredictorParameters) -> FilterPlan {
    FilterPlan::from_stages(&[FilterStage::new(
        StreamFilter::FlateDecode,
        FilterDecodeParameters::Predictor(parameters),
    )
    .unwrap()])
    .unwrap()
}

fn decode_predictor(
    predictor_bytes: &[u8],
    parameters: PredictorParameters,
) -> pdf_rs_filters::DecodedStream {
    let encoded = zlib_stored(predictor_bytes);
    decode_stream(
        Fixture::new(&encoded).request(predictor_plan(parameters), DecodeLimits::default()),
        &NeverCancelled,
    )
    .unwrap()
}

fn parameters(predictor: i64, colors: u32, bits: u8, columns: u32) -> PredictorParameters {
    PredictorParameters::new(
        predictor,
        i64::from(colors),
        i64::from(bits),
        i64::from(columns),
    )
    .unwrap()
}

#[test]
fn predictor_defaults_are_explicit_and_attested_without_a_transform() {
    let defaults = PredictorParameters::default();
    assert_eq!(defaults.predictor(), 1);
    assert_eq!(defaults.colors(), 1);
    assert_eq!(defaults.bits_per_component(), 8);
    assert_eq!(defaults.columns(), 1);

    let decoded = decode_predictor(b"plain", defaults);
    assert_eq!(decoded.bytes(), b"plain");
    assert_eq!(
        decoded.attestation().filter_plan().stages(),
        &[FilterStage::new(
            StreamFilter::FlateDecode,
            FilterDecodeParameters::Predictor(defaults),
        )
        .unwrap()]
    );
}

#[test]
fn predictor_parameters_stay_bound_to_their_filter_in_a_chain() {
    let predictor = parameters(10, 1, 8, 5);
    let stages = [
        FilterStage::new(
            StreamFilter::FlateDecode,
            FilterDecodeParameters::Predictor(predictor),
        )
        .unwrap(),
        FilterStage::without_parameters(StreamFilter::AsciiHexDecode),
    ];
    let plan = FilterPlan::from_stages(&stages).unwrap();
    let encoded = zlib_stored(&[0, b'4', b'1', b'4', b'2', b'>']);
    let decoded = decode_stream(
        Fixture::new(&encoded).request(plan, DecodeLimits::default()),
        &NeverCancelled,
    )
    .unwrap();

    assert_eq!(decoded.bytes(), b"AB");
    assert_eq!(decoded.attestation().filter_plan().stages(), &stages);
    assert_eq!(decoded.attestation().cumulative_output_bytes(), 13);
}

#[test]
fn tiff_horizontal_differencing_is_sample_accurate_for_every_supported_width() {
    for bits in [1_u8, 2, 4, 8, 16] {
        let mask = if bits == 16 {
            u16::MAX
        } else {
            (1_u16 << bits) - 1
        };
        let rows = [
            [0, 1, 3, 2, 7, 5].map(|value| value & mask),
            [1, 0, 2, 6, 4, 9].map(|value| value & mask),
        ];
        let mut encoded = Vec::new();
        let mut expected = Vec::new();
        for row in rows {
            let mut differences = row;
            for sample in (2..differences.len()).rev() {
                differences[sample] = differences[sample].wrapping_sub(row[sample - 2]) & mask;
            }
            encoded.extend(pack_samples(&differences, bits));
            expected.extend(pack_samples(&row, bits));
        }
        let decoded = decode_predictor(&encoded, parameters(2, 2, bits, 3));
        assert_eq!(decoded.bytes(), expected, "BitsPerComponent={bits}");
    }
}

#[test]
fn tiff_resets_component_history_at_each_row_and_preserves_padding_bits() {
    let mut first = pack_samples(&[1, 2, 1], 2);
    let mut second = pack_samples(&[3, 1, 2], 2);
    first[0] |= 0b0000_0010;
    second[0] |= 0b0000_0001;
    let encoded = [first, second].concat();
    let decoded = decode_predictor(&encoded, parameters(2, 1, 2, 3));
    assert_eq!(decoded.bytes()[0] & 0b11, 0b10);
    assert_eq!(decoded.bytes()[1] & 0b11, 0b01);
    assert_eq!(unpack_samples(&decoded.bytes()[0..1], 2, 3), [1, 3, 0]);
    assert_eq!(unpack_samples(&decoded.bytes()[1..2], 2, 3), [3, 0, 2]);
}

#[test]
fn every_png_predictor_value_uses_the_row_tag_for_all_five_algorithms() {
    let rows = vec![vec![10, 20, 30, 40, 50], vec![15, 18, 35, 39, 60]];
    for tag in 0_u8..=4 {
        let encoded = png_encode(&rows, &[tag, tag], 1);
        for predictor in [10_i64, 12, 15, 16, i64::MAX] {
            let decoded = decode_predictor(&encoded, parameters(predictor, 1, 8, 5));
            assert_eq!(
                decoded.bytes(),
                rows.concat(),
                "Predictor={predictor}, PNG tag {tag}"
            );
        }
    }

    let sub_under_predictor_twelve = png_encode(&rows, &[1, 1], 1);
    assert_eq!(
        decode_predictor(&sub_under_predictor_twelve, parameters(12, 1, 8, 5)).bytes(),
        rows.concat()
    );
}

#[test]
fn png_rows_select_algorithms_and_sub_uses_packed_pixel_width() {
    let rows = vec![
        vec![1, 2, 3, 4],
        vec![5, 6, 7, 8],
        vec![9, 10, 11, 12],
        vec![13, 14, 15, 16],
        vec![17, 18, 19, 20],
    ];
    let tags = [0, 1, 2, 3, 4];
    let decoded = decode_predictor(&png_encode(&rows, &tags, 1), parameters(15, 1, 8, 4));
    assert_eq!(decoded.bytes(), rows.concat());

    for bits in [1_u8, 2, 4, 8, 16] {
        let row_bytes = (u32::from(2_u8) * 3 * u32::from(bits)).div_ceil(8) as usize;
        let bytes_per_pixel = (u32::from(2_u8) * u32::from(bits)).div_ceil(8) as usize;
        let raw = vec![(0..row_bytes as u8).map(|v| v * 7 + 3).collect()];
        let encoded = png_encode(&raw, &[1], bytes_per_pixel);
        let decoded = decode_predictor(&encoded, parameters(11, 2, bits, 3));
        assert_eq!(decoded.bytes(), raw.concat(), "packed PNG width {bits}");
    }
}

#[test]
fn invalid_and_unsupported_decode_parameters_remain_distinct() {
    for result in [
        PredictorParameters::new(0, 1, 8, 1),
        PredictorParameters::new(1, 0, 8, 1),
        PredictorParameters::new(1, 1, 0, 1),
        PredictorParameters::new(1, 1, 8, 0),
        PredictorParameters::new(2, i64::MAX, 16, i64::MAX),
        PredictorParameters::new(-1, 1, 8, 1),
    ] {
        let error = result.unwrap_err();
        assert_eq!(error.code(), DecodeErrorCode::InvalidDecodeParameters);
        assert_eq!(error.category(), DecodeErrorCategory::Syntax);
    }
    for result in [
        PredictorParameters::new(9, 1, 8, 1),
        PredictorParameters::new(2, 1, 3, 1),
    ] {
        let error = result.unwrap_err();
        assert_eq!(error.code(), DecodeErrorCode::UnsupportedPredictor);
        assert_eq!(error.category(), DecodeErrorCategory::Unsupported);
        assert_eq!(
            error.recoverability(),
            DecodeRecoverability::ReportUnsupported
        );
    }
    let error = FilterStage::new(
        StreamFilter::AsciiHexDecode,
        FilterDecodeParameters::Predictor(PredictorParameters::default()),
    )
    .unwrap_err();
    assert_eq!(error.code(), DecodeErrorCode::UnsupportedDecodeParameters);
}

#[test]
fn predictor_row_framing_and_png_tags_are_strict() {
    for bytes in [&[1, 2, 3][..], &[1, 2, 3, 4, 5][..]] {
        let error = decode_error(bytes, parameters(2, 1, 8, 4), DecodeLimits::default());
        assert_eq!(error.code(), DecodeErrorCode::InvalidPredictorData);
    }
    for bytes in [&[0, 1, 2, 3][..], &[0, 1, 2, 3, 4, 5][..]] {
        let error = decode_error(bytes, parameters(15, 1, 8, 4), DecodeLimits::default());
        assert_eq!(error.code(), DecodeErrorCode::InvalidPredictorData);
    }
    let illegal = decode_error(
        &[5, 1, 2, 3, 4],
        parameters(16, 1, 8, 4),
        DecodeLimits::default(),
    );
    assert_eq!(illegal.code(), DecodeErrorCode::InvalidPredictorData);
    let formerly_mismatched_tag = decode_predictor(&[0, 1, 2, 3, 4], parameters(11, 1, 8, 4));
    assert_eq!(formerly_mismatched_tag.bytes(), [1, 2, 3, 4]);
}

#[test]
fn predictor_output_fuel_total_and_retained_capacity_limits_are_enforced() {
    let rows = vec![vec![1, 3, 6, 10], vec![2, 5, 9, 14]];
    let predictor_bytes = png_encode(&rows, &[4, 4], 1);
    let parameters = parameters(14, 1, 8, 4);
    let success = decode_predictor(&predictor_bytes, parameters);
    assert_eq!(success.attestation().cumulative_output_bytes(), 18);
    let fuel = success.attestation().fuel_consumed();
    let peak = success.attestation().peak_retained_capacity_bytes();
    assert!(peak > success.len());

    let final_error = decode_error(
        &predictor_bytes,
        parameters,
        configured(|config| config.max_final_output_bytes = 7),
    );
    assert_limit(final_error, DecodeLimitKind::FinalOutputBytes);

    let total_error = decode_error(
        &predictor_bytes,
        parameters,
        configured(|config| {
            config.max_final_output_bytes = 8;
            config.max_total_output_bytes = 17;
        }),
    );
    assert_limit(total_error, DecodeLimitKind::TotalOutputBytes);

    let fuel_error = decode_error(
        &predictor_bytes,
        parameters,
        configured(|config| {
            config.max_fuel = fuel - 1;
            config.cancellation_check_interval_fuel = 1;
        }),
    );
    assert_limit(fuel_error, DecodeLimitKind::Fuel);

    let retained_error = decode_error(
        &predictor_bytes,
        parameters,
        configured(|config| {
            config.max_final_output_bytes = 8;
            config.max_retained_capacity_bytes = 10;
        }),
    );
    assert_eq!(retained_error.limit().unwrap().consumed(), 10);
    assert_limit(retained_error, DecodeLimitKind::RetainedCapacityBytes);
}

#[test]
fn predictor_work_obeys_cooperative_cancellation() {
    let predictor_bytes = png_encode(&[vec![1, 2, 3, 4]], &[1], 1);
    let encoded = zlib_stored(&predictor_bytes);
    let limits = configured(|config| config.cancellation_check_interval_fuel = 1);
    let counter = ProbeCounter::default();
    let plain_plan = predictor_plan(PredictorParameters::default());
    decode_stream(Fixture::new(&encoded).request(plain_plan, limits), &counter).unwrap();
    let cancel = CancelAtProbe {
        probes: AtomicUsize::new(0),
        cancel_at: counter.probes.load(Ordering::SeqCst) + 9,
    };
    let error = decode_stream(
        Fixture::new(&encoded).request(predictor_plan(parameters(11, 1, 8, 4)), limits),
        &cancel,
    )
    .unwrap_err();
    assert_eq!(error.code(), DecodeErrorCode::Cancelled);
    assert_eq!(error.filter_index(), Some(0));
    assert_eq!(cancel.probes.load(Ordering::SeqCst), cancel.cancel_at);
}

fn decode_error(
    predictor_bytes: &[u8],
    parameters: PredictorParameters,
    limits: DecodeLimits,
) -> pdf_rs_filters::DecodeError {
    let encoded = zlib_stored(predictor_bytes);
    decode_stream(
        Fixture::new(&encoded).request(predictor_plan(parameters), limits),
        &NeverCancelled,
    )
    .unwrap_err()
}

fn configured(update: impl FnOnce(&mut DecodeLimitConfig)) -> DecodeLimits {
    let mut config = DecodeLimitConfig::default();
    update(&mut config);
    DecodeLimits::validate(config).unwrap()
}

fn assert_limit(error: pdf_rs_filters::DecodeError, expected: DecodeLimitKind) {
    assert_eq!(error.code(), DecodeErrorCode::ResourceLimit);
    assert_eq!(error.limit().unwrap().kind(), expected);
    assert_eq!(error.filter_index(), Some(0));
}

#[derive(Default)]
struct ProbeCounter {
    probes: AtomicUsize,
}

impl DecodeCancellation for ProbeCounter {
    fn is_cancelled(&self) -> bool {
        self.probes.fetch_add(1, Ordering::SeqCst);
        false
    }
}

struct CancelAtProbe {
    probes: AtomicUsize,
    cancel_at: usize,
}

impl DecodeCancellation for CancelAtProbe {
    fn is_cancelled(&self) -> bool {
        self.probes.fetch_add(1, Ordering::SeqCst) + 1 >= self.cancel_at
    }
}

fn pack_samples(samples: &[u16], bits: u8) -> Vec<u8> {
    let bit_len = samples.len() * usize::from(bits);
    let mut output = vec![0_u8; bit_len.div_ceil(8)];
    for (sample_index, sample) in samples.iter().copied().enumerate() {
        for offset in 0..usize::from(bits) {
            let source_shift = usize::from(bits) - 1 - offset;
            let bit = ((sample >> source_shift) & 1) as u8;
            let target = sample_index * usize::from(bits) + offset;
            output[target / 8] |= bit << (7 - target % 8);
        }
    }
    output
}

fn unpack_samples(bytes: &[u8], bits: u8, count: usize) -> Vec<u16> {
    (0..count)
        .map(|sample| {
            let mut value = 0_u16;
            for offset in 0..usize::from(bits) {
                let bit = sample * usize::from(bits) + offset;
                value = (value << 1) | u16::from((bytes[bit / 8] >> (7 - bit % 8)) & 1);
            }
            value
        })
        .collect()
}

fn png_encode(rows: &[Vec<u8>], tags: &[u8], bytes_per_pixel: usize) -> Vec<u8> {
    assert_eq!(rows.len(), tags.len());
    let mut output = Vec::new();
    for (row_index, (row, tag)) in rows.iter().zip(tags).enumerate() {
        output.push(*tag);
        for (column, byte) in row.iter().copied().enumerate() {
            let left = if column >= bytes_per_pixel {
                row[column - bytes_per_pixel]
            } else {
                0
            };
            let above = if row_index > 0 {
                rows[row_index - 1][column]
            } else {
                0
            };
            let upper_left = if row_index > 0 && column >= bytes_per_pixel {
                rows[row_index - 1][column - bytes_per_pixel]
            } else {
                0
            };
            let predicted = match tag {
                0 => 0,
                1 => left,
                2 => above,
                3 => ((u16::from(left) + u16::from(above)) / 2) as u8,
                4 => test_paeth(left, above, upper_left),
                _ => 0,
            };
            output.push(byte.wrapping_sub(predicted));
        }
    }
    output
}

fn test_paeth(left: u8, above: u8, upper_left: u8) -> u8 {
    let left = i32::from(left);
    let above = i32::from(above);
    let upper_left = i32::from(upper_left);
    let estimate = left + above - upper_left;
    let distances = [
        (estimate - left).abs(),
        (estimate - above).abs(),
        (estimate - upper_left).abs(),
    ];
    if distances[0] <= distances[1] && distances[0] <= distances[2] {
        left as u8
    } else if distances[1] <= distances[2] {
        above as u8
    } else {
        upper_left as u8
    }
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

fn adler32(bytes: &[u8]) -> u32 {
    let mut first = 1_u32;
    let mut second = 0_u32;
    for byte in bytes {
        first = (first + u32::from(*byte)) % 65_521;
        second = (second + first) % 65_521;
    }
    (second << 16) | first
}
