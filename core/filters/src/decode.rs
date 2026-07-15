use crate::{
    DecodeAttestation, DecodeError, DecodeErrorCode, DecodeFuelScheduleVersion, DecodeLimitKind,
    DecodeLimits, DecodeRequest, DecodedStream, StreamFilter,
};

const INITIAL_OUTPUT_RESERVE: usize = 4 * 1024;

/// Cooperative cancellation probe for bounded stream decoding.
pub trait DecodeCancellation: Send + Sync {
    /// Reports whether the owning runtime has abandoned this operation.
    fn is_cancelled(&self) -> bool;
}

/// Cancellation probe that never cancels.
#[derive(Clone, Copy, Debug, Default)]
pub struct NeverCancelled;

impl DecodeCancellation for NeverCancelled {
    fn is_cancelled(&self) -> bool {
        false
    }
}

/// Decodes one exact snapshot-bound physical stream slice.
///
/// Explicit filters run in source order. An empty plan uses an internal
/// identity layer; it does not model or accept a PDF `/Identity` filter.
pub fn decode_stream<C: DecodeCancellation + ?Sized>(
    request: DecodeRequest,
    cancellation: &C,
) -> Result<DecodedStream, DecodeError> {
    if cancellation.is_cancelled() {
        return Err(DecodeError::for_code(DecodeErrorCode::Cancelled, None));
    }

    let DecodeRequest {
        snapshot,
        owner,
        dictionary_span,
        encoded_span,
        encoded,
        plan,
        profile,
        limits,
    } = request;
    let input_len = u64::try_from(encoded.bytes().len())
        .map_err(|_| DecodeError::for_code(DecodeErrorCode::InternalState, None))?;
    if input_len > limits.max_input_bytes {
        return Err(DecodeError::resource(
            DecodeLimitKind::InputBytes,
            limits.max_input_bytes,
            0,
            input_len,
            None,
        ));
    }
    let filter_count = u64::try_from(plan.len())
        .map_err(|_| DecodeError::for_code(DecodeErrorCode::InternalState, None))?;
    if filter_count > u64::from(limits.max_filters) {
        return Err(DecodeError::resource(
            DecodeLimitKind::FilterCount,
            u64::from(limits.max_filters),
            0,
            filter_count,
            None,
        ));
    }

    let schedule = profile.fuel_schedule();
    let mut budget = DecodeBudget::new(limits, schedule);
    let decoded = if plan.is_empty() {
        budget.charge_setup(None, cancellation)?;
        decode_identity(encoded.bytes(), &mut budget, cancellation)?
    } else {
        let mut current: Option<Vec<u8>> = None;
        for (index, filter) in plan.filters().iter().copied().enumerate() {
            let filter_index =
                u16::try_from(index).expect("validated hard filter count always fits u16");
            let is_final = index + 1 == plan.len();
            budget.charge_setup(Some(filter_index), cancellation)?;
            let (input, input_capacity) = match current.as_ref() {
                Some(bytes) => (bytes.as_slice(), bytes.capacity()),
                None => (encoded.bytes(), 0),
            };
            let output = decode_layer(
                filter,
                input,
                input_capacity,
                is_final,
                filter_index,
                &mut budget,
                cancellation,
            )?;
            current = Some(output);
        }
        current.ok_or_else(|| DecodeError::for_code(DecodeErrorCode::InternalState, None))?
    };

    let decoded_length = u64::try_from(decoded.len())
        .map_err(|_| DecodeError::for_code(DecodeErrorCode::InternalState, None))?;
    let source_identity = snapshot.identity();
    let attestation = DecodeAttestation {
        snapshot,
        source_identity,
        owner,
        dictionary_span,
        encoded_span,
        encoded,
        plan,
        profile,
        limits,
        fuel_schedule: schedule,
        fuel_consumed: budget.fuel_consumed,
        cumulative_output_bytes: budget.cumulative_output_bytes,
        peak_retained_capacity_bytes: budget.peak_retained_capacity_bytes,
        decoded_length,
    };
    Ok(DecodedStream {
        bytes: decoded,
        attestation,
    })
}

fn decode_layer<C: DecodeCancellation + ?Sized>(
    filter: StreamFilter,
    input: &[u8],
    input_capacity: usize,
    is_final: bool,
    filter_index: u16,
    budget: &mut DecodeBudget,
    cancellation: &C,
) -> Result<Vec<u8>, DecodeError> {
    match filter {
        StreamFilter::AsciiHexDecode => decode_ascii_hex(
            input,
            input_capacity,
            is_final,
            filter_index,
            budget,
            cancellation,
        ),
        StreamFilter::Ascii85Decode => decode_ascii85(
            input,
            input_capacity,
            is_final,
            filter_index,
            budget,
            cancellation,
        ),
        StreamFilter::RunLengthDecode => decode_run_length(
            input,
            input_capacity,
            is_final,
            filter_index,
            budget,
            cancellation,
        ),
    }
}

fn decode_identity<C: DecodeCancellation + ?Sized>(
    input: &[u8],
    budget: &mut DecodeBudget,
    cancellation: &C,
) -> Result<Vec<u8>, DecodeError> {
    let hint = u64::try_from(input.len()).unwrap_or(u64::MAX);
    let mut output = OutputBuffer::new(0, true, None, hint, budget);
    for byte in input {
        output.consume_input(cancellation)?;
        output.push(*byte, cancellation)?;
    }
    Ok(output.finish())
}

fn decode_ascii_hex<C: DecodeCancellation + ?Sized>(
    input: &[u8],
    input_capacity: usize,
    is_final: bool,
    filter_index: u16,
    budget: &mut DecodeBudget,
    cancellation: &C,
) -> Result<Vec<u8>, DecodeError> {
    let input_len = u64::try_from(input.len()).unwrap_or(u64::MAX);
    let hint = input_len.saturating_add(1) / 2;
    let mut output = OutputBuffer::new(input_capacity, is_final, Some(filter_index), hint, budget);
    let mut high_nibble = None;
    let mut terminated = false;

    for byte in input {
        output.consume_input(cancellation)?;
        if terminated {
            if is_pdf_whitespace(*byte) {
                continue;
            }
            return Err(DecodeError::for_code(
                DecodeErrorCode::TrailingData,
                Some(filter_index),
            ));
        }
        if is_pdf_whitespace(*byte) {
            continue;
        }
        if *byte == b'>' {
            if let Some(high) = high_nibble.take() {
                output.push(high << 4, cancellation)?;
            }
            terminated = true;
            continue;
        }
        let nibble = hex_nibble(*byte).ok_or_else(|| {
            DecodeError::for_code(DecodeErrorCode::InvalidAsciiHex, Some(filter_index))
        })?;
        match high_nibble.take() {
            Some(high) => output.push((high << 4) | nibble, cancellation)?,
            None => high_nibble = Some(nibble),
        }
    }

    if !terminated {
        return Err(DecodeError::for_code(
            DecodeErrorCode::MissingEndMarker,
            Some(filter_index),
        ));
    }
    Ok(output.finish())
}

fn decode_ascii85<C: DecodeCancellation + ?Sized>(
    input: &[u8],
    input_capacity: usize,
    is_final: bool,
    filter_index: u16,
    budget: &mut DecodeBudget,
    cancellation: &C,
) -> Result<Vec<u8>, DecodeError> {
    let input_len = u64::try_from(input.len()).unwrap_or(u64::MAX);
    let hint = input_len
        .saturating_mul(4)
        .saturating_div(5)
        .saturating_add(4);
    let mut output = OutputBuffer::new(input_capacity, is_final, Some(filter_index), hint, budget);
    let mut digits = [0_u32; 5];
    let mut digit_count = 0_usize;
    let mut pending_tilde = false;
    let mut terminated = false;

    for byte in input {
        output.consume_input(cancellation)?;
        if terminated {
            if is_pdf_whitespace(*byte) {
                continue;
            }
            return Err(DecodeError::for_code(
                DecodeErrorCode::TrailingData,
                Some(filter_index),
            ));
        }
        if pending_tilde {
            if *byte != b'>' {
                return Err(DecodeError::for_code(
                    DecodeErrorCode::InvalidAscii85,
                    Some(filter_index),
                ));
            }
            emit_ascii85_partial(
                &digits,
                digit_count,
                filter_index,
                &mut output,
                cancellation,
            )?;
            pending_tilde = false;
            terminated = true;
            continue;
        }
        if is_pdf_whitespace(*byte) {
            continue;
        }
        match *byte {
            b'~' => pending_tilde = true,
            b'z' if digit_count == 0 => {
                for _ in 0..4 {
                    output.push(0, cancellation)?;
                }
            }
            b'z' => {
                return Err(DecodeError::for_code(
                    DecodeErrorCode::InvalidAscii85,
                    Some(filter_index),
                ));
            }
            b'!'..=b'u' => {
                digits[digit_count] = u32::from(*byte - b'!');
                digit_count += 1;
                if digit_count == 5 {
                    let value = ascii85_group_value(&digits).ok_or_else(|| {
                        DecodeError::for_code(DecodeErrorCode::InvalidAscii85, Some(filter_index))
                    })?;
                    for byte in value.to_be_bytes() {
                        output.push(byte, cancellation)?;
                    }
                    digit_count = 0;
                }
            }
            _ => {
                return Err(DecodeError::for_code(
                    DecodeErrorCode::InvalidAscii85,
                    Some(filter_index),
                ));
            }
        }
    }

    if !terminated {
        return Err(DecodeError::for_code(
            DecodeErrorCode::MissingEndMarker,
            Some(filter_index),
        ));
    }
    Ok(output.finish())
}

fn emit_ascii85_partial<C: DecodeCancellation + ?Sized>(
    digits: &[u32; 5],
    digit_count: usize,
    filter_index: u16,
    output: &mut OutputBuffer<'_>,
    cancellation: &C,
) -> Result<(), DecodeError> {
    if digit_count == 0 {
        return Ok(());
    }
    if digit_count == 1 {
        return Err(DecodeError::for_code(
            DecodeErrorCode::InvalidAscii85,
            Some(filter_index),
        ));
    }
    let mut padded = *digits;
    for digit in &mut padded[digit_count..] {
        *digit = 84;
    }
    let value = ascii85_group_value(&padded).ok_or_else(|| {
        DecodeError::for_code(DecodeErrorCode::InvalidAscii85, Some(filter_index))
    })?;
    for byte in value.to_be_bytes().into_iter().take(digit_count - 1) {
        output.push(byte, cancellation)?;
    }
    Ok(())
}

fn ascii85_group_value(digits: &[u32; 5]) -> Option<u32> {
    let mut value = 0_u64;
    for digit in digits {
        value = value.checked_mul(85)?.checked_add(u64::from(*digit))?;
    }
    u32::try_from(value).ok()
}

fn decode_run_length<C: DecodeCancellation + ?Sized>(
    input: &[u8],
    input_capacity: usize,
    is_final: bool,
    filter_index: u16,
    budget: &mut DecodeBudget,
    cancellation: &C,
) -> Result<Vec<u8>, DecodeError> {
    let input_len = u64::try_from(input.len()).unwrap_or(u64::MAX);
    let hint = input_len.saturating_mul(128);
    let mut output = OutputBuffer::new(input_capacity, is_final, Some(filter_index), hint, budget);
    let mut cursor = 0_usize;

    while cursor < input.len() {
        output.consume_input(cancellation)?;
        let control = input[cursor];
        cursor += 1;
        match control {
            128 => {
                if cursor != input.len() {
                    return Err(DecodeError::for_code(
                        DecodeErrorCode::TrailingData,
                        Some(filter_index),
                    ));
                }
                return Ok(output.finish());
            }
            0..=127 => {
                let count = usize::from(control) + 1;
                let end = cursor.checked_add(count).ok_or_else(|| {
                    DecodeError::for_code(DecodeErrorCode::InvalidRunLength, Some(filter_index))
                })?;
                if end > input.len() {
                    return Err(DecodeError::for_code(
                        DecodeErrorCode::InvalidRunLength,
                        Some(filter_index),
                    ));
                }
                for byte in &input[cursor..end] {
                    output.consume_input(cancellation)?;
                    output.push(*byte, cancellation)?;
                }
                cursor = end;
            }
            129..=255 => {
                if cursor == input.len() {
                    return Err(DecodeError::for_code(
                        DecodeErrorCode::InvalidRunLength,
                        Some(filter_index),
                    ));
                }
                output.consume_input(cancellation)?;
                let byte = input[cursor];
                cursor += 1;
                let count = usize::from(257_u16 - u16::from(control));
                for _ in 0..count {
                    output.push(byte, cancellation)?;
                }
            }
        }
    }

    Err(DecodeError::for_code(
        DecodeErrorCode::MissingEndMarker,
        Some(filter_index),
    ))
}

fn hex_nibble(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

const fn is_pdf_whitespace(byte: u8) -> bool {
    matches!(byte, 0 | b'\t' | b'\n' | 0x0c | b'\r' | b' ')
}

struct DecodeBudget {
    limits: DecodeLimits,
    schedule: DecodeFuelScheduleVersion,
    fuel_consumed: u64,
    last_cancellation_probe_fuel: u64,
    cumulative_output_bytes: u64,
    peak_retained_capacity_bytes: u64,
}

impl DecodeBudget {
    const fn new(limits: DecodeLimits, schedule: DecodeFuelScheduleVersion) -> Self {
        Self {
            limits,
            schedule,
            fuel_consumed: 0,
            last_cancellation_probe_fuel: 0,
            cumulative_output_bytes: 0,
            peak_retained_capacity_bytes: 0,
        }
    }

    fn charge_setup<C: DecodeCancellation + ?Sized>(
        &mut self,
        filter_index: Option<u16>,
        cancellation: &C,
    ) -> Result<(), DecodeError> {
        self.charge_fuel(self.schedule.layer_setup_cost(), filter_index, cancellation)
    }

    fn consume_input<C: DecodeCancellation + ?Sized>(
        &mut self,
        filter_index: Option<u16>,
        cancellation: &C,
    ) -> Result<(), DecodeError> {
        self.charge_fuel(self.schedule.input_byte_cost(), filter_index, cancellation)
    }

    fn prepare_output<C: DecodeCancellation + ?Sized>(
        &mut self,
        layer_output_bytes: u64,
        is_final: bool,
        filter_index: Option<u16>,
        cancellation: &C,
    ) -> Result<(u64, u64), DecodeError> {
        let attempted_layer = layer_output_bytes.checked_add(1).ok_or_else(|| {
            DecodeError::resource(
                DecodeLimitKind::LayerOutputBytes,
                self.limits.max_layer_output_bytes,
                layer_output_bytes,
                u64::MAX,
                filter_index,
            )
        })?;
        if attempted_layer > self.limits.max_layer_output_bytes {
            return Err(DecodeError::resource(
                DecodeLimitKind::LayerOutputBytes,
                self.limits.max_layer_output_bytes,
                layer_output_bytes,
                attempted_layer,
                filter_index,
            ));
        }
        let attempted_total = self.cumulative_output_bytes.checked_add(1).ok_or_else(|| {
            DecodeError::resource(
                DecodeLimitKind::TotalOutputBytes,
                self.limits.max_total_output_bytes,
                self.cumulative_output_bytes,
                u64::MAX,
                filter_index,
            )
        })?;
        if attempted_total > self.limits.max_total_output_bytes {
            return Err(DecodeError::resource(
                DecodeLimitKind::TotalOutputBytes,
                self.limits.max_total_output_bytes,
                self.cumulative_output_bytes,
                attempted_total,
                filter_index,
            ));
        }
        if is_final && attempted_layer > self.limits.max_final_output_bytes {
            return Err(DecodeError::resource(
                DecodeLimitKind::FinalOutputBytes,
                self.limits.max_final_output_bytes,
                layer_output_bytes,
                attempted_layer,
                filter_index,
            ));
        }
        self.charge_fuel(self.schedule.output_byte_cost(), filter_index, cancellation)?;
        Ok((attempted_layer, attempted_total))
    }

    fn charge_fuel<C: DecodeCancellation + ?Sized>(
        &mut self,
        amount: u64,
        filter_index: Option<u16>,
        cancellation: &C,
    ) -> Result<(), DecodeError> {
        let attempted = self.fuel_consumed.saturating_add(amount);
        if attempted > self.limits.max_fuel {
            return Err(DecodeError::resource(
                DecodeLimitKind::Fuel,
                self.limits.max_fuel,
                self.fuel_consumed,
                attempted,
                filter_index,
            ));
        }
        self.fuel_consumed = attempted;
        if self
            .fuel_consumed
            .saturating_sub(self.last_cancellation_probe_fuel)
            >= self.limits.cancellation_check_interval_fuel
        {
            self.last_cancellation_probe_fuel = self.fuel_consumed;
            if cancellation.is_cancelled() {
                return Err(DecodeError::for_code(
                    DecodeErrorCode::Cancelled,
                    filter_index,
                ));
            }
        }
        Ok(())
    }

    fn capacity_ceiling(&self, is_final: bool) -> u64 {
        let final_limit = if is_final {
            self.limits.max_final_output_bytes
        } else {
            u64::MAX
        };
        let total_remaining = self
            .limits
            .max_total_output_bytes
            .saturating_sub(self.cumulative_output_bytes);
        self.limits
            .max_layer_output_bytes
            .min(final_limit)
            .min(total_remaining)
    }

    fn observe_retained_capacity(&mut self, retained: u64) {
        self.peak_retained_capacity_bytes = self.peak_retained_capacity_bytes.max(retained);
    }
}

struct OutputBuffer<'a> {
    bytes: Vec<u8>,
    input_capacity: usize,
    is_final: bool,
    filter_index: Option<u16>,
    expected_output: u64,
    layer_output_bytes: u64,
    budget: &'a mut DecodeBudget,
}

impl<'a> OutputBuffer<'a> {
    fn new(
        input_capacity: usize,
        is_final: bool,
        filter_index: Option<u16>,
        expected_output: u64,
        budget: &'a mut DecodeBudget,
    ) -> Self {
        Self {
            bytes: Vec::new(),
            input_capacity,
            is_final,
            filter_index,
            expected_output,
            layer_output_bytes: 0,
            budget,
        }
    }

    fn consume_input<C: DecodeCancellation + ?Sized>(
        &mut self,
        cancellation: &C,
    ) -> Result<(), DecodeError> {
        self.budget.consume_input(self.filter_index, cancellation)
    }

    fn push<C: DecodeCancellation + ?Sized>(
        &mut self,
        byte: u8,
        cancellation: &C,
    ) -> Result<(), DecodeError> {
        let (attempted_layer, attempted_total) = self.budget.prepare_output(
            self.layer_output_bytes,
            self.is_final,
            self.filter_index,
            cancellation,
        )?;
        self.ensure_capacity()?;
        self.bytes.push(byte);
        self.layer_output_bytes = attempted_layer;
        self.budget.cumulative_output_bytes = attempted_total;
        Ok(())
    }

    fn ensure_capacity(&mut self) -> Result<(), DecodeError> {
        if self.bytes.len() < self.bytes.capacity() {
            return Ok(());
        }
        let required = self.bytes.len().checked_add(1).ok_or_else(|| {
            DecodeError::for_code(DecodeErrorCode::InternalState, self.filter_index)
        })?;
        let retained_limit =
            usize::try_from(self.budget.limits.max_retained_capacity_bytes).unwrap_or(usize::MAX);
        let available = retained_limit.saturating_sub(self.input_capacity);
        if required > available {
            return Err(self.retained_error(required));
        }
        let logical_ceiling = usize::try_from(self.budget.capacity_ceiling(self.is_final))
            .unwrap_or(usize::MAX)
            .min(available);
        let hint = usize::try_from(self.expected_output)
            .unwrap_or(usize::MAX)
            .max(required)
            .min(INITIAL_OUTPUT_RESERVE);
        let desired = if self.bytes.capacity() == 0 {
            hint
        } else {
            self.bytes.capacity().saturating_mul(2).max(required)
        }
        .min(logical_ceiling)
        .min(available);
        if desired < required {
            return Err(self.retained_error(required));
        }
        let additional = desired.checked_sub(self.bytes.len()).ok_or_else(|| {
            DecodeError::for_code(DecodeErrorCode::InternalState, self.filter_index)
        })?;
        let consumed = self.current_retained_u64();
        let attempted =
            u64::try_from(self.input_capacity.saturating_add(desired)).unwrap_or(u64::MAX);
        self.bytes.try_reserve_exact(additional).map_err(|_| {
            DecodeError::resource(
                DecodeLimitKind::Allocation,
                self.budget.limits.max_retained_capacity_bytes,
                consumed,
                attempted,
                self.filter_index,
            )
        })?;
        let retained = self.current_retained_u64();
        if retained > self.budget.limits.max_retained_capacity_bytes {
            return Err(DecodeError::resource(
                DecodeLimitKind::RetainedCapacityBytes,
                self.budget.limits.max_retained_capacity_bytes,
                consumed,
                retained,
                self.filter_index,
            ));
        }
        self.budget.observe_retained_capacity(retained);
        Ok(())
    }

    fn retained_error(&self, required_output_capacity: usize) -> DecodeError {
        let consumed = self.current_retained_u64();
        let attempted = u64::try_from(self.input_capacity.saturating_add(required_output_capacity))
            .unwrap_or(u64::MAX);
        DecodeError::resource(
            DecodeLimitKind::RetainedCapacityBytes,
            self.budget.limits.max_retained_capacity_bytes,
            consumed,
            attempted,
            self.filter_index,
        )
    }

    fn current_retained_u64(&self) -> u64 {
        u64::try_from(self.input_capacity.saturating_add(self.bytes.capacity())).unwrap_or(u64::MAX)
    }

    fn finish(self) -> Vec<u8> {
        self.bytes
    }
}
