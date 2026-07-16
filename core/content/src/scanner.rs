use std::mem::size_of;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use crate::{
    ContentDictionaryEntry, ContentError, ContentErrorCode, ContentExtent, ContentLimitKind,
    ContentLimits, ContentName, ContentOperand, ContentOperator, ContentOperatorSource,
    ContentPosition, ContentProgram, ContentReal, ContentScanStats, ContentString,
    ContentStringKind, DecodedContentStream, DecodedSpan, LocatedOperand, OperatorKind,
    ScannedOperator,
};

const CANCELLATION_PROBE_INTERVAL: u64 = 256;

/// Cooperative cancellation supplied by the owning runtime.
pub trait ContentCancellation: Send + Sync {
    /// Reports whether scanning must stop before publication.
    fn is_cancelled(&self) -> bool;
}

/// Cancellation probe that never stops scanning.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct NeverCancelled;

impl ContentCancellation for NeverCancelled {
    fn is_cancelled(&self) -> bool {
        false
    }
}

impl ContentCancellation for AtomicBool {
    fn is_cancelled(&self) -> bool {
        self.load(Ordering::Acquire)
    }
}

/// Observable terminal phase of a content scan job.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ContentScanPhase {
    /// No scan has run yet.
    Pending,
    /// A complete immutable program was published.
    Ready,
    /// Scanning failed terminally without partial publication.
    Failed,
}

/// One replayable content scan outcome.
#[derive(Clone, Debug)]
pub enum ContentScanPoll {
    /// Complete immutable program.
    Ready(Arc<ContentProgram>),
    /// Terminal structured failure.
    Failed(ContentError),
}

enum JobState {
    Pending,
    Ready(Arc<ContentProgram>),
    Failed(ContentError),
}

/// Single-owner, terminal-replay content scanner.
pub struct ContentScanJob<'a> {
    streams: &'a [DecodedContentStream<'a>],
    limits: ContentLimits,
    state: JobState,
    stats: ContentScanStats,
}

impl<'a> ContentScanJob<'a> {
    /// Validates ordered stream metadata and creates a pending job.
    pub fn new(
        streams: &'a [DecodedContentStream<'a>],
        limits: ContentLimits,
    ) -> Result<Self, ContentError> {
        validate_stream_order(streams)?;
        Ok(Self {
            streams,
            limits,
            state: JobState::Pending,
            stats: ContentScanStats::default(),
        })
    }

    /// Returns the current terminal-or-pending phase.
    pub const fn phase(&self) -> ContentScanPhase {
        match self.state {
            JobState::Pending => ContentScanPhase::Pending,
            JobState::Ready(_) => ContentScanPhase::Ready,
            JobState::Failed(_) => ContentScanPhase::Failed,
        }
    }

    /// Returns work charged by the initial attempt or its terminal replay.
    pub const fn stats(&self) -> ContentScanStats {
        self.stats
    }

    /// Runs once and then replays the exact terminal outcome without further work.
    pub fn poll(&mut self, cancellation: &dyn ContentCancellation) -> ContentScanPoll {
        match &self.state {
            JobState::Ready(program) => return ContentScanPoll::Ready(Arc::clone(program)),
            JobState::Failed(error) => return ContentScanPoll::Failed(*error),
            JobState::Pending => {}
        }
        match run_scan(self.streams, self.limits, cancellation) {
            RunOutcome::Ready(program) => {
                self.stats = program.stats();
                let program = Arc::new(program);
                self.state = JobState::Ready(Arc::clone(&program));
                ContentScanPoll::Ready(program)
            }
            RunOutcome::Failed { error, stats } => {
                self.stats = stats;
                self.state = JobState::Failed(error);
                ContentScanPoll::Failed(error)
            }
        }
    }
}

/// Scans one ordered stream sequence without retaining a job after completion.
pub fn scan_content_streams(
    streams: &[DecodedContentStream<'_>],
    limits: ContentLimits,
    cancellation: &dyn ContentCancellation,
) -> Result<ContentProgram, ContentError> {
    validate_stream_order(streams)?;
    match run_scan(streams, limits, cancellation) {
        RunOutcome::Ready(program) => Ok(program),
        RunOutcome::Failed { error, .. } => Err(error),
    }
}

fn validate_stream_order(streams: &[DecodedContentStream<'_>]) -> Result<(), ContentError> {
    for (index, stream) in streams.iter().enumerate() {
        let ordinal = u32::try_from(index)
            .map_err(|_| ContentError::for_code(ContentErrorCode::InvalidStreamOrder, None))?;
        if stream.ordinal() != ordinal {
            return Err(ContentError::for_code(
                ContentErrorCode::InvalidStreamOrder,
                None,
            ));
        }
    }
    Ok(())
}

fn run_scan(
    streams: &[DecodedContentStream<'_>],
    limits: ContentLimits,
    cancellation: &dyn ContentCancellation,
) -> RunOutcome {
    let mut runner = Runner::new(streams, limits, cancellation);
    match runner.scan() {
        Ok(program) => RunOutcome::Ready(program),
        Err(error) => RunOutcome::Failed {
            error,
            stats: runner.stats,
        },
    }
}

enum RunOutcome {
    Ready(ContentProgram),
    Failed {
        error: ContentError,
        stats: ContentScanStats,
    },
}

#[derive(Clone, Copy)]
struct RawToken {
    stream_index: usize,
    start: usize,
    end: usize,
    span: DecodedSpan,
}

enum ParsedNumber {
    Integer(i64),
    Real,
}

struct Runner<'a> {
    streams: &'a [DecodedContentStream<'a>],
    limits: ContentLimits,
    cancellation: &'a dyn ContentCancellation,
    stream_index: usize,
    offset: usize,
    stats: ContentScanStats,
}

impl<'a> Runner<'a> {
    const fn new(
        streams: &'a [DecodedContentStream<'a>],
        limits: ContentLimits,
        cancellation: &'a dyn ContentCancellation,
    ) -> Self {
        Self {
            streams,
            limits,
            cancellation,
            stream_index: 0,
            offset: 0,
            stats: ContentScanStats {
                streams: 0,
                total_decoded_bytes: 0,
                tokens: 0,
                max_token_bytes: 0,
                operands: 0,
                max_operands_per_operator: 0,
                max_nesting_depth: 0,
                operators: 0,
                unknown_operators: 0,
                fuel: 0,
                retained_bytes: 0,
            },
        }
    }

    fn scan(&mut self) -> Result<ContentProgram, ContentError> {
        self.check_cancelled(None)?;
        self.admit_inputs()?;

        let mut operators = Vec::new();
        let mut pending = Vec::new();
        loop {
            self.skip_trivia()?;
            if self.at_end() {
                break;
            }

            if self.next_begins_complex_operand() {
                let operand = self.parse_operand(0)?;
                self.push_pending(&mut pending, operand)?;
                continue;
            }

            let byte = self
                .peek()
                .ok_or_else(|| ContentError::for_code(ContentErrorCode::InternalState, None))?;
            if is_number_start(byte) || matches!(byte, b'n' | b't' | b'f') {
                let token = self.scan_regular()?;
                if let Some(operand) = self.primitive_operand(token)? {
                    self.record_operand(Some(operand.extent().start()))?;
                    self.push_pending(&mut pending, operand)?;
                } else {
                    self.publish_operator(&mut operators, &mut pending, token)?;
                }
            } else if is_delimiter(byte) {
                return Err(ContentError::for_code(
                    if matches!(byte, b']' | b'>') {
                        ContentErrorCode::MismatchedDelimiter
                    } else {
                        ContentErrorCode::MalformedToken
                    },
                    self.current_position(),
                ));
            } else {
                let token = self.scan_regular()?;
                self.publish_operator(&mut operators, &mut pending, token)?;
            }
        }

        if !pending.is_empty() {
            return Err(ContentError::for_code(
                ContentErrorCode::DanglingOperands,
                self.current_position().or_else(|| self.final_position()),
            ));
        }
        self.check_cancelled(self.final_position())?;
        Ok(ContentProgram::new(operators, self.limits, self.stats))
    }

    fn admit_inputs(&mut self) -> Result<(), ContentError> {
        let stream_count = u64::try_from(self.streams.len())
            .map_err(|_| ContentError::for_code(ContentErrorCode::InternalState, None))?;
        if stream_count > u64::from(self.limits.max_streams()) {
            return Err(ContentError::resource(
                ContentLimitKind::Streams,
                u64::from(self.limits.max_streams()),
                0,
                stream_count,
                None,
            ));
        }
        self.stats.streams = u32::try_from(stream_count)
            .map_err(|_| ContentError::for_code(ContentErrorCode::InternalState, None))?;

        let mut total = 0_u64;
        for stream in self.streams {
            let len = u64::try_from(stream.decoded().len())
                .map_err(|_| ContentError::for_code(ContentErrorCode::InternalState, None))?;
            let next = total
                .checked_add(len)
                .ok_or_else(|| ContentError::for_code(ContentErrorCode::InternalState, None))?;
            if next > self.limits.max_total_decoded_bytes() {
                return Err(ContentError::resource(
                    ContentLimitKind::TotalDecodedBytes,
                    self.limits.max_total_decoded_bytes(),
                    total,
                    len,
                    Some(ContentPosition::new(stream.object(), stream.ordinal(), 0)),
                ));
            }
            total = next;
        }
        self.stats.total_decoded_bytes = total;
        Ok(())
    }

    fn push_pending(
        &mut self,
        pending: &mut Vec<LocatedOperand>,
        operand: LocatedOperand,
    ) -> Result<(), ContentError> {
        let count = u64::try_from(pending.len())
            .map_err(|_| ContentError::for_code(ContentErrorCode::InternalState, None))?;
        if count >= u64::from(self.limits.max_operands_per_operator()) {
            return Err(ContentError::resource(
                ContentLimitKind::OperandsPerOperator,
                u64::from(self.limits.max_operands_per_operator()),
                count,
                1,
                Some(operand.extent().start()),
            ));
        }
        self.reserve_slot(pending, Some(operand.extent().start()))?;
        pending.push(operand);
        self.stats.max_operands_per_operator = self.stats.max_operands_per_operator.max(
            u32::try_from(pending.len())
                .map_err(|_| ContentError::for_code(ContentErrorCode::InternalState, None))?,
        );
        Ok(())
    }

    fn publish_operator(
        &mut self,
        operators: &mut Vec<ScannedOperator>,
        pending: &mut Vec<LocatedOperand>,
        token: RawToken,
    ) -> Result<(), ContentError> {
        if self.stats.operators >= self.limits.max_operators() {
            return Err(ContentError::resource(
                ContentLimitKind::Operators,
                self.limits.max_operators(),
                self.stats.operators,
                1,
                Some(token.span.start()),
            ));
        }

        let known = {
            let raw = self.raw(token);
            OperatorKind::from_token(raw)
        };
        let operator = if let Some(kind) = known {
            ContentOperator::Known(kind)
        } else {
            let raw = self.copy_token_bytes(token)?;
            self.stats.unknown_operators =
                self.stats.unknown_operators.checked_add(1).ok_or_else(|| {
                    ContentError::for_code(
                        ContentErrorCode::InternalState,
                        Some(token.span.start()),
                    )
                })?;
            ContentOperator::Unknown(raw)
        };

        self.reserve_slot(operators, Some(token.span.start()))?;
        let operands = std::mem::take(pending);
        operators.push(ScannedOperator::new(
            operator,
            operands,
            ContentOperatorSource::new(token.span, self.stats.operators),
        ));
        self.stats.operators = self.stats.operators.checked_add(1).ok_or_else(|| {
            ContentError::for_code(ContentErrorCode::InternalState, Some(token.span.start()))
        })?;
        Ok(())
    }

    fn parse_operand(&mut self, depth: u16) -> Result<LocatedOperand, ContentError> {
        self.skip_trivia()?;
        let position = self.current_position();
        let byte = self.peek().ok_or_else(|| {
            ContentError::for_code(ContentErrorCode::MismatchedDelimiter, position)
        })?;
        let operand = match byte {
            b'[' => self.parse_array(depth)?,
            b'<' if self.peek_next() == Some(b'<') => self.parse_dictionary(depth)?,
            b'(' => self.parse_literal_string()?,
            b'<' => self.parse_hex_string()?,
            b'/' => self.parse_name_operand()?,
            b']' | b'>' => {
                return Err(ContentError::for_code(
                    ContentErrorCode::MismatchedDelimiter,
                    position,
                ));
            }
            b')' | b'{' | b'}' => {
                return Err(ContentError::for_code(
                    ContentErrorCode::MalformedToken,
                    position,
                ));
            }
            _ => {
                let token = self.scan_regular()?;
                self.primitive_operand(token)?.ok_or_else(|| {
                    ContentError::for_code(
                        ContentErrorCode::MalformedToken,
                        Some(token.span.start()),
                    )
                })?
            }
        };
        self.record_operand(position)?;
        Ok(operand)
    }

    fn record_operand(&mut self, position: Option<ContentPosition>) -> Result<(), ContentError> {
        self.stats.operands = self
            .stats
            .operands
            .checked_add(1)
            .ok_or_else(|| ContentError::for_code(ContentErrorCode::InternalState, position))?;
        Ok(())
    }

    fn primitive_operand(
        &mut self,
        token: RawToken,
    ) -> Result<Option<LocatedOperand>, ContentError> {
        let raw = self.raw(token);
        let value = match raw {
            b"null" => Some(ContentOperand::Null),
            b"true" => Some(ContentOperand::Boolean(true)),
            b"false" => Some(ContentOperand::Boolean(false)),
            _ if is_number_start(raw[0]) => match parse_number(raw) {
                Ok(ParsedNumber::Integer(value)) => Some(ContentOperand::Integer(value)),
                Ok(ParsedNumber::Real) => {
                    let raw = self.copy_token_bytes(token)?;
                    Some(ContentOperand::Real(ContentReal::new(raw)))
                }
                Err(()) => {
                    return Err(ContentError::for_code(
                        ContentErrorCode::InvalidNumber,
                        Some(token.span.start()),
                    ));
                }
            },
            _ => None,
        };
        Ok(value.map(|value| LocatedOperand::new(ContentExtent::from_span(token.span), value)))
    }

    fn parse_array(&mut self, parent_depth: u16) -> Result<LocatedOperand, ContentError> {
        let depth = self.enter_container(parent_depth)?;
        let open = self.scan_fixed(1)?;
        let mut values = Vec::new();
        loop {
            self.skip_trivia()?;
            let Some(byte) = self.peek() else {
                return Err(ContentError::for_code(
                    ContentErrorCode::MismatchedDelimiter,
                    self.final_position(),
                ));
            };
            if byte == b']' {
                let close = self.scan_fixed(1)?;
                let extent = ContentExtent::new(open.span.start(), close.span.end_exclusive());
                return Ok(LocatedOperand::new(extent, ContentOperand::Array(values)));
            }
            if byte == b'>' && self.peek_next() == Some(b'>') {
                return Err(ContentError::for_code(
                    ContentErrorCode::MismatchedDelimiter,
                    self.current_position(),
                ));
            }
            let value = self.parse_operand(depth)?;
            self.reserve_slot(&mut values, Some(value.extent().start()))?;
            values.push(value);
        }
    }

    fn parse_dictionary(&mut self, parent_depth: u16) -> Result<LocatedOperand, ContentError> {
        let depth = self.enter_container(parent_depth)?;
        let open = self.scan_fixed(2)?;
        let mut entries = Vec::new();
        loop {
            self.skip_trivia()?;
            let Some(byte) = self.peek() else {
                return Err(ContentError::for_code(
                    ContentErrorCode::MismatchedDelimiter,
                    self.final_position(),
                ));
            };
            if byte == b'>' && self.peek_next() == Some(b'>') {
                let close = self.scan_fixed(2)?;
                let extent = ContentExtent::new(open.span.start(), close.span.end_exclusive());
                return Ok(LocatedOperand::new(
                    extent,
                    ContentOperand::Dictionary(entries),
                ));
            }
            if byte == b']' {
                return Err(ContentError::for_code(
                    ContentErrorCode::MismatchedDelimiter,
                    self.current_position(),
                ));
            }
            if byte != b'/' {
                return Err(ContentError::for_code(
                    ContentErrorCode::InvalidDictionaryKey,
                    self.current_position(),
                ));
            }
            let (key_span, key) = self.scan_name()?;
            let value = self.parse_operand(depth)?;
            self.reserve_slot(&mut entries, Some(key_span.start()))?;
            entries.push(ContentDictionaryEntry::new(key_span, key, value));
        }
    }

    fn enter_container(&mut self, parent_depth: u16) -> Result<u16, ContentError> {
        let depth = parent_depth.checked_add(1).ok_or_else(|| {
            ContentError::for_code(ContentErrorCode::InternalState, self.current_position())
        })?;
        if depth > self.limits.max_nesting_depth() {
            return Err(ContentError::resource(
                ContentLimitKind::NestingDepth,
                u64::from(self.limits.max_nesting_depth()),
                u64::from(parent_depth),
                1,
                self.current_position(),
            ));
        }
        self.stats.max_nesting_depth = self.stats.max_nesting_depth.max(depth);
        Ok(depth)
    }

    fn parse_name_operand(&mut self) -> Result<LocatedOperand, ContentError> {
        let (span, name) = self.scan_name()?;
        Ok(LocatedOperand::new(
            ContentExtent::from_span(span),
            ContentOperand::Name(name),
        ))
    }

    fn scan_name(&mut self) -> Result<(DecodedSpan, ContentName), ContentError> {
        let stream_index = self.stream_index;
        let start = self.offset;
        let bytes = self.stream_bytes(stream_index);
        let mut index = start;
        if bytes.get(index) != Some(&b'/') {
            return Err(ContentError::for_code(
                ContentErrorCode::InternalState,
                self.current_position(),
            ));
        }
        self.charge_fuel(1, self.position(stream_index, index))?;
        index += 1;
        let mut decoded_len = 0_usize;
        while let Some(&byte) = bytes.get(index) {
            if is_delimiter(byte) {
                break;
            }
            self.charge_fuel(1, self.position(stream_index, index))?;
            if byte == b'#' {
                let Some(high) = bytes.get(index + 1).and_then(|value| hex_value(*value)) else {
                    return Err(ContentError::for_code(
                        ContentErrorCode::InvalidNameEscape,
                        self.position(stream_index, index),
                    ));
                };
                let Some(low) = bytes.get(index + 2).and_then(|value| hex_value(*value)) else {
                    return Err(ContentError::for_code(
                        ContentErrorCode::InvalidNameEscape,
                        self.position(stream_index, index),
                    ));
                };
                let _ = (high, low);
                self.charge_fuel(2, self.position(stream_index, index + 1))?;
                index += 3;
            } else {
                index += 1;
            }
            decoded_len = decoded_len.checked_add(1).ok_or_else(|| {
                ContentError::for_code(
                    ContentErrorCode::InternalState,
                    self.position(stream_index, start),
                )
            })?;
        }
        self.offset = index;
        let span = self.make_span(stream_index, start, index)?;
        self.charge_token(span)?;
        let mut decoded = self.allocate_bytes(decoded_len, Some(span.start()))?;
        let mut source = start + 1;
        while source < index {
            if bytes[source] == b'#' {
                let high = hex_value(bytes[source + 1]).expect("validated name escape");
                let low = hex_value(bytes[source + 2]).expect("validated name escape");
                decoded.push((high << 4) | low);
                source += 3;
            } else {
                decoded.push(bytes[source]);
                source += 1;
            }
        }
        Ok((span, ContentName::new(decoded)))
    }

    fn parse_literal_string(&mut self) -> Result<LocatedOperand, ContentError> {
        let stream_index = self.stream_index;
        let start = self.offset;
        let bytes = self.stream_bytes(stream_index);
        let mut index = start;
        self.charge_fuel(1, self.position(stream_index, index))?;
        index += 1;
        let mut nesting = 1_u32;
        let mut decoded_len = 0_usize;
        let end = loop {
            let Some(&byte) = bytes.get(index) else {
                return Err(ContentError::for_code(
                    ContentErrorCode::UnterminatedString,
                    self.position(stream_index, index),
                ));
            };
            self.charge_fuel(1, self.position(stream_index, index))?;
            match byte {
                b'\\' => {
                    index += 1;
                    let Some(&escaped) = bytes.get(index) else {
                        return Err(ContentError::for_code(
                            ContentErrorCode::UnterminatedString,
                            self.position(stream_index, index),
                        ));
                    };
                    self.charge_fuel(1, self.position(stream_index, index))?;
                    if escaped == b'\r' {
                        index += 1;
                        if bytes.get(index) == Some(&b'\n') {
                            self.charge_fuel(1, self.position(stream_index, index))?;
                            index += 1;
                        }
                    } else if escaped == b'\n' {
                        index += 1;
                    } else if is_octal(escaped) {
                        index += 1;
                        for _ in 0..2 {
                            if bytes.get(index).is_some_and(|value| is_octal(*value)) {
                                self.charge_fuel(1, self.position(stream_index, index))?;
                                index += 1;
                            } else {
                                break;
                            }
                        }
                        decoded_len = checked_inc(decoded_len, self.position(stream_index, start))?;
                    } else {
                        index += 1;
                        decoded_len = checked_inc(decoded_len, self.position(stream_index, start))?;
                    }
                }
                b'(' => {
                    nesting = nesting.checked_add(1).ok_or_else(|| {
                        ContentError::for_code(
                            ContentErrorCode::InternalState,
                            self.position(stream_index, index),
                        )
                    })?;
                    index += 1;
                    decoded_len = checked_inc(decoded_len, self.position(stream_index, start))?;
                }
                b')' => {
                    nesting -= 1;
                    index += 1;
                    if nesting == 0 {
                        break index;
                    }
                    decoded_len = checked_inc(decoded_len, self.position(stream_index, start))?;
                }
                b'\r' => {
                    index += 1;
                    if bytes.get(index) == Some(&b'\n') {
                        self.charge_fuel(1, self.position(stream_index, index))?;
                        index += 1;
                    }
                    decoded_len = checked_inc(decoded_len, self.position(stream_index, start))?;
                }
                _ => {
                    index += 1;
                    decoded_len = checked_inc(decoded_len, self.position(stream_index, start))?;
                }
            }
        };
        self.offset = end;
        let span = self.make_span(stream_index, start, end)?;
        self.charge_token(span)?;
        let mut decoded = self.allocate_bytes(decoded_len, Some(span.start()))?;
        decode_literal(&bytes[start + 1..end - 1], &mut decoded);
        Ok(LocatedOperand::new(
            ContentExtent::from_span(span),
            ContentOperand::String(ContentString::new(decoded, ContentStringKind::Literal)),
        ))
    }

    fn parse_hex_string(&mut self) -> Result<LocatedOperand, ContentError> {
        let stream_index = self.stream_index;
        let start = self.offset;
        let bytes = self.stream_bytes(stream_index);
        let mut index = start;
        self.charge_fuel(1, self.position(stream_index, index))?;
        index += 1;
        let mut nibbles = 0_usize;
        let end = loop {
            let Some(&byte) = bytes.get(index) else {
                return Err(ContentError::for_code(
                    ContentErrorCode::UnterminatedString,
                    self.position(stream_index, index),
                ));
            };
            self.charge_fuel(1, self.position(stream_index, index))?;
            index += 1;
            if byte == b'>' {
                break index;
            }
            if is_pdf_whitespace(byte) {
                continue;
            }
            if hex_value(byte).is_none() {
                return Err(ContentError::for_code(
                    ContentErrorCode::InvalidHexString,
                    self.position(stream_index, index - 1),
                ));
            }
            nibbles = checked_inc(nibbles, self.position(stream_index, start))?;
        };
        self.offset = end;
        let span = self.make_span(stream_index, start, end)?;
        self.charge_token(span)?;
        let decoded_len = nibbles.checked_add(1).ok_or_else(|| {
            ContentError::for_code(ContentErrorCode::InternalState, Some(span.start()))
        })? / 2;
        let mut decoded = self.allocate_bytes(decoded_len, Some(span.start()))?;
        decode_hex(&bytes[start + 1..end - 1], &mut decoded);
        Ok(LocatedOperand::new(
            ContentExtent::from_span(span),
            ContentOperand::String(ContentString::new(decoded, ContentStringKind::Hexadecimal)),
        ))
    }

    fn next_begins_complex_operand(&self) -> bool {
        matches!(self.peek(), Some(b'[' | b'(' | b'/')) || self.peek() == Some(b'<')
    }

    fn scan_regular(&mut self) -> Result<RawToken, ContentError> {
        let stream_index = self.stream_index;
        let start = self.offset;
        let bytes = self.stream_bytes(stream_index);
        let mut end = start;
        while let Some(&byte) = bytes.get(end) {
            if is_delimiter(byte) {
                break;
            }
            self.charge_fuel(1, self.position(stream_index, end))?;
            end += 1;
        }
        if end == start {
            return Err(ContentError::for_code(
                ContentErrorCode::MalformedToken,
                self.position(stream_index, start),
            ));
        }
        self.offset = end;
        let span = self.make_span(stream_index, start, end)?;
        self.charge_token(span)?;
        Ok(RawToken {
            stream_index,
            start,
            end,
            span,
        })
    }

    fn scan_fixed(&mut self, len: usize) -> Result<RawToken, ContentError> {
        let stream_index = self.stream_index;
        let start = self.offset;
        let end = start.checked_add(len).ok_or_else(|| {
            ContentError::for_code(ContentErrorCode::InternalState, self.current_position())
        })?;
        if end > self.stream_bytes(stream_index).len() {
            return Err(ContentError::for_code(
                ContentErrorCode::MismatchedDelimiter,
                self.current_position(),
            ));
        }
        self.charge_fuel(
            u64::try_from(len).map_err(|_| {
                ContentError::for_code(ContentErrorCode::InternalState, self.current_position())
            })?,
            self.current_position(),
        )?;
        self.offset = end;
        let span = self.make_span(stream_index, start, end)?;
        self.charge_token(span)?;
        Ok(RawToken {
            stream_index,
            start,
            end,
            span,
        })
    }

    fn skip_trivia(&mut self) -> Result<(), ContentError> {
        loop {
            self.advance_boundaries();
            let Some(byte) = self.peek() else {
                return Ok(());
            };
            if is_pdf_whitespace(byte) {
                self.charge_fuel(1, self.current_position())?;
                self.offset += 1;
                continue;
            }
            if byte == b'%' {
                while let Some(current) = self.peek() {
                    self.charge_fuel(1, self.current_position())?;
                    self.offset += 1;
                    if matches!(current, b'\r' | b'\n') {
                        break;
                    }
                }
                continue;
            }
            return Ok(());
        }
    }

    fn charge_token(&mut self, span: DecodedSpan) -> Result<(), ContentError> {
        if span.decoded_len() > self.limits.max_token_bytes() {
            return Err(ContentError::resource(
                ContentLimitKind::TokenBytes,
                self.limits.max_token_bytes(),
                0,
                span.decoded_len(),
                Some(span.start()),
            ));
        }
        if self.stats.tokens >= self.limits.max_tokens() {
            return Err(ContentError::resource(
                ContentLimitKind::Tokens,
                self.limits.max_tokens(),
                self.stats.tokens,
                1,
                Some(span.start()),
            ));
        }
        self.charge_fuel(1, Some(span.start()))?;
        self.stats.tokens = self.stats.tokens.checked_add(1).ok_or_else(|| {
            ContentError::for_code(ContentErrorCode::InternalState, Some(span.start()))
        })?;
        self.stats.max_token_bytes = self.stats.max_token_bytes.max(span.decoded_len());
        Ok(())
    }

    fn charge_fuel(
        &mut self,
        amount: u64,
        position: Option<ContentPosition>,
    ) -> Result<(), ContentError> {
        let next = self
            .stats
            .fuel
            .checked_add(amount)
            .ok_or_else(|| ContentError::for_code(ContentErrorCode::InternalState, position))?;
        if self.stats.fuel.is_multiple_of(CANCELLATION_PROBE_INTERVAL)
            || next / CANCELLATION_PROBE_INTERVAL > self.stats.fuel / CANCELLATION_PROBE_INTERVAL
        {
            self.check_cancelled(position)?;
        }
        if next > self.limits.max_fuel() {
            return Err(ContentError::resource(
                ContentLimitKind::Fuel,
                self.limits.max_fuel(),
                self.stats.fuel,
                amount,
                position,
            ));
        }
        self.stats.fuel = next;
        Ok(())
    }

    fn check_cancelled(&self, position: Option<ContentPosition>) -> Result<(), ContentError> {
        if self.cancellation.is_cancelled() {
            Err(ContentError::for_code(
                ContentErrorCode::Cancelled,
                position,
            ))
        } else {
            Ok(())
        }
    }

    fn reserve_slot<T>(
        &mut self,
        values: &mut Vec<T>,
        position: Option<ContentPosition>,
    ) -> Result<(), ContentError> {
        let needed = values
            .len()
            .checked_add(1)
            .ok_or_else(|| ContentError::for_code(ContentErrorCode::InternalState, position))?;
        if needed <= values.capacity() {
            return Ok(());
        }
        let desired = values
            .capacity()
            .max(1)
            .checked_mul(2)
            .map(|value| value.max(needed))
            .ok_or_else(|| ContentError::for_code(ContentErrorCode::InternalState, position))?;
        let additional = desired
            .checked_sub(values.capacity())
            .ok_or_else(|| ContentError::for_code(ContentErrorCode::InternalState, position))?;
        let minimum_bytes = capacity_bytes::<T>(additional, position)?;
        self.preflight_retained(minimum_bytes, position)?;
        let old_capacity = values.capacity();
        values.try_reserve_exact(additional).map_err(|_| {
            ContentError::resource(
                ContentLimitKind::Allocation,
                self.limits.max_retained_bytes(),
                self.stats.retained_bytes,
                minimum_bytes,
                position,
            )
        })?;
        let actual_additional = values
            .capacity()
            .checked_sub(old_capacity)
            .ok_or_else(|| ContentError::for_code(ContentErrorCode::InternalState, position))?;
        let actual_bytes = capacity_bytes::<T>(actual_additional, position)?;
        self.commit_retained(actual_bytes, position)
    }

    fn allocate_bytes(
        &mut self,
        capacity: usize,
        position: Option<ContentPosition>,
    ) -> Result<Vec<u8>, ContentError> {
        let bytes = u64::try_from(capacity)
            .map_err(|_| ContentError::for_code(ContentErrorCode::InternalState, position))?;
        self.preflight_retained(bytes, position)?;
        let mut output = Vec::new();
        output.try_reserve_exact(capacity).map_err(|_| {
            ContentError::resource(
                ContentLimitKind::Allocation,
                self.limits.max_retained_bytes(),
                self.stats.retained_bytes,
                bytes,
                position,
            )
        })?;
        let actual = u64::try_from(output.capacity())
            .map_err(|_| ContentError::for_code(ContentErrorCode::InternalState, position))?;
        self.commit_retained(actual, position)?;
        Ok(output)
    }

    fn copy_token_bytes(&mut self, token: RawToken) -> Result<Vec<u8>, ContentError> {
        let mut copy = self.allocate_bytes(token.end - token.start, Some(token.span.start()))?;
        copy.extend_from_slice(self.raw(token));
        Ok(copy)
    }

    fn preflight_retained(
        &self,
        additional: u64,
        position: Option<ContentPosition>,
    ) -> Result<(), ContentError> {
        let next = self
            .stats
            .retained_bytes
            .checked_add(additional)
            .ok_or_else(|| ContentError::for_code(ContentErrorCode::InternalState, position))?;
        if next > self.limits.max_retained_bytes() {
            return Err(ContentError::resource(
                ContentLimitKind::RetainedBytes,
                self.limits.max_retained_bytes(),
                self.stats.retained_bytes,
                additional,
                position,
            ));
        }
        Ok(())
    }

    fn commit_retained(
        &mut self,
        additional: u64,
        position: Option<ContentPosition>,
    ) -> Result<(), ContentError> {
        self.preflight_retained(additional, position)?;
        self.stats.retained_bytes = self
            .stats
            .retained_bytes
            .checked_add(additional)
            .ok_or_else(|| ContentError::for_code(ContentErrorCode::InternalState, position))?;
        Ok(())
    }

    fn raw(&self, token: RawToken) -> &[u8] {
        &self.streams[token.stream_index].decoded()[token.start..token.end]
    }

    fn stream_bytes(&self, index: usize) -> &'a [u8] {
        self.streams[index].decoded()
    }

    fn advance_boundaries(&mut self) {
        while self
            .streams
            .get(self.stream_index)
            .is_some_and(|stream| self.offset == stream.decoded().len())
        {
            self.stream_index += 1;
            self.offset = 0;
        }
    }

    fn at_end(&self) -> bool {
        self.stream_index >= self.streams.len()
    }

    fn peek(&self) -> Option<u8> {
        self.streams
            .get(self.stream_index)
            .and_then(|stream| stream.decoded().get(self.offset))
            .copied()
    }

    fn peek_next(&self) -> Option<u8> {
        self.streams
            .get(self.stream_index)
            .and_then(|stream| stream.decoded().get(self.offset + 1))
            .copied()
    }

    fn current_position(&self) -> Option<ContentPosition> {
        self.position(self.stream_index, self.offset)
    }

    fn final_position(&self) -> Option<ContentPosition> {
        let stream = self.streams.last()?;
        Some(ContentPosition::new(
            stream.object(),
            stream.ordinal(),
            u64::try_from(stream.decoded().len()).ok()?,
        ))
    }

    fn position(&self, stream_index: usize, offset: usize) -> Option<ContentPosition> {
        let stream = self.streams.get(stream_index)?;
        Some(ContentPosition::new(
            stream.object(),
            stream.ordinal(),
            u64::try_from(offset).ok()?,
        ))
    }

    fn make_span(
        &self,
        stream_index: usize,
        start: usize,
        end: usize,
    ) -> Result<DecodedSpan, ContentError> {
        let stream = self
            .streams
            .get(stream_index)
            .ok_or_else(|| ContentError::for_code(ContentErrorCode::InternalState, None))?;
        let decoded_start = u64::try_from(start)
            .map_err(|_| ContentError::for_code(ContentErrorCode::InternalState, None))?;
        let decoded_len = end
            .checked_sub(start)
            .ok_or_else(|| ContentError::for_code(ContentErrorCode::InternalState, None))?;
        let decoded_len = u64::try_from(decoded_len)
            .map_err(|_| ContentError::for_code(ContentErrorCode::InternalState, None))?;
        decoded_start
            .checked_add(decoded_len)
            .ok_or_else(|| ContentError::for_code(ContentErrorCode::InternalState, None))?;
        Ok(DecodedSpan::new(
            stream.object(),
            stream.ordinal(),
            decoded_start,
            decoded_len,
        ))
    }
}

fn capacity_bytes<T>(
    capacity: usize,
    position: Option<ContentPosition>,
) -> Result<u64, ContentError> {
    capacity
        .checked_mul(size_of::<T>())
        .and_then(|bytes| u64::try_from(bytes).ok())
        .ok_or_else(|| ContentError::for_code(ContentErrorCode::InternalState, position))
}

fn checked_inc(value: usize, position: Option<ContentPosition>) -> Result<usize, ContentError> {
    value
        .checked_add(1)
        .ok_or_else(|| ContentError::for_code(ContentErrorCode::InternalState, position))
}

fn parse_number(raw: &[u8]) -> Result<ParsedNumber, ()> {
    let mut index = 0;
    let negative = match raw.first() {
        Some(b'-') => {
            index += 1;
            true
        }
        Some(b'+') => {
            index += 1;
            false
        }
        _ => false,
    };
    let integer_start = index;
    while raw.get(index).is_some_and(u8::is_ascii_digit) {
        index += 1;
    }
    let integer_digits = index - integer_start;
    let mut has_dot = false;
    let mut fractional_digits = 0;
    if raw.get(index) == Some(&b'.') {
        has_dot = true;
        index += 1;
        let fractional_start = index;
        while raw.get(index).is_some_and(u8::is_ascii_digit) {
            index += 1;
        }
        fractional_digits = index - fractional_start;
    }
    if integer_digits == 0 && fractional_digits == 0 {
        return Err(());
    }
    let mut has_exponent = false;
    if matches!(raw.get(index), Some(b'e' | b'E')) {
        has_exponent = true;
        index += 1;
        if matches!(raw.get(index), Some(b'+' | b'-')) {
            index += 1;
        }
        let exponent_start = index;
        while raw.get(index).is_some_and(u8::is_ascii_digit) {
            index += 1;
        }
        if index == exponent_start {
            return Err(());
        }
    }
    if index != raw.len() {
        return Err(());
    }
    if has_dot || has_exponent {
        return Ok(ParsedNumber::Real);
    }

    let mut magnitude = 0_u64;
    for digit in &raw[integer_start..] {
        magnitude = magnitude
            .checked_mul(10)
            .and_then(|value| value.checked_add(u64::from(*digit - b'0')))
            .ok_or(())?;
    }
    let value = if negative {
        if magnitude == (i64::MAX as u64) + 1 {
            i64::MIN
        } else {
            -i64::try_from(magnitude).map_err(|_| ())?
        }
    } else {
        i64::try_from(magnitude).map_err(|_| ())?
    };
    Ok(ParsedNumber::Integer(value))
}

fn decode_literal(raw: &[u8], output: &mut Vec<u8>) {
    let mut index = 0;
    while index < raw.len() {
        match raw[index] {
            b'\\' => {
                index += 1;
                match raw[index] {
                    b'n' => {
                        output.push(b'\n');
                        index += 1;
                    }
                    b'r' => {
                        output.push(b'\r');
                        index += 1;
                    }
                    b't' => {
                        output.push(b'\t');
                        index += 1;
                    }
                    b'b' => {
                        output.push(8);
                        index += 1;
                    }
                    b'f' => {
                        output.push(12);
                        index += 1;
                    }
                    b'\r' => {
                        index += 1;
                        if raw.get(index) == Some(&b'\n') {
                            index += 1;
                        }
                    }
                    b'\n' => index += 1,
                    byte if is_octal(byte) => {
                        let mut value = byte - b'0';
                        index += 1;
                        for _ in 0..2 {
                            if raw.get(index).is_some_and(|next| is_octal(*next)) {
                                value = value.wrapping_mul(8).wrapping_add(raw[index] - b'0');
                                index += 1;
                            } else {
                                break;
                            }
                        }
                        output.push(value);
                    }
                    byte => {
                        output.push(byte);
                        index += 1;
                    }
                }
            }
            b'\r' => {
                output.push(b'\n');
                index += 1;
                if raw.get(index) == Some(&b'\n') {
                    index += 1;
                }
            }
            byte => {
                output.push(byte);
                index += 1;
            }
        }
    }
}

fn decode_hex(raw: &[u8], output: &mut Vec<u8>) {
    let mut high = None;
    for byte in raw {
        if is_pdf_whitespace(*byte) {
            continue;
        }
        let nibble = hex_value(*byte).expect("validated hexadecimal string");
        if let Some(previous) = high.take() {
            output.push((previous << 4) | nibble);
        } else {
            high = Some(nibble);
        }
    }
    if let Some(previous) = high {
        output.push(previous << 4);
    }
}

const fn is_pdf_whitespace(byte: u8) -> bool {
    matches!(byte, 0 | 9 | 10 | 12 | 13 | 32)
}

const fn is_delimiter(byte: u8) -> bool {
    is_pdf_whitespace(byte)
        || matches!(
            byte,
            b'(' | b')' | b'<' | b'>' | b'[' | b']' | b'{' | b'}' | b'/' | b'%'
        )
}

const fn is_number_start(byte: u8) -> bool {
    byte.is_ascii_digit() || matches!(byte, b'+' | b'-' | b'.')
}

const fn is_octal(byte: u8) -> bool {
    matches!(byte, b'0'..=b'7')
}

const fn hex_value(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}
