use std::fmt;

use pdf_rs_bytes::{ByteSlice, SourceIdentity};

use crate::{
    ByteSpan, DictionaryEntry, Located, ObjectRef, PdfArray, PdfDictionary, PdfHeader, PdfName,
    PdfReal, PdfString, RealNotation, StringKind, SyntaxError, SyntaxErrorCode, SyntaxLimitKind,
    SyntaxLimits, SyntaxObject,
};

const CANCELLATION_PROBE_INTERVAL: u16 = 256;

/// Cooperative cancellation probe supplied by the owning runtime.
pub trait SyntaxCancellation: Send + Sync {
    /// Reports whether the current parser operation must stop.
    fn is_cancelled(&self) -> bool;
}

/// Cancellation probe used by [`SyntaxParser::new`] that never stops parsing.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct NeverCancelled;

impl SyntaxCancellation for NeverCancelled {
    fn is_cancelled(&self) -> bool {
        false
    }
}

static NEVER_CANCELLED: NeverCancelled = NeverCancelled;

/// Whether a contiguous parser window can still be extended by its owner.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InputExtent {
    /// The immutable source may contain bytes beyond this window.
    MayContinue,
    /// The window reaches the source length proven by its immutable snapshot.
    KnownSourceEnd,
}

/// One immutable contiguous source window supplied to the syntax parser.
#[derive(Clone, Copy)]
pub struct SyntaxInput<'a> {
    source: SourceIdentity,
    base_offset: u64,
    bytes: &'a [u8],
    extent: InputExtent,
}

impl<'a> SyntaxInput<'a> {
    /// Creates a source-bound input window after checking its absolute end.
    pub fn new(
        source: SourceIdentity,
        base_offset: u64,
        bytes: &'a [u8],
        extent: InputExtent,
    ) -> Result<Self, SyntaxError> {
        let len = u64::try_from(bytes.len()).map_err(|_| {
            SyntaxError::for_code(SyntaxErrorCode::InternalState, Some(base_offset))
        })?;
        base_offset.checked_add(len).ok_or_else(|| {
            SyntaxError::for_code(SyntaxErrorCode::InternalState, Some(base_offset))
        })?;
        Ok(Self {
            source,
            base_offset,
            bytes,
            extent,
        })
    }

    /// Borrows an owned byte slice while retaining its identity and absolute range.
    pub fn from_byte_slice(bytes: &'a ByteSlice, extent: InputExtent) -> Result<Self, SyntaxError> {
        Self::new(
            bytes.identity(),
            bytes.range().start(),
            bytes.bytes(),
            extent,
        )
    }

    /// Returns the immutable source identity.
    pub const fn source(self) -> SourceIdentity {
        self.source
    }

    /// Returns the absolute offset of the first window byte.
    pub const fn base_offset(self) -> u64 {
        self.base_offset
    }

    /// Returns the contiguous source bytes.
    pub const fn bytes(self) -> &'a [u8] {
        self.bytes
    }

    /// Returns whether this window may be extended or reaches a proven source end.
    pub const fn extent(self) -> InputExtent {
        self.extent
    }
}

impl fmt::Debug for SyntaxInput<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SyntaxInput")
            .field("source", &self.source)
            .field("base_offset", &self.base_offset)
            .field("len", &self.bytes.len())
            .field("extent", &self.extent)
            .field("bytes", &"[REDACTED]")
            .finish()
    }
}

/// Result of parsing against one bounded contiguous window.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SyntaxPoll<T> {
    /// A complete value was parsed.
    Ready(T),
    /// More contiguous bytes are required through at least this absolute end.
    NeedMore {
        /// Smallest useful exclusive end for the next window attempt.
        minimum_end: u64,
    },
    /// No object begins before the known final source end.
    EndOfInput,
    /// Parsing failed deterministically.
    Failed(SyntaxError),
}

/// Cumulative deterministic work charged by one parser attempt.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct SyntaxStats {
    input_bytes: u64,
    tokens: u64,
    owned_bytes: u64,
    container_bytes: u64,
    container_entries: u64,
    max_depth: u16,
}

impl SyntaxStats {
    /// Returns the complete input-window size charged for this attempt.
    ///
    /// ByteSource-facing jobs can sum this conservative value across retries so
    /// one-byte window growth cannot hide repeated scanning work.
    pub const fn input_bytes(self) -> u64 {
        self.input_bytes
    }

    /// Returns lexical tokens consumed, including speculative reference lookahead.
    pub const fn tokens(self) -> u64 {
        self.tokens
    }

    /// Returns allocator-reported scalar storage capacity retained by parsed values.
    pub const fn owned_bytes(self) -> u64 {
        self.owned_bytes
    }

    /// Returns allocator-reported array and dictionary vector capacity bytes.
    ///
    /// For a successful parse, this is the exact sum of `Vec::capacity()`
    /// multiplied by element size for every container retained by values this
    /// parser has returned. An unsuccessful operation can include container
    /// capacity allocated during that attempt and released when parsing stops.
    pub const fn container_bytes(self) -> u64 {
        self.container_bytes
    }

    /// Returns array items and dictionary entries charged by this attempt.
    pub const fn container_entries(self) -> u64 {
        self.container_entries
    }

    /// Returns the deepest array or dictionary reached by this attempt.
    pub const fn max_depth(self) -> u16 {
        self.max_depth
    }
}

/// Borrowed opaque source bytes with their immutable identity and exact span.
#[derive(Clone, Copy, Eq, PartialEq)]
pub struct RawBytes<'a> {
    source: SourceIdentity,
    span: ByteSpan,
    bytes: &'a [u8],
}

impl<'a> RawBytes<'a> {
    /// Returns the immutable source identity.
    pub const fn source(self) -> SourceIdentity {
        self.source
    }

    /// Returns the exact raw source span.
    pub const fn span(self) -> ByteSpan {
        self.span
    }

    /// Borrows the opaque bytes.
    pub const fn bytes(self) -> &'a [u8] {
        self.bytes
    }
}

impl fmt::Debug for RawBytes<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RawBytes")
            .field("source", &self.source)
            .field("span", &self.span)
            .field("bytes", &"[REDACTED]")
            .finish()
    }
}

enum ParseFailure {
    End,
    Incomplete {
        final_code: SyntaxErrorCode,
        offset: u64,
    },
    Failed(SyntaxError),
}

type ParseResult<T> = Result<T, ParseFailure>;

struct Token<'a> {
    start: u64,
    end: u64,
    kind: TokenKind<'a>,
}

enum TokenKind<'a> {
    ArrayOpen,
    ArrayClose,
    DictionaryOpen,
    DictionaryClose,
    Integer(i64),
    Real(PdfReal),
    Name(PdfName),
    String(PdfString),
    Keyword(&'a [u8]),
}

/// Pure bounded lexer and strict direct-object parser for one input window.
pub struct SyntaxParser<'a> {
    input: SyntaxInput<'a>,
    limits: SyntaxLimits,
    cancellation: &'a dyn SyntaxCancellation,
    cancellation_probe_countdown: u16,
    cursor: usize,
    stats: SyntaxStats,
}

impl<'a> SyntaxParser<'a> {
    /// Creates a parser after enforcing the configured contiguous-input budget.
    pub fn new(input: SyntaxInput<'a>, limits: SyntaxLimits) -> Result<Self, SyntaxError> {
        Self::new_with_cancellation(input, limits, &NEVER_CANCELLED)
    }

    /// Creates a parser with a cooperative runtime cancellation probe.
    pub fn new_with_cancellation(
        input: SyntaxInput<'a>,
        limits: SyntaxLimits,
        cancellation: &'a dyn SyntaxCancellation,
    ) -> Result<Self, SyntaxError> {
        let input_len = u64::try_from(input.bytes.len()).map_err(|_| {
            SyntaxError::for_code(SyntaxErrorCode::InternalState, Some(input.base_offset))
        })?;
        if input_len > limits.max_input_bytes {
            return Err(SyntaxError::resource(
                SyntaxLimitKind::InputBytes,
                limits.max_input_bytes,
                0,
                input_len,
                Some(input.base_offset),
            ));
        }
        Ok(Self {
            input,
            limits,
            cancellation,
            cancellation_probe_countdown: CANCELLATION_PROBE_INTERVAL,
            cursor: 0,
            stats: SyntaxStats {
                input_bytes: input_len,
                ..SyntaxStats::default()
            },
        })
    }

    /// Parses a supported `%PDF-x.y` header at the current position.
    pub fn parse_header(&mut self) -> SyntaxPoll<Located<PdfHeader>> {
        let result = self
            .begin_operation()
            .and_then(|()| self.parse_header_inner());
        match result {
            Ok(value) => SyntaxPoll::Ready(value),
            Err(failure) => self.poll_failure(failure, false),
        }
    }

    /// Parses one strict direct object, preserving all nested raw spans.
    pub fn parse_object(&mut self) -> SyntaxPoll<Located<SyntaxObject>> {
        let result = self
            .begin_operation()
            .and_then(|()| self.parse_object_at(0));
        match result {
            Ok(value) => SyntaxPoll::Ready(value),
            Err(failure) => self.poll_failure(failure, true),
        }
    }

    /// Consumes and borrows one keyword after PDF whitespace and comments.
    pub fn parse_keyword(&mut self) -> SyntaxPoll<RawBytes<'a>> {
        let result = self
            .begin_operation()
            .and_then(|()| self.parse_keyword_inner());
        match result {
            Ok(keyword) => SyntaxPoll::Ready(keyword),
            Err(failure) => self.poll_failure(failure, false),
        }
    }

    /// Consumes one exact keyword after PDF whitespace and comments.
    pub fn expect_keyword(&mut self, expected: &[u8]) -> SyntaxPoll<ByteSpan> {
        let result = (|| {
            self.begin_operation()?;
            let keyword = self.parse_keyword_inner()?;
            if keyword.bytes() == expected {
                Ok(keyword.span())
            } else {
                Err(ParseFailure::Failed(SyntaxError::for_code(
                    SyntaxErrorCode::UnexpectedToken,
                    Some(keyword.span().start()),
                )))
            }
        })();
        match result {
            Ok(span) => SyntaxPoll::Ready(span),
            Err(failure) => self.poll_failure(failure, false),
        }
    }

    /// Consumes the strict line ending required immediately after `stream`.
    pub fn consume_stream_line_ending(&mut self) -> SyntaxPoll<ByteSpan> {
        let result = self
            .begin_operation()
            .and_then(|()| self.consume_stream_line_ending_inner());
        match result {
            Ok(span) => SyntaxPoll::Ready(span),
            Err(failure) => self.poll_failure(failure, false),
        }
    }

    /// Borrows an exact byte count without interpreting stream contents.
    pub fn take_raw_bytes(&mut self, len: u64) -> SyntaxPoll<RawBytes<'a>> {
        if let Err(failure) = self.begin_operation() {
            return self.poll_failure(failure, false);
        }
        let start = self.cursor;
        let start_u64 = match u64::try_from(start) {
            Ok(value) => value,
            Err(_) => {
                return SyntaxPoll::Failed(SyntaxError::for_code(
                    SyntaxErrorCode::InternalState,
                    Some(self.position()),
                ));
            }
        };
        let required_relative_end = match start_u64.checked_add(len) {
            Some(value) => value,
            None => {
                return SyntaxPoll::Failed(SyntaxError::resource(
                    SyntaxLimitKind::InputBytes,
                    self.limits.max_input_bytes,
                    0,
                    u64::MAX,
                    Some(self.position()),
                ));
            }
        };
        if required_relative_end > self.limits.max_input_bytes {
            return SyntaxPoll::Failed(SyntaxError::resource(
                SyntaxLimitKind::InputBytes,
                self.limits.max_input_bytes,
                0,
                required_relative_end,
                Some(self.position()),
            ));
        }
        let Ok(len_usize) = usize::try_from(len) else {
            return SyntaxPoll::Failed(SyntaxError::resource(
                SyntaxLimitKind::InputBytes,
                self.limits.max_input_bytes,
                0,
                len,
                Some(self.position()),
            ));
        };
        let Some(end) = start.checked_add(len_usize) else {
            return SyntaxPoll::Failed(SyntaxError::for_code(
                SyntaxErrorCode::InternalState,
                Some(self.position()),
            ));
        };
        if end > self.input.bytes.len() {
            if self.input.extent == InputExtent::KnownSourceEnd {
                return SyntaxPoll::Failed(SyntaxError::for_code(
                    SyntaxErrorCode::UnexpectedEndOfInput,
                    Some(self.absolute(self.input.bytes.len())),
                ));
            }
            let Some(minimum_end) = self.position().checked_add(len) else {
                return SyntaxPoll::Failed(SyntaxError::for_code(
                    SyntaxErrorCode::InternalState,
                    Some(self.position()),
                ));
            };
            return SyntaxPoll::NeedMore { minimum_end };
        }
        let absolute_start = self.absolute(start);
        let absolute_end = self.absolute(end);
        let span = match ByteSpan::from_bounds(absolute_start, absolute_end) {
            Ok(span) => span,
            Err(error) => return SyntaxPoll::Failed(error),
        };
        self.cursor = end;
        SyntaxPoll::Ready(RawBytes {
            source: self.input.source,
            span,
            bytes: &self.input.bytes[start..end],
        })
    }

    /// Returns the current absolute source offset.
    pub fn position(&self) -> u64 {
        self.absolute(self.cursor)
    }

    /// Returns the number of unconsumed bytes in this window.
    pub fn remaining(&self) -> usize {
        self.input.bytes.len() - self.cursor
    }

    /// Returns deterministic work charged by this parser attempt.
    pub const fn stats(&self) -> SyntaxStats {
        self.stats
    }

    fn begin_operation(&mut self) -> ParseResult<()> {
        self.cancellation_probe_countdown = CANCELLATION_PROBE_INTERVAL;
        self.check_cancelled(self.position())
    }

    fn probe_iteration(&mut self, offset: u64) -> ParseResult<()> {
        if self.cancellation_probe_countdown > 1 {
            self.cancellation_probe_countdown -= 1;
            return Ok(());
        }
        self.cancellation_probe_countdown = CANCELLATION_PROBE_INTERVAL;
        self.check_cancelled(offset)
    }

    fn check_cancelled(&self, offset: u64) -> ParseResult<()> {
        if self.cancellation.is_cancelled() {
            return Err(ParseFailure::Failed(SyntaxError::for_code(
                SyntaxErrorCode::Cancelled,
                Some(offset),
            )));
        }
        Ok(())
    }

    fn parse_keyword_inner(&mut self) -> ParseResult<RawBytes<'a>> {
        self.skip_trivia()?;
        if self.cursor == self.input.bytes.len() {
            return Err(ParseFailure::Incomplete {
                final_code: SyntaxErrorCode::UnexpectedEndOfInput,
                offset: self.absolute(self.input.bytes.len()),
            });
        }

        let start = self.cursor;
        let absolute_start = self.absolute(start);
        self.charge_token(absolute_start)?;
        let first = self.input.bytes[start];
        if is_delimiter(first) || is_number_start(first) {
            return Err(ParseFailure::Failed(SyntaxError::for_code(
                SyntaxErrorCode::UnexpectedToken,
                Some(absolute_start),
            )));
        }

        let mut end = start;
        while end < self.input.bytes.len() && !is_delimiter(self.input.bytes[end]) {
            self.probe_iteration(self.absolute(end))?;
            end += 1;
            self.check_token_len(start, end)?;
        }
        if end == self.input.bytes.len() && self.input.extent == InputExtent::MayContinue {
            return Err(ParseFailure::Incomplete {
                final_code: SyntaxErrorCode::UnexpectedEndOfInput,
                offset: self.absolute(end),
            });
        }

        let span = ByteSpan::from_bounds(absolute_start, self.absolute(end))
            .map_err(ParseFailure::Failed)?;
        self.cursor = end;
        Ok(RawBytes {
            source: self.input.source,
            span,
            bytes: &self.input.bytes[start..end],
        })
    }

    fn parse_header_inner(&mut self) -> ParseResult<Located<PdfHeader>> {
        let start = self.cursor;
        self.charge_token(self.absolute(start))?;
        self.check_single_limit(
            SyntaxLimitKind::TokenBytes,
            self.limits.max_token_bytes,
            8,
            self.absolute(start),
        )?;

        const PREFIX: &[u8] = b"%PDF-";
        let available = &self.input.bytes[start..];
        let prefix_len = available.len().min(PREFIX.len());
        if available[..prefix_len] != PREFIX[..prefix_len] {
            return Err(ParseFailure::Failed(SyntaxError::for_code(
                SyntaxErrorCode::InvalidHeader,
                Some(self.absolute(start)),
            )));
        }
        if available.len() < 8 {
            return Err(ParseFailure::Incomplete {
                final_code: SyntaxErrorCode::InvalidHeader,
                offset: self.absolute(start),
            });
        }
        let raw = &available[..8];
        let major = raw[5];
        let dot = raw[6];
        let minor = raw[7];
        let supported = dot == b'.'
            && ((major == b'1' && (b'0'..=b'7').contains(&minor))
                || (major == b'2' && minor == b'0'));
        if !supported {
            return Err(ParseFailure::Failed(SyntaxError::for_code(
                SyntaxErrorCode::InvalidHeader,
                Some(self.absolute(start)),
            )));
        }
        self.cursor += 8;
        let span = ByteSpan::from_bounds(self.absolute(start), self.absolute(self.cursor))
            .map_err(ParseFailure::Failed)?;
        Ok(Located::new(
            self.input.source,
            span,
            PdfHeader::new(major - b'0', minor - b'0'),
        ))
    }

    fn parse_object_at(&mut self, depth: u16) -> ParseResult<Located<SyntaxObject>> {
        let token = self.next_token()?;
        self.object_from_token(token, depth)
    }

    fn parse_required_object_at(&mut self, depth: u16) -> ParseResult<Located<SyntaxObject>> {
        let token = self.next_required_token(SyntaxErrorCode::UnexpectedEndOfInput)?;
        self.object_from_token(token, depth)
    }

    fn object_from_token(
        &mut self,
        token: Token<'a>,
        depth: u16,
    ) -> ParseResult<Located<SyntaxObject>> {
        let start = token.start;
        let (end, value) = match token.kind {
            TokenKind::Integer(value) => return self.integer_or_reference(token, value),
            TokenKind::Real(value) => (token.end, SyntaxObject::Real(value)),
            TokenKind::Name(value) => (token.end, SyntaxObject::Name(value)),
            TokenKind::String(value) => (token.end, SyntaxObject::String(value)),
            TokenKind::Keyword(b"null") => (token.end, SyntaxObject::Null),
            TokenKind::Keyword(b"true") => (token.end, SyntaxObject::Boolean(true)),
            TokenKind::Keyword(b"false") => (token.end, SyntaxObject::Boolean(false)),
            TokenKind::ArrayOpen => return self.parse_array(token, depth),
            TokenKind::DictionaryOpen => return self.parse_dictionary(token, depth),
            TokenKind::ArrayClose | TokenKind::DictionaryClose => {
                return Err(ParseFailure::Failed(SyntaxError::for_code(
                    SyntaxErrorCode::MismatchedDelimiter,
                    Some(start),
                )));
            }
            TokenKind::Keyword(_) => {
                return Err(ParseFailure::Failed(SyntaxError::for_code(
                    SyntaxErrorCode::UnexpectedToken,
                    Some(start),
                )));
            }
        };
        self.located_object(start, end, value)
    }

    fn integer_or_reference(
        &mut self,
        first: Token<'a>,
        first_value: i64,
    ) -> ParseResult<Located<SyntaxObject>> {
        let checkpoint = self.cursor;
        self.skip_trivia()?;
        if self.cursor == self.input.bytes.len() {
            if self.input.extent == InputExtent::KnownSourceEnd {
                self.cursor = checkpoint;
                return self.located_object(
                    first.start,
                    first.end,
                    SyntaxObject::Integer(first_value),
                );
            }
            return Err(ParseFailure::Incomplete {
                final_code: SyntaxErrorCode::UnexpectedEndOfInput,
                offset: self.absolute(self.cursor),
            });
        }
        if !is_number_start(self.input.bytes[self.cursor]) {
            self.cursor = checkpoint;
            return self.located_object(first.start, first.end, SyntaxObject::Integer(first_value));
        }

        let second_value = match self.probe_integer_token()? {
            Some(value) => value,
            None => {
                self.cursor = checkpoint;
                return self.located_object(
                    first.start,
                    first.end,
                    SyntaxObject::Integer(first_value),
                );
            }
        };

        self.skip_trivia()?;
        if self.cursor == self.input.bytes.len() {
            if self.input.extent == InputExtent::KnownSourceEnd {
                self.cursor = checkpoint;
                return self.located_object(
                    first.start,
                    first.end,
                    SyntaxObject::Integer(first_value),
                );
            }
            return Err(ParseFailure::Incomplete {
                final_code: SyntaxErrorCode::UnexpectedEndOfInput,
                offset: self.absolute(self.cursor),
            });
        }
        if self.input.bytes[self.cursor] != b'R' {
            self.cursor = checkpoint;
            return self.located_object(first.start, first.end, SyntaxObject::Integer(first_value));
        }
        let after_r = self.cursor + 1;
        if after_r == self.input.bytes.len() && self.input.extent == InputExtent::MayContinue {
            return Err(ParseFailure::Incomplete {
                final_code: SyntaxErrorCode::UnexpectedEndOfInput,
                offset: self.absolute(after_r),
            });
        }
        if self
            .input
            .bytes
            .get(after_r)
            .is_some_and(|byte| !is_delimiter(*byte))
        {
            self.cursor = checkpoint;
            return self.located_object(first.start, first.end, SyntaxObject::Integer(first_value));
        }
        let third = self.next_token()?;
        if !matches!(third.kind, TokenKind::Keyword(b"R")) {
            return Err(ParseFailure::Failed(SyntaxError::for_code(
                SyntaxErrorCode::InternalState,
                Some(third.start),
            )));
        }

        let object_number = u32::try_from(first_value).ok().filter(|value| *value != 0);
        let generation = u16::try_from(second_value).ok();
        let (Some(object_number), Some(generation)) = (object_number, generation) else {
            return Err(ParseFailure::Failed(SyntaxError::for_code(
                SyntaxErrorCode::InvalidReference,
                Some(first.start),
            )));
        };
        self.located_object(
            first.start,
            third.end,
            SyntaxObject::Reference(ObjectRef::from_valid_parts(object_number, generation)),
        )
    }

    fn parse_array(
        &mut self,
        open: Token<'a>,
        parent_depth: u16,
    ) -> ParseResult<Located<SyntaxObject>> {
        let depth = self.enter_container(parent_depth, open.start)?;
        let mut values = Vec::new();
        loop {
            self.probe_iteration(self.position())?;
            self.skip_trivia()?;
            if self.cursor == self.input.bytes.len() {
                return Err(ParseFailure::Incomplete {
                    final_code: SyntaxErrorCode::UnexpectedEndOfInput,
                    offset: self.absolute(self.cursor),
                });
            }
            if self.input.bytes[self.cursor] == b']' {
                let close = self.next_token()?;
                return self.located_object(
                    open.start,
                    close.end,
                    SyntaxObject::Array(PdfArray::new(values)),
                );
            }
            let item_offset = self.absolute(self.cursor);
            self.charge_container_entry(item_offset)?;
            self.reserve_container_entry(&mut values, item_offset)?;
            let value = self.parse_required_object_at(depth)?;
            values.push(value);
        }
    }

    fn parse_dictionary(
        &mut self,
        open: Token<'a>,
        parent_depth: u16,
    ) -> ParseResult<Located<SyntaxObject>> {
        let depth = self.enter_container(parent_depth, open.start)?;
        let mut entries = Vec::new();
        loop {
            self.probe_iteration(self.position())?;
            self.skip_trivia()?;
            if self.cursor == self.input.bytes.len() {
                return Err(ParseFailure::Incomplete {
                    final_code: SyntaxErrorCode::UnexpectedEndOfInput,
                    offset: self.absolute(self.cursor),
                });
            }
            if self.input.bytes[self.cursor..].starts_with(b">>") {
                let close = self.next_token()?;
                return self.located_object(
                    open.start,
                    close.end,
                    SyntaxObject::Dictionary(PdfDictionary::new(entries)),
                );
            }
            if self.input.bytes[self.cursor] == b'>'
                && self.cursor + 1 == self.input.bytes.len()
                && self.input.extent == InputExtent::MayContinue
            {
                return Err(ParseFailure::Incomplete {
                    final_code: SyntaxErrorCode::UnexpectedByte,
                    offset: self.absolute(self.cursor + 1),
                });
            }
            let entry_offset = self.absolute(self.cursor);
            self.charge_container_entry(entry_offset)?;
            self.reserve_container_entry(&mut entries, entry_offset)?;
            let key_token = self.next_required_token(SyntaxErrorCode::UnexpectedEndOfInput)?;
            let (key_start, key_end, key) = match key_token.kind {
                TokenKind::ArrayClose => {
                    return Err(ParseFailure::Failed(SyntaxError::for_code(
                        SyntaxErrorCode::MismatchedDelimiter,
                        Some(key_token.start),
                    )));
                }
                TokenKind::Name(key) => (key_token.start, key_token.end, key),
                _ => {
                    return Err(ParseFailure::Failed(SyntaxError::for_code(
                        SyntaxErrorCode::UnexpectedToken,
                        Some(key_token.start),
                    )));
                }
            };
            let key_span =
                ByteSpan::from_bounds(key_start, key_end).map_err(ParseFailure::Failed)?;
            let located_key = Located::new(self.input.source, key_span, key);
            let value = self.parse_required_object_at(depth)?;
            entries.push(DictionaryEntry::new(located_key, value));
        }
    }

    fn located_object(
        &self,
        start: u64,
        end: u64,
        value: SyntaxObject,
    ) -> ParseResult<Located<SyntaxObject>> {
        let span = ByteSpan::from_bounds(start, end).map_err(ParseFailure::Failed)?;
        Ok(Located::new(self.input.source, span, value))
    }

    fn next_required_token(&mut self, final_code: SyntaxErrorCode) -> ParseResult<Token<'a>> {
        match self.next_token() {
            Err(ParseFailure::End) => Err(ParseFailure::Incomplete {
                final_code,
                offset: self.absolute(self.input.bytes.len()),
            }),
            result => result,
        }
    }

    fn next_token(&mut self) -> ParseResult<Token<'a>> {
        self.skip_trivia()?;
        if self.cursor == self.input.bytes.len() {
            return Err(ParseFailure::End);
        }
        let start = self.cursor;
        let absolute_start = self.absolute(start);
        self.charge_token(absolute_start)?;
        match self.input.bytes[start] {
            b'[' => self.fixed_token(start, 1, TokenKind::ArrayOpen),
            b']' => self.fixed_token(start, 1, TokenKind::ArrayClose),
            b'(' => self.scan_literal_string(start),
            b'/' => self.scan_name(start),
            b'<' => {
                if start + 1 == self.input.bytes.len() {
                    if self.input.extent == InputExtent::KnownSourceEnd {
                        self.scan_hex_string(start)
                    } else {
                        Err(ParseFailure::Incomplete {
                            final_code: SyntaxErrorCode::InvalidHexString,
                            offset: self.absolute(start + 1),
                        })
                    }
                } else if self.input.bytes[start + 1] == b'<' {
                    self.fixed_token(start, 2, TokenKind::DictionaryOpen)
                } else {
                    self.scan_hex_string(start)
                }
            }
            b'>' => {
                if start + 1 == self.input.bytes.len()
                    && self.input.extent == InputExtent::MayContinue
                {
                    Err(ParseFailure::Incomplete {
                        final_code: SyntaxErrorCode::UnexpectedByte,
                        offset: self.absolute(start + 1),
                    })
                } else if self.input.bytes.get(start + 1) == Some(&b'>') {
                    self.fixed_token(start, 2, TokenKind::DictionaryClose)
                } else {
                    Err(ParseFailure::Failed(SyntaxError::for_code(
                        SyntaxErrorCode::UnexpectedByte,
                        Some(absolute_start),
                    )))
                }
            }
            b')' | b'{' | b'}' => Err(ParseFailure::Failed(SyntaxError::for_code(
                SyntaxErrorCode::UnexpectedByte,
                Some(absolute_start),
            ))),
            _ => self.scan_regular(start),
        }
    }

    fn fixed_token(
        &mut self,
        start: usize,
        len: usize,
        kind: TokenKind<'a>,
    ) -> ParseResult<Token<'a>> {
        self.check_token_len(start, start + len)?;
        self.cursor = start + len;
        Ok(Token {
            start: self.absolute(start),
            end: self.absolute(self.cursor),
            kind,
        })
    }

    fn scan_regular(&mut self, start: usize) -> ParseResult<Token<'a>> {
        let mut end = start;
        while end < self.input.bytes.len() && !is_delimiter(self.input.bytes[end]) {
            self.probe_iteration(self.absolute(end))?;
            end += 1;
            self.check_token_len(start, end)?;
        }
        if end == start {
            return Err(ParseFailure::Failed(SyntaxError::for_code(
                SyntaxErrorCode::UnexpectedByte,
                Some(self.absolute(start)),
            )));
        }
        if end == self.input.bytes.len() && self.input.extent == InputExtent::MayContinue {
            return Err(ParseFailure::Incomplete {
                final_code: if is_number_start(self.input.bytes[start]) {
                    SyntaxErrorCode::InvalidNumber
                } else {
                    SyntaxErrorCode::UnexpectedEndOfInput
                },
                offset: self.absolute(end),
            });
        }
        let raw = &self.input.bytes[start..end];
        let kind = if is_number_start(raw[0]) {
            self.parse_number(raw, self.absolute(start))?
        } else {
            TokenKind::Keyword(raw)
        };
        self.cursor = end;
        Ok(Token {
            start: self.absolute(start),
            end: self.absolute(end),
            kind,
        })
    }

    fn probe_integer_token(&mut self) -> ParseResult<Option<i64>> {
        let start = self.cursor;
        let mut index = start;
        let negative = match self.input.bytes.get(index) {
            Some(b'-') => {
                index += 1;
                true
            }
            Some(b'+') => {
                index += 1;
                false
            }
            Some(byte) if byte.is_ascii_digit() => false,
            _ => return Ok(None),
        };
        self.check_token_len(start, index)?;
        if index == self.input.bytes.len() {
            if self.input.extent == InputExtent::MayContinue {
                return Err(ParseFailure::Incomplete {
                    final_code: SyntaxErrorCode::InvalidNumber,
                    offset: self.absolute(index),
                });
            }
            return Ok(None);
        }
        if !self.input.bytes[index].is_ascii_digit() {
            return Ok(None);
        }

        let mut magnitude = 0_u64;
        let mut overflowed = false;
        while self.input.bytes.get(index).is_some_and(u8::is_ascii_digit) {
            self.probe_iteration(self.absolute(index))?;
            overflowed |= match magnitude
                .checked_mul(10)
                .and_then(|value| value.checked_add(u64::from(self.input.bytes[index] - b'0')))
            {
                Some(value) => {
                    magnitude = value;
                    false
                }
                None => true,
            };
            index += 1;
            self.check_token_len(start, index)?;
        }
        if index == self.input.bytes.len() {
            if self.input.extent == InputExtent::MayContinue {
                return Err(ParseFailure::Incomplete {
                    final_code: SyntaxErrorCode::InvalidNumber,
                    offset: self.absolute(index),
                });
            }
        } else if !is_delimiter(self.input.bytes[index]) {
            return Ok(None);
        }
        if overflowed {
            return Ok(None);
        }
        let value = if negative {
            if magnitude == (i64::MAX as u64) + 1 {
                i64::MIN
            } else {
                let Ok(positive) = i64::try_from(magnitude) else {
                    return Ok(None);
                };
                -positive
            }
        } else {
            let Ok(value) = i64::try_from(magnitude) else {
                return Ok(None);
            };
            value
        };
        self.charge_token(self.absolute(start))?;
        self.cursor = index;
        Ok(Some(value))
    }

    fn parse_number(&mut self, raw: &[u8], offset: u64) -> ParseResult<TokenKind<'a>> {
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
            self.probe_iteration(offset)?;
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
                self.probe_iteration(offset)?;
                index += 1;
            }
            fractional_digits = index - fractional_start;
        }
        if integer_digits == 0 && fractional_digits == 0 {
            return Err(ParseFailure::Failed(SyntaxError::for_code(
                SyntaxErrorCode::InvalidNumber,
                Some(offset),
            )));
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
                self.probe_iteration(offset)?;
                index += 1;
            }
            if index == exponent_start {
                return Err(ParseFailure::Failed(SyntaxError::for_code(
                    SyntaxErrorCode::InvalidNumber,
                    Some(offset),
                )));
            }
        }
        if index != raw.len() {
            return Err(ParseFailure::Failed(SyntaxError::for_code(
                SyntaxErrorCode::InvalidNumber,
                Some(offset),
            )));
        }
        if has_dot || has_exponent {
            let owned = self.copy_owned(raw, offset)?;
            return Ok(TokenKind::Real(PdfReal::new(
                owned,
                if has_exponent {
                    RealNotation::Exponent
                } else {
                    RealNotation::Decimal
                },
            )));
        }

        let digits = &raw[integer_start..];
        let mut magnitude: u64 = 0;
        for digit in digits {
            self.probe_iteration(offset)?;
            magnitude = magnitude
                .checked_mul(10)
                .and_then(|value| value.checked_add(u64::from(*digit - b'0')))
                .ok_or_else(|| {
                    ParseFailure::Failed(SyntaxError::for_code(
                        SyntaxErrorCode::IntegerOutOfRange,
                        Some(offset),
                    ))
                })?;
        }
        let value = if negative {
            if magnitude == (i64::MAX as u64) + 1 {
                i64::MIN
            } else {
                let positive = i64::try_from(magnitude).map_err(|_| {
                    ParseFailure::Failed(SyntaxError::for_code(
                        SyntaxErrorCode::IntegerOutOfRange,
                        Some(offset),
                    ))
                })?;
                -positive
            }
        } else {
            i64::try_from(magnitude).map_err(|_| {
                ParseFailure::Failed(SyntaxError::for_code(
                    SyntaxErrorCode::IntegerOutOfRange,
                    Some(offset),
                ))
            })?
        };
        Ok(TokenKind::Integer(value))
    }

    fn scan_name(&mut self, start: usize) -> ParseResult<Token<'a>> {
        let mut index = start + 1;
        let mut decoded_len = 0_u64;
        while index < self.input.bytes.len() && !is_delimiter(self.input.bytes[index]) {
            self.probe_iteration(self.absolute(index))?;
            if self.input.bytes[index] == b'#' {
                if index + 2 >= self.input.bytes.len() {
                    if self.input.extent == InputExtent::MayContinue {
                        return Err(ParseFailure::Incomplete {
                            final_code: SyntaxErrorCode::InvalidNameEscape,
                            offset: self.absolute(index),
                        });
                    }
                    return Err(ParseFailure::Failed(SyntaxError::for_code(
                        SyntaxErrorCode::InvalidNameEscape,
                        Some(self.absolute(index)),
                    )));
                }
                if hex_value(self.input.bytes[index + 1]).is_none()
                    || hex_value(self.input.bytes[index + 2]).is_none()
                {
                    return Err(ParseFailure::Failed(SyntaxError::for_code(
                        SyntaxErrorCode::InvalidNameEscape,
                        Some(self.absolute(index)),
                    )));
                }
                index += 3;
            } else {
                index += 1;
            }
            decoded_len += 1;
            self.check_token_len(start, index)?;
            self.check_single_limit(
                SyntaxLimitKind::NameBytes,
                self.limits.max_name_bytes,
                decoded_len,
                self.absolute(start),
            )?;
        }
        if index == self.input.bytes.len() && self.input.extent == InputExtent::MayContinue {
            return Err(ParseFailure::Incomplete {
                final_code: SyntaxErrorCode::UnexpectedEndOfInput,
                offset: self.absolute(index),
            });
        }
        self.check_token_len(start, index)?;
        let mut decoded = self.allocate_owned(decoded_len, self.absolute(start))?;
        let mut source = start + 1;
        while source < index {
            self.probe_iteration(self.absolute(source))?;
            if self.input.bytes[source] == b'#' {
                let high = hex_value(self.input.bytes[source + 1]).expect("validated name escape");
                let low = hex_value(self.input.bytes[source + 2]).expect("validated name escape");
                decoded.push((high << 4) | low);
                source += 3;
            } else {
                decoded.push(self.input.bytes[source]);
                source += 1;
            }
        }
        self.cursor = index;
        Ok(Token {
            start: self.absolute(start),
            end: self.absolute(index),
            kind: TokenKind::Name(PdfName::new(decoded)),
        })
    }

    fn scan_literal_string(&mut self, start: usize) -> ParseResult<Token<'a>> {
        let mut index = start + 1;
        let mut nesting = 1_u16;
        let mut decoded_len = 0_u64;
        let end = loop {
            self.probe_iteration(self.absolute(index))?;
            if index == self.input.bytes.len() {
                return Err(ParseFailure::Incomplete {
                    final_code: SyntaxErrorCode::UnterminatedLiteralString,
                    offset: self.absolute(index),
                });
            }
            self.check_string_source(start, index + 1)?;
            match self.input.bytes[index] {
                b'(' => {
                    nesting = nesting.checked_add(1).ok_or_else(|| {
                        ParseFailure::Failed(SyntaxError::resource(
                            SyntaxLimitKind::ContainerDepth,
                            u64::from(self.limits.max_container_depth),
                            u64::from(nesting),
                            1,
                            Some(self.absolute(index)),
                        ))
                    })?;
                    if nesting > self.limits.max_container_depth {
                        return Err(ParseFailure::Failed(SyntaxError::resource(
                            SyntaxLimitKind::ContainerDepth,
                            u64::from(self.limits.max_container_depth),
                            u64::from(nesting - 1),
                            1,
                            Some(self.absolute(index)),
                        )));
                    }
                    decoded_len += 1;
                    index += 1;
                }
                b')' => {
                    nesting -= 1;
                    index += 1;
                    if nesting == 0 {
                        break index;
                    }
                    decoded_len += 1;
                }
                b'\\' => {
                    index += 1;
                    if index == self.input.bytes.len() {
                        return Err(ParseFailure::Incomplete {
                            final_code: SyntaxErrorCode::UnterminatedLiteralString,
                            offset: self.absolute(index),
                        });
                    }
                    self.check_string_source(start, index + 1)?;
                    match self.input.bytes[index] {
                        b'\r' => {
                            index += 1;
                            if self.input.bytes.get(index) == Some(&b'\n') {
                                index += 1;
                                self.check_string_source(start, index)?;
                            }
                        }
                        b'\n' => index += 1,
                        b'0'..=b'7' => {
                            index += 1;
                            let mut extra = 0;
                            while extra < 2
                                && matches!(self.input.bytes.get(index), Some(b'0'..=b'7'))
                            {
                                index += 1;
                                extra += 1;
                            }
                            decoded_len += 1;
                        }
                        _ => {
                            index += 1;
                            decoded_len += 1;
                        }
                    }
                    self.check_string_source(start, index)?;
                }
                b'\r' => {
                    index += 1;
                    if self.input.bytes.get(index) == Some(&b'\n') {
                        index += 1;
                    }
                    decoded_len += 1;
                }
                b'\n' => {
                    index += 1;
                    decoded_len += 1;
                }
                _ => {
                    index += 1;
                    decoded_len += 1;
                }
            }
            self.check_single_limit(
                SyntaxLimitKind::StringDecodedBytes,
                self.limits.max_string_decoded_bytes,
                decoded_len,
                self.absolute(start),
            )?;
        };
        self.check_single_limit(
            SyntaxLimitKind::StringDecodedBytes,
            self.limits.max_string_decoded_bytes,
            decoded_len,
            self.absolute(start),
        )?;
        let mut decoded = self.allocate_owned(decoded_len, self.absolute(start))?;
        let mut source = start + 1;
        let mut depth = 1_u16;
        while source < end {
            self.probe_iteration(self.absolute(source))?;
            match self.input.bytes[source] {
                b'(' => {
                    depth += 1;
                    decoded.push(b'(');
                    source += 1;
                }
                b')' => {
                    depth -= 1;
                    source += 1;
                    if depth == 0 {
                        break;
                    }
                    decoded.push(b')');
                }
                b'\\' => {
                    source += 1;
                    match self.input.bytes[source] {
                        b'n' => {
                            decoded.push(b'\n');
                            source += 1;
                        }
                        b'r' => {
                            decoded.push(b'\r');
                            source += 1;
                        }
                        b't' => {
                            decoded.push(b'\t');
                            source += 1;
                        }
                        b'b' => {
                            decoded.push(8);
                            source += 1;
                        }
                        b'f' => {
                            decoded.push(12);
                            source += 1;
                        }
                        b'(' | b')' | b'\\' => {
                            decoded.push(self.input.bytes[source]);
                            source += 1;
                        }
                        b'\r' => {
                            source += 1;
                            if self.input.bytes.get(source) == Some(&b'\n') {
                                source += 1;
                            }
                        }
                        b'\n' => source += 1,
                        b'0'..=b'7' => {
                            let mut value = 0_u16;
                            let mut count = 0;
                            while count < 3
                                && matches!(self.input.bytes.get(source), Some(b'0'..=b'7'))
                            {
                                value = (value << 3) | u16::from(self.input.bytes[source] - b'0');
                                source += 1;
                                count += 1;
                            }
                            decoded.push((value & 0xff) as u8);
                        }
                        other => {
                            decoded.push(other);
                            source += 1;
                        }
                    }
                }
                b'\r' => {
                    source += 1;
                    if self.input.bytes.get(source) == Some(&b'\n') {
                        source += 1;
                    }
                    decoded.push(b'\n');
                }
                b'\n' => {
                    decoded.push(b'\n');
                    source += 1;
                }
                byte => {
                    decoded.push(byte);
                    source += 1;
                }
            }
        }
        self.cursor = end;
        Ok(Token {
            start: self.absolute(start),
            end: self.absolute(end),
            kind: TokenKind::String(PdfString::new(decoded, StringKind::Literal)),
        })
    }

    fn scan_hex_string(&mut self, start: usize) -> ParseResult<Token<'a>> {
        let mut index = start + 1;
        let mut nibbles = 0_u64;
        let end = loop {
            self.probe_iteration(self.absolute(index))?;
            if index == self.input.bytes.len() {
                return Err(ParseFailure::Incomplete {
                    final_code: SyntaxErrorCode::InvalidHexString,
                    offset: self.absolute(index),
                });
            }
            self.check_string_source(start, index + 1)?;
            let byte = self.input.bytes[index];
            if byte == b'>' {
                index += 1;
                break index;
            }
            if is_whitespace(byte) {
                index += 1;
                continue;
            }
            if hex_value(byte).is_none() {
                return Err(ParseFailure::Failed(SyntaxError::for_code(
                    SyntaxErrorCode::InvalidHexString,
                    Some(self.absolute(index)),
                )));
            }
            nibbles += 1;
            index += 1;
        };
        let decoded_len = nibbles.div_ceil(2);
        self.check_single_limit(
            SyntaxLimitKind::StringDecodedBytes,
            self.limits.max_string_decoded_bytes,
            decoded_len,
            self.absolute(start),
        )?;
        let mut decoded = self.allocate_owned(decoded_len, self.absolute(start))?;
        let mut high = None;
        for index in start + 1..end - 1 {
            self.probe_iteration(self.absolute(index))?;
            let byte = self.input.bytes[index];
            if is_whitespace(byte) {
                continue;
            }
            let nibble = hex_value(byte).expect("validated hex string");
            if let Some(high_nibble) = high.take() {
                decoded.push((high_nibble << 4) | nibble);
            } else {
                high = Some(nibble);
            }
        }
        if let Some(high_nibble) = high {
            decoded.push(high_nibble << 4);
        }
        self.cursor = end;
        Ok(Token {
            start: self.absolute(start),
            end: self.absolute(end),
            kind: TokenKind::String(PdfString::new(decoded, StringKind::Hexadecimal)),
        })
    }

    fn skip_trivia(&mut self) -> ParseResult<()> {
        loop {
            self.probe_iteration(self.position())?;
            while self
                .input
                .bytes
                .get(self.cursor)
                .is_some_and(|byte| is_whitespace(*byte))
            {
                self.probe_iteration(self.position())?;
                self.cursor += 1;
            }
            if self.input.bytes.get(self.cursor) != Some(&b'%') {
                return Ok(());
            }
            let start = self.cursor;
            self.cursor += 1;
            while !matches!(
                self.input.bytes.get(self.cursor),
                None | Some(b'\r' | b'\n')
            ) {
                self.probe_iteration(self.position())?;
                self.cursor += 1;
                let comment_len = u64::try_from(self.cursor - start).map_err(|_| {
                    ParseFailure::Failed(SyntaxError::for_code(
                        SyntaxErrorCode::InternalState,
                        Some(self.absolute(start)),
                    ))
                })?;
                self.check_single_limit(
                    SyntaxLimitKind::CommentBytes,
                    self.limits.max_comment_bytes,
                    comment_len,
                    self.absolute(start),
                )?;
            }
            if self.cursor == self.input.bytes.len() {
                if self.input.extent == InputExtent::KnownSourceEnd {
                    return Ok(());
                }
                return Err(ParseFailure::Incomplete {
                    final_code: SyntaxErrorCode::UnexpectedEndOfInput,
                    offset: self.absolute(self.cursor),
                });
            }
        }
    }

    fn consume_stream_line_ending_inner(&mut self) -> ParseResult<ByteSpan> {
        let start = self.cursor;
        match self.input.bytes.get(start) {
            Some(b'\n') => {
                self.cursor += 1;
            }
            Some(b'\r') => match self.input.bytes.get(start + 1) {
                Some(b'\n') => self.cursor += 2,
                None if self.input.extent == InputExtent::MayContinue => {
                    return Err(ParseFailure::Incomplete {
                        final_code: SyntaxErrorCode::InvalidStreamBoundary,
                        offset: self.absolute(start + 1),
                    });
                }
                _ => {
                    return Err(ParseFailure::Failed(SyntaxError::for_code(
                        SyntaxErrorCode::InvalidStreamBoundary,
                        Some(self.absolute(start)),
                    )));
                }
            },
            None if self.input.extent == InputExtent::MayContinue => {
                return Err(ParseFailure::Incomplete {
                    final_code: SyntaxErrorCode::InvalidStreamBoundary,
                    offset: self.absolute(start),
                });
            }
            _ => {
                return Err(ParseFailure::Failed(SyntaxError::for_code(
                    SyntaxErrorCode::InvalidStreamBoundary,
                    Some(self.absolute(start)),
                )));
            }
        }
        ByteSpan::from_bounds(self.absolute(start), self.absolute(self.cursor))
            .map_err(ParseFailure::Failed)
    }

    fn enter_container(&mut self, parent_depth: u16, offset: u64) -> ParseResult<u16> {
        let depth = parent_depth.checked_add(1).ok_or_else(|| {
            ParseFailure::Failed(SyntaxError::resource(
                SyntaxLimitKind::ContainerDepth,
                u64::from(self.limits.max_container_depth),
                u64::from(parent_depth),
                1,
                Some(offset),
            ))
        })?;
        if depth > self.limits.max_container_depth {
            return Err(ParseFailure::Failed(SyntaxError::resource(
                SyntaxLimitKind::ContainerDepth,
                u64::from(self.limits.max_container_depth),
                u64::from(parent_depth),
                1,
                Some(offset),
            )));
        }
        self.stats.max_depth = self.stats.max_depth.max(depth);
        Ok(depth)
    }

    fn charge_token(&mut self, offset: u64) -> ParseResult<()> {
        self.charge_total(
            SyntaxLimitKind::Tokens,
            self.limits.max_total_tokens,
            self.stats.tokens,
            1,
            offset,
        )?;
        self.stats.tokens += 1;
        Ok(())
    }

    fn charge_container_entry(&mut self, offset: u64) -> ParseResult<()> {
        self.charge_total(
            SyntaxLimitKind::ContainerEntries,
            self.limits.max_container_entries,
            self.stats.container_entries,
            1,
            offset,
        )?;
        self.stats.container_entries += 1;
        Ok(())
    }

    fn reserve_container_entry<T>(
        &mut self,
        container: &mut Vec<T>,
        offset: u64,
    ) -> ParseResult<()> {
        let element_size = u64::try_from(std::mem::size_of::<T>()).map_err(|_| {
            ParseFailure::Failed(SyntaxError::for_code(
                SyntaxErrorCode::InternalState,
                Some(offset),
            ))
        })?;
        let capacity_before = container.capacity();
        if container.len() == capacity_before {
            let target_capacity = if capacity_before == 0 {
                4
            } else {
                capacity_before.checked_mul(2).ok_or_else(|| {
                    ParseFailure::Failed(SyntaxError::for_code(
                        SyntaxErrorCode::InternalState,
                        Some(offset),
                    ))
                })?
            };
            let requested_capacity =
                target_capacity
                    .checked_sub(capacity_before)
                    .ok_or_else(|| {
                        ParseFailure::Failed(SyntaxError::for_code(
                            SyntaxErrorCode::InternalState,
                            Some(offset),
                        ))
                    })?;
            let requested_bytes = u64::try_from(requested_capacity)
                .ok()
                .and_then(|capacity| capacity.checked_mul(element_size))
                .ok_or_else(|| {
                    ParseFailure::Failed(SyntaxError::for_code(
                        SyntaxErrorCode::InternalState,
                        Some(offset),
                    ))
                })?;
            self.charge_total(
                SyntaxLimitKind::ContainerBytes,
                self.limits.max_container_bytes,
                self.stats.container_bytes,
                requested_bytes,
                offset,
            )?;
            container
                .try_reserve_exact(requested_capacity)
                .map_err(|_| self.allocation_failure(offset, requested_bytes))?;
        }
        let capacity_after = container.capacity();
        let added_capacity = capacity_after.checked_sub(capacity_before).ok_or_else(|| {
            ParseFailure::Failed(SyntaxError::for_code(
                SyntaxErrorCode::InternalState,
                Some(offset),
            ))
        })?;
        let added_capacity = u64::try_from(added_capacity).map_err(|_| {
            ParseFailure::Failed(SyntaxError::for_code(
                SyntaxErrorCode::InternalState,
                Some(offset),
            ))
        })?;
        let added_bytes = added_capacity.checked_mul(element_size).ok_or_else(|| {
            ParseFailure::Failed(SyntaxError::for_code(
                SyntaxErrorCode::InternalState,
                Some(offset),
            ))
        })?;
        let total_container_bytes = self
            .stats
            .container_bytes
            .checked_add(added_bytes)
            .ok_or_else(|| {
                ParseFailure::Failed(SyntaxError::for_code(
                    SyntaxErrorCode::InternalState,
                    Some(offset),
                ))
            })?;
        self.stats.container_bytes = total_container_bytes;
        if total_container_bytes > self.limits.max_container_bytes {
            return Err(ParseFailure::Failed(SyntaxError::resource(
                SyntaxLimitKind::ContainerBytes,
                self.limits.max_container_bytes,
                total_container_bytes.saturating_sub(added_bytes),
                added_bytes,
                Some(offset),
            )));
        }
        Ok(())
    }

    fn allocate_owned(&mut self, len: u64, offset: u64) -> ParseResult<Vec<u8>> {
        self.charge_total(
            SyntaxLimitKind::OwnedBytes,
            self.limits.max_owned_bytes,
            self.stats.owned_bytes,
            len,
            offset,
        )?;
        let len_usize = usize::try_from(len).map_err(|_| {
            ParseFailure::Failed(SyntaxError::resource(
                SyntaxLimitKind::Allocation,
                self.limits.max_owned_bytes,
                self.stats.owned_bytes,
                len,
                Some(offset),
            ))
        })?;
        let mut bytes = Vec::new();
        bytes.try_reserve_exact(len_usize).map_err(|_| {
            ParseFailure::Failed(SyntaxError::resource(
                SyntaxLimitKind::Allocation,
                self.limits.max_owned_bytes,
                self.stats.owned_bytes,
                len,
                Some(offset),
            ))
        })?;
        let capacity = u64::try_from(bytes.capacity()).map_err(|_| {
            ParseFailure::Failed(SyntaxError::for_code(
                SyntaxErrorCode::InternalState,
                Some(offset),
            ))
        })?;
        self.charge_total(
            SyntaxLimitKind::OwnedBytes,
            self.limits.max_owned_bytes,
            self.stats.owned_bytes,
            capacity,
            offset,
        )?;
        self.stats.owned_bytes += capacity;
        Ok(bytes)
    }

    fn copy_owned(&mut self, source: &[u8], offset: u64) -> ParseResult<Vec<u8>> {
        let len = u64::try_from(source.len()).map_err(|_| {
            ParseFailure::Failed(SyntaxError::for_code(
                SyntaxErrorCode::InternalState,
                Some(offset),
            ))
        })?;
        let mut copy = self.allocate_owned(len, offset)?;
        copy.extend_from_slice(source);
        Ok(copy)
    }

    fn check_token_len(&self, start: usize, end: usize) -> ParseResult<()> {
        let len = u64::try_from(end - start).map_err(|_| {
            ParseFailure::Failed(SyntaxError::for_code(
                SyntaxErrorCode::InternalState,
                Some(self.absolute(start)),
            ))
        })?;
        self.check_single_limit(
            SyntaxLimitKind::TokenBytes,
            self.limits.max_token_bytes,
            len,
            self.absolute(start),
        )
    }

    fn check_string_source(&self, start: usize, end: usize) -> ParseResult<()> {
        self.check_token_len(start, end)?;
        let len = u64::try_from(end - start).map_err(|_| {
            ParseFailure::Failed(SyntaxError::for_code(
                SyntaxErrorCode::InternalState,
                Some(self.absolute(start)),
            ))
        })?;
        self.check_single_limit(
            SyntaxLimitKind::StringSourceBytes,
            self.limits.max_string_source_bytes,
            len,
            self.absolute(start),
        )
    }

    fn check_single_limit(
        &self,
        kind: SyntaxLimitKind,
        limit: u64,
        attempted: u64,
        offset: u64,
    ) -> ParseResult<()> {
        if attempted > limit {
            return Err(ParseFailure::Failed(SyntaxError::resource(
                kind,
                limit,
                0,
                attempted,
                Some(offset),
            )));
        }
        Ok(())
    }

    fn charge_total(
        &self,
        kind: SyntaxLimitKind,
        limit: u64,
        consumed: u64,
        attempted: u64,
        offset: u64,
    ) -> ParseResult<()> {
        if consumed
            .checked_add(attempted)
            .is_none_or(|total| total > limit)
        {
            return Err(ParseFailure::Failed(SyntaxError::resource(
                kind,
                limit,
                consumed,
                attempted,
                Some(offset),
            )));
        }
        Ok(())
    }

    fn allocation_failure(&self, offset: u64, attempted: u64) -> ParseFailure {
        ParseFailure::Failed(SyntaxError::resource(
            SyntaxLimitKind::Allocation,
            self.limits.max_container_entries,
            self.stats.container_entries,
            attempted,
            Some(offset),
        ))
    }

    fn poll_failure<T>(&self, failure: ParseFailure, clean_end: bool) -> SyntaxPoll<T> {
        match failure {
            ParseFailure::End if clean_end && self.input.extent == InputExtent::KnownSourceEnd => {
                SyntaxPoll::EndOfInput
            }
            ParseFailure::End => self.need_more_or_failure(),
            ParseFailure::Incomplete { final_code, offset }
                if self.input.extent == InputExtent::KnownSourceEnd =>
            {
                SyntaxPoll::Failed(SyntaxError::for_code(final_code, Some(offset)))
            }
            ParseFailure::Incomplete { .. } => self.need_more_or_failure(),
            ParseFailure::Failed(error) => SyntaxPoll::Failed(error),
        }
    }

    fn need_more_or_failure<T>(&self) -> SyntaxPoll<T> {
        let current_len = match u64::try_from(self.input.bytes.len()) {
            Ok(value) => value,
            Err(_) => {
                return SyntaxPoll::Failed(SyntaxError::for_code(
                    SyntaxErrorCode::InternalState,
                    Some(self.input.base_offset),
                ));
            }
        };
        let Some(required_len) = current_len.checked_add(1) else {
            return SyntaxPoll::Failed(SyntaxError::for_code(
                SyntaxErrorCode::InternalState,
                Some(self.absolute(self.input.bytes.len())),
            ));
        };
        if required_len > self.limits.max_input_bytes {
            return SyntaxPoll::Failed(SyntaxError::resource(
                SyntaxLimitKind::InputBytes,
                self.limits.max_input_bytes,
                0,
                required_len,
                Some(self.absolute(self.input.bytes.len())),
            ));
        }
        match self.input.base_offset.checked_add(required_len) {
            Some(minimum_end) => SyntaxPoll::NeedMore { minimum_end },
            None => SyntaxPoll::Failed(SyntaxError::for_code(
                SyntaxErrorCode::InternalState,
                Some(self.absolute(self.input.bytes.len())),
            )),
        }
    }

    fn absolute(&self, index: usize) -> u64 {
        self.input.base_offset
            + u64::try_from(index).expect("SyntaxInput proved its window length fits u64")
    }
}

fn is_whitespace(byte: u8) -> bool {
    matches!(byte, 0 | b'\t' | b'\n' | 12 | b'\r' | b' ')
}

fn is_delimiter(byte: u8) -> bool {
    is_whitespace(byte)
        || matches!(
            byte,
            b'(' | b')' | b'<' | b'>' | b'[' | b']' | b'{' | b'}' | b'/' | b'%'
        )
}

fn is_number_start(byte: u8) -> bool {
    byte.is_ascii_digit() || matches!(byte, b'+' | b'-' | b'.')
}

fn hex_value(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}
