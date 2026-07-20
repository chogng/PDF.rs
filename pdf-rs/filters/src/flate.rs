use crate::decode::{DecodeBudget, OutputBuffer};
use crate::{DecodeCancellation, DecodeError, DecodeErrorCode};

const MAX_CODE_BITS: usize = 15;
const MAX_HUFFMAN_SYMBOLS: usize = 288;
const MAX_HUFFMAN_NODES: usize = MAX_HUFFMAN_SYMBOLS * 2;
const NO_CHILD: u16 = u16::MAX;
const ADLER_MODULUS: u32 = 65_521;

const LENGTH_BASES: [u16; 29] = [
    3, 4, 5, 6, 7, 8, 9, 10, 11, 13, 15, 17, 19, 23, 27, 31, 35, 43, 51, 59, 67, 83, 99, 115, 131,
    163, 195, 227, 258,
];
const LENGTH_EXTRA_BITS: [u8; 29] = [
    0, 0, 0, 0, 0, 0, 0, 0, 1, 1, 1, 1, 2, 2, 2, 2, 3, 3, 3, 3, 4, 4, 4, 4, 5, 5, 5, 5, 0,
];
const DISTANCE_BASES: [u16; 30] = [
    1, 2, 3, 4, 5, 7, 9, 13, 17, 25, 33, 49, 65, 97, 129, 193, 257, 385, 513, 769, 1025, 1537,
    2049, 3073, 4097, 6145, 8193, 12_289, 16_385, 24_577,
];
const DISTANCE_EXTRA_BITS: [u8; 30] = [
    0, 0, 0, 0, 1, 1, 2, 2, 3, 3, 4, 4, 5, 5, 6, 6, 7, 7, 8, 8, 9, 9, 10, 10, 11, 11, 12, 12, 13,
    13,
];
const CODE_LENGTH_ORDER: [usize; 19] = [
    16, 17, 18, 0, 8, 7, 9, 6, 10, 5, 11, 4, 12, 3, 13, 2, 14, 1, 15,
];

pub(crate) fn decode_flate<C: DecodeCancellation + ?Sized>(
    input: &[u8],
    input_capacity: usize,
    is_final: bool,
    filter_index: u16,
    budget: &mut DecodeBudget,
    cancellation: &C,
) -> Result<Vec<u8>, DecodeError> {
    let input_len = u64::try_from(input.len()).unwrap_or(u64::MAX);
    let expected_output = input_len.saturating_mul(4);
    FlateDecoder {
        input,
        cursor: 0,
        bit_buffer: 0,
        bit_count: 0,
        output: OutputBuffer::new(
            input_capacity,
            is_final,
            Some(filter_index),
            expected_output,
            budget,
        ),
        cancellation,
        filter_index,
        window_limit: 0,
        adler_a: 1,
        adler_b: 0,
    }
    .run()
}

struct FlateDecoder<'input, 'budget, 'cancel, C: ?Sized> {
    input: &'input [u8],
    cursor: usize,
    bit_buffer: u32,
    bit_count: u8,
    output: OutputBuffer<'budget>,
    cancellation: &'cancel C,
    filter_index: u16,
    window_limit: usize,
    adler_a: u32,
    adler_b: u32,
}

impl<C: DecodeCancellation + ?Sized> FlateDecoder<'_, '_, '_, C> {
    fn run(mut self) -> Result<Vec<u8>, DecodeError> {
        let cmf = self.read_aligned_byte()?;
        let flg = self.read_aligned_byte()?;
        let header = (u16::from(cmf) << 8) | u16::from(flg);
        let compression_method = cmf & 0x0f;
        let window_code = cmf >> 4;
        if compression_method != 8 || window_code > 7 || header % 31 != 0 {
            return Err(self.invalid());
        }
        if flg & 0x20 != 0 {
            return Err(DecodeError::for_code(
                DecodeErrorCode::UnsupportedFlateDictionary,
                Some(self.filter_index),
            ));
        }
        self.window_limit = 1_usize << (usize::from(window_code) + 8);

        loop {
            self.output.charge_algorithm(1, self.cancellation)?;
            let is_final_block = self.read_bits(1)? != 0;
            match self.read_bits(2)? {
                0 => self.decode_stored_block()?,
                1 => {
                    let (literal_length, distance) = self.fixed_tables()?;
                    self.decode_compressed_block(&literal_length, &distance)?;
                }
                2 => {
                    let (literal_length, distance) = self.dynamic_tables()?;
                    self.decode_compressed_block(&literal_length, &distance)?;
                }
                _ => return Err(self.invalid()),
            }
            if is_final_block {
                break;
            }
        }

        self.align_to_byte();
        let expected_adler = u32::from_be_bytes([
            self.read_aligned_byte()?,
            self.read_aligned_byte()?,
            self.read_aligned_byte()?,
            self.read_aligned_byte()?,
        ]);
        let actual_adler = (self.adler_b << 16) | self.adler_a;
        if expected_adler != actual_adler {
            return Err(self.invalid());
        }
        if self.cursor != self.input.len() {
            let _ = self.read_aligned_byte()?;
            return Err(DecodeError::for_code(
                DecodeErrorCode::TrailingData,
                Some(self.filter_index),
            ));
        }
        Ok(self.output.finish())
    }

    fn decode_stored_block(&mut self) -> Result<(), DecodeError> {
        self.align_to_byte();
        let len = self.read_u16_le()?;
        let complement = self.read_u16_le()?;
        if len != !complement {
            return Err(self.invalid());
        }
        for _ in 0..usize::from(len) {
            let byte = self.read_aligned_byte()?;
            self.emit(byte)?;
        }
        Ok(())
    }

    fn fixed_tables(&mut self) -> Result<(Huffman, Huffman), DecodeError> {
        let mut literal_lengths = [0_u8; 288];
        fill_code_lengths(
            &mut self.output,
            self.cancellation,
            &mut literal_lengths[..144],
            8,
        )?;
        fill_code_lengths(
            &mut self.output,
            self.cancellation,
            &mut literal_lengths[144..256],
            9,
        )?;
        fill_code_lengths(
            &mut self.output,
            self.cancellation,
            &mut literal_lengths[256..280],
            7,
        )?;
        fill_code_lengths(
            &mut self.output,
            self.cancellation,
            &mut literal_lengths[280..],
            8,
        )?;
        let mut distance_lengths = [0_u8; 32];
        fill_code_lengths(
            &mut self.output,
            self.cancellation,
            &mut distance_lengths,
            5,
        )?;
        Ok((
            Huffman::build(
                &literal_lengths,
                false,
                false,
                &mut self.output,
                self.cancellation,
                self.filter_index,
            )?,
            Huffman::build(
                &distance_lengths,
                false,
                false,
                &mut self.output,
                self.cancellation,
                self.filter_index,
            )?,
        ))
    }

    fn dynamic_tables(&mut self) -> Result<(Huffman, Huffman), DecodeError> {
        let literal_count = usize::try_from(self.read_bits(5)?).unwrap_or(usize::MAX) + 257;
        let distance_count = usize::try_from(self.read_bits(5)?).unwrap_or(usize::MAX) + 1;
        let code_length_count = usize::try_from(self.read_bits(4)?).unwrap_or(usize::MAX) + 4;
        if literal_count > 286 || distance_count > 32 || code_length_count > 19 {
            return Err(self.invalid());
        }

        let mut code_length_lengths = [0_u8; 19];
        for position in CODE_LENGTH_ORDER.iter().take(code_length_count) {
            let length = u8::try_from(self.read_bits(3)?).unwrap_or(u8::MAX);
            self.output.charge_algorithm(1, self.cancellation)?;
            code_length_lengths[*position] = length;
        }
        let code_length_tree = Huffman::build(
            &code_length_lengths,
            false,
            false,
            &mut self.output,
            self.cancellation,
            self.filter_index,
        )?;

        let total = literal_count
            .checked_add(distance_count)
            .ok_or_else(|| self.invalid())?;
        let mut lengths = [0_u8; 318];
        let mut filled = 0_usize;
        while filled < total {
            let symbol = self.decode_symbol(&code_length_tree)?;
            match symbol {
                0..=15 => {
                    self.output.charge_algorithm(1, self.cancellation)?;
                    lengths[filled] = u8::try_from(symbol).unwrap_or(u8::MAX);
                    filled += 1;
                }
                16 => {
                    if filled == 0 {
                        return Err(self.invalid());
                    }
                    let repeat = usize::try_from(self.read_bits(2)?).unwrap_or(usize::MAX) + 3;
                    let end = filled.checked_add(repeat).ok_or_else(|| self.invalid())?;
                    if end > total {
                        return Err(self.invalid());
                    }
                    let previous = lengths[filled - 1];
                    fill_code_lengths(
                        &mut self.output,
                        self.cancellation,
                        &mut lengths[filled..end],
                        previous,
                    )?;
                    filled = end;
                }
                17 => {
                    let repeat = usize::try_from(self.read_bits(3)?).unwrap_or(usize::MAX) + 3;
                    filled = self.fill_zero_lengths(&mut lengths, filled, total, repeat)?;
                }
                18 => {
                    let repeat = usize::try_from(self.read_bits(7)?).unwrap_or(usize::MAX) + 11;
                    filled = self.fill_zero_lengths(&mut lengths, filled, total, repeat)?;
                }
                _ => return Err(self.invalid()),
            }
        }
        if lengths.get(256).copied().unwrap_or(0) == 0 {
            return Err(self.invalid());
        }

        let literal_length = Huffman::build(
            &lengths[..literal_count],
            false,
            true,
            &mut self.output,
            self.cancellation,
            self.filter_index,
        )?;
        let distance = Huffman::build(
            &lengths[literal_count..total],
            true,
            true,
            &mut self.output,
            self.cancellation,
            self.filter_index,
        )?;
        Ok((literal_length, distance))
    }

    fn fill_zero_lengths(
        &mut self,
        lengths: &mut [u8],
        filled: usize,
        total: usize,
        repeat: usize,
    ) -> Result<usize, DecodeError> {
        let end = filled.checked_add(repeat).ok_or_else(|| self.invalid())?;
        if end > total {
            return Err(self.invalid());
        }
        fill_code_lengths(
            &mut self.output,
            self.cancellation,
            &mut lengths[filled..end],
            0,
        )?;
        Ok(end)
    }

    fn decode_compressed_block(
        &mut self,
        literal_length: &Huffman,
        distance: &Huffman,
    ) -> Result<(), DecodeError> {
        loop {
            match self.decode_symbol(literal_length)? {
                literal @ 0..=255 => self.emit(u8::try_from(literal).unwrap_or(u8::MAX))?,
                256 => return Ok(()),
                length_symbol @ 257..=285 => {
                    let length_index = usize::from(length_symbol - 257);
                    let extra_length = self.read_bits(LENGTH_EXTRA_BITS[length_index])?;
                    let length = usize::from(LENGTH_BASES[length_index])
                        .checked_add(usize::try_from(extra_length).unwrap_or(usize::MAX))
                        .ok_or_else(|| self.invalid())?;

                    let distance_symbol = self.decode_symbol(distance)?;
                    if distance_symbol > 29 {
                        return Err(self.invalid());
                    }
                    let distance_index = usize::from(distance_symbol);
                    let extra_distance = self.read_bits(DISTANCE_EXTRA_BITS[distance_index])?;
                    let copy_distance = usize::from(DISTANCE_BASES[distance_index])
                        .checked_add(usize::try_from(extra_distance).unwrap_or(usize::MAX))
                        .ok_or_else(|| self.invalid())?;
                    if copy_distance == 0
                        || copy_distance > self.window_limit
                        || copy_distance > self.output.len()
                    {
                        return Err(self.invalid());
                    }
                    for _ in 0..length {
                        let byte = self
                            .output
                            .byte_at_distance(copy_distance)
                            .ok_or_else(|| self.invalid())?;
                        self.emit(byte)?;
                    }
                }
                _ => return Err(self.invalid()),
            }
        }
    }

    fn decode_symbol(&mut self, table: &Huffman) -> Result<u16, DecodeError> {
        if table.is_empty {
            return Err(self.invalid());
        }
        let mut node_index = 0_usize;
        for _ in 0..MAX_CODE_BITS {
            let bit = usize::try_from(self.read_bits(1)?).unwrap_or(usize::MAX);
            let node = table.nodes.get(node_index).ok_or_else(|| self.invalid())?;
            let child = *node.children.get(bit).ok_or_else(|| self.invalid())?;
            if child == NO_CHILD {
                return Err(self.invalid());
            }
            node_index = usize::from(child);
            let child_node = table.nodes.get(node_index).ok_or_else(|| self.invalid())?;
            if let Some(symbol) = child_node.symbol {
                return Ok(symbol);
            }
        }
        Err(self.invalid())
    }

    fn emit(&mut self, byte: u8) -> Result<(), DecodeError> {
        self.output.push(byte, self.cancellation)?;
        self.adler_a = (self.adler_a + u32::from(byte)) % ADLER_MODULUS;
        self.adler_b = (self.adler_b + self.adler_a) % ADLER_MODULUS;
        Ok(())
    }

    fn read_bits(&mut self, count: u8) -> Result<u32, DecodeError> {
        if count > 15 {
            return Err(self.invalid());
        }
        self.output
            .charge_algorithm(u64::from(count), self.cancellation)?;
        while self.bit_count < count {
            let byte = self.take_input_byte()?;
            self.bit_buffer |= u32::from(byte) << self.bit_count;
            self.bit_count += 8;
        }
        let mask = if count == 0 { 0 } else { (1_u32 << count) - 1 };
        let value = self.bit_buffer & mask;
        self.bit_buffer >>= count;
        self.bit_count -= count;
        Ok(value)
    }

    fn align_to_byte(&mut self) {
        self.bit_buffer = 0;
        self.bit_count = 0;
    }

    fn read_u16_le(&mut self) -> Result<u16, DecodeError> {
        Ok(u16::from_le_bytes([
            self.read_aligned_byte()?,
            self.read_aligned_byte()?,
        ]))
    }

    fn read_aligned_byte(&mut self) -> Result<u8, DecodeError> {
        if self.bit_count != 0 {
            return Err(self.invalid());
        }
        self.take_input_byte()
    }

    fn take_input_byte(&mut self) -> Result<u8, DecodeError> {
        let byte = self
            .input
            .get(self.cursor)
            .copied()
            .ok_or_else(|| self.invalid())?;
        self.output.consume_input(self.cancellation)?;
        self.cursor += 1;
        Ok(byte)
    }

    const fn invalid(&self) -> DecodeError {
        DecodeError::for_code(DecodeErrorCode::InvalidFlate, Some(self.filter_index))
    }
}

fn fill_code_lengths<C: DecodeCancellation + ?Sized>(
    output: &mut OutputBuffer<'_>,
    cancellation: &C,
    lengths: &mut [u8],
    value: u8,
) -> Result<(), DecodeError> {
    for length in lengths {
        output.charge_algorithm(1, cancellation)?;
        *length = value;
    }
    Ok(())
}

#[derive(Clone, Copy)]
struct HuffmanNode {
    children: [u16; 2],
    symbol: Option<u16>,
}

impl HuffmanNode {
    const EMPTY: Self = Self {
        children: [NO_CHILD; 2],
        symbol: None,
    };
}

struct Huffman {
    nodes: [HuffmanNode; MAX_HUFFMAN_NODES],
    is_empty: bool,
}

impl Huffman {
    fn build<C: DecodeCancellation + ?Sized>(
        lengths: &[u8],
        allow_empty: bool,
        allow_single_incomplete: bool,
        output: &mut OutputBuffer<'_>,
        cancellation: &C,
        filter_index: u16,
    ) -> Result<Self, DecodeError> {
        if lengths.len() > MAX_HUFFMAN_SYMBOLS {
            return Err(DecodeError::for_code(
                DecodeErrorCode::InvalidFlate,
                Some(filter_index),
            ));
        }
        let mut counts = [0_u16; MAX_CODE_BITS + 1];
        let mut symbol_count = 0_u32;
        let mut max_length = 0_usize;
        for length in lengths {
            output.charge_algorithm(1, cancellation)?;
            if usize::from(*length) > MAX_CODE_BITS {
                return Err(DecodeError::for_code(
                    DecodeErrorCode::InvalidFlate,
                    Some(filter_index),
                ));
            }
            if *length != 0 {
                let length = usize::from(*length);
                counts[length] += 1;
                symbol_count = symbol_count.checked_add(1).ok_or_else(|| {
                    DecodeError::for_code(DecodeErrorCode::InvalidFlate, Some(filter_index))
                })?;
                max_length = max_length.max(length);
            }
        }
        if symbol_count == 0 {
            if allow_empty {
                return Ok(Self {
                    nodes: [HuffmanNode::EMPTY; MAX_HUFFMAN_NODES],
                    is_empty: true,
                });
            }
            return Err(DecodeError::for_code(
                DecodeErrorCode::InvalidFlate,
                Some(filter_index),
            ));
        }

        let mut remaining = 1_i32;
        for count in counts.iter().skip(1) {
            output.charge_algorithm(1, cancellation)?;
            remaining = remaining
                .checked_mul(2)
                .and_then(|value| value.checked_sub(i32::from(*count)))
                .ok_or_else(|| {
                    DecodeError::for_code(DecodeErrorCode::InvalidFlate, Some(filter_index))
                })?;
            if remaining < 0 {
                return Err(DecodeError::for_code(
                    DecodeErrorCode::InvalidFlate,
                    Some(filter_index),
                ));
            }
        }
        if remaining > 0 && !(allow_single_incomplete && symbol_count == 1 && max_length == 1) {
            return Err(DecodeError::for_code(
                DecodeErrorCode::InvalidFlate,
                Some(filter_index),
            ));
        }

        let mut next_code = [0_u16; MAX_CODE_BITS + 1];
        let mut code = 0_u16;
        for bits in 1..=MAX_CODE_BITS {
            output.charge_algorithm(1, cancellation)?;
            code = code
                .checked_add(counts[bits - 1])
                .and_then(|value| value.checked_mul(2))
                .ok_or_else(|| {
                    DecodeError::for_code(DecodeErrorCode::InvalidFlate, Some(filter_index))
                })?;
            next_code[bits] = code;
        }

        let mut table = Self {
            nodes: [HuffmanNode::EMPTY; MAX_HUFFMAN_NODES],
            is_empty: false,
        };
        let mut node_count = 1_usize;
        for (symbol, length) in lengths.iter().copied().enumerate() {
            if length == 0 {
                continue;
            }
            let length_index = usize::from(length);
            let canonical = next_code[length_index];
            next_code[length_index] = canonical.checked_add(1).ok_or_else(|| {
                DecodeError::for_code(DecodeErrorCode::InvalidFlate, Some(filter_index))
            })?;
            let mut node_index = 0_usize;
            for depth in 0..length_index {
                output.charge_algorithm(1, cancellation)?;
                let shift = length_index - depth - 1;
                let bit = usize::from((canonical >> shift) & 1);
                let is_leaf = depth + 1 == length_index;
                let existing = table.nodes[node_index].children[bit];
                if existing == NO_CHILD {
                    if node_count >= MAX_HUFFMAN_NODES {
                        return Err(DecodeError::for_code(
                            DecodeErrorCode::InvalidFlate,
                            Some(filter_index),
                        ));
                    }
                    let new_index = u16::try_from(node_count).map_err(|_| {
                        DecodeError::for_code(DecodeErrorCode::InvalidFlate, Some(filter_index))
                    })?;
                    table.nodes[node_index].children[bit] = new_index;
                    node_index = node_count;
                    node_count += 1;
                    if is_leaf {
                        table.nodes[node_index].symbol =
                            Some(u16::try_from(symbol).map_err(|_| {
                                DecodeError::for_code(
                                    DecodeErrorCode::InvalidFlate,
                                    Some(filter_index),
                                )
                            })?);
                    }
                } else {
                    node_index = usize::from(existing);
                    if is_leaf || table.nodes[node_index].symbol.is_some() {
                        return Err(DecodeError::for_code(
                            DecodeErrorCode::InvalidFlate,
                            Some(filter_index),
                        ));
                    }
                }
            }
        }
        Ok(table)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::{Huffman, fill_code_lengths};
    use crate::decode::{DecodeBudget, OutputBuffer};
    use crate::{
        DecodeCancellation, DecodeErrorCode, DecodeFuelScheduleVersion, DecodeLimitConfig,
        DecodeLimits, NeverCancelled,
    };

    struct CancelOnCall {
        calls: AtomicUsize,
        call: usize,
    }

    impl DecodeCancellation for CancelOnCall {
        fn is_cancelled(&self) -> bool {
            self.calls.fetch_add(1, Ordering::SeqCst) + 1 >= self.call
        }
    }

    fn build(
        lengths: &[u8],
        allow_empty: bool,
        allow_single_incomplete: bool,
    ) -> Result<Huffman, crate::DecodeError> {
        let mut budget =
            DecodeBudget::new(DecodeLimits::default(), DecodeFuelScheduleVersion::M1V1);
        let mut output = OutputBuffer::new(0, true, Some(0), 1, &mut budget);
        Huffman::build(
            lengths,
            allow_empty,
            allow_single_incomplete,
            &mut output,
            &NeverCancelled,
            0,
        )
    }

    #[test]
    fn incomplete_and_oversubscribed_tables_have_only_bounded_rfc_exceptions() {
        for lengths in [&[1_u8][..], &[2, 2][..], &[1, 1, 1][..]] {
            let error = build(lengths, false, false)
                .err()
                .expect("code-length and complete tables reject malformed code space");
            assert_eq!(error.code(), DecodeErrorCode::InvalidFlate);
        }

        assert!(build(&[1], false, true).is_ok());
        assert!(build(&[1, 1], false, false).is_ok());
        assert!(build(&[], true, true).unwrap().is_empty);
        assert!(build(&[1], true, true).is_ok());
        assert_eq!(
            build(&[2], true, true).err().unwrap().code(),
            DecodeErrorCode::InvalidFlate
        );
    }

    #[test]
    fn code_length_fill_charges_and_probes_before_each_mutation() {
        let config = DecodeLimitConfig {
            cancellation_check_interval_fuel: 1,
            ..DecodeLimitConfig::default()
        };
        let limits = DecodeLimits::validate(config).unwrap();
        let mut budget = DecodeBudget::new(limits, DecodeFuelScheduleVersion::M1V1);
        let mut output = OutputBuffer::new(0, true, Some(0), 1, &mut budget);
        let cancellation = CancelOnCall {
            calls: AtomicUsize::new(0),
            call: 2,
        };
        let mut lengths = [0_u8; 138];

        let error = fill_code_lengths(&mut output, &cancellation, &mut lengths, 7).unwrap_err();

        assert_eq!(error.code(), DecodeErrorCode::Cancelled);
        assert_eq!(cancellation.calls.load(Ordering::SeqCst), 2);
        assert_eq!(lengths[0], 7);
        assert!(lengths[1..].iter().all(|length| *length == 0));
    }
}
