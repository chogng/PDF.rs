use pdf_rs_bytes::{SourceIdentity, SourceSnapshot};
use pdf_rs_syntax::{
    ByteSpan, InputExtent, Located, PdfDictionary, SyntaxCancellation, SyntaxErrorCategory,
    SyntaxInput, SyntaxLimits, SyntaxObject, SyntaxParser, SyntaxPoll,
};

use crate::job::XrefCancellation;
use crate::{
    TraditionalRevisionSection, XrefEntry, XrefEntryKind, XrefError, XrefErrorCode, XrefLimitKind,
    XrefLimits, XrefSection,
};

pub(crate) enum TailParse {
    Found { startxref: u64, tail_start: u64 },
    NeedLarger,
}

pub(crate) struct SectionWindow<'a> {
    snapshot: SourceSnapshot,
    startxref: u64,
    bytes: &'a [u8],
    extent: InputExtent,
    source_len: u64,
}

impl<'a> SectionWindow<'a> {
    pub(crate) const fn new(
        snapshot: SourceSnapshot,
        startxref: u64,
        bytes: &'a [u8],
        extent: InputExtent,
        source_len: u64,
    ) -> Self {
        Self {
            snapshot,
            startxref,
            bytes,
            extent,
            source_len,
        }
    }
}

enum LocalFailure {
    NeedMore,
    Failed(XrefError),
}

type LocalResult<T> = Result<T, LocalFailure>;

struct XrefSyntaxCancellation<'a>(&'a dyn XrefCancellation);

impl SyntaxCancellation for XrefSyntaxCancellation<'_> {
    fn is_cancelled(&self) -> bool {
        self.0.is_cancelled()
    }
}

pub(crate) fn parse_tail(
    bytes: &[u8],
    base_offset: u64,
    source_len: u64,
    cancellation: &dyn XrefCancellation,
) -> Result<TailParse, XrefError> {
    if cancellation.is_cancelled() {
        return Err(XrefError::for_code(XrefErrorCode::Cancelled, None));
    }
    let mut end = bytes.len();
    while end != 0 && is_pdf_whitespace(bytes[end - 1]) {
        end -= 1;
        if end.is_multiple_of(1024) && cancellation.is_cancelled() {
            return Err(XrefError::for_code(XrefErrorCode::Cancelled, None));
        }
    }
    if end == 0 {
        return if base_offset == 0 {
            Err(XrefError::for_code(
                XrefErrorCode::StartXrefNotFound,
                Some(0),
            ))
        } else {
            Ok(TailParse::NeedLarger)
        };
    }
    const EOF_MARKER: &[u8] = b"%%EOF";
    if end < EOF_MARKER.len() {
        return if base_offset == 0 {
            Err(XrefError::for_code(
                XrefErrorCode::InvalidStartXref,
                Some(base_offset),
            ))
        } else {
            Ok(TailParse::NeedLarger)
        };
    }
    if &bytes[end - EOF_MARKER.len()..end] != EOF_MARKER {
        return Err(XrefError::for_code(
            XrefErrorCode::InvalidStartXref,
            Some(absolute(base_offset, end - EOF_MARKER.len())?),
        ));
    }
    let marker_start = end - EOF_MARKER.len();
    if marker_start == 0 {
        return if base_offset == 0 {
            Err(XrefError::for_code(
                XrefErrorCode::InvalidStartXref,
                Some(0),
            ))
        } else {
            Ok(TailParse::NeedLarger)
        };
    }
    let mut cursor = marker_start;
    let whitespace_after_value = cursor;
    while cursor != 0 && is_pdf_whitespace(bytes[cursor - 1]) {
        cursor -= 1;
        if cursor.is_multiple_of(1024) && cancellation.is_cancelled() {
            return Err(XrefError::for_code(XrefErrorCode::Cancelled, None));
        }
    }
    if cursor == whitespace_after_value
        || !contains_line_ending(&bytes[cursor..whitespace_after_value])
    {
        return Err(XrefError::for_code(
            XrefErrorCode::InvalidStartXref,
            Some(absolute(base_offset, marker_start)?),
        ));
    }
    if cursor == 0 {
        return if base_offset == 0 {
            Err(XrefError::for_code(
                XrefErrorCode::InvalidStartXref,
                Some(0),
            ))
        } else {
            Ok(TailParse::NeedLarger)
        };
    }
    let digits_end = cursor;
    while cursor != 0 && bytes[cursor - 1].is_ascii_digit() {
        cursor -= 1;
        if cursor.is_multiple_of(1024) && cancellation.is_cancelled() {
            return Err(XrefError::for_code(XrefErrorCode::Cancelled, None));
        }
    }
    if cursor == digits_end {
        return Err(XrefError::for_code(
            XrefErrorCode::InvalidStartXref,
            Some(absolute(base_offset, cursor)?),
        ));
    }
    if digits_end - cursor > 20 {
        return Err(XrefError::for_code(
            XrefErrorCode::InvalidStartXref,
            Some(absolute(base_offset, cursor)?),
        ));
    }
    let value = parse_digits(&bytes[cursor..digits_end]).ok_or_else(|| {
        XrefError::for_code(
            XrefErrorCode::InvalidStartXref,
            absolute(base_offset, cursor).ok(),
        )
    })?;
    if cursor == 0 && base_offset != 0 {
        return Ok(TailParse::NeedLarger);
    }
    let whitespace_before_value = cursor;
    while cursor != 0 && is_pdf_whitespace(bytes[cursor - 1]) {
        cursor -= 1;
        if cursor.is_multiple_of(1024) && cancellation.is_cancelled() {
            return Err(XrefError::for_code(XrefErrorCode::Cancelled, None));
        }
    }
    if cursor == whitespace_before_value
        || !contains_line_ending(&bytes[cursor..whitespace_before_value])
    {
        return Err(XrefError::for_code(
            XrefErrorCode::InvalidStartXref,
            Some(absolute(base_offset, cursor)?),
        ));
    }
    const STARTXREF: &[u8] = b"startxref";
    if cursor < STARTXREF.len() {
        return if base_offset == 0 {
            Err(XrefError::for_code(
                XrefErrorCode::InvalidStartXref,
                Some(0),
            ))
        } else {
            Ok(TailParse::NeedLarger)
        };
    }
    let keyword_start = cursor - STARTXREF.len();
    if &bytes[keyword_start..cursor] != STARTXREF {
        return Err(XrefError::for_code(
            XrefErrorCode::InvalidStartXref,
            Some(absolute(base_offset, keyword_start)?),
        ));
    }
    if keyword_start == 0 && base_offset != 0 {
        return Ok(TailParse::NeedLarger);
    }
    if keyword_start != 0 && !is_pdf_whitespace(bytes[keyword_start - 1]) {
        return Err(XrefError::for_code(
            XrefErrorCode::InvalidStartXref,
            Some(absolute(base_offset, keyword_start)?),
        ));
    }
    if value >= source_len {
        return Err(XrefError::for_code(
            XrefErrorCode::StartXrefOutOfBounds,
            Some(absolute(base_offset, whitespace_before_value)?),
        ));
    }
    Ok(TailParse::Found {
        startxref: value,
        tail_start: absolute(base_offset, keyword_start)?,
    })
}

pub(crate) fn parse_section(
    window: SectionWindow<'_>,
    limits: XrefLimits,
    syntax_limits: SyntaxLimits,
    cancellation: &dyn XrefCancellation,
) -> Result<Option<XrefSection>, XrefError> {
    let mut cursor = Cursor::new(window.bytes, window.startxref, window.extent, cancellation);
    match parse_section_inner(
        window.snapshot,
        window.startxref,
        window.source_len,
        limits,
        syntax_limits,
        &mut cursor,
    ) {
        Ok(section) => finalize_base_section(section, cancellation).map(Some),
        Err(LocalFailure::NeedMore) => Ok(None),
        Err(LocalFailure::Failed(error)) => Err(error),
    }
}

pub(crate) fn parse_traditional_revision_section(
    window: SectionWindow<'_>,
    limits: XrefLimits,
    syntax_limits: SyntaxLimits,
    cancellation: &dyn XrefCancellation,
) -> Result<Option<TraditionalRevisionSection>, XrefError> {
    let mut cursor = Cursor::new(window.bytes, window.startxref, window.extent, cancellation);
    match parse_section_inner(
        window.snapshot,
        window.startxref,
        window.source_len,
        limits,
        syntax_limits,
        &mut cursor,
    ) {
        Ok(section) => finalize_revision_section(section, cancellation).map(Some),
        Err(LocalFailure::NeedMore) => Ok(None),
        Err(LocalFailure::Failed(error)) => Err(error),
    }
}

fn classify_non_table_target(
    source: SourceIdentity,
    startxref: u64,
    bytes: &[u8],
    extent: InputExtent,
    syntax_limits: SyntaxLimits,
    cancellation: &dyn XrefCancellation,
) -> LocalResult<XrefErrorCode> {
    if !bytes[0].is_ascii_digit() && !matches!(bytes[0], b'+' | b'-') {
        return Ok(XrefErrorCode::InvalidXrefKeyword);
    }
    let input = SyntaxInput::new(source, startxref, bytes, extent)
        .map_err(|error| LocalFailure::Failed(XrefError::from_syntax(error)))?;
    let syntax_cancellation = XrefSyntaxCancellation(cancellation);
    let mut parser =
        SyntaxParser::new_with_cancellation(input, syntax_limits, &syntax_cancellation)
            .map_err(|error| LocalFailure::Failed(XrefError::from_syntax(error)))?;

    let Some(number) = probe_syntax(parser.parse_object())? else {
        return Ok(XrefErrorCode::InvalidXrefKeyword);
    };
    if number
        .value()
        .as_integer()
        .and_then(|value| u32::try_from(value).ok())
        .is_none_or(|value| value == 0)
    {
        return Ok(XrefErrorCode::InvalidXrefKeyword);
    }

    let Some(generation) = probe_syntax(parser.parse_object())? else {
        return Ok(XrefErrorCode::InvalidXrefKeyword);
    };
    if generation
        .value()
        .as_integer()
        .and_then(|value| u16::try_from(value).ok())
        .is_none()
    {
        return Ok(XrefErrorCode::InvalidXrefKeyword);
    }

    if probe_syntax(parser.expect_keyword(b"obj"))?.is_none() {
        return Ok(XrefErrorCode::InvalidXrefKeyword);
    }
    let Some(dictionary) = probe_syntax(parser.parse_object())? else {
        return Ok(XrefErrorCode::InvalidXrefKeyword);
    };
    let SyntaxObject::Dictionary(dictionary) = dictionary.value() else {
        return Ok(XrefErrorCode::InvalidXrefKeyword);
    };
    let is_xref = matches!(
        unique_dictionary_value(dictionary, b"Type", cancellation, startxref)?,
        Some(SyntaxObject::Name(name)) if name.bytes() == b"XRef"
    );
    let valid_size = unique_dictionary_value(dictionary, b"Size", cancellation, startxref)?
        .and_then(SyntaxObject::as_integer)
        .is_some_and(|value| value > 0 && u32::try_from(value).is_ok());
    let valid_widths = matches!(
        unique_dictionary_value(dictionary, b"W", cancellation, startxref)?,
        Some(SyntaxObject::Array(widths))
            if widths.values().len() == 3
                && widths.values().iter().all(|width| {
                    width
                        .value()
                        .as_integer()
                        .is_some_and(|value| value >= 0 && u32::try_from(value).is_ok())
                })
    );
    let length = unique_dictionary_value(dictionary, b"Length", cancellation, startxref)?;
    let valid_length = matches!(length, Some(SyntaxObject::Integer(value)) if *value >= 0)
        || matches!(length, Some(SyntaxObject::Reference(_)));
    if !is_xref || !valid_size || !valid_widths || !valid_length {
        return Ok(XrefErrorCode::InvalidXrefKeyword);
    }
    if probe_syntax(parser.expect_keyword(b"stream"))?.is_none()
        || probe_syntax(parser.consume_stream_line_ending())?.is_none()
    {
        return Ok(XrefErrorCode::InvalidXrefKeyword);
    }
    Ok(XrefErrorCode::UnsupportedXrefStream)
}

fn unique_dictionary_value<'a>(
    dictionary: &'a PdfDictionary,
    key: &[u8],
    cancellation: &dyn XrefCancellation,
    offset: u64,
) -> LocalResult<Option<&'a SyntaxObject>> {
    let mut value = None;
    for (index, entry) in dictionary.entries().iter().enumerate() {
        if index.is_multiple_of(256) && cancellation.is_cancelled() {
            return Err(LocalFailure::Failed(XrefError::for_code(
                XrefErrorCode::Cancelled,
                Some(offset),
            )));
        }
        if entry.key().value().bytes() == key {
            if value.is_some() {
                return Ok(None);
            }
            value = Some(entry.value().value());
        }
    }
    Ok(value)
}

fn probe_syntax<T>(poll: SyntaxPoll<T>) -> LocalResult<Option<T>> {
    match poll {
        SyntaxPoll::Ready(value) => Ok(Some(value)),
        SyntaxPoll::NeedMore { .. } => Err(LocalFailure::NeedMore),
        SyntaxPoll::EndOfInput => Ok(None),
        SyntaxPoll::Failed(error) if error.category() == SyntaxErrorCategory::Syntax => Ok(None),
        SyntaxPoll::Failed(error) => Err(LocalFailure::Failed(XrefError::from_syntax(error))),
    }
}

fn parse_section_inner(
    snapshot: SourceSnapshot,
    startxref: u64,
    source_len: u64,
    limits: XrefLimits,
    syntax_limits: SyntaxLimits,
    cursor: &mut Cursor<'_, '_>,
) -> LocalResult<ParsedTraditionalSection> {
    if cursor.bytes.len() < 4 {
        if b"xref".starts_with(cursor.bytes) && cursor.extent == InputExtent::MayContinue {
            return Err(LocalFailure::NeedMore);
        }
        if !cursor.bytes.is_empty()
            && cursor.extent == InputExtent::MayContinue
            && (cursor.bytes[0].is_ascii_digit() || matches!(cursor.bytes[0], b'+' | b'-'))
        {
            let code = classify_non_table_target(
                snapshot.identity(),
                startxref,
                cursor.bytes,
                cursor.extent,
                syntax_limits,
                cursor.cancellation,
            )?;
            return Err(cursor.failure(code, 0));
        }
        return Err(cursor.failure(XrefErrorCode::InvalidXrefKeyword, 0));
    }
    if &cursor.bytes[..4] != b"xref" {
        let code = classify_non_table_target(
            snapshot.identity(),
            startxref,
            cursor.bytes,
            cursor.extent,
            syntax_limits,
            cursor.cancellation,
        )?;
        return Err(cursor.failure(code, 0));
    }
    cursor.position = 4;
    cursor.consume_line_ending(XrefErrorCode::InvalidXrefKeyword)?;

    let mut entries = Vec::new();
    let mut subsection_count = 0_u64;
    let mut previous_end = None;
    loop {
        cursor.skip_whitespace()?;
        if cursor.position == cursor.bytes.len() {
            return Err(cursor.incomplete(XrefErrorCode::InvalidTrailer));
        }
        if cursor.keyword_at_position(b"trailer")? {
            if subsection_count == 0 {
                return Err(cursor.failure(XrefErrorCode::InvalidSubsection, cursor.position));
            }
            cursor.position += b"trailer".len();
            cursor.skip_whitespace()?;
            return parse_trailer(snapshot, startxref, limits, syntax_limits, cursor, entries);
        }

        subsection_count = subsection_count.checked_add(1).ok_or_else(|| {
            LocalFailure::Failed(XrefError::resource(
                XrefLimitKind::Subsections,
                limits.max_subsections,
                subsection_count,
                1,
                cursor.absolute_position().ok(),
            ))
        })?;
        if subsection_count > limits.max_subsections {
            return Err(LocalFailure::Failed(XrefError::resource(
                XrefLimitKind::Subsections,
                limits.max_subsections,
                subsection_count - 1,
                1,
                cursor.absolute_position().ok(),
            )));
        }
        let first = cursor.parse_decimal(XrefErrorCode::InvalidSubsection, 10)?;
        cursor.consume_horizontal_separator(XrefErrorCode::InvalidSubsection)?;
        let count = cursor.parse_decimal(XrefErrorCode::InvalidSubsection, 10)?;
        if count == 0 {
            return Err(cursor.failure(XrefErrorCode::InvalidSubsection, cursor.position));
        }
        cursor.consume_line_ending(XrefErrorCode::InvalidSubsection)?;
        let end = first
            .checked_add(count)
            .ok_or_else(|| cursor.failure(XrefErrorCode::InvalidSubsection, cursor.position))?;
        if first > u64::from(u32::MAX) || end > u64::from(u32::MAX) + 1 {
            return Err(cursor.failure(XrefErrorCode::InvalidSubsection, cursor.position));
        }
        if previous_end.is_some_and(|previous| first < previous) {
            return Err(cursor.failure(XrefErrorCode::InvalidSubsection, cursor.position));
        }
        previous_end = Some(end);
        let existing = u64::try_from(entries.len()).map_err(|_| {
            LocalFailure::Failed(XrefError::for_code(
                XrefErrorCode::InternalState,
                cursor.absolute_position().ok(),
            ))
        })?;
        if existing
            .checked_add(count)
            .is_none_or(|total| total > limits.max_entries)
        {
            return Err(LocalFailure::Failed(XrefError::resource(
                XrefLimitKind::Entries,
                limits.max_entries,
                existing,
                count,
                cursor.absolute_position().ok(),
            )));
        }
        let count_usize = usize::try_from(count).map_err(|_| {
            LocalFailure::Failed(XrefError::resource(
                XrefLimitKind::Allocation,
                limits.max_entries,
                existing,
                count,
                cursor.absolute_position().ok(),
            ))
        })?;
        entries.try_reserve_exact(count_usize).map_err(|_| {
            LocalFailure::Failed(XrefError::resource(
                XrefLimitKind::Allocation,
                limits.max_entries,
                existing,
                count,
                cursor.absolute_position().ok(),
            ))
        })?;
        let capacity = u64::try_from(entries.capacity()).map_err(|_| {
            LocalFailure::Failed(XrefError::for_code(
                XrefErrorCode::InternalState,
                cursor.absolute_position().ok(),
            ))
        })?;
        if capacity > limits.max_entries {
            return Err(LocalFailure::Failed(XrefError::resource(
                XrefLimitKind::Allocation,
                limits.max_entries,
                existing,
                capacity.saturating_sub(existing),
                cursor.absolute_position().ok(),
            )));
        }
        for index in 0..count {
            cursor.check_cancelled()?;
            let object_number = u32::try_from(first + index)
                .map_err(|_| cursor.failure(XrefErrorCode::InvalidSubsection, cursor.position))?;
            entries.push(cursor.parse_entry(object_number, source_len)?);
        }
    }
}

fn parse_trailer(
    snapshot: SourceSnapshot,
    startxref: u64,
    limits: XrefLimits,
    syntax_limits: SyntaxLimits,
    cursor: &mut Cursor<'_, '_>,
    entries: Vec<XrefEntry>,
) -> LocalResult<ParsedTraditionalSection> {
    if cursor.position == cursor.bytes.len() {
        return Err(cursor.incomplete(XrefErrorCode::InvalidTrailer));
    }
    let trailer_offset = cursor.absolute_position().map_err(LocalFailure::Failed)?;
    let input = SyntaxInput::new(
        snapshot.identity(),
        trailer_offset,
        &cursor.bytes[cursor.position..],
        cursor.extent,
    )
    .map_err(|error| LocalFailure::Failed(XrefError::from_syntax(error)))?;
    let syntax_cancellation = XrefSyntaxCancellation(cursor.cancellation);
    let mut parser =
        SyntaxParser::new_with_cancellation(input, syntax_limits, &syntax_cancellation)
            .map_err(|error| LocalFailure::Failed(XrefError::from_syntax(error)))?;
    let located = match parser.parse_object() {
        SyntaxPoll::Ready(value) => value,
        SyntaxPoll::NeedMore { .. } => return Err(LocalFailure::NeedMore),
        SyntaxPoll::EndOfInput => {
            return Err(cursor.failure(XrefErrorCode::InvalidTrailer, cursor.position));
        }
        SyntaxPoll::Failed(error) => {
            return Err(LocalFailure::Failed(XrefError::from_syntax(error)));
        }
    };
    let trailer_end = located.span().end_exclusive();
    let trailer = located
        .try_map(|object| match object {
            SyntaxObject::Dictionary(dictionary) => Ok(dictionary),
            _ => Err(XrefError::for_code(
                XrefErrorCode::InvalidTrailer,
                Some(trailer_offset),
            )),
        })
        .map_err(LocalFailure::Failed)?;
    let dictionary = trailer.value();
    let size_value =
        unique_trailer_value(dictionary, b"Size", trailer_offset, cursor.cancellation)?;
    let root_value =
        unique_trailer_value(dictionary, b"Root", trailer_offset, cursor.cancellation)?;
    let previous_value =
        unique_trailer_value(dictionary, b"Prev", trailer_offset, cursor.cancellation)?;
    let xref_stream_value =
        unique_trailer_value(dictionary, b"XRefStm", trailer_offset, cursor.cancellation)?;

    let declared_size_i64 = size_value
        .and_then(|value| value.value().as_integer())
        .ok_or_else(|| {
            LocalFailure::Failed(XrefError::for_code(
                XrefErrorCode::InvalidTrailer,
                Some(trailer_offset),
            ))
        })?;
    let declared_size = u32::try_from(declared_size_i64)
        .ok()
        .filter(|size| *size != 0)
        .ok_or_else(|| {
            LocalFailure::Failed(XrefError::for_code(
                XrefErrorCode::InvalidTrailer,
                Some(trailer_offset),
            ))
        })?;
    if u64::from(declared_size) > limits.max_entries {
        return Err(LocalFailure::Failed(XrefError::resource(
            XrefLimitKind::Entries,
            limits.max_entries,
            0,
            u64::from(declared_size),
            Some(trailer_offset),
        )));
    }
    let root = match root_value {
        Some(value) => Some(value.value().as_reference().ok_or_else(|| {
            LocalFailure::Failed(XrefError::for_code(
                XrefErrorCode::InvalidTrailer,
                Some(trailer_offset),
            ))
        })?),
        None => None,
    };
    let previous = parse_backward_offset(previous_value);
    let xref_stream = parse_backward_offset(xref_stream_value);

    let span_len = trailer_end.checked_sub(startxref).ok_or_else(|| {
        LocalFailure::Failed(XrefError::for_code(
            XrefErrorCode::InternalState,
            Some(startxref),
        ))
    })?;
    let span = ByteSpan::new(startxref, span_len).map_err(|_| {
        LocalFailure::Failed(XrefError::for_code(
            XrefErrorCode::InternalState,
            Some(startxref),
        ))
    })?;
    Ok(ParsedTraditionalSection {
        snapshot,
        startxref,
        span,
        declared_size,
        root,
        previous,
        xref_stream,
        entries,
        trailer,
    })
}

struct ParsedTraditionalSection {
    snapshot: SourceSnapshot,
    startxref: u64,
    span: ByteSpan,
    declared_size: u32,
    root: Option<pdf_rs_syntax::ObjectRef>,
    previous: Option<ParsedBackwardOffset>,
    xref_stream: Option<ParsedBackwardOffset>,
    entries: Vec<XrefEntry>,
    trailer: Located<PdfDictionary>,
}

fn finalize_base_section(
    section: ParsedTraditionalSection,
    cancellation: &dyn XrefCancellation,
) -> Result<XrefSection, XrefError> {
    let root = section.root.ok_or_else(|| {
        XrefError::for_code(
            XrefErrorCode::InvalidTrailer,
            Some(section.trailer.span().start()),
        )
    })?;
    let previous = validate_backward_offset(section.previous, section.startxref)?;
    if previous.is_some() {
        return Err(XrefError::for_code(
            XrefErrorCode::UnsupportedIncrementalRevision,
            section.previous.map(|value| value.operand_offset),
        ));
    }
    let xref_stream = validate_backward_offset(section.xref_stream, section.startxref)?;
    if xref_stream.is_some() {
        return Err(XrefError::for_code(
            XrefErrorCode::UnsupportedHybridXref,
            section.xref_stream.map(|value| value.operand_offset),
        ));
    }
    let maximum_object = section
        .entries
        .last()
        .map_or(0, |entry| entry.object_number());
    if maximum_object >= section.declared_size {
        return Err(XrefError::for_code(
            XrefErrorCode::InvalidTrailer,
            Some(section.trailer.span().start()),
        ));
    }
    let complete_entry_count = usize::try_from(section.declared_size)
        .is_ok_and(|declared| declared == section.entries.len());
    let mut complete_object_range = true;
    for (number, entry) in section.entries.iter().enumerate() {
        if number.is_multiple_of(256) && cancellation.is_cancelled() {
            return Err(XrefError::for_code(
                XrefErrorCode::Cancelled,
                Some(section.startxref),
            ));
        }
        if u32::try_from(number) != Ok(entry.object_number()) {
            complete_object_range = false;
            break;
        }
    }
    if !complete_entry_count || !complete_object_range {
        return Err(XrefError::for_code(
            XrefErrorCode::InvalidEntry,
            Some(section.startxref),
        ));
    }
    validate_object_zero(&section.entries, section.startxref, true)?;
    validate_free_entries(
        &section.entries,
        section.declared_size,
        section.startxref,
        cancellation,
    )?;
    let root_entry = section
        .entries
        .binary_search_by_key(&root.number(), |entry| entry.object_number())
        .ok()
        .map(|index| section.entries[index])
        .filter(|entry| {
            entry.generation() == root.generation()
                && matches!(entry.kind(), XrefEntryKind::InUse { .. })
        });
    if root.number() >= section.declared_size || root_entry.is_none() {
        return Err(XrefError::for_code(
            XrefErrorCode::InvalidTrailer,
            Some(section.trailer.span().start()),
        ));
    }
    Ok(XrefSection::new(
        section.snapshot,
        section.startxref,
        section.span,
        section.declared_size,
        root,
        section.entries,
        section.trailer,
    ))
}

fn finalize_revision_section(
    section: ParsedTraditionalSection,
    cancellation: &dyn XrefCancellation,
) -> Result<TraditionalRevisionSection, XrefError> {
    let previous = validate_backward_offset(section.previous, section.startxref)?;
    let xref_stream = validate_backward_offset(section.xref_stream, section.startxref)?;
    if previous
        .zip(xref_stream)
        .is_some_and(|(previous, stream)| previous >= stream)
    {
        return Err(XrefError::for_code(
            XrefErrorCode::InvalidTrailer,
            section.xref_stream.map(|value| value.operand_offset),
        ));
    }
    let maximum_object = section
        .entries
        .last()
        .map_or(0, |entry| entry.object_number());
    if maximum_object >= section.declared_size
        || section
            .root
            .is_some_and(|root| root.number() >= section.declared_size)
    {
        return Err(XrefError::for_code(
            XrefErrorCode::InvalidTrailer,
            Some(section.trailer.span().start()),
        ));
    }
    validate_free_entries(
        &section.entries,
        section.declared_size,
        section.startxref,
        cancellation,
    )?;
    validate_object_zero(&section.entries, section.startxref, false)?;
    for (index, entry) in section.entries.iter().enumerate() {
        if index.is_multiple_of(256) && cancellation.is_cancelled() {
            return Err(XrefError::for_code(
                XrefErrorCode::Cancelled,
                Some(section.startxref),
            ));
        }
        if matches!(entry.kind(), XrefEntryKind::InUse { offset } if offset == 0 || offset >= section.startxref)
        {
            return Err(XrefError::for_code(
                XrefErrorCode::InvalidEntry,
                Some(section.startxref),
            ));
        }
    }
    Ok(TraditionalRevisionSection::new(
        section.snapshot,
        section.startxref,
        section.span,
        section.declared_size,
        section.root,
        previous,
        xref_stream,
        section.entries,
        section.trailer,
    ))
}

fn validate_object_zero(
    entries: &[XrefEntry],
    startxref: u64,
    required: bool,
) -> Result<(), XrefError> {
    let object_zero = entries.first().filter(|entry| entry.object_number() == 0);
    if required && object_zero.is_none()
        || object_zero.is_some_and(|entry| {
            entry.generation() != u16::MAX || !matches!(entry.kind(), XrefEntryKind::Free { .. })
        })
    {
        return Err(XrefError::for_code(
            XrefErrorCode::InvalidEntry,
            Some(startxref),
        ));
    }
    Ok(())
}

#[derive(Clone, Copy)]
struct ParsedBackwardOffset {
    value: Option<u64>,
    operand_offset: u64,
}

fn parse_backward_offset(value: Option<&Located<SyntaxObject>>) -> Option<ParsedBackwardOffset> {
    value.map(|value| ParsedBackwardOffset {
        value: value
            .value()
            .as_integer()
            .and_then(|value| u64::try_from(value).ok()),
        operand_offset: value.span().start(),
    })
}

fn validate_backward_offset(
    value: Option<ParsedBackwardOffset>,
    startxref: u64,
) -> Result<Option<u64>, XrefError> {
    let Some(value) = value else {
        return Ok(None);
    };
    value
        .value
        .filter(|value| *value < startxref)
        .map(Some)
        .ok_or_else(|| {
            XrefError::for_code(XrefErrorCode::InvalidTrailer, Some(value.operand_offset))
        })
}

fn validate_free_entries(
    entries: &[XrefEntry],
    declared_size: u32,
    startxref: u64,
    cancellation: &dyn XrefCancellation,
) -> Result<(), XrefError> {
    for (index, entry) in entries.iter().enumerate() {
        if index.is_multiple_of(256) && cancellation.is_cancelled() {
            return Err(XrefError::for_code(
                XrefErrorCode::Cancelled,
                Some(startxref),
            ));
        }
        if let XrefEntryKind::Free { next_free } = entry.kind()
            && next_free >= declared_size
        {
            return Err(XrefError::for_code(
                XrefErrorCode::InvalidEntry,
                Some(startxref),
            ));
        }
    }
    Ok(())
}

fn unique_trailer_value<'a>(
    dictionary: &'a PdfDictionary,
    key: &[u8],
    trailer_offset: u64,
    cancellation: &dyn XrefCancellation,
) -> LocalResult<Option<&'a Located<SyntaxObject>>> {
    let mut value = None;
    for (index, entry) in dictionary.entries().iter().enumerate() {
        if index.is_multiple_of(256) && cancellation.is_cancelled() {
            return Err(LocalFailure::Failed(XrefError::for_code(
                XrefErrorCode::Cancelled,
                Some(trailer_offset),
            )));
        }
        if entry.key().value().bytes() == key {
            if value.is_some() {
                return Err(LocalFailure::Failed(XrefError::for_code(
                    XrefErrorCode::InvalidTrailer,
                    Some(trailer_offset),
                )));
            }
            value = Some(entry.value());
        }
    }
    Ok(value)
}

struct Cursor<'a, 'c> {
    bytes: &'a [u8],
    base_offset: u64,
    extent: InputExtent,
    position: usize,
    cancellation: &'c dyn XrefCancellation,
}

impl<'a, 'c> Cursor<'a, 'c> {
    const fn new(
        bytes: &'a [u8],
        base_offset: u64,
        extent: InputExtent,
        cancellation: &'c dyn XrefCancellation,
    ) -> Self {
        Self {
            bytes,
            base_offset,
            extent,
            position: 0,
            cancellation,
        }
    }

    fn check_cancelled(&self) -> LocalResult<()> {
        if self.cancellation.is_cancelled() {
            return Err(LocalFailure::Failed(XrefError::for_code(
                XrefErrorCode::Cancelled,
                self.absolute_position().ok(),
            )));
        }
        Ok(())
    }

    fn absolute_position(&self) -> Result<u64, XrefError> {
        absolute(self.base_offset, self.position)
    }

    fn failure(&self, code: XrefErrorCode, position: usize) -> LocalFailure {
        LocalFailure::Failed(XrefError::for_code(
            code,
            absolute(self.base_offset, position).ok(),
        ))
    }

    fn incomplete(&self, final_code: XrefErrorCode) -> LocalFailure {
        if self.extent == InputExtent::KnownSourceEnd {
            self.failure(final_code, self.bytes.len())
        } else {
            LocalFailure::NeedMore
        }
    }

    fn require(&self, len: usize, final_code: XrefErrorCode) -> LocalResult<()> {
        if self
            .position
            .checked_add(len)
            .is_none_or(|end| end > self.bytes.len())
        {
            return Err(self.incomplete(final_code));
        }
        Ok(())
    }

    fn skip_whitespace(&mut self) -> LocalResult<()> {
        loop {
            while self
                .bytes
                .get(self.position)
                .is_some_and(|byte| is_pdf_whitespace(*byte))
            {
                self.position += 1;
                if self.position.is_multiple_of(1024) {
                    self.check_cancelled()?;
                }
            }
            if self.bytes.get(self.position) != Some(&b'%') {
                return Ok(());
            }
            self.position += 1;
            while !matches!(self.bytes.get(self.position), None | Some(b'\r' | b'\n')) {
                self.position += 1;
                if self.position.is_multiple_of(1024) {
                    self.check_cancelled()?;
                }
            }
            if self.position == self.bytes.len() {
                return if self.extent == InputExtent::KnownSourceEnd {
                    Ok(())
                } else {
                    Err(LocalFailure::NeedMore)
                };
            }
        }
    }

    fn keyword_at_position(&self, keyword: &[u8]) -> LocalResult<bool> {
        let remaining = &self.bytes[self.position..];
        if remaining.len() < keyword.len() {
            if keyword.starts_with(remaining) && self.extent == InputExtent::MayContinue {
                return Err(LocalFailure::NeedMore);
            }
            return Ok(false);
        }
        if &remaining[..keyword.len()] != keyword {
            return Ok(false);
        }
        if remaining.len() == keyword.len() {
            return if self.extent == InputExtent::MayContinue {
                Err(LocalFailure::NeedMore)
            } else {
                Ok(true)
            };
        }
        Ok(is_pdf_whitespace(remaining[keyword.len()])
            || is_pdf_delimiter(remaining[keyword.len()]))
    }

    fn parse_decimal(&mut self, code: XrefErrorCode, max_digits: usize) -> LocalResult<u64> {
        let start = self.position;
        while self
            .bytes
            .get(self.position)
            .is_some_and(u8::is_ascii_digit)
        {
            self.position += 1;
            if self.position - start > max_digits {
                return Err(self.failure(code, start));
            }
        }
        if self.position == start {
            return Err(self.failure(code, start));
        }
        if self.position == self.bytes.len() && self.extent == InputExtent::MayContinue {
            return Err(LocalFailure::NeedMore);
        }
        parse_digits(&self.bytes[start..self.position]).ok_or_else(|| self.failure(code, start))
    }

    fn consume_horizontal_separator(&mut self, code: XrefErrorCode) -> LocalResult<()> {
        let start = self.position;
        while self
            .bytes
            .get(self.position)
            .is_some_and(|byte| is_horizontal_whitespace(*byte))
        {
            self.position += 1;
            if self.position.is_multiple_of(1024) {
                self.check_cancelled()?;
            }
        }
        if self.position == start {
            return Err(self.failure(code, start));
        }
        if self.position == self.bytes.len() {
            return Err(self.incomplete(code));
        }
        Ok(())
    }

    fn consume_line_ending(&mut self, code: XrefErrorCode) -> LocalResult<()> {
        while self
            .bytes
            .get(self.position)
            .is_some_and(|byte| is_horizontal_whitespace(*byte))
        {
            self.position += 1;
            if self.position.is_multiple_of(1024) {
                self.check_cancelled()?;
            }
        }
        match self.bytes.get(self.position) {
            Some(b'\n') => self.position += 1,
            Some(b'\r') => {
                self.position += 1;
                if self.bytes.get(self.position) == Some(&b'\n') {
                    self.position += 1;
                }
            }
            None => return Err(self.incomplete(code)),
            _ => return Err(self.failure(code, self.position)),
        }
        Ok(())
    }

    fn parse_entry(&mut self, object_number: u32, source_len: u64) -> LocalResult<XrefEntry> {
        let start = self.position;
        self.require(20, XrefErrorCode::InvalidEntry)?;
        let row = &self.bytes[start..start + 20];
        if !row[..10].iter().all(u8::is_ascii_digit)
            || row[10] != b' '
            || !row[11..16].iter().all(u8::is_ascii_digit)
            || row[16] != b' '
            || !matches!(
                (row[18], row[19]),
                (b' ', b'\n') | (b'\r', b'\n') | (b' ', b'\r')
            )
        {
            return Err(self.failure(XrefErrorCode::InvalidEntry, start));
        }
        let field = parse_digits(&row[..10])
            .ok_or_else(|| self.failure(XrefErrorCode::InvalidEntry, start))?;
        let generation_u64 = parse_digits(&row[11..16])
            .ok_or_else(|| self.failure(XrefErrorCode::InvalidEntry, start + 11))?;
        let generation = u16::try_from(generation_u64)
            .map_err(|_| self.failure(XrefErrorCode::InvalidEntry, start + 11))?;
        let kind = match row[17] {
            b'n' if field < source_len => XrefEntryKind::InUse { offset: field },
            b'f' => XrefEntryKind::Free {
                next_free: u32::try_from(field)
                    .map_err(|_| self.failure(XrefErrorCode::InvalidEntry, start))?,
            },
            _ => return Err(self.failure(XrefErrorCode::InvalidEntry, start + 17)),
        };
        self.position += 20;
        Ok(XrefEntry::new(object_number, generation, kind))
    }
}

fn absolute(base: u64, relative: usize) -> Result<u64, XrefError> {
    let relative = u64::try_from(relative)
        .map_err(|_| XrefError::for_code(XrefErrorCode::InternalState, Some(base)))?;
    base.checked_add(relative)
        .ok_or_else(|| XrefError::for_code(XrefErrorCode::InternalState, Some(base)))
}

fn parse_digits(bytes: &[u8]) -> Option<u64> {
    let mut value = 0_u64;
    for byte in bytes {
        if !byte.is_ascii_digit() {
            return None;
        }
        value = value
            .checked_mul(10)?
            .checked_add(u64::from(*byte - b'0'))?;
    }
    Some(value)
}

fn contains_line_ending(bytes: &[u8]) -> bool {
    bytes.iter().any(|byte| matches!(byte, b'\r' | b'\n'))
}

fn is_pdf_whitespace(byte: u8) -> bool {
    matches!(byte, 0 | b'\t' | b'\n' | 12 | b'\r' | b' ')
}

pub(crate) fn is_horizontal_whitespace(byte: u8) -> bool {
    matches!(byte, 0 | b'\t' | 12 | b' ')
}

fn is_pdf_delimiter(byte: u8) -> bool {
    matches!(
        byte,
        b'(' | b')' | b'<' | b'>' | b'[' | b']' | b'{' | b'}' | b'/' | b'%'
    )
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use pdf_rs_bytes::{
        SourceIdentity, SourceRevision, SourceSnapshot, SourceStableId, SourceValidator,
        SourceValidatorKind,
    };
    use pdf_rs_syntax::{InputExtent, SyntaxErrorCode, SyntaxLimits};

    use super::{SectionWindow, parse_section};
    use crate::{
        XrefCancellation, XrefErrorCategory, XrefErrorCode, XrefLimits, XrefRecoverability,
    };

    struct CancelOnSecondProbe(AtomicUsize);

    impl XrefCancellation for CancelOnSecondProbe {
        fn is_cancelled(&self) -> bool {
            self.0.fetch_add(1, Ordering::AcqRel) + 1 >= 2
        }
    }

    #[test]
    fn trailer_syntax_cancellation_maps_to_the_xref_terminal_policy() {
        let bytes = b"xref\n0 1\n0000000000 65535 f \ntrailer\n<< /Size 1 /Root 1 0 R >>";
        let source = SourceIdentity::new(SourceStableId::new([0x7a; 32]), SourceRevision::new(1));
        let snapshot = SourceSnapshot::new(
            source,
            Some(u64::try_from(bytes.len()).unwrap()),
            SourceValidator::new(SourceValidatorKind::FrozenResponse, [0x7b; 32]),
        );
        let cancellation = CancelOnSecondProbe(AtomicUsize::new(0));
        let error = match parse_section(
            SectionWindow::new(
                snapshot,
                0,
                bytes,
                InputExtent::KnownSourceEnd,
                u64::try_from(bytes.len()).unwrap(),
            ),
            XrefLimits::default(),
            SyntaxLimits::default(),
            &cancellation,
        ) {
            Err(error) => error,
            Ok(_) => panic!("the second probe must cancel trailer syntax parsing"),
        };

        assert_eq!(error.code(), XrefErrorCode::Cancelled);
        assert_eq!(error.category(), XrefErrorCategory::Cancellation);
        assert_eq!(error.recoverability(), XrefRecoverability::AbandonOperation);
        assert_eq!(
            error.syntax_error().map(|syntax| syntax.code()),
            Some(SyntaxErrorCode::Cancelled)
        );
    }
}
