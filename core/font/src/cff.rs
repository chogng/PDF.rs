use std::mem::size_of;
use std::ops::Range;
use std::sync::Arc;

use crate::model::{GlyphRecord, standard_name_for_sid};
use crate::{
    CffFont, CffParseOutcome, CffParseReport, FontBounds, FontCancellation, FontCoordinate,
    FontError, FontErrorCode, FontLimit, FontLimitKind, FontLimits, FontPoint, FontProfile,
    FontStats, FontUnsupported, FontUnsupportedKind, OutlineSegment,
};

const FIXED_ONE: i64 = 1 << 16;
const TYPE2_STACK_LIMIT: usize = 48;
const CFF_STANDARD_STRING_COUNT: u16 = 391;
const CFF_ISO_ADOBE_GLYPHS: usize = 229;

/// Parses and atomically publishes one standalone non-CID CFF1 font.
///
/// The foundational profile owns CFF INDEX, DICT, charset, StandardEncoding, bounded custom
/// Encoding validation, Type 2 subroutine, hint-mask, line, and cubic-curve semantics. It rejects
/// CID-keyed, CFF2, ExpertEncoding, non-default FontMatrix, and escaped Type 2 operators as typed
/// unsupported outcomes.
pub fn parse_cff<C: FontCancellation + ?Sized>(
    bytes: &[u8],
    profile: FontProfile,
    limits: FontLimits,
    cancellation: &C,
) -> CffParseReport {
    let mut parser = Parser::new(bytes, profile, limits, cancellation);
    let outcome = match parser.run() {
        Ok(font) => CffParseOutcome::Ready(font),
        Err(Stop::Unsupported(unsupported)) => CffParseOutcome::Unsupported(unsupported),
        Err(Stop::Error(error)) if error.code() == FontErrorCode::Cancelled => {
            CffParseOutcome::Cancelled(error)
        }
        Err(Stop::Error(error)) => CffParseOutcome::Failed(error),
    };
    CffParseReport {
        outcome,
        stats: parser.stats,
    }
}

#[derive(Clone, Debug, Default)]
struct Index {
    items: Vec<Range<usize>>,
    next: usize,
}

impl Index {
    fn len(&self) -> usize {
        self.items.len()
    }

    fn item(&self, index: usize) -> Option<Range<usize>> {
        self.items.get(index).cloned()
    }
}

#[derive(Clone, Copy, Debug, Default)]
struct TopDict {
    charset_offset: usize,
    encoding_offset: usize,
    charstrings_offset: Option<usize>,
    private_range: Option<(usize, usize)>,
}

#[derive(Clone, Copy, Debug, Default)]
struct PrivateDict {
    local_subrs_offset: Option<usize>,
    default_width: i64,
    nominal_width: i64,
}

enum Stop {
    Error(FontError),
    Unsupported(FontUnsupported),
}

type Result<T> = std::result::Result<T, Stop>;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Flow {
    Continue,
    Return,
    End,
}

struct GlyphContext {
    stack: Vec<i64>,
    x: i64,
    y: i64,
    stems: usize,
    width: Option<i64>,
    active_contour: bool,
}

impl GlyphContext {
    fn new() -> Self {
        Self {
            stack: Vec::new(),
            x: 0,
            y: 0,
            stems: 0,
            width: None,
            active_contour: false,
        }
    }
}

struct Parser<'a, C: FontCancellation + ?Sized> {
    bytes: &'a [u8],
    profile: FontProfile,
    limits: FontLimits,
    cancellation: &'a C,
    stats: FontStats,
    next_cancellation_fuel: u64,
}

impl<'a, C: FontCancellation + ?Sized> Parser<'a, C> {
    const fn new(
        bytes: &'a [u8],
        profile: FontProfile,
        limits: FontLimits,
        cancellation: &'a C,
    ) -> Self {
        Self {
            bytes,
            profile,
            limits,
            cancellation,
            stats: FontStats {
                input_bytes: 0,
                tables_visited: 0,
                glyphs: 0,
                cmap_segments: 0,
                glyph_data_bytes: 0,
                source_contours: 0,
                source_points: 0,
                components: 0,
                path_segments: 0,
                fuel: 0,
                retained_bytes: 0,
                peak_retained_bytes: 0,
            },
            next_cancellation_fuel: limits.cancellation_check_interval_fuel,
        }
    }

    fn run(&mut self) -> Result<CffFont> {
        self.check_cancelled()?;
        if self.profile != FontProfile::SimpleType1CStandardV1 {
            return Err(Stop::Unsupported(FontUnsupported::new(
                FontUnsupportedKind::ProfileMismatch,
                None,
            )));
        }
        let input_bytes = u64::try_from(self.bytes.len())
            .map_err(|_| self.error(FontErrorCode::NumericOverflow, None))?;
        if input_bytes > self.limits.max_input_bytes {
            return Err(self.resource(
                FontLimitKind::InputBytes,
                self.limits.max_input_bytes,
                0,
                input_bytes,
            ));
        }
        self.stats.input_bytes = input_bytes;
        self.charge(input_bytes)?;
        if self.bytes.len() < 4
            || self.bytes[0] != 1
            || self.bytes[2] < 4
            || usize::from(self.bytes[2]) > self.bytes.len()
            || !(1..=4).contains(&self.bytes[3])
        {
            return Err(self.error(FontErrorCode::InvalidCff, None));
        }

        let name_index = self.parse_index(usize::from(self.bytes[2]))?;
        let top_index = self.parse_index(name_index.next)?;
        let string_index = self.parse_index(top_index.next)?;
        let global_subrs = self.parse_index(string_index.next)?;
        self.stats.tables_visited = 4;
        if name_index.len() != 1 || top_index.len() != 1 {
            return Err(self.error(FontErrorCode::InvalidCff, None));
        }

        let top_range = top_index
            .item(0)
            .ok_or_else(|| self.error(FontErrorCode::InvalidCff, None))?;
        let top = self.parse_top_dict(top_range)?;
        let charstrings = self.parse_index(
            top.charstrings_offset
                .ok_or_else(|| self.error(FontErrorCode::InvalidCff, None))?,
        )?;
        self.stats.tables_visited = 5;
        if charstrings.len() == 0
            || charstrings.len() > usize::try_from(self.limits.max_glyphs).unwrap_or(usize::MAX)
            || charstrings.len() > usize::from(u16::MAX)
        {
            return Err(self.resource(
                FontLimitKind::Glyphs,
                u64::from(self.limits.max_glyphs),
                0,
                u64::try_from(charstrings.len()).unwrap_or(u64::MAX),
            ));
        }

        let charset = self.parse_charset(top.charset_offset, charstrings.len())?;
        self.validate_encoding(top.encoding_offset, charstrings.len(), &charset)?;
        if top.encoding_offset > 1 {
            self.stats.tables_visited = self
                .stats
                .tables_visited
                .checked_add(1)
                .ok_or_else(|| self.error(FontErrorCode::NumericOverflow, None))?;
        }
        let (private, local_subrs) = self.parse_private(top.private_range)?;
        let glyph_data_bytes = charstrings.items.iter().try_fold(0_u64, |total, range| {
            total.checked_add(u64::try_from(range.len()).ok()?)
        });
        let glyph_data_bytes =
            glyph_data_bytes.ok_or_else(|| self.error(FontErrorCode::NumericOverflow, None))?;
        if glyph_data_bytes > self.limits.max_glyph_data_bytes {
            return Err(self.resource(
                FontLimitKind::GlyphDataBytes,
                self.limits.max_glyph_data_bytes,
                0,
                glyph_data_bytes,
            ));
        }
        self.stats.glyph_data_bytes = glyph_data_bytes;

        let glyph_names = self.build_glyph_names(&charset, &string_index)?;
        let mut glyphs = Vec::new();
        glyphs
            .try_reserve_exact(charstrings.len())
            .map_err(|_| self.allocation_error())?;
        let mut segments = Vec::new();
        for glyph_index in 0..charstrings.len() {
            self.check_cancelled()?;
            let glyph_id = u16::try_from(glyph_index)
                .map_err(|_| self.error(FontErrorCode::NumericOverflow, None))?;
            let charstring = charstrings
                .item(glyph_index)
                .ok_or_else(|| self.error(FontErrorCode::InvalidCff, Some(glyph_id)))?;
            let glyph_bytes = u64::try_from(charstring.len()).unwrap_or(u64::MAX);
            if glyph_bytes > self.limits.max_glyph_bytes {
                return Err(self.resource_for_glyph(
                    FontLimitKind::GlyphBytes,
                    self.limits.max_glyph_bytes,
                    0,
                    glyph_bytes,
                    glyph_id,
                ));
            }
            let start = segments.len();
            let mut context = GlyphContext::new();
            let flow = self.execute_charstring(
                charstring,
                &local_subrs,
                &global_subrs,
                private.nominal_width,
                glyph_id,
                0,
                &mut context,
                &mut segments,
            )?;
            if flow != Flow::End {
                return Err(self.error(FontErrorCode::InvalidCharString, Some(glyph_id)));
            }
            let width = context.width.unwrap_or(private.default_width);
            let advance_width = fixed_to_u16(width)
                .ok_or_else(|| self.error(FontErrorCode::InvalidCharString, Some(glyph_id)))?;
            let segment_len = segments
                .len()
                .checked_sub(start)
                .ok_or_else(|| self.error(FontErrorCode::InternalState, Some(glyph_id)))?;
            let segment_start = u32::try_from(start)
                .map_err(|_| self.error(FontErrorCode::NumericOverflow, Some(glyph_id)))?;
            let segment_len = u32::try_from(segment_len)
                .map_err(|_| self.error(FontErrorCode::NumericOverflow, Some(glyph_id)))?;
            glyphs.push(GlyphRecord {
                advance_width,
                bounds: outline_bounds(&segments[start..])
                    .ok_or_else(|| self.error(FontErrorCode::NumericOverflow, Some(glyph_id)))?,
                segment_start,
                segment_len,
            });
            self.stats.glyphs = self
                .stats
                .glyphs
                .checked_add(1)
                .ok_or_else(|| self.error(FontErrorCode::NumericOverflow, Some(glyph_id)))?;
        }

        let retained = retained_bytes(&glyph_names, &glyphs, &segments)
            .ok_or_else(|| self.error(FontErrorCode::NumericOverflow, None))?;
        if retained > self.limits.max_retained_bytes {
            return Err(self.resource(
                FontLimitKind::RetainedBytes,
                self.limits.max_retained_bytes,
                0,
                retained,
            ));
        }
        self.stats.retained_bytes = retained;
        self.stats.peak_retained_bytes = retained;
        Ok(CffFont {
            profile: self.profile,
            limits: self.limits,
            stats: self.stats,
            units_per_em: 1_000,
            glyph_names: Arc::new(glyph_names),
            glyphs: Arc::new(glyphs),
            segments: Arc::new(segments),
        })
    }

    fn parse_index(&self, offset: usize) -> Result<Index> {
        let count = usize::from(
            read_u16(self.bytes, offset)
                .ok_or_else(|| self.error(FontErrorCode::InvalidCff, None))?,
        );
        if count == 0 {
            return Ok(Index {
                items: Vec::new(),
                next: offset
                    .checked_add(2)
                    .ok_or_else(|| self.error(FontErrorCode::NumericOverflow, None))?,
            });
        }
        let off_size_position = offset
            .checked_add(2)
            .ok_or_else(|| self.error(FontErrorCode::NumericOverflow, None))?;
        let off_size = usize::from(
            *self
                .bytes
                .get(off_size_position)
                .ok_or_else(|| self.error(FontErrorCode::InvalidCff, None))?,
        );
        if !(1..=4).contains(&off_size) {
            return Err(self.error(FontErrorCode::InvalidCff, None));
        }
        let offsets_position = off_size_position
            .checked_add(1)
            .ok_or_else(|| self.error(FontErrorCode::NumericOverflow, None))?;
        let offsets_bytes = count
            .checked_add(1)
            .and_then(|value| value.checked_mul(off_size))
            .ok_or_else(|| self.error(FontErrorCode::NumericOverflow, None))?;
        let data_position = offsets_position
            .checked_add(offsets_bytes)
            .ok_or_else(|| self.error(FontErrorCode::NumericOverflow, None))?;
        let mut offsets = Vec::new();
        offsets
            .try_reserve_exact(count + 1)
            .map_err(|_| self.allocation_error())?;
        for index in 0..=count {
            let position = offsets_position
                .checked_add(
                    index
                        .checked_mul(off_size)
                        .ok_or_else(|| self.error(FontErrorCode::NumericOverflow, None))?,
                )
                .ok_or_else(|| self.error(FontErrorCode::NumericOverflow, None))?;
            offsets.push(
                read_offset(self.bytes, position, off_size)
                    .ok_or_else(|| self.error(FontErrorCode::InvalidCff, None))?,
            );
        }
        if offsets.first().copied() != Some(1)
            || offsets.contains(&0)
            || offsets.windows(2).any(|pair| pair[0] > pair[1])
        {
            return Err(self.error(FontErrorCode::InvalidCff, None));
        }
        let end = data_position
            .checked_add(
                offsets[count]
                    .checked_sub(1)
                    .ok_or_else(|| self.error(FontErrorCode::InvalidCff, None))?,
            )
            .ok_or_else(|| self.error(FontErrorCode::NumericOverflow, None))?;
        if end > self.bytes.len() {
            return Err(self.error(FontErrorCode::InvalidCff, None));
        }
        let mut items = Vec::new();
        items
            .try_reserve_exact(count)
            .map_err(|_| self.allocation_error())?;
        for pair in offsets.windows(2) {
            let start = data_position + pair[0] - 1;
            let end = data_position + pair[1] - 1;
            items.push(start..end);
        }
        Ok(Index { items, next: end })
    }

    fn parse_top_dict(&self, range: Range<usize>) -> Result<TopDict> {
        let mut top = TopDict::default();
        for (operator, operands) in self.parse_dict(range)? {
            match operator {
                15 => {
                    top.charset_offset = one_offset(&operands).ok_or_else(|| self.invalid_cff())?
                }
                16 => {
                    top.encoding_offset = one_offset(&operands).ok_or_else(|| self.invalid_cff())?
                }
                17 => {
                    top.charstrings_offset =
                        Some(one_offset(&operands).ok_or_else(|| self.invalid_cff())?)
                }
                18 => {
                    if operands.len() != 2 {
                        return Err(self.invalid_cff());
                    }
                    let size = fixed_offset(operands[0]).ok_or_else(|| self.invalid_cff())?;
                    let offset = fixed_offset(operands[1]).ok_or_else(|| self.invalid_cff())?;
                    top.private_range = Some((offset, size));
                }
                1_207 => {
                    return Err(Stop::Unsupported(FontUnsupported::new(
                        FontUnsupportedKind::CffFontMatrix,
                        None,
                    )));
                }
                1_230 | 1_236 | 1_237 => {
                    return Err(Stop::Unsupported(FontUnsupported::new(
                        FontUnsupportedKind::CffCidFont,
                        None,
                    )));
                }
                _ => {}
            }
        }
        Ok(top)
    }

    fn parse_private(&self, range: Option<(usize, usize)>) -> Result<(PrivateDict, Index)> {
        let Some((offset, size)) = range else {
            return Ok((PrivateDict::default(), Index::default()));
        };
        let end = offset
            .checked_add(size)
            .ok_or_else(|| self.error(FontErrorCode::NumericOverflow, None))?;
        if end > self.bytes.len() {
            return Err(self.invalid_cff());
        }
        let mut private = PrivateDict::default();
        for (operator, operands) in self.parse_dict(offset..end)? {
            match operator {
                19 => {
                    private.local_subrs_offset =
                        Some(one_offset(&operands).ok_or_else(|| self.invalid_cff())?)
                }
                20 => {
                    private.default_width =
                        one_fixed(&operands).ok_or_else(|| self.invalid_cff())?
                }
                21 => {
                    private.nominal_width =
                        one_fixed(&operands).ok_or_else(|| self.invalid_cff())?
                }
                _ => {}
            }
        }
        let local_subrs = if let Some(relative) = private.local_subrs_offset {
            self.parse_index(
                offset
                    .checked_add(relative)
                    .ok_or_else(|| self.error(FontErrorCode::NumericOverflow, None))?,
            )?
        } else {
            Index::default()
        };
        Ok((private, local_subrs))
    }

    fn parse_dict(&self, range: Range<usize>) -> Result<Vec<(u16, Vec<i64>)>> {
        let data = self.bytes.get(range).ok_or_else(|| self.invalid_cff())?;
        let mut position = 0_usize;
        let mut operands = Vec::new();
        let mut entries = Vec::new();
        while position < data.len() {
            let byte = data[position];
            position += 1;
            match byte {
                0..=21 => {
                    let operator = if byte == 12 {
                        let escaped = *data.get(position).ok_or_else(|| self.invalid_cff())?;
                        position += 1;
                        1_200 + u16::from(escaped)
                    } else {
                        u16::from(byte)
                    };
                    entries.push((operator, std::mem::take(&mut operands)));
                }
                28 => {
                    let value = read_i16(data, position).ok_or_else(|| self.invalid_cff())?;
                    position += 2;
                    push_operand(&mut operands, i64::from(value) * FIXED_ONE)
                        .map_err(|_| self.invalid_cff())?;
                }
                29 => {
                    let value = read_i32(data, position).ok_or_else(|| self.invalid_cff())?;
                    position += 4;
                    push_operand(&mut operands, i64::from(value) * FIXED_ONE)
                        .map_err(|_| self.invalid_cff())?;
                }
                30 => {
                    loop {
                        let nibbles = *data.get(position).ok_or_else(|| self.invalid_cff())?;
                        position += 1;
                        if nibbles >> 4 == 0x0f || nibbles & 0x0f == 0x0f {
                            break;
                        }
                    }
                    push_operand(&mut operands, 0).map_err(|_| self.invalid_cff())?;
                }
                32..=254 => {
                    let value = decode_compact_integer(byte, data, &mut position)
                        .ok_or_else(|| self.invalid_cff())?;
                    push_operand(&mut operands, value * FIXED_ONE)
                        .map_err(|_| self.invalid_cff())?;
                }
                _ => return Err(self.invalid_cff()),
            }
        }
        if !operands.is_empty() {
            return Err(self.invalid_cff());
        }
        Ok(entries)
    }

    fn parse_charset(&self, offset: usize, glyph_count: usize) -> Result<Vec<u16>> {
        if offset == 0 {
            if glyph_count > CFF_ISO_ADOBE_GLYPHS {
                return Err(self.invalid_cff());
            }
            return (0..glyph_count)
                .map(|value| u16::try_from(value).map_err(|_| self.invalid_cff()))
                .collect();
        }
        if offset == 1 || offset == 2 {
            return Err(Stop::Unsupported(FontUnsupported::new(
                FontUnsupportedKind::CffEncoding,
                None,
            )));
        }
        let format = *self.bytes.get(offset).ok_or_else(|| self.invalid_cff())?;
        let mut position = offset + 1;
        let mut sids = Vec::new();
        sids.try_reserve_exact(glyph_count)
            .map_err(|_| self.allocation_error())?;
        sids.push(0);
        match format {
            0 => {
                while sids.len() < glyph_count {
                    sids.push(read_u16(self.bytes, position).ok_or_else(|| self.invalid_cff())?);
                    position += 2;
                }
            }
            1 | 2 => {
                while sids.len() < glyph_count {
                    let first = read_u16(self.bytes, position).ok_or_else(|| self.invalid_cff())?;
                    position += 2;
                    let left = if format == 1 {
                        let value = usize::from(
                            *self.bytes.get(position).ok_or_else(|| self.invalid_cff())?,
                        );
                        position += 1;
                        value
                    } else {
                        let value = usize::from(
                            read_u16(self.bytes, position).ok_or_else(|| self.invalid_cff())?,
                        );
                        position += 2;
                        value
                    };
                    if sids.len().saturating_add(left + 1) > glyph_count {
                        return Err(self.invalid_cff());
                    }
                    for delta in 0..=left {
                        sids.push(
                            first
                                .checked_add(u16::try_from(delta).map_err(|_| self.invalid_cff())?)
                                .ok_or_else(|| self.invalid_cff())?,
                        );
                    }
                }
            }
            _ => return Err(self.invalid_cff()),
        }
        Ok(sids)
    }

    fn validate_encoding(&self, offset: usize, glyph_count: usize, charset: &[u16]) -> Result<()> {
        if offset == 0 {
            return Ok(());
        }
        if offset == 1 {
            return Err(Stop::Unsupported(FontUnsupported::new(
                FontUnsupportedKind::CffEncoding,
                None,
            )));
        }
        let format = *self.bytes.get(offset).ok_or_else(|| self.invalid_cff())?;
        let base_format = format & 0x7f;
        if base_format > 1 {
            return Err(self.invalid_cff());
        }
        let mut position = offset
            .checked_add(1)
            .ok_or_else(|| self.error(FontErrorCode::NumericOverflow, None))?;
        let mut seen = [false; 256];
        seen[0] = true;
        let mut encoded_glyphs = 1_usize;
        match base_format {
            0 => {
                let count =
                    usize::from(*self.bytes.get(position).ok_or_else(|| self.invalid_cff())?);
                position = position
                    .checked_add(1)
                    .ok_or_else(|| self.error(FontErrorCode::NumericOverflow, None))?;
                if count > glyph_count.saturating_sub(1) {
                    return Err(self.invalid_cff());
                }
                for code in self
                    .bytes
                    .get(
                        position
                            ..position
                                .checked_add(count)
                                .ok_or_else(|| self.error(FontErrorCode::NumericOverflow, None))?,
                    )
                    .ok_or_else(|| self.invalid_cff())?
                {
                    if seen[usize::from(*code)] {
                        return Err(self.invalid_cff());
                    }
                    seen[usize::from(*code)] = true;
                    encoded_glyphs = encoded_glyphs
                        .checked_add(1)
                        .ok_or_else(|| self.error(FontErrorCode::NumericOverflow, None))?;
                }
                position = position
                    .checked_add(count)
                    .ok_or_else(|| self.error(FontErrorCode::NumericOverflow, None))?;
            }
            1 => {
                let range_count =
                    usize::from(*self.bytes.get(position).ok_or_else(|| self.invalid_cff())?);
                position = position
                    .checked_add(1)
                    .ok_or_else(|| self.error(FontErrorCode::NumericOverflow, None))?;
                for _ in 0..range_count {
                    let first = *self.bytes.get(position).ok_or_else(|| self.invalid_cff())?;
                    let left = *self
                        .bytes
                        .get(position + 1)
                        .ok_or_else(|| self.invalid_cff())?;
                    position = position
                        .checked_add(2)
                        .ok_or_else(|| self.error(FontErrorCode::NumericOverflow, None))?;
                    let last = first.checked_add(left).ok_or_else(|| self.invalid_cff())?;
                    let range_glyphs = usize::from(left) + 1;
                    encoded_glyphs = encoded_glyphs
                        .checked_add(range_glyphs)
                        .ok_or_else(|| self.error(FontErrorCode::NumericOverflow, None))?;
                    if encoded_glyphs > glyph_count {
                        return Err(self.invalid_cff());
                    }
                    for code in first..=last {
                        if seen[usize::from(code)] {
                            return Err(self.invalid_cff());
                        }
                        seen[usize::from(code)] = true;
                    }
                }
            }
            _ => return Err(self.invalid_cff()),
        }
        if encoded_glyphs > glyph_count {
            return Err(self.invalid_cff());
        }
        if format & 0x80 != 0 {
            let supplement_count =
                usize::from(*self.bytes.get(position).ok_or_else(|| self.invalid_cff())?);
            position = position
                .checked_add(1)
                .ok_or_else(|| self.error(FontErrorCode::NumericOverflow, None))?;
            for _ in 0..supplement_count {
                let code = *self.bytes.get(position).ok_or_else(|| self.invalid_cff())?;
                let sid = read_u16(self.bytes, position + 1).ok_or_else(|| self.invalid_cff())?;
                position = position
                    .checked_add(3)
                    .ok_or_else(|| self.error(FontErrorCode::NumericOverflow, None))?;
                if seen[usize::from(code)] || !charset.contains(&sid) {
                    return Err(self.invalid_cff());
                }
                seen[usize::from(code)] = true;
            }
        }
        Ok(())
    }

    fn build_glyph_names(&self, charset: &[u16], strings: &Index) -> Result<Vec<Option<Box<str>>>> {
        let mut names = Vec::new();
        names
            .try_reserve_exact(charset.len())
            .map_err(|_| self.allocation_error())?;
        for sid in charset {
            let name = if let Some(name) = standard_name_for_sid(*sid) {
                Some(Box::<str>::from(name))
            } else if *sid >= CFF_STANDARD_STRING_COUNT {
                let index = usize::from(*sid - CFF_STANDARD_STRING_COUNT);
                strings.item(index).and_then(|range| {
                    std::str::from_utf8(self.bytes.get(range)?)
                        .ok()
                        .map(Box::<str>::from)
                })
            } else {
                None
            };
            names.push(name);
        }
        Ok(names)
    }

    #[allow(clippy::too_many_arguments)]
    fn execute_charstring(
        &mut self,
        range: Range<usize>,
        local_subrs: &Index,
        global_subrs: &Index,
        nominal_width: i64,
        glyph_id: u16,
        depth: u16,
        context: &mut GlyphContext,
        segments: &mut Vec<OutlineSegment>,
    ) -> Result<Flow> {
        if depth > self.limits.max_component_depth {
            return Err(self.resource_for_glyph(
                FontLimitKind::ComponentDepth,
                u64::from(self.limits.max_component_depth),
                u64::from(depth.saturating_sub(1)),
                1,
                glyph_id,
            ));
        }
        let data = self
            .bytes
            .get(range)
            .ok_or_else(|| self.error(FontErrorCode::InvalidCharString, Some(glyph_id)))?;
        let mut position = 0_usize;
        while position < data.len() {
            self.charge(1)?;
            let byte = data[position];
            position += 1;
            if byte == 28 || byte >= 32 {
                let value = if byte == 28 {
                    let value = read_i16(data, position)
                        .ok_or_else(|| self.invalid_charstring(glyph_id))?;
                    position += 2;
                    i64::from(value) * FIXED_ONE
                } else if byte == 255 {
                    let value = read_i32(data, position)
                        .ok_or_else(|| self.invalid_charstring(glyph_id))?;
                    position += 4;
                    i64::from(value)
                } else {
                    let value = decode_compact_integer(byte, data, &mut position)
                        .ok_or_else(|| self.invalid_charstring(glyph_id))?;
                    value * FIXED_ONE
                };
                push_operand(&mut context.stack, value)
                    .map_err(|_| self.invalid_charstring(glyph_id))?;
                continue;
            }
            match byte {
                1 | 3 | 18 | 23 => {
                    self.consume_stems(context, nominal_width, glyph_id)?;
                }
                4 => {
                    let values = self.take_move_arguments(context, 1, nominal_width, glyph_id)?;
                    self.move_by(0, values[0], context, segments, glyph_id)?;
                }
                5 => self.relative_lines(context, segments, glyph_id)?,
                6 => self.alternating_lines(context, segments, glyph_id, true)?,
                7 => self.alternating_lines(context, segments, glyph_id, false)?,
                8 => self.relative_curves(context, segments, glyph_id)?,
                10 | 29 => {
                    let operand = context
                        .stack
                        .pop()
                        .ok_or_else(|| self.invalid_charstring(glyph_id))?;
                    let subrs = if byte == 10 {
                        local_subrs
                    } else {
                        global_subrs
                    };
                    let index = subroutine_index(operand, subrs.len())
                        .ok_or_else(|| self.invalid_charstring(glyph_id))?;
                    let subroutine = subrs
                        .item(index)
                        .ok_or_else(|| self.invalid_charstring(glyph_id))?;
                    match self.execute_charstring(
                        subroutine,
                        local_subrs,
                        global_subrs,
                        nominal_width,
                        glyph_id,
                        depth + 1,
                        context,
                        segments,
                    )? {
                        Flow::Return => {}
                        Flow::End => return Ok(Flow::End),
                        Flow::Continue => {
                            return Err(self.invalid_charstring(glyph_id));
                        }
                    }
                }
                11 => return Ok(Flow::Return),
                12 => {
                    let escaped = *data
                        .get(position)
                        .ok_or_else(|| self.invalid_charstring(glyph_id))?;
                    position = position
                        .checked_add(1)
                        .ok_or_else(|| self.numeric(glyph_id))?;
                    self.execute_escaped_operator(escaped, context, segments, glyph_id)?;
                }
                14 => {
                    if context.width.is_none() && context.stack.len() == 1 {
                        context.width = Some(
                            nominal_width
                                .checked_add(context.stack[0])
                                .ok_or_else(|| self.numeric(glyph_id))?,
                        );
                        context.stack.clear();
                    }
                    if !context.stack.is_empty() {
                        return Err(Stop::Unsupported(FontUnsupported::new(
                            FontUnsupportedKind::CffCharStringOperator,
                            Some(glyph_id),
                        )));
                    }
                    self.close_contour(context, segments, glyph_id)?;
                    return Ok(Flow::End);
                }
                19 | 20 => {
                    self.consume_stems(context, nominal_width, glyph_id)?;
                    let mask_bytes = context
                        .stems
                        .checked_add(7)
                        .map(|value| value / 8)
                        .ok_or_else(|| self.numeric(glyph_id))?;
                    position = position
                        .checked_add(mask_bytes)
                        .ok_or_else(|| self.numeric(glyph_id))?;
                    if position > data.len() {
                        return Err(self.invalid_charstring(glyph_id));
                    }
                }
                21 => {
                    let values = self.take_move_arguments(context, 2, nominal_width, glyph_id)?;
                    self.move_by(values[0], values[1], context, segments, glyph_id)?;
                }
                22 => {
                    let values = self.take_move_arguments(context, 1, nominal_width, glyph_id)?;
                    self.move_by(values[0], 0, context, segments, glyph_id)?;
                }
                24 => self.curve_then_line(context, segments, glyph_id)?,
                25 => self.lines_then_curve(context, segments, glyph_id)?,
                26 => self.vv_curves(context, segments, glyph_id)?,
                27 => self.hh_curves(context, segments, glyph_id)?,
                30 => self.alternating_curves(context, segments, glyph_id, false)?,
                31 => self.alternating_curves(context, segments, glyph_id, true)?,
                _ => return Err(self.invalid_charstring(glyph_id)),
            }
        }
        Ok(Flow::Continue)
    }

    fn consume_stems(
        &self,
        context: &mut GlyphContext,
        nominal_width: i64,
        glyph_id: u16,
    ) -> Result<()> {
        if context.width.is_none() && !context.stack.len().is_multiple_of(2) {
            let width = context.stack.remove(0);
            context.width = Some(
                nominal_width
                    .checked_add(width)
                    .ok_or_else(|| self.numeric(glyph_id))?,
            );
        }
        if !context.stack.len().is_multiple_of(2) {
            return Err(self.invalid_charstring(glyph_id));
        }
        context.stems = context
            .stems
            .checked_add(context.stack.len() / 2)
            .ok_or_else(|| self.numeric(glyph_id))?;
        context.stack.clear();
        Ok(())
    }

    fn take_move_arguments(
        &self,
        context: &mut GlyphContext,
        expected: usize,
        nominal_width: i64,
        glyph_id: u16,
    ) -> Result<Vec<i64>> {
        if context.width.is_none() && context.stack.len() == expected + 1 {
            let width = context.stack.remove(0);
            context.width = Some(
                nominal_width
                    .checked_add(width)
                    .ok_or_else(|| self.numeric(glyph_id))?,
            );
        }
        if context.stack.len() != expected {
            return Err(self.invalid_charstring(glyph_id));
        }
        Ok(std::mem::take(&mut context.stack))
    }

    fn relative_lines(
        &mut self,
        context: &mut GlyphContext,
        segments: &mut Vec<OutlineSegment>,
        glyph_id: u16,
    ) -> Result<()> {
        let values = std::mem::take(&mut context.stack);
        if values.is_empty() || !values.len().is_multiple_of(2) {
            return Err(self.invalid_charstring(glyph_id));
        }
        for pair in values.chunks_exact(2) {
            self.line_by(pair[0], pair[1], context, segments, glyph_id)?;
        }
        Ok(())
    }

    fn alternating_lines(
        &mut self,
        context: &mut GlyphContext,
        segments: &mut Vec<OutlineSegment>,
        glyph_id: u16,
        horizontal_first: bool,
    ) -> Result<()> {
        let values = std::mem::take(&mut context.stack);
        if values.is_empty() {
            return Err(self.invalid_charstring(glyph_id));
        }
        let mut horizontal = horizontal_first;
        for value in values {
            let (dx, dy) = if horizontal { (value, 0) } else { (0, value) };
            self.line_by(dx, dy, context, segments, glyph_id)?;
            horizontal = !horizontal;
        }
        Ok(())
    }

    fn relative_curves(
        &mut self,
        context: &mut GlyphContext,
        segments: &mut Vec<OutlineSegment>,
        glyph_id: u16,
    ) -> Result<()> {
        let values = std::mem::take(&mut context.stack);
        if values.is_empty() || !values.len().is_multiple_of(6) {
            return Err(self.invalid_charstring(glyph_id));
        }
        for curve in values.chunks_exact(6) {
            self.curve_by(curve, context, segments, glyph_id)?;
        }
        Ok(())
    }

    fn curve_then_line(
        &mut self,
        context: &mut GlyphContext,
        segments: &mut Vec<OutlineSegment>,
        glyph_id: u16,
    ) -> Result<()> {
        let values = std::mem::take(&mut context.stack);
        if values.len() < 8 || !(values.len() - 2).is_multiple_of(6) {
            return Err(self.invalid_charstring(glyph_id));
        }
        let curve_end = values.len() - 2;
        for curve in values[..curve_end].chunks_exact(6) {
            self.curve_by(curve, context, segments, glyph_id)?;
        }
        self.line_by(
            values[curve_end],
            values[curve_end + 1],
            context,
            segments,
            glyph_id,
        )
    }

    fn lines_then_curve(
        &mut self,
        context: &mut GlyphContext,
        segments: &mut Vec<OutlineSegment>,
        glyph_id: u16,
    ) -> Result<()> {
        let values = std::mem::take(&mut context.stack);
        if values.len() < 8 || !(values.len() - 6).is_multiple_of(2) {
            return Err(self.invalid_charstring(glyph_id));
        }
        let line_end = values.len() - 6;
        for pair in values[..line_end].chunks_exact(2) {
            self.line_by(pair[0], pair[1], context, segments, glyph_id)?;
        }
        self.curve_by(&values[line_end..], context, segments, glyph_id)
    }

    fn vv_curves(
        &mut self,
        context: &mut GlyphContext,
        segments: &mut Vec<OutlineSegment>,
        glyph_id: u16,
    ) -> Result<()> {
        let values = std::mem::take(&mut context.stack);
        let mut index = usize::from(values.len() % 4 == 1);
        if values.len() < 4 || !(values.len() - index).is_multiple_of(4) {
            return Err(self.invalid_charstring(glyph_id));
        }
        let mut dx1 = if index == 1 { values[0] } else { 0 };
        while index < values.len() {
            let curve = [
                dx1,
                values[index],
                values[index + 1],
                values[index + 2],
                0,
                values[index + 3],
            ];
            self.curve_by(&curve, context, segments, glyph_id)?;
            dx1 = 0;
            index += 4;
        }
        Ok(())
    }

    fn hh_curves(
        &mut self,
        context: &mut GlyphContext,
        segments: &mut Vec<OutlineSegment>,
        glyph_id: u16,
    ) -> Result<()> {
        let values = std::mem::take(&mut context.stack);
        let mut index = usize::from(values.len() % 4 == 1);
        if values.len() < 4 || !(values.len() - index).is_multiple_of(4) {
            return Err(self.invalid_charstring(glyph_id));
        }
        let mut dy1 = if index == 1 { values[0] } else { 0 };
        while index < values.len() {
            let curve = [
                values[index],
                dy1,
                values[index + 1],
                values[index + 2],
                values[index + 3],
                0,
            ];
            self.curve_by(&curve, context, segments, glyph_id)?;
            dy1 = 0;
            index += 4;
        }
        Ok(())
    }

    fn alternating_curves(
        &mut self,
        context: &mut GlyphContext,
        segments: &mut Vec<OutlineSegment>,
        glyph_id: u16,
        horizontal_first: bool,
    ) -> Result<()> {
        let values = std::mem::take(&mut context.stack);
        if values.len() < 4 || values.len() % 4 > 1 {
            return Err(self.invalid_charstring(glyph_id));
        }
        let mut index = 0_usize;
        let mut horizontal = horizontal_first;
        while values.len() - index >= 4 {
            let remaining = values.len() - index;
            let extra = if remaining == 5 { values[index + 4] } else { 0 };
            let curve = if horizontal {
                [
                    values[index],
                    0,
                    values[index + 1],
                    values[index + 2],
                    extra,
                    values[index + 3],
                ]
            } else {
                [
                    0,
                    values[index],
                    values[index + 1],
                    values[index + 2],
                    values[index + 3],
                    extra,
                ]
            };
            self.curve_by(&curve, context, segments, glyph_id)?;
            index += if remaining == 5 { 5 } else { 4 };
            horizontal = !horizontal;
        }
        if index != values.len() {
            return Err(self.invalid_charstring(glyph_id));
        }
        Ok(())
    }

    fn execute_escaped_operator(
        &mut self,
        operator: u8,
        context: &mut GlyphContext,
        segments: &mut Vec<OutlineSegment>,
        glyph_id: u16,
    ) -> Result<()> {
        let values = std::mem::take(&mut context.stack);
        match operator {
            34 if values.len() == 7 => {
                let negative_dy = values[2]
                    .checked_neg()
                    .ok_or_else(|| self.numeric(glyph_id))?;
                self.curve_by(
                    &[values[0], 0, values[1], values[2], values[3], 0],
                    context,
                    segments,
                    glyph_id,
                )?;
                self.curve_by(
                    &[values[4], 0, values[5], negative_dy, values[6], 0],
                    context,
                    segments,
                    glyph_id,
                )
            }
            35 if values.len() == 13 => {
                self.curve_by(&values[..6], context, segments, glyph_id)?;
                self.curve_by(&values[6..12], context, segments, glyph_id)
            }
            36 if values.len() == 9 => {
                let dy6 = values[1]
                    .checked_add(values[3])
                    .and_then(|value| value.checked_add(values[7]))
                    .and_then(i64::checked_neg)
                    .ok_or_else(|| self.numeric(glyph_id))?;
                self.curve_by(
                    &[values[0], values[1], values[2], values[3], values[4], 0],
                    context,
                    segments,
                    glyph_id,
                )?;
                self.curve_by(
                    &[values[5], 0, values[6], values[7], values[8], dy6],
                    context,
                    segments,
                    glyph_id,
                )
            }
            37 if values.len() == 11 => {
                let dx = checked_sum(&[values[0], values[2], values[4], values[6], values[8]])
                    .ok_or_else(|| self.numeric(glyph_id))?;
                let dy = checked_sum(&[values[1], values[3], values[5], values[7], values[9]])
                    .ok_or_else(|| self.numeric(glyph_id))?;
                let (dx6, dy6) = if dx.checked_abs().ok_or_else(|| self.numeric(glyph_id))?
                    > dy.checked_abs().ok_or_else(|| self.numeric(glyph_id))?
                {
                    (
                        values[10],
                        dy.checked_neg().ok_or_else(|| self.numeric(glyph_id))?,
                    )
                } else {
                    (
                        dx.checked_neg().ok_or_else(|| self.numeric(glyph_id))?,
                        values[10],
                    )
                };
                self.curve_by(&values[..6], context, segments, glyph_id)?;
                self.curve_by(
                    &[values[6], values[7], values[8], values[9], dx6, dy6],
                    context,
                    segments,
                    glyph_id,
                )
            }
            34..=37 => Err(self.invalid_charstring(glyph_id)),
            _ => Err(Stop::Unsupported(FontUnsupported::new(
                FontUnsupportedKind::CffCharStringOperator,
                Some(glyph_id),
            ))),
        }
    }

    fn move_by(
        &mut self,
        dx: i64,
        dy: i64,
        context: &mut GlyphContext,
        segments: &mut Vec<OutlineSegment>,
        glyph_id: u16,
    ) -> Result<()> {
        self.close_contour(context, segments, glyph_id)?;
        context.x = context
            .x
            .checked_add(dx)
            .ok_or_else(|| self.numeric(glyph_id))?;
        context.y = context
            .y
            .checked_add(dy)
            .ok_or_else(|| self.numeric(glyph_id))?;
        let point = fixed_point(context.x, context.y)
            .ok_or_else(|| self.error(FontErrorCode::NumericOverflow, Some(glyph_id)))?;
        self.push_segment(segments, OutlineSegment::MoveTo(point), glyph_id)?;
        context.active_contour = true;
        self.stats.source_contours = self
            .stats
            .source_contours
            .checked_add(1)
            .ok_or_else(|| self.numeric(glyph_id))?;
        Ok(())
    }

    fn line_by(
        &mut self,
        dx: i64,
        dy: i64,
        context: &mut GlyphContext,
        segments: &mut Vec<OutlineSegment>,
        glyph_id: u16,
    ) -> Result<()> {
        if !context.active_contour {
            return Err(self.invalid_charstring(glyph_id));
        }
        context.x = context
            .x
            .checked_add(dx)
            .ok_or_else(|| self.numeric(glyph_id))?;
        context.y = context
            .y
            .checked_add(dy)
            .ok_or_else(|| self.numeric(glyph_id))?;
        let point = fixed_point(context.x, context.y)
            .ok_or_else(|| self.error(FontErrorCode::NumericOverflow, Some(glyph_id)))?;
        self.push_segment(segments, OutlineSegment::LineTo(point), glyph_id)
    }

    fn curve_by(
        &mut self,
        values: &[i64],
        context: &mut GlyphContext,
        segments: &mut Vec<OutlineSegment>,
        glyph_id: u16,
    ) -> Result<()> {
        if !context.active_contour || values.len() != 6 {
            return Err(self.invalid_charstring(glyph_id));
        }
        let control_1_x = context
            .x
            .checked_add(values[0])
            .ok_or_else(|| self.numeric(glyph_id))?;
        let control_1_y = context
            .y
            .checked_add(values[1])
            .ok_or_else(|| self.numeric(glyph_id))?;
        let control_2_x = control_1_x
            .checked_add(values[2])
            .ok_or_else(|| self.numeric(glyph_id))?;
        let control_2_y = control_1_y
            .checked_add(values[3])
            .ok_or_else(|| self.numeric(glyph_id))?;
        context.x = control_2_x
            .checked_add(values[4])
            .ok_or_else(|| self.numeric(glyph_id))?;
        context.y = control_2_y
            .checked_add(values[5])
            .ok_or_else(|| self.numeric(glyph_id))?;
        let control_1 =
            fixed_point(control_1_x, control_1_y).ok_or_else(|| self.numeric(glyph_id))?;
        let control_2 =
            fixed_point(control_2_x, control_2_y).ok_or_else(|| self.numeric(glyph_id))?;
        let end = fixed_point(context.x, context.y).ok_or_else(|| self.numeric(glyph_id))?;
        self.push_segment(
            segments,
            OutlineSegment::CubicTo {
                control_1,
                control_2,
                end,
            },
            glyph_id,
        )
    }

    fn close_contour(
        &mut self,
        context: &mut GlyphContext,
        segments: &mut Vec<OutlineSegment>,
        glyph_id: u16,
    ) -> Result<()> {
        if context.active_contour {
            self.push_segment(segments, OutlineSegment::CloseContour, glyph_id)?;
            context.active_contour = false;
        }
        Ok(())
    }

    fn push_segment(
        &mut self,
        segments: &mut Vec<OutlineSegment>,
        segment: OutlineSegment,
        glyph_id: u16,
    ) -> Result<()> {
        if self.stats.path_segments >= self.limits.max_path_segments {
            return Err(self.resource_for_glyph(
                FontLimitKind::PathSegments,
                self.limits.max_path_segments,
                self.stats.path_segments,
                1,
                glyph_id,
            ));
        }
        segments
            .try_reserve(1)
            .map_err(|_| self.allocation_error())?;
        segments.push(segment);
        self.stats.path_segments += 1;
        let points = match segment {
            OutlineSegment::MoveTo(_) | OutlineSegment::LineTo(_) => 1,
            OutlineSegment::QuadTo { .. } => 2,
            OutlineSegment::CubicTo { .. } => 3,
            OutlineSegment::CloseContour => 0,
        };
        self.stats.source_points = self
            .stats
            .source_points
            .checked_add(points)
            .ok_or_else(|| self.numeric(glyph_id))?;
        Ok(())
    }

    fn charge(&mut self, amount: u64) -> Result<()> {
        let attempted = self
            .stats
            .fuel
            .checked_add(amount)
            .ok_or_else(|| self.error(FontErrorCode::NumericOverflow, None))?;
        if attempted > self.limits.max_fuel {
            return Err(self.resource(
                FontLimitKind::Fuel,
                self.limits.max_fuel,
                self.stats.fuel,
                amount,
            ));
        }
        self.stats.fuel = attempted;
        if attempted >= self.next_cancellation_fuel {
            self.check_cancelled()?;
            self.next_cancellation_fuel =
                attempted.saturating_add(self.limits.cancellation_check_interval_fuel);
        }
        Ok(())
    }

    fn check_cancelled(&self) -> Result<()> {
        if self.cancellation.is_cancelled() {
            Err(self.error(FontErrorCode::Cancelled, None))
        } else {
            Ok(())
        }
    }

    fn invalid_cff(&self) -> Stop {
        self.error(FontErrorCode::InvalidCff, None)
    }

    fn invalid_charstring(&self, glyph_id: u16) -> Stop {
        self.error(FontErrorCode::InvalidCharString, Some(glyph_id))
    }

    fn numeric(&self, glyph_id: u16) -> Stop {
        self.error(FontErrorCode::NumericOverflow, Some(glyph_id))
    }

    fn allocation_error(&self) -> Stop {
        self.resource(
            FontLimitKind::Allocation,
            self.limits.max_retained_bytes,
            0,
            1,
        )
    }

    fn error(&self, code: FontErrorCode, glyph_id: Option<u16>) -> Stop {
        Stop::Error(FontError::for_code(code, glyph_id))
    }

    fn resource(&self, kind: FontLimitKind, limit: u64, consumed: u64, attempted: u64) -> Stop {
        Stop::Error(FontError::resource(FontLimit::new(
            kind, limit, consumed, attempted,
        )))
    }

    fn resource_for_glyph(
        &self,
        kind: FontLimitKind,
        limit: u64,
        consumed: u64,
        attempted: u64,
        glyph_id: u16,
    ) -> Stop {
        Stop::Error(FontError::resource_for_glyph(
            FontLimit::new(kind, limit, consumed, attempted),
            glyph_id,
        ))
    }
}

fn one_offset(operands: &[i64]) -> Option<usize> {
    let value = *operands.first()?;
    if operands.len() != 1 {
        return None;
    }
    fixed_offset(value)
}

fn fixed_offset(value: i64) -> Option<usize> {
    if value < 0 || value % FIXED_ONE != 0 {
        return None;
    }
    usize::try_from(value / FIXED_ONE).ok()
}

fn one_fixed(operands: &[i64]) -> Option<i64> {
    (operands.len() == 1).then_some(operands[0])
}

fn push_operand(stack: &mut Vec<i64>, value: i64) -> std::result::Result<(), ()> {
    if stack.len() >= TYPE2_STACK_LIMIT {
        return Err(());
    }
    stack.push(value);
    Ok(())
}

fn decode_compact_integer(byte: u8, data: &[u8], position: &mut usize) -> Option<i64> {
    match byte {
        32..=246 => Some(i64::from(byte) - 139),
        247..=250 => {
            let extra = i64::from(*data.get(*position)?);
            *position += 1;
            Some((i64::from(byte) - 247) * 256 + extra + 108)
        }
        251..=254 => {
            let extra = i64::from(*data.get(*position)?);
            *position += 1;
            Some(-((i64::from(byte) - 251) * 256) - extra - 108)
        }
        _ => None,
    }
}

fn subroutine_index(operand: i64, count: usize) -> Option<usize> {
    if operand % FIXED_ONE != 0 {
        return None;
    }
    let bias = if count < 1_240 {
        107_i64
    } else if count < 33_900 {
        1_131
    } else {
        32_768
    };
    usize::try_from(operand / FIXED_ONE + bias).ok()
}

fn fixed_to_u16(value: i64) -> Option<u16> {
    if value < 0 {
        return None;
    }
    let rounded = value.checked_add(FIXED_ONE / 2)? / FIXED_ONE;
    u16::try_from(rounded).ok()
}

fn checked_sum(values: &[i64]) -> Option<i64> {
    values
        .iter()
        .try_fold(0_i64, |total, value| total.checked_add(*value))
}

fn fixed_point(x: i64, y: i64) -> Option<FontPoint> {
    Some(FontPoint::new(
        FontCoordinate::from_half_units(fixed_to_half_units(x)?),
        FontCoordinate::from_half_units(fixed_to_half_units(y)?),
    ))
}

fn fixed_to_half_units(value: i64) -> Option<i32> {
    let rounded = if value >= 0 {
        value.checked_add(FIXED_ONE / 4)? / (FIXED_ONE / 2)
    } else {
        value.checked_sub(FIXED_ONE / 4)? / (FIXED_ONE / 2)
    };
    i32::try_from(rounded).ok()
}

fn outline_bounds(segments: &[OutlineSegment]) -> Option<Option<FontBounds>> {
    let mut bounds: Option<(i32, i32, i32, i32)> = None;
    let mut include = |point: FontPoint| {
        let x = point.x().half_units();
        let y = point.y().half_units();
        bounds = Some(match bounds {
            Some((x_min, y_min, x_max, y_max)) => {
                (x_min.min(x), y_min.min(y), x_max.max(x), y_max.max(y))
            }
            None => (x, y, x, y),
        });
    };
    for segment in segments {
        match *segment {
            OutlineSegment::MoveTo(point) | OutlineSegment::LineTo(point) => include(point),
            OutlineSegment::QuadTo { control, end } => {
                include(control);
                include(end);
            }
            OutlineSegment::CubicTo {
                control_1,
                control_2,
                end,
            } => {
                include(control_1);
                include(control_2);
                include(end);
            }
            OutlineSegment::CloseContour => {}
        }
    }
    let Some((x_min, y_min, x_max, y_max)) = bounds else {
        return Some(None);
    };
    let x_min = i16::try_from(x_min.div_euclid(2)).ok()?;
    let y_min = i16::try_from(y_min.div_euclid(2)).ok()?;
    let x_max = i16::try_from(-(-i64::from(x_max)).div_euclid(2)).ok()?;
    let y_max = i16::try_from(-(-i64::from(y_max)).div_euclid(2)).ok()?;
    Some(Some(FontBounds::new(x_min, y_min, x_max, y_max)))
}

fn retained_bytes(
    names: &[Option<Box<str>>],
    glyphs: &[GlyphRecord],
    segments: &[OutlineSegment],
) -> Option<u64> {
    let names_vector = names.len().checked_mul(size_of::<Option<Box<str>>>())?;
    let name_bytes = names.iter().try_fold(0_usize, |total, name| {
        total.checked_add(name.as_deref().map_or(0, str::len))
    })?;
    let glyph_bytes = glyphs.len().checked_mul(size_of::<GlyphRecord>())?;
    let segment_bytes = segments.len().checked_mul(size_of::<OutlineSegment>())?;
    u64::try_from(
        names_vector
            .checked_add(name_bytes)?
            .checked_add(glyph_bytes)?
            .checked_add(segment_bytes)?,
    )
    .ok()
}

fn read_u16(bytes: &[u8], offset: usize) -> Option<u16> {
    let end = offset.checked_add(2)?;
    Some(u16::from_be_bytes(bytes.get(offset..end)?.try_into().ok()?))
}

fn read_i16(bytes: &[u8], offset: usize) -> Option<i16> {
    let end = offset.checked_add(2)?;
    Some(i16::from_be_bytes(bytes.get(offset..end)?.try_into().ok()?))
}

fn read_i32(bytes: &[u8], offset: usize) -> Option<i32> {
    let end = offset.checked_add(4)?;
    Some(i32::from_be_bytes(bytes.get(offset..end)?.try_into().ok()?))
}

fn read_offset(bytes: &[u8], offset: usize, size: usize) -> Option<usize> {
    let mut value = 0_usize;
    let end = offset.checked_add(size)?;
    for byte in bytes.get(offset..end)? {
        value = value.checked_mul(256)?.checked_add(usize::from(*byte))?;
    }
    Some(value)
}
