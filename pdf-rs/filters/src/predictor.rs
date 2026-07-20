use crate::decode::{DecodeBudget, DecodeCancellation, OutputBuffer};
use crate::{DecodeError, DecodeErrorCode, PredictorParameters};

pub(crate) fn decode_predictor<C: DecodeCancellation + ?Sized>(
    input: &[u8],
    input_capacity: usize,
    is_final: bool,
    filter_index: u16,
    parameters: PredictorParameters,
    budget: &mut DecodeBudget,
    cancellation: &C,
) -> Result<Vec<u8>, DecodeError> {
    match parameters.predictor() {
        2 => decode_tiff(
            input,
            input_capacity,
            is_final,
            filter_index,
            parameters,
            budget,
            cancellation,
        ),
        10.. => decode_png(
            input,
            input_capacity,
            is_final,
            filter_index,
            parameters,
            budget,
            cancellation,
        ),
        _ => Err(DecodeError::for_code(
            DecodeErrorCode::InternalState,
            Some(filter_index),
        )),
    }
}

fn decode_tiff<C: DecodeCancellation + ?Sized>(
    input: &[u8],
    input_capacity: usize,
    is_final: bool,
    filter_index: u16,
    parameters: PredictorParameters,
    budget: &mut DecodeBudget,
    cancellation: &C,
) -> Result<Vec<u8>, DecodeError> {
    let hint = u64::try_from(input.len()).unwrap_or(u64::MAX);
    let mut output = OutputBuffer::new(input_capacity, is_final, Some(filter_index), hint, budget);
    output.charge_algorithm(1, cancellation)?;
    let geometry = PredictorGeometry::new(parameters, filter_index)?;
    if !input.len().is_multiple_of(geometry.row_bytes) {
        return Err(invalid_data(filter_index));
    }

    for row in input.chunks_exact(geometry.row_bytes) {
        let row_output_start = output.len();
        for byte in row {
            output.consume_input(cancellation)?;
            output.push(*byte, cancellation)?;
        }
        for sample in geometry.colors..geometry.samples_per_row {
            reconstruct_tiff_sample(
                row,
                row_output_start,
                sample,
                geometry.colors,
                parameters.bits_per_component(),
                filter_index,
                &mut output,
                cancellation,
            )?;
        }
    }
    Ok(output.finish())
}

fn decode_png<C: DecodeCancellation + ?Sized>(
    input: &[u8],
    input_capacity: usize,
    is_final: bool,
    filter_index: u16,
    parameters: PredictorParameters,
    budget: &mut DecodeBudget,
    cancellation: &C,
) -> Result<Vec<u8>, DecodeError> {
    let hint = u64::try_from(input.len()).unwrap_or(u64::MAX);
    let mut output = OutputBuffer::new(input_capacity, is_final, Some(filter_index), hint, budget);
    output.charge_algorithm(1, cancellation)?;
    let geometry = PredictorGeometry::new(parameters, filter_index)?;
    let encoded_row_bytes = geometry
        .row_bytes
        .checked_add(1)
        .ok_or_else(|| invalid_data(filter_index))?;
    if !input.len().is_multiple_of(encoded_row_bytes) {
        return Err(invalid_data(filter_index));
    }

    let mut cursor = 0_usize;
    let mut row_index = 0_usize;
    while cursor < input.len() {
        output.consume_input(cancellation)?;
        let tag = input[cursor];
        cursor += 1;
        output.charge_algorithm(1, cancellation)?;
        if !tag_is_allowed(tag) {
            return Err(invalid_data(filter_index));
        }
        for column in 0..geometry.row_bytes {
            output.consume_input(cancellation)?;
            let encoded = input[cursor];
            cursor += 1;
            output.charge_algorithm(1, cancellation)?;
            let left = if column >= geometry.bytes_per_pixel {
                output
                    .byte_at_distance(geometry.bytes_per_pixel)
                    .ok_or_else(|| invalid_data(filter_index))?
            } else {
                0
            };
            output.charge_algorithm(1, cancellation)?;
            let above = if row_index > 0 {
                output
                    .byte_at_distance(geometry.row_bytes)
                    .ok_or_else(|| invalid_data(filter_index))?
            } else {
                0
            };
            output.charge_algorithm(1, cancellation)?;
            let upper_left = if row_index > 0 && column >= geometry.bytes_per_pixel {
                output
                    .byte_at_distance(
                        geometry
                            .row_bytes
                            .checked_add(geometry.bytes_per_pixel)
                            .ok_or_else(|| invalid_data(filter_index))?,
                    )
                    .ok_or_else(|| invalid_data(filter_index))?
            } else {
                0
            };
            output.charge_algorithm(1, cancellation)?;
            let predicted = match tag {
                0 => 0,
                1 => left,
                2 => above,
                3 => ((u16::from(left) + u16::from(above)) / 2) as u8,
                4 => paeth(left, above, upper_left),
                _ => return Err(invalid_data(filter_index)),
            };
            output.charge_algorithm(1, cancellation)?;
            let reconstructed = encoded.wrapping_add(predicted);
            output.push(reconstructed, cancellation)?;
        }
        row_index = row_index
            .checked_add(1)
            .ok_or_else(|| invalid_data(filter_index))?;
    }
    Ok(output.finish())
}

struct PredictorGeometry {
    colors: usize,
    samples_per_row: usize,
    row_bytes: usize,
    bytes_per_pixel: usize,
}

impl PredictorGeometry {
    fn new(parameters: PredictorParameters, filter_index: u16) -> Result<Self, DecodeError> {
        let colors =
            usize::try_from(parameters.colors()).map_err(|_| invalid_parameters(filter_index))?;
        let samples_per_row_u64 = u64::from(parameters.colors())
            .checked_mul(u64::from(parameters.columns()))
            .ok_or_else(|| invalid_parameters(filter_index))?;
        let row_bits = samples_per_row_u64
            .checked_mul(u64::from(parameters.bits_per_component()))
            .ok_or_else(|| invalid_parameters(filter_index))?;
        let row_bytes = row_bits
            .checked_add(7)
            .map(|value| value / 8)
            .and_then(|value| usize::try_from(value).ok())
            .ok_or_else(|| invalid_parameters(filter_index))?;
        let pixel_bits = u64::from(parameters.colors())
            .checked_mul(u64::from(parameters.bits_per_component()))
            .ok_or_else(|| invalid_parameters(filter_index))?;
        let bytes_per_pixel = pixel_bits
            .checked_add(7)
            .map(|value| value / 8)
            .and_then(|value| usize::try_from(value).ok())
            .ok_or_else(|| invalid_parameters(filter_index))?;
        let samples_per_row =
            usize::try_from(samples_per_row_u64).map_err(|_| invalid_parameters(filter_index))?;
        if row_bytes == 0 || bytes_per_pixel == 0 {
            return Err(invalid_parameters(filter_index));
        }
        Ok(Self {
            colors,
            samples_per_row,
            row_bytes,
            bytes_per_pixel,
        })
    }
}

#[allow(clippy::too_many_arguments)]
fn reconstruct_tiff_sample<C: DecodeCancellation + ?Sized>(
    row: &[u8],
    row_output_start: usize,
    sample: usize,
    colors: usize,
    width: u8,
    filter_index: u16,
    output: &mut OutputBuffer<'_>,
    cancellation: &C,
) -> Result<(), DecodeError> {
    output.charge_algorithm(1, cancellation)?;
    let bit_offset = sample
        .checked_mul(usize::from(width))
        .ok_or_else(|| invalid_data(filter_index))?;
    let previous_bit_offset = (sample - colors)
        .checked_mul(usize::from(width))
        .ok_or_else(|| invalid_data(filter_index))?;
    let output_row_bit = row_output_start
        .checked_mul(8)
        .and_then(|value| value.checked_add(bit_offset))
        .ok_or_else(|| invalid_data(filter_index))?;
    let previous_output_bit = row_output_start
        .checked_mul(8)
        .and_then(|value| value.checked_add(previous_bit_offset))
        .ok_or_else(|| invalid_data(filter_index))?;
    let delta = read_input_bits(row, bit_offset, width, filter_index, output, cancellation)?;
    let previous = read_output_bits(
        previous_output_bit,
        width,
        filter_index,
        output,
        cancellation,
    )?;
    output.charge_algorithm(1, cancellation)?;
    let mask = if width == 16 {
        u16::MAX
    } else {
        (1_u16 << width) - 1
    };
    let reconstructed = delta.wrapping_add(previous) & mask;
    write_output_bits(
        output_row_bit,
        width,
        reconstructed,
        filter_index,
        output,
        cancellation,
    )
}

fn read_input_bits<C: DecodeCancellation + ?Sized>(
    bytes: &[u8],
    start_bit: usize,
    width: u8,
    filter_index: u16,
    output: &mut OutputBuffer<'_>,
    cancellation: &C,
) -> Result<u16, DecodeError> {
    let mut value = 0_u16;
    for offset in 0..usize::from(width) {
        output.charge_algorithm(1, cancellation)?;
        let bit = start_bit
            .checked_add(offset)
            .ok_or_else(|| invalid_data(filter_index))?;
        let byte = *bytes
            .get(bit / 8)
            .ok_or_else(|| invalid_data(filter_index))?;
        let shift = 7_usize
            .checked_sub(bit % 8)
            .ok_or_else(|| invalid_data(filter_index))?;
        value = (value << 1) | u16::from((byte >> shift) & 1);
    }
    Ok(value)
}

fn read_output_bits<C: DecodeCancellation + ?Sized>(
    start_bit: usize,
    width: u8,
    filter_index: u16,
    output: &mut OutputBuffer<'_>,
    cancellation: &C,
) -> Result<u16, DecodeError> {
    let mut value = 0_u16;
    for offset in 0..usize::from(width) {
        output.charge_algorithm(1, cancellation)?;
        let bit = start_bit
            .checked_add(offset)
            .ok_or_else(|| invalid_data(filter_index))?;
        let current = output
            .bit_at(bit)
            .ok_or_else(|| invalid_data(filter_index))?;
        value = (value << 1) | u16::from(current);
    }
    Ok(value)
}

fn write_output_bits<C: DecodeCancellation + ?Sized>(
    start_bit: usize,
    width: u8,
    value: u16,
    filter_index: u16,
    output: &mut OutputBuffer<'_>,
    cancellation: &C,
) -> Result<(), DecodeError> {
    for offset in 0..usize::from(width) {
        output.charge_algorithm(1, cancellation)?;
        let bit = start_bit
            .checked_add(offset)
            .ok_or_else(|| invalid_data(filter_index))?;
        let source_shift = usize::from(width) - 1 - offset;
        let source_bit = ((value >> source_shift) & 1) as u8;
        if !output.replace_bit(bit, source_bit) {
            return Err(invalid_data(filter_index));
        }
    }
    Ok(())
}

const fn tag_is_allowed(tag: u8) -> bool {
    tag <= 4
}

fn paeth(left: u8, above: u8, upper_left: u8) -> u8 {
    let left = i32::from(left);
    let above = i32::from(above);
    let upper_left = i32::from(upper_left);
    let estimate = left + above - upper_left;
    let left_distance = (estimate - left).abs();
    let above_distance = (estimate - above).abs();
    let upper_left_distance = (estimate - upper_left).abs();
    if left_distance <= above_distance && left_distance <= upper_left_distance {
        left as u8
    } else if above_distance <= upper_left_distance {
        above as u8
    } else {
        upper_left as u8
    }
}

fn invalid_parameters(filter_index: u16) -> DecodeError {
    DecodeError::for_code(DecodeErrorCode::InvalidDecodeParameters, Some(filter_index))
}

fn invalid_data(filter_index: u16) -> DecodeError {
    DecodeError::for_code(DecodeErrorCode::InvalidPredictorData, Some(filter_index))
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::reconstruct_tiff_sample;
    use crate::decode::{DecodeBudget, DecodeCancellation, OutputBuffer};
    use crate::{
        DecodeErrorCode, DecodeFuelScheduleVersion, DecodeLimitConfig, DecodeLimits, NeverCancelled,
    };

    struct CancelAtProbe {
        probes: AtomicUsize,
        cancel_at: usize,
    }

    impl DecodeCancellation for CancelAtProbe {
        fn is_cancelled(&self) -> bool {
            self.probes.fetch_add(1, Ordering::SeqCst) + 1 >= self.cancel_at
        }
    }

    #[test]
    fn cancellation_stops_between_tiff_bit_mutations() {
        let config = DecodeLimitConfig {
            cancellation_check_interval_fuel: 1,
            ..DecodeLimitConfig::default()
        };
        let limits = DecodeLimits::validate(config).unwrap();
        let mut budget = DecodeBudget::new(limits, DecodeFuelScheduleVersion::M1V1);
        let mut output = OutputBuffer::new(0, true, Some(0), 1, &mut budget);
        output.push(0x52, &NeverCancelled).unwrap();
        let cancellation = CancelAtProbe {
            probes: AtomicUsize::new(0),
            cancel_at: 13,
        };

        let error = reconstruct_tiff_sample(&[0x52], 0, 1, 1, 4, 0, &mut output, &cancellation)
            .unwrap_err();

        assert_eq!(error.code(), DecodeErrorCode::Cancelled);
        assert_eq!(error.filter_index(), Some(0));
        assert_eq!(cancellation.probes.load(Ordering::SeqCst), 13);
        assert_eq!(output.finish(), [0x56]);
    }
}
