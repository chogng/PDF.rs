use std::mem::size_of;
use std::sync::Arc;

use crate::limits::HARD_MAX_COMPONENT_DEPTH;
use crate::model::GlyphRecord;
use crate::{
    FontBounds, FontCancellation, FontCoordinate, FontError, FontErrorCode, FontLimit,
    FontLimitKind, FontLimits, FontParseOutcome, FontParseReport, FontPoint, FontProfile,
    FontStats, FontUnsupported, FontUnsupportedKind, GlyphId, OutlineSegment, TrueTypeFont,
};

const REQUIRED_TAGS: [[u8; 4]; 7] = [
    *b"head", *b"hhea", *b"maxp", *b"loca", *b"glyf", *b"hmtx", *b"cmap",
];
const SFNT_TRUE_TYPE: u32 = 0x0001_0000;
const HEAD_MAGIC: u32 = 0x5f0f_3cf5;
const COMPOUND_ARG_WORDS: u16 = 0x0001;
const COMPOUND_ARGS_XY: u16 = 0x0002;
const COMPOUND_ROUND_XY: u16 = 0x0004;
const COMPOUND_HAVE_SCALE: u16 = 0x0008;
const COMPOUND_MORE: u16 = 0x0020;
const COMPOUND_HAVE_XY_SCALE: u16 = 0x0040;
const COMPOUND_HAVE_TWO_BY_TWO: u16 = 0x0080;
const COMPOUND_HAVE_INSTRUCTIONS: u16 = 0x0100;
const COMPOUND_USE_MY_METRICS: u16 = 0x0200;
const COMPOUND_OVERLAP: u16 = 0x0400;
const COMPOUND_SCALED_OFFSET: u16 = 0x0800;
const COMPOUND_UNSCALED_OFFSET: u16 = 0x1000;
const COMPOUND_KNOWN: u16 = COMPOUND_ARG_WORDS
    | COMPOUND_ARGS_XY
    | COMPOUND_ROUND_XY
    | COMPOUND_HAVE_SCALE
    | COMPOUND_MORE
    | COMPOUND_HAVE_XY_SCALE
    | COMPOUND_HAVE_TWO_BY_TWO
    | COMPOUND_HAVE_INSTRUCTIONS
    | COMPOUND_USE_MY_METRICS
    | COMPOUND_OVERLAP
    | COMPOUND_SCALED_OFFSET
    | COMPOUND_UNSCALED_OFFSET;
const GLYPH_FLAG_ON_CURVE: u8 = 0x01;
const GLYPH_FLAG_X_SHORT: u8 = 0x02;
const GLYPH_FLAG_Y_SHORT: u8 = 0x04;
const GLYPH_FLAG_REPEAT: u8 = 0x08;
const GLYPH_FLAG_X_SAME_OR_POSITIVE: u8 = 0x10;
const GLYPH_FLAG_Y_SAME_OR_POSITIVE: u8 = 0x20;
const GLYPH_FLAG_RESERVED: u8 = 0x80;

/// Parses and atomically publishes one foundational TrueType font.
///
/// The complete font is measured and all configured budgets are preflighted before the output
/// glyph and segment vectors are allocated. Every terminal outcome carries deterministic work
/// statistics; cancellation and unsupported capability are never collapsed into malformed input.
pub fn parse_truetype<C: FontCancellation + ?Sized>(
    bytes: &[u8],
    profile: FontProfile,
    limits: FontLimits,
    cancellation: &C,
) -> FontParseReport {
    let mut parser = Parser::new(bytes, profile, limits, cancellation);
    let outcome = match parser.run() {
        Ok(font) => FontParseOutcome::Ready(font),
        Err(ParseStop::Unsupported(unsupported)) => FontParseOutcome::Unsupported(unsupported),
        Err(ParseStop::Error(error)) if error.code() == FontErrorCode::Cancelled => {
            FontParseOutcome::Cancelled(error)
        }
        Err(ParseStop::Error(error)) => FontParseOutcome::Failed(error),
    };
    FontParseReport {
        outcome,
        stats: parser.stats,
    }
}

#[derive(Clone, Copy, Debug)]
struct Table {
    offset: usize,
    length: usize,
}

#[derive(Clone, Copy, Debug)]
struct Tables {
    head: Table,
    hhea: Table,
    maxp: Table,
    loca: Table,
    glyf: Table,
    hmtx: Table,
    cmap: Table,
}

#[derive(Clone, Copy, Debug)]
struct CmapFormat4 {
    table: Table,
    subtable_offset: usize,
    subtable_length: usize,
    segment_count: usize,
    end_codes_offset: usize,
    start_codes_offset: usize,
    deltas_offset: usize,
    range_offsets_offset: usize,
}

#[derive(Clone, Copy, Debug)]
struct Metadata {
    tables: Tables,
    units_per_em: u16,
    glyph_count: u16,
    long_loca: bool,
    number_of_h_metrics: u16,
    winansi_glyphs: [GlyphId; 224],
}

#[derive(Clone, Copy, Debug)]
struct GlyphHeader {
    contours: i16,
    bounds: FontBounds,
}

#[derive(Clone, Copy, Debug)]
struct SimpleLayout {
    contours: usize,
    point_count: usize,
    endpoints_offset: usize,
    flags_offset: usize,
    x_offset: usize,
    y_offset: usize,
    end_offset: usize,
}

#[derive(Clone, Copy, Debug)]
struct CompoundRecord {
    flags: u16,
    component: u16,
    x: i32,
    y: i32,
    end: usize,
}

#[derive(Clone, Copy, Debug, Default)]
struct RawPoint {
    x: i32,
    y: i32,
    on_curve: bool,
}

#[derive(Debug, Default)]
struct Scratch {
    flags: Vec<u8>,
    points: Vec<RawPoint>,
}

enum ParseStop {
    Error(FontError),
    Unsupported(FontUnsupported),
}

type ParseResult<T> = Result<T, ParseStop>;

struct Parser<'a, C: FontCancellation + ?Sized> {
    bytes: &'a [u8],
    profile: FontProfile,
    limits: FontLimits,
    cancellation: &'a C,
    stats: FontStats,
    next_cancellation_fuel: u64,
    max_scratch_points: u64,
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
            max_scratch_points: 0,
        }
    }

    fn run(&mut self) -> ParseResult<TrueTypeFont> {
        self.check_cancelled()?;
        let input_bytes = u64::try_from(self.bytes.len()).map_err(|_| {
            ParseStop::Error(FontError::for_code(FontErrorCode::NumericOverflow, None))
        })?;
        if input_bytes > self.limits.max_input_bytes {
            return Err(self.resource(
                FontLimitKind::InputBytes,
                self.limits.max_input_bytes,
                0,
                input_bytes,
            ));
        }
        self.stats.input_bytes = input_bytes;
        if self.bytes.len() < 12 {
            return Err(self.error(FontErrorCode::InvalidRequest, None));
        }

        let tables = self.parse_table_directory()?;
        let metadata = self.parse_metadata(tables)?;
        self.validate_loca(&metadata)?;

        let mut total_segments = 0_u64;
        let mut stack = [0_u16; HARD_MAX_COMPONENT_DEPTH as usize + 1];
        for glyph_id in 0..metadata.glyph_count {
            self.stats.glyphs = self.stats.glyphs.checked_add(1).ok_or_else(|| {
                ParseStop::Error(FontError::for_code(FontErrorCode::NumericOverflow, None))
            })?;
            let measured = self.measure_glyph(&metadata, glyph_id, 0, &mut stack, true)?;
            total_segments = self.checked_limit_add(
                FontLimitKind::PathSegments,
                total_segments,
                measured,
                self.limits.max_path_segments,
            )?;
        }
        self.stats.path_segments = total_segments;

        let glyph_capacity = usize::from(metadata.glyph_count);
        let segment_capacity = usize::try_from(total_segments).map_err(|_| {
            ParseStop::Error(FontError::for_code(FontErrorCode::NumericOverflow, None))
        })?;
        let scratch_capacity = usize::try_from(self.max_scratch_points).map_err(|_| {
            ParseStop::Error(FontError::for_code(FontErrorCode::NumericOverflow, None))
        })?;
        let nominal_final = retained_font_bytes(glyph_capacity, segment_capacity)?;
        let nominal_scratch = retained_scratch_bytes(scratch_capacity)?;
        let nominal_peak = nominal_final.checked_add(nominal_scratch).ok_or_else(|| {
            ParseStop::Error(FontError::for_code(FontErrorCode::NumericOverflow, None))
        })?;
        if nominal_peak > self.limits.max_retained_bytes {
            return Err(self.resource(
                FontLimitKind::RetainedBytes,
                self.limits.max_retained_bytes,
                0,
                nominal_peak,
            ));
        }

        let mut glyphs = Vec::new();
        try_reserve_exact(
            &mut glyphs,
            glyph_capacity,
            self.limits.max_retained_bytes,
            nominal_peak,
        )?;
        let mut segments = Vec::new();
        try_reserve_exact(
            &mut segments,
            segment_capacity,
            self.limits.max_retained_bytes,
            nominal_peak,
        )?;
        let mut scratch = Scratch::default();
        try_reserve_exact(
            &mut scratch.flags,
            scratch_capacity,
            self.limits.max_retained_bytes,
            nominal_peak,
        )?;
        try_reserve_exact(
            &mut scratch.points,
            scratch_capacity,
            self.limits.max_retained_bytes,
            nominal_peak,
        )?;

        let actual_final = retained_font_bytes(glyphs.capacity(), segments.capacity())?;
        let actual_scratch = retained_scratch_bytes_with_capacities(
            scratch.flags.capacity(),
            scratch.points.capacity(),
        )?;
        let actual_peak = actual_final.checked_add(actual_scratch).ok_or_else(|| {
            ParseStop::Error(FontError::for_code(FontErrorCode::NumericOverflow, None))
        })?;
        if actual_peak > self.limits.max_retained_bytes {
            return Err(self.resource(
                FontLimitKind::RetainedBytes,
                self.limits.max_retained_bytes,
                0,
                actual_peak,
            ));
        }

        for glyph_id in 0..metadata.glyph_count {
            let start = segments.len();
            let advance_width = self.emit_glyph(
                &metadata,
                glyph_id,
                0,
                0,
                0,
                &mut stack,
                &mut segments,
                &mut scratch,
            )?;
            let len = segments.len().checked_sub(start).ok_or_else(|| {
                ParseStop::Error(FontError::for_code(
                    FontErrorCode::InternalState,
                    Some(glyph_id),
                ))
            })?;
            let segment_start = u32::try_from(start).map_err(|_| {
                ParseStop::Error(FontError::for_code(
                    FontErrorCode::NumericOverflow,
                    Some(glyph_id),
                ))
            })?;
            let segment_len = u32::try_from(len).map_err(|_| {
                ParseStop::Error(FontError::for_code(
                    FontErrorCode::NumericOverflow,
                    Some(glyph_id),
                ))
            })?;
            let range = self.glyph_range(&metadata, glyph_id)?;
            let bounds = if range.is_empty() {
                None
            } else {
                Some(self.glyph_header(range, glyph_id)?.bounds)
            };
            glyphs.push(GlyphRecord {
                advance_width,
                bounds,
                segment_start,
                segment_len,
            });
        }
        if glyphs.len() != glyph_capacity || segments.len() != segment_capacity {
            return Err(self.error(FontErrorCode::InternalState, None));
        }

        self.stats.retained_bytes = actual_final;
        self.stats.peak_retained_bytes = actual_peak;
        let stats = self.stats;
        Ok(TrueTypeFont {
            profile: self.profile,
            limits: self.limits,
            stats,
            units_per_em: metadata.units_per_em,
            winansi_glyphs: metadata.winansi_glyphs,
            glyphs: Arc::new(glyphs),
            segments: Arc::new(segments),
        })
    }

    fn parse_table_directory(&mut self) -> ParseResult<Tables> {
        let flavor = read_u32(self.bytes, 0).ok_or_else(|| {
            ParseStop::Error(FontError::for_code(FontErrorCode::InvalidRequest, None))
        })?;
        if flavor != SFNT_TRUE_TYPE {
            return Err(self.unsupported(FontUnsupportedKind::SfntFlavor, None));
        }
        let table_count = read_u16(self.bytes, 4).ok_or_else(|| {
            ParseStop::Error(FontError::for_code(FontErrorCode::InvalidRequest, None))
        })?;
        if table_count == 0 {
            return Err(self.error(FontErrorCode::MalformedSfnt, None));
        }
        if table_count > self.limits.max_tables {
            return Err(self.resource(
                FontLimitKind::Tables,
                u64::from(self.limits.max_tables),
                0,
                u64::from(table_count),
            ));
        }
        let directory_bytes = usize::from(table_count)
            .checked_mul(16)
            .and_then(|value| value.checked_add(12))
            .ok_or_else(|| {
                ParseStop::Error(FontError::for_code(FontErrorCode::NumericOverflow, None))
            })?;
        if directory_bytes > self.bytes.len() {
            return Err(self.error(FontErrorCode::InvalidRequest, None));
        }

        let mut required: [Option<Table>; 7] = [None; 7];
        for index in 0..usize::from(table_count) {
            self.charge(4)?;
            let record = 12 + index * 16;
            let tag: [u8; 4] = self.bytes[record..record + 4]
                .try_into()
                .map_err(|_| self.error(FontErrorCode::InternalState, None))?;
            let offset_u32 = read_u32(self.bytes, record + 8)
                .ok_or_else(|| self.error(FontErrorCode::MalformedSfnt, None))?;
            let length_u32 = read_u32(self.bytes, record + 12)
                .ok_or_else(|| self.error(FontErrorCode::MalformedSfnt, None))?;
            let offset = usize::try_from(offset_u32)
                .map_err(|_| self.error(FontErrorCode::NumericOverflow, None))?;
            let length = usize::try_from(length_u32)
                .map_err(|_| self.error(FontErrorCode::NumericOverflow, None))?;
            let end = offset
                .checked_add(length)
                .ok_or_else(|| self.error(FontErrorCode::NumericOverflow, None))?;
            if end > self.bytes.len()
                || (length != 0 && (offset % 4 != 0 || offset < directory_bytes))
            {
                return Err(self.error(FontErrorCode::InvalidTableGeometry, None));
            }
            if let Some(required_index) = REQUIRED_TAGS.iter().position(|entry| *entry == tag) {
                if required[required_index].is_some() {
                    return Err(self.error(FontErrorCode::DuplicateRequiredTable, None));
                }
                required[required_index] = Some(Table { offset, length });
            }
            self.stats.tables_visited += 1;
        }

        for left in 0..usize::from(table_count) {
            let left_record = 12 + left * 16;
            let left_table = directory_record(self.bytes, left_record)?;
            if left_table.length == 0 {
                continue;
            }
            for right in left + 1..usize::from(table_count) {
                self.charge(1)?;
                let right_record = 12 + right * 16;
                let right_table = directory_record(self.bytes, right_record)?;
                if right_table.length != 0 && ranges_overlap(left_table, right_table)? {
                    return Err(self.error(FontErrorCode::InvalidTableGeometry, None));
                }
            }
        }

        let [head, hhea, maxp, loca, glyf, hmtx, cmap] = required;
        Ok(Tables {
            head: head.ok_or_else(|| self.error(FontErrorCode::MissingRequiredTable, None))?,
            hhea: hhea.ok_or_else(|| self.error(FontErrorCode::MissingRequiredTable, None))?,
            maxp: maxp.ok_or_else(|| self.error(FontErrorCode::MissingRequiredTable, None))?,
            loca: loca.ok_or_else(|| self.error(FontErrorCode::MissingRequiredTable, None))?,
            glyf: glyf.ok_or_else(|| self.error(FontErrorCode::MissingRequiredTable, None))?,
            hmtx: hmtx.ok_or_else(|| self.error(FontErrorCode::MissingRequiredTable, None))?,
            cmap: cmap.ok_or_else(|| self.error(FontErrorCode::MissingRequiredTable, None))?,
        })
    }

    fn parse_metadata(&mut self, tables: Tables) -> ParseResult<Metadata> {
        let head = table_slice(self.bytes, tables.head, FontErrorCode::InvalidHead)?;
        if head.len() < 54
            || read_u32(head, 0) != Some(SFNT_TRUE_TYPE)
            || read_u32(head, 12) != Some(HEAD_MAGIC)
            || read_i16(head, 52) != Some(0)
        {
            return Err(self.error(FontErrorCode::InvalidHead, None));
        }
        let units_per_em =
            read_u16(head, 18).ok_or_else(|| self.error(FontErrorCode::InvalidHead, None))?;
        if !(16..=16_384).contains(&units_per_em) {
            return Err(self.error(FontErrorCode::InvalidHead, None));
        }
        let loca_format =
            read_i16(head, 50).ok_or_else(|| self.error(FontErrorCode::InvalidHead, None))?;
        let long_loca = match loca_format {
            0 => false,
            1 => true,
            _ => return Err(self.error(FontErrorCode::InvalidHead, None)),
        };

        let maxp = table_slice(self.bytes, tables.maxp, FontErrorCode::InvalidMaxp)?;
        if maxp.len() < 6 {
            return Err(self.error(FontErrorCode::InvalidMaxp, None));
        }
        let maxp_version =
            read_u32(maxp, 0).ok_or_else(|| self.error(FontErrorCode::InvalidMaxp, None))?;
        if maxp_version != SFNT_TRUE_TYPE {
            return Err(self.unsupported(FontUnsupportedKind::MaxpVersion, None));
        }
        if maxp.len() < 32 {
            return Err(self.error(FontErrorCode::InvalidMaxp, None));
        }
        let glyph_count =
            read_u16(maxp, 4).ok_or_else(|| self.error(FontErrorCode::InvalidMaxp, None))?;
        if glyph_count == 0 {
            return Err(self.error(FontErrorCode::InvalidMaxp, None));
        }
        if u32::from(glyph_count) > self.limits.max_glyphs {
            return Err(self.resource(
                FontLimitKind::Glyphs,
                u64::from(self.limits.max_glyphs),
                0,
                u64::from(glyph_count),
            ));
        }

        let hhea = table_slice(self.bytes, tables.hhea, FontErrorCode::InvalidHhea)?;
        if hhea.len() < 36
            || read_u32(hhea, 0) != Some(SFNT_TRUE_TYPE)
            || read_i16(hhea, 32) != Some(0)
        {
            return Err(self.error(FontErrorCode::InvalidHhea, None));
        }
        let number_of_h_metrics =
            read_u16(hhea, 34).ok_or_else(|| self.error(FontErrorCode::InvalidHhea, None))?;
        if number_of_h_metrics == 0 || number_of_h_metrics > glyph_count {
            return Err(self.error(FontErrorCode::InvalidHhea, None));
        }
        let long_metrics = usize::from(number_of_h_metrics)
            .checked_mul(4)
            .ok_or_else(|| self.error(FontErrorCode::NumericOverflow, None))?;
        let trailing_bearings = usize::from(glyph_count - number_of_h_metrics)
            .checked_mul(2)
            .ok_or_else(|| self.error(FontErrorCode::NumericOverflow, None))?;
        let required_hmtx = long_metrics
            .checked_add(trailing_bearings)
            .ok_or_else(|| self.error(FontErrorCode::NumericOverflow, None))?;
        if tables.hmtx.length < required_hmtx {
            return Err(self.error(FontErrorCode::InvalidHmtx, None));
        }

        let cmap = self.parse_cmap(tables.cmap, glyph_count)?;
        let winansi_glyphs = match self.profile {
            FontProfile::SimpleTrueTypeWinAnsiAsciiV1 => {
                self.map_winansi_ascii(&cmap, glyph_count)?
            }
            FontProfile::SimpleTrueTypeWinAnsiV1 => self.map_winansi(&cmap, glyph_count)?,
        };
        Ok(Metadata {
            tables,
            units_per_em,
            glyph_count,
            long_loca,
            number_of_h_metrics,
            winansi_glyphs,
        })
    }

    fn parse_cmap(&mut self, table: Table, glyph_count: u16) -> ParseResult<CmapFormat4> {
        let cmap = table_slice(self.bytes, table, FontErrorCode::InvalidCmap)?;
        if cmap.len() < 4 || read_u16(cmap, 0) != Some(0) {
            return Err(self.error(FontErrorCode::InvalidCmap, None));
        }
        let encoding_count =
            read_u16(cmap, 2).ok_or_else(|| self.error(FontErrorCode::InvalidCmap, None))?;
        let records_end = usize::from(encoding_count)
            .checked_mul(8)
            .and_then(|value| value.checked_add(4))
            .ok_or_else(|| self.error(FontErrorCode::NumericOverflow, None))?;
        if encoding_count == 0 || records_end > cmap.len() {
            return Err(self.error(FontErrorCode::InvalidCmap, None));
        }
        let mut selected = None;
        for index in 0..usize::from(encoding_count) {
            self.charge(2)?;
            let record = 4 + index * 8;
            let platform = read_u16(cmap, record)
                .ok_or_else(|| self.error(FontErrorCode::InvalidCmap, None))?;
            let encoding = read_u16(cmap, record + 2)
                .ok_or_else(|| self.error(FontErrorCode::InvalidCmap, None))?;
            let offset = usize::try_from(
                read_u32(cmap, record + 4)
                    .ok_or_else(|| self.error(FontErrorCode::InvalidCmap, None))?,
            )
            .map_err(|_| self.error(FontErrorCode::NumericOverflow, None))?;
            if offset >= cmap.len() {
                return Err(self.error(FontErrorCode::InvalidCmap, None));
            }
            if platform == 3 && encoding == 1 && selected.is_none() {
                selected = Some(offset);
            }
        }
        let subtable_offset =
            selected.ok_or_else(|| self.unsupported(FontUnsupportedKind::CmapPlatform, None))?;
        let format = read_u16(cmap, subtable_offset)
            .ok_or_else(|| self.error(FontErrorCode::InvalidCmap, None))?;
        if format != 4 {
            return Err(self.unsupported(FontUnsupportedKind::CmapFormat, None));
        }
        let subtable_length = usize::from(
            read_u16(cmap, subtable_offset + 2)
                .ok_or_else(|| self.error(FontErrorCode::InvalidCmap, None))?,
        );
        let subtable_end = subtable_offset
            .checked_add(subtable_length)
            .ok_or_else(|| self.error(FontErrorCode::NumericOverflow, None))?;
        if subtable_length < 24 || subtable_end > cmap.len() {
            return Err(self.error(FontErrorCode::InvalidCmap, None));
        }
        let subtable = &cmap[subtable_offset..subtable_end];
        let segment_count_x2 =
            read_u16(subtable, 6).ok_or_else(|| self.error(FontErrorCode::InvalidCmap, None))?;
        if segment_count_x2 == 0 || segment_count_x2 % 2 != 0 {
            return Err(self.error(FontErrorCode::InvalidCmap, None));
        }
        let segment_count = usize::from(segment_count_x2 / 2);
        if u32::try_from(segment_count).map_or(true, |count| count > self.limits.max_cmap_segments)
        {
            return Err(self.resource(
                FontLimitKind::CmapSegments,
                u64::from(self.limits.max_cmap_segments),
                0,
                u64::try_from(segment_count).unwrap_or(u64::MAX),
            ));
        }
        let required = segment_count
            .checked_mul(8)
            .and_then(|value| value.checked_add(16))
            .ok_or_else(|| self.error(FontErrorCode::NumericOverflow, None))?;
        if required > subtable.len() {
            return Err(self.error(FontErrorCode::InvalidCmap, None));
        }
        let end_codes_offset = 14;
        let reserved_pad = end_codes_offset + segment_count * 2;
        if read_u16(subtable, reserved_pad) != Some(0) {
            return Err(self.error(FontErrorCode::InvalidCmap, None));
        }
        let start_codes_offset = reserved_pad + 2;
        let deltas_offset = start_codes_offset + segment_count * 2;
        let range_offsets_offset = deltas_offset + segment_count * 2;
        let glyph_id_array_offset = range_offsets_offset
            .checked_add(segment_count * 2)
            .ok_or_else(|| self.error(FontErrorCode::NumericOverflow, None))?;
        let mut previous_end = None;
        for index in 0..segment_count {
            self.charge(2)?;
            let end = read_u16(subtable, end_codes_offset + index * 2)
                .ok_or_else(|| self.error(FontErrorCode::InvalidCmap, None))?;
            let start = read_u16(subtable, start_codes_offset + index * 2)
                .ok_or_else(|| self.error(FontErrorCode::InvalidCmap, None))?;
            if start > end || previous_end.is_some_and(|prior| start <= prior) {
                return Err(self.error(FontErrorCode::InvalidCmap, None));
            }
            let range_position = range_offsets_offset + index * 2;
            let range_offset = usize::from(
                read_u16(subtable, range_position)
                    .ok_or_else(|| self.error(FontErrorCode::InvalidCmap, None))?,
            );
            if range_offset % 2 != 0 {
                return Err(self.error(FontErrorCode::InvalidCmap, None));
            }
            if range_offset != 0 {
                let first_glyph = range_position
                    .checked_add(range_offset)
                    .ok_or_else(|| self.error(FontErrorCode::NumericOverflow, None))?;
                if first_glyph < glyph_id_array_offset {
                    return Err(self.error(FontErrorCode::InvalidCmap, None));
                }
                let glyph_bytes = usize::from(end - start)
                    .checked_add(1)
                    .and_then(|value| value.checked_mul(2))
                    .ok_or_else(|| self.error(FontErrorCode::NumericOverflow, None))?;
                let glyphs_end = first_glyph
                    .checked_add(glyph_bytes)
                    .ok_or_else(|| self.error(FontErrorCode::NumericOverflow, None))?;
                if glyphs_end > subtable.len() {
                    return Err(self.error(FontErrorCode::InvalidCmap, None));
                }
            }
            previous_end = Some(end);
        }
        if previous_end != Some(u16::MAX) {
            return Err(self.error(FontErrorCode::InvalidCmap, None));
        }
        let sentinel = segment_count - 1;
        if read_u16(subtable, start_codes_offset + sentinel * 2) != Some(u16::MAX)
            || read_i16(subtable, deltas_offset + sentinel * 2) != Some(1)
            || read_u16(subtable, range_offsets_offset + sentinel * 2) != Some(0)
        {
            return Err(self.error(FontErrorCode::InvalidCmap, None));
        }
        self.stats.cmap_segments = u64::try_from(segment_count)
            .map_err(|_| self.error(FontErrorCode::NumericOverflow, None))?;
        let parsed = CmapFormat4 {
            table,
            subtable_offset,
            subtable_length,
            segment_count,
            end_codes_offset,
            start_codes_offset,
            deltas_offset,
            range_offsets_offset,
        };
        for code in 0x20_u16..=0x7e {
            let glyph = self.cmap_glyph(&parsed, code)?;
            if glyph >= glyph_count {
                return Err(self.error(FontErrorCode::InvalidCmap, None));
            }
        }
        Ok(parsed)
    }

    fn map_winansi_ascii(
        &mut self,
        cmap: &CmapFormat4,
        glyph_count: u16,
    ) -> ParseResult<[GlyphId; 224]> {
        let mut mapped = [GlyphId::new(0); 224];
        for (index, code) in (0x20_u16..=0x7e).enumerate() {
            let glyph = self.cmap_glyph(cmap, code)?;
            if glyph >= glyph_count {
                return Err(self.error(FontErrorCode::InvalidCmap, None));
            }
            mapped[index] = GlyphId::new(glyph);
        }
        Ok(mapped)
    }

    fn map_winansi(&mut self, cmap: &CmapFormat4, glyph_count: u16) -> ParseResult<[GlyphId; 224]> {
        let mut mapped = [GlyphId::new(0); 224];
        for (index, byte) in (0x20_u8..=u8::MAX).enumerate() {
            let unicode = winansi_unicode(byte)
                .ok_or_else(|| self.error(FontErrorCode::InternalState, None))?;
            let glyph = self.cmap_glyph(cmap, unicode)?;
            if glyph >= glyph_count {
                return Err(self.error(FontErrorCode::InvalidCmap, None));
            }
            mapped[index] = GlyphId::new(glyph);
        }
        Ok(mapped)
    }

    fn cmap_glyph(&mut self, cmap: &CmapFormat4, code: u16) -> ParseResult<u16> {
        let table = table_slice(self.bytes, cmap.table, FontErrorCode::InvalidCmap)?;
        let subtable_end = cmap
            .subtable_offset
            .checked_add(cmap.subtable_length)
            .ok_or_else(|| self.error(FontErrorCode::NumericOverflow, None))?;
        let subtable = table
            .get(cmap.subtable_offset..subtable_end)
            .ok_or_else(|| self.error(FontErrorCode::InvalidCmap, None))?;
        for index in 0..cmap.segment_count {
            self.charge(1)?;
            let end = read_u16(subtable, cmap.end_codes_offset + index * 2)
                .ok_or_else(|| self.error(FontErrorCode::InvalidCmap, None))?;
            if code > end {
                continue;
            }
            let start = read_u16(subtable, cmap.start_codes_offset + index * 2)
                .ok_or_else(|| self.error(FontErrorCode::InvalidCmap, None))?;
            if code < start {
                return Ok(0);
            }
            let delta = read_i16(subtable, cmap.deltas_offset + index * 2)
                .ok_or_else(|| self.error(FontErrorCode::InvalidCmap, None))?;
            let range_offset_position = cmap.range_offsets_offset + index * 2;
            let range_offset = usize::from(
                read_u16(subtable, range_offset_position)
                    .ok_or_else(|| self.error(FontErrorCode::InvalidCmap, None))?,
            );
            if range_offset == 0 {
                return Ok(code.wrapping_add_signed(delta));
            }
            let code_delta = usize::from(code - start)
                .checked_mul(2)
                .ok_or_else(|| self.error(FontErrorCode::NumericOverflow, None))?;
            let glyph_offset = range_offset_position
                .checked_add(range_offset)
                .and_then(|value| value.checked_add(code_delta))
                .ok_or_else(|| self.error(FontErrorCode::NumericOverflow, None))?;
            let mut glyph = read_u16(subtable, glyph_offset)
                .ok_or_else(|| self.error(FontErrorCode::InvalidCmap, None))?;
            if glyph != 0 {
                glyph = glyph.wrapping_add_signed(delta);
            }
            return Ok(glyph);
        }
        Err(self.error(FontErrorCode::InvalidCmap, None))
    }

    fn validate_loca(&mut self, metadata: &Metadata) -> ParseResult<()> {
        let entry_size = if metadata.long_loca { 4 } else { 2 };
        let required = usize::from(metadata.glyph_count)
            .checked_add(1)
            .and_then(|count| count.checked_mul(entry_size))
            .ok_or_else(|| self.error(FontErrorCode::NumericOverflow, None))?;
        if metadata.tables.loca.length < required {
            return Err(self.error(FontErrorCode::InvalidLoca, None));
        }
        let mut prior = 0_usize;
        for index in 0..=metadata.glyph_count {
            self.charge(1)?;
            let offset = self.loca_offset(metadata, index)?;
            if (index == 0 && offset != 0) || offset < prior || offset > metadata.tables.glyf.length
            {
                return Err(self.error(FontErrorCode::InvalidLoca, None));
            }
            if index != 0 {
                let glyph_bytes = offset - prior;
                if u64::try_from(glyph_bytes)
                    .map_or(true, |value| value > self.limits.max_glyph_bytes)
                {
                    return Err(self.resource(
                        FontLimitKind::GlyphBytes,
                        self.limits.max_glyph_bytes,
                        0,
                        u64::try_from(glyph_bytes).unwrap_or(u64::MAX),
                    ));
                }
            }
            prior = offset;
        }
        let addressed =
            u64::try_from(prior).map_err(|_| self.error(FontErrorCode::NumericOverflow, None))?;
        if addressed > self.limits.max_glyph_data_bytes {
            return Err(self.resource(
                FontLimitKind::GlyphDataBytes,
                self.limits.max_glyph_data_bytes,
                0,
                addressed,
            ));
        }
        self.stats.glyph_data_bytes = addressed;
        Ok(())
    }

    fn measure_glyph(
        &mut self,
        metadata: &Metadata,
        glyph_id: u16,
        depth: u16,
        stack: &mut [u16; HARD_MAX_COMPONENT_DEPTH as usize + 1],
        account_source: bool,
    ) -> ParseResult<u64> {
        self.enter_glyph(glyph_id, depth, stack)?;
        let glyph = self.glyph_range(metadata, glyph_id)?;
        if glyph.is_empty() {
            return Ok(0);
        }
        let header = self.glyph_header(glyph, glyph_id)?;
        match header.contours {
            0..=i16::MAX => {
                let layout = self.simple_layout(glyph, header.contours as usize, glyph_id)?;
                if account_source {
                    self.account_simple_source(&layout)?;
                }
                self.simple_segment_count(glyph, &layout, glyph_id)
            }
            _ => self.measure_compound(metadata, glyph, glyph_id, depth, stack, account_source),
        }
    }

    fn account_simple_source(&mut self, layout: &SimpleLayout) -> ParseResult<()> {
        let contours = u64::try_from(layout.contours)
            .map_err(|_| self.error(FontErrorCode::NumericOverflow, None))?;
        if contours > u64::from(self.limits.max_glyph_contours) {
            return Err(self.resource(
                FontLimitKind::GlyphContours,
                u64::from(self.limits.max_glyph_contours),
                0,
                contours,
            ));
        }
        self.stats.source_contours = self.checked_limit_add(
            FontLimitKind::TotalContours,
            self.stats.source_contours,
            contours,
            self.limits.max_total_contours,
        )?;
        let points = u64::try_from(layout.point_count)
            .map_err(|_| self.error(FontErrorCode::NumericOverflow, None))?;
        if points > u64::from(self.limits.max_glyph_points) {
            return Err(self.resource(
                FontLimitKind::GlyphPoints,
                u64::from(self.limits.max_glyph_points),
                0,
                points,
            ));
        }
        self.stats.source_points = self.checked_limit_add(
            FontLimitKind::TotalPoints,
            self.stats.source_points,
            points,
            self.limits.max_total_points,
        )?;
        self.max_scratch_points = self.max_scratch_points.max(points);
        Ok(())
    }

    fn simple_layout(
        &mut self,
        glyph: &[u8],
        contours: usize,
        glyph_id: u16,
    ) -> ParseResult<SimpleLayout> {
        let endpoints_offset = 10_usize;
        if contours == 0 && glyph.len() == endpoints_offset {
            return Ok(SimpleLayout {
                contours: 0,
                point_count: 0,
                endpoints_offset,
                flags_offset: endpoints_offset,
                x_offset: endpoints_offset,
                y_offset: endpoints_offset,
                end_offset: endpoints_offset,
            });
        }
        let endpoints_end = endpoints_offset
            .checked_add(
                contours
                    .checked_mul(2)
                    .ok_or_else(|| self.error(FontErrorCode::NumericOverflow, Some(glyph_id)))?,
            )
            .ok_or_else(|| self.error(FontErrorCode::NumericOverflow, Some(glyph_id)))?;
        if endpoints_end
            .checked_add(2)
            .is_none_or(|end| end > glyph.len())
        {
            return Err(self.error(FontErrorCode::InvalidGlyph, Some(glyph_id)));
        }
        let mut prior = None;
        for contour in 0..contours {
            self.charge(1)?;
            let endpoint = read_u16(glyph, endpoints_offset + contour * 2)
                .ok_or_else(|| self.error(FontErrorCode::InvalidGlyph, Some(glyph_id)))?;
            if prior.is_some_and(|value| endpoint <= value) {
                return Err(self.error(FontErrorCode::InvalidGlyph, Some(glyph_id)));
            }
            prior = Some(endpoint);
        }
        let point_count = match prior {
            Some(endpoint) => usize::from(endpoint)
                .checked_add(1)
                .ok_or_else(|| self.error(FontErrorCode::NumericOverflow, Some(glyph_id)))?,
            None => 0,
        };
        if u64::try_from(point_count).map_or(true, |count| {
            count > u64::from(self.limits.max_glyph_points)
        }) {
            return Err(self.resource(
                FontLimitKind::GlyphPoints,
                u64::from(self.limits.max_glyph_points),
                0,
                u64::try_from(point_count).unwrap_or(u64::MAX),
            ));
        }
        let instruction_length = usize::from(
            read_u16(glyph, endpoints_end)
                .ok_or_else(|| self.error(FontErrorCode::InvalidGlyph, Some(glyph_id)))?,
        );
        let flags_offset = endpoints_end
            .checked_add(2)
            .and_then(|value| value.checked_add(instruction_length))
            .ok_or_else(|| self.error(FontErrorCode::NumericOverflow, Some(glyph_id)))?;
        if flags_offset > glyph.len() {
            return Err(self.error(FontErrorCode::InvalidGlyph, Some(glyph_id)));
        }
        self.charge(u64::try_from(instruction_length).unwrap_or(u64::MAX))?;
        let mut iterator = FlagIter::new(glyph, flags_offset, point_count, glyph_id);
        let mut x_bytes = 0_usize;
        let mut y_bytes = 0_usize;
        while let Some(flag) = iterator.next()? {
            self.charge(1)?;
            x_bytes = x_bytes
                .checked_add(coordinate_bytes(
                    flag,
                    GLYPH_FLAG_X_SHORT,
                    GLYPH_FLAG_X_SAME_OR_POSITIVE,
                ))
                .ok_or_else(|| self.error(FontErrorCode::NumericOverflow, Some(glyph_id)))?;
            y_bytes = y_bytes
                .checked_add(coordinate_bytes(
                    flag,
                    GLYPH_FLAG_Y_SHORT,
                    GLYPH_FLAG_Y_SAME_OR_POSITIVE,
                ))
                .ok_or_else(|| self.error(FontErrorCode::NumericOverflow, Some(glyph_id)))?;
        }
        let x_offset = iterator.position();
        let y_offset = x_offset
            .checked_add(x_bytes)
            .ok_or_else(|| self.error(FontErrorCode::NumericOverflow, Some(glyph_id)))?;
        let end_offset = y_offset
            .checked_add(y_bytes)
            .ok_or_else(|| self.error(FontErrorCode::NumericOverflow, Some(glyph_id)))?;
        if end_offset > glyph.len() {
            return Err(self.error(FontErrorCode::InvalidGlyph, Some(glyph_id)));
        }
        Ok(SimpleLayout {
            contours,
            point_count,
            endpoints_offset,
            flags_offset,
            x_offset,
            y_offset,
            end_offset,
        })
    }

    fn simple_segment_count(
        &mut self,
        glyph: &[u8],
        layout: &SimpleLayout,
        glyph_id: u16,
    ) -> ParseResult<u64> {
        if layout.contours == 0 {
            return Ok(0);
        }
        let mut iterator = FlagIter::new(glyph, layout.flags_offset, layout.point_count, glyph_id);
        let mut total = 0_u64;
        let mut contour = 0_usize;
        let mut first_on = None;
        let mut on_count = 0_u64;
        let mut off_pairs = 0_u64;
        let mut point_in_contour = 0_usize;
        let mut previous_on = None;
        for point_index in 0..layout.point_count {
            self.charge(1)?;
            let flag = iterator
                .next()?
                .ok_or_else(|| self.error(FontErrorCode::InvalidGlyph, Some(glyph_id)))?;
            let on = flag & GLYPH_FLAG_ON_CURVE != 0;
            if point_in_contour == 0 {
                first_on = Some(on);
            } else if previous_on == Some(false) && !on {
                off_pairs = off_pairs
                    .checked_add(1)
                    .ok_or_else(|| self.error(FontErrorCode::NumericOverflow, Some(glyph_id)))?;
            }
            if on {
                on_count = on_count
                    .checked_add(1)
                    .ok_or_else(|| self.error(FontErrorCode::NumericOverflow, Some(glyph_id)))?;
            }
            let penultimate_on = previous_on.unwrap_or(true);
            previous_on = Some(on);
            point_in_contour += 1;

            let endpoint = usize::from(
                read_u16(glyph, layout.endpoints_offset + contour * 2)
                    .ok_or_else(|| self.error(FontErrorCode::InvalidGlyph, Some(glyph_id)))?,
            );
            if point_index == endpoint {
                let contour_first_on = first_on
                    .ok_or_else(|| self.error(FontErrorCode::InternalState, Some(glyph_id)))?;
                let draws = if contour_first_on {
                    on_count
                        .checked_sub(1)
                        .and_then(|value| value.checked_add(off_pairs))
                        .and_then(|value| value.checked_add(u64::from(!on)))
                } else if on {
                    on_count
                        .checked_sub(1)
                        .and_then(|value| value.checked_add(off_pairs))
                        .and_then(|value| value.checked_add(u64::from(!penultimate_on)))
                } else {
                    on_count
                        .checked_add(off_pairs)
                        .and_then(|value| value.checked_add(1))
                }
                .ok_or_else(|| self.error(FontErrorCode::NumericOverflow, Some(glyph_id)))?;
                total = total
                    .checked_add(draws)
                    .and_then(|value| value.checked_add(2))
                    .ok_or_else(|| self.error(FontErrorCode::NumericOverflow, Some(glyph_id)))?;
                contour += 1;
                first_on = None;
                on_count = 0;
                off_pairs = 0;
                point_in_contour = 0;
                previous_on = None;
            }
        }
        if contour != layout.contours || iterator.next()?.is_some() {
            return Err(self.error(FontErrorCode::InvalidGlyph, Some(glyph_id)));
        }
        Ok(total)
    }

    fn measure_compound(
        &mut self,
        metadata: &Metadata,
        glyph: &[u8],
        glyph_id: u16,
        depth: u16,
        stack: &mut [u16; HARD_MAX_COMPONENT_DEPTH as usize + 1],
        account_source: bool,
    ) -> ParseResult<u64> {
        self.validate_compound_structure(metadata, glyph, glyph_id, account_source)?;
        let mut position = 10_usize;
        let mut total = 0_u64;
        loop {
            self.charge(3)?;
            let record = self.compound_record(metadata, glyph, position, glyph_id)?;
            position = record.end;
            let child = self.measure_glyph(metadata, record.component, depth + 1, stack, false)?;
            total = self.checked_limit_add(
                FontLimitKind::PathSegments,
                total,
                child,
                self.limits.max_path_segments,
            )?;
            if record.flags & COMPOUND_MORE == 0 {
                break;
            }
        }
        Ok(total)
    }

    #[allow(clippy::too_many_arguments)]
    fn emit_glyph(
        &mut self,
        metadata: &Metadata,
        glyph_id: u16,
        translate_x: i32,
        translate_y: i32,
        depth: u16,
        stack: &mut [u16; HARD_MAX_COMPONENT_DEPTH as usize + 1],
        output: &mut Vec<OutlineSegment>,
        scratch: &mut Scratch,
    ) -> ParseResult<u16> {
        self.enter_glyph(glyph_id, depth, stack)?;
        let own_advance = self.advance_width(metadata, glyph_id)?;
        let glyph = self.glyph_range(metadata, glyph_id)?;
        if glyph.is_empty() {
            return Ok(own_advance);
        }
        let header = self.glyph_header(glyph, glyph_id)?;
        let output_start = output.len();
        match header.contours {
            0..=i16::MAX => {
                let layout = self.simple_layout(glyph, header.contours as usize, glyph_id)?;
                self.decode_simple_points(glyph, &layout, header.bounds, glyph_id, scratch)?;
                self.emit_simple_contours(
                    glyph,
                    &layout,
                    glyph_id,
                    translate_x,
                    translate_y,
                    &scratch.points,
                    output,
                )?;
                Ok(own_advance)
            }
            _ => {
                let inherited_advance = self.emit_compound(
                    metadata,
                    glyph,
                    glyph_id,
                    translate_x,
                    translate_y,
                    depth,
                    stack,
                    output,
                    scratch,
                )?;
                self.validate_compound_bounds(
                    &output[output_start..],
                    header.bounds,
                    translate_x,
                    translate_y,
                    glyph_id,
                )?;
                Ok(inherited_advance.unwrap_or(own_advance))
            }
        }
    }

    fn decode_simple_points(
        &mut self,
        glyph: &[u8],
        layout: &SimpleLayout,
        bounds: FontBounds,
        glyph_id: u16,
        scratch: &mut Scratch,
    ) -> ParseResult<()> {
        scratch.flags.clear();
        scratch.points.clear();
        if scratch.flags.capacity() < layout.point_count
            || scratch.points.capacity() < layout.point_count
        {
            return Err(self.error(FontErrorCode::InternalState, Some(glyph_id)));
        }
        let mut iterator = FlagIter::new(glyph, layout.flags_offset, layout.point_count, glyph_id);
        while let Some(flag) = iterator.next()? {
            scratch.flags.push(flag);
        }
        if iterator.position() != layout.x_offset || scratch.flags.len() != layout.point_count {
            return Err(self.error(FontErrorCode::InternalState, Some(glyph_id)));
        }
        let mut x_position = layout.x_offset;
        let mut x = 0_i32;
        for flag in &scratch.flags {
            self.charge(1)?;
            let delta = decode_coordinate(
                glyph,
                &mut x_position,
                *flag,
                GLYPH_FLAG_X_SHORT,
                GLYPH_FLAG_X_SAME_OR_POSITIVE,
            )
            .ok_or_else(|| self.error(FontErrorCode::InvalidGlyph, Some(glyph_id)))?;
            x = x
                .checked_add(delta)
                .ok_or_else(|| self.error(FontErrorCode::NumericOverflow, Some(glyph_id)))?;
            if i16::try_from(x).is_err() {
                return Err(self.error(FontErrorCode::InvalidGlyph, Some(glyph_id)));
            }
            scratch.points.push(RawPoint {
                x,
                y: 0,
                on_curve: *flag & GLYPH_FLAG_ON_CURVE != 0,
            });
        }
        if x_position != layout.y_offset {
            return Err(self.error(FontErrorCode::InternalState, Some(glyph_id)));
        }
        let mut y_position = layout.y_offset;
        let mut y = 0_i32;
        for (point, flag) in scratch.points.iter_mut().zip(&scratch.flags) {
            self.charge(1)?;
            let delta = decode_coordinate(
                glyph,
                &mut y_position,
                *flag,
                GLYPH_FLAG_Y_SHORT,
                GLYPH_FLAG_Y_SAME_OR_POSITIVE,
            )
            .ok_or_else(|| self.error(FontErrorCode::InvalidGlyph, Some(glyph_id)))?;
            y = y
                .checked_add(delta)
                .ok_or_else(|| self.error(FontErrorCode::NumericOverflow, Some(glyph_id)))?;
            if i16::try_from(y).is_err() {
                return Err(self.error(FontErrorCode::InvalidGlyph, Some(glyph_id)));
            }
            point.y = y;
        }
        if y_position != layout.end_offset {
            return Err(self.error(FontErrorCode::InternalState, Some(glyph_id)));
        }
        if !scratch.points.is_empty() {
            let mut x_min = i32::MAX;
            let mut y_min = i32::MAX;
            let mut x_max = i32::MIN;
            let mut y_max = i32::MIN;
            for point in &scratch.points {
                x_min = x_min.min(point.x);
                y_min = y_min.min(point.y);
                x_max = x_max.max(point.x);
                y_max = y_max.max(point.y);
            }
            if x_min != i32::from(bounds.x_min())
                || y_min != i32::from(bounds.y_min())
                || x_max != i32::from(bounds.x_max())
                || y_max != i32::from(bounds.y_max())
            {
                return Err(self.error(FontErrorCode::InvalidGlyph, Some(glyph_id)));
            }
        }
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    fn emit_simple_contours(
        &mut self,
        glyph: &[u8],
        layout: &SimpleLayout,
        glyph_id: u16,
        translate_x: i32,
        translate_y: i32,
        points: &[RawPoint],
        output: &mut Vec<OutlineSegment>,
    ) -> ParseResult<()> {
        let mut start_index = 0_usize;
        for contour in 0..layout.contours {
            self.charge(1)?;
            let end_index = usize::from(
                read_u16(glyph, layout.endpoints_offset + contour * 2)
                    .ok_or_else(|| self.error(FontErrorCode::InvalidGlyph, Some(glyph_id)))?,
            );
            let contour_points = points
                .get(start_index..=end_index)
                .ok_or_else(|| self.error(FontErrorCode::InvalidGlyph, Some(glyph_id)))?;
            self.emit_one_contour(contour_points, translate_x, translate_y, glyph_id, output)?;
            start_index = end_index
                .checked_add(1)
                .ok_or_else(|| self.error(FontErrorCode::NumericOverflow, Some(glyph_id)))?;
        }
        if start_index != points.len() {
            return Err(self.error(FontErrorCode::InternalState, Some(glyph_id)));
        }
        Ok(())
    }

    fn emit_one_contour(
        &mut self,
        points: &[RawPoint],
        translate_x: i32,
        translate_y: i32,
        glyph_id: u16,
        output: &mut Vec<OutlineSegment>,
    ) -> ParseResult<()> {
        let first = *points
            .first()
            .ok_or_else(|| self.error(FontErrorCode::InvalidGlyph, Some(glyph_id)))?;
        let last = *points
            .last()
            .ok_or_else(|| self.error(FontErrorCode::InvalidGlyph, Some(glyph_id)))?;
        let (start, process_start, process_end) = if first.on_curve {
            (
                translated_raw_point(first, translate_x, translate_y, glyph_id)?,
                1,
                points.len(),
            )
        } else if last.on_curve {
            (
                translated_raw_point(last, translate_x, translate_y, glyph_id)?,
                0,
                points.len() - 1,
            )
        } else {
            let first = translated_raw_point(first, translate_x, translate_y, glyph_id)?;
            let last = translated_raw_point(last, translate_x, translate_y, glyph_id)?;
            (midpoint(last, first, glyph_id)?, 0, points.len())
        };
        push_segment(output, OutlineSegment::MoveTo(start), glyph_id)?;
        let mut pending = None;
        for raw_point in &points[process_start..process_end] {
            self.charge(1)?;
            let point = translated_raw_point(*raw_point, translate_x, translate_y, glyph_id)?;
            if raw_point.on_curve {
                let segment = match pending.take() {
                    Some(control) => OutlineSegment::QuadTo {
                        control,
                        end: point,
                    },
                    None => OutlineSegment::LineTo(point),
                };
                push_segment(output, segment, glyph_id)?;
            } else if let Some(control) = pending.replace(point) {
                push_segment(
                    output,
                    OutlineSegment::QuadTo {
                        control,
                        end: midpoint(control, point, glyph_id)?,
                    },
                    glyph_id,
                )?;
            }
        }
        if let Some(control) = pending {
            push_segment(
                output,
                OutlineSegment::QuadTo {
                    control,
                    end: start,
                },
                glyph_id,
            )?;
        }
        push_segment(output, OutlineSegment::CloseContour, glyph_id)
    }

    #[allow(clippy::too_many_arguments)]
    fn emit_compound(
        &mut self,
        metadata: &Metadata,
        glyph: &[u8],
        glyph_id: u16,
        translate_x: i32,
        translate_y: i32,
        depth: u16,
        stack: &mut [u16; HARD_MAX_COMPONENT_DEPTH as usize + 1],
        output: &mut Vec<OutlineSegment>,
        scratch: &mut Scratch,
    ) -> ParseResult<Option<u16>> {
        self.validate_compound_structure(metadata, glyph, glyph_id, false)?;
        let mut position = 10_usize;
        let mut inherited_advance = None;
        loop {
            self.charge(3)?;
            let record = self.compound_record(metadata, glyph, position, glyph_id)?;
            position = record.end;
            let child_x =
                translate_x
                    .checked_add(record.x.checked_mul(2).ok_or_else(|| {
                        self.error(FontErrorCode::NumericOverflow, Some(glyph_id))
                    })?)
                    .ok_or_else(|| self.error(FontErrorCode::NumericOverflow, Some(glyph_id)))?;
            let child_y =
                translate_y
                    .checked_add(record.y.checked_mul(2).ok_or_else(|| {
                        self.error(FontErrorCode::NumericOverflow, Some(glyph_id))
                    })?)
                    .ok_or_else(|| self.error(FontErrorCode::NumericOverflow, Some(glyph_id)))?;
            let child_advance = self.emit_glyph(
                metadata,
                record.component,
                child_x,
                child_y,
                depth + 1,
                stack,
                output,
                scratch,
            )?;
            if record.flags & COMPOUND_USE_MY_METRICS != 0 {
                inherited_advance = Some(child_advance);
            }
            if record.flags & COMPOUND_MORE == 0 {
                break;
            }
        }
        Ok(inherited_advance)
    }

    fn validate_compound_structure(
        &mut self,
        metadata: &Metadata,
        glyph: &[u8],
        glyph_id: u16,
        account_source: bool,
    ) -> ParseResult<()> {
        let mut position = 10_usize;
        let mut component_count = 0_u64;
        let mut have_instructions = false;
        let mut first_unsupported = None;
        loop {
            self.charge(3)?;
            let record = self.compound_record(metadata, glyph, position, glyph_id)?;
            component_count = component_count
                .checked_add(1)
                .ok_or_else(|| self.error(FontErrorCode::NumericOverflow, Some(glyph_id)))?;
            let attempted_components = if account_source {
                self.stats.components.saturating_add(component_count)
            } else {
                component_count
            };
            if attempted_components > self.limits.max_components {
                return Err(self.resource(
                    FontLimitKind::Components,
                    self.limits.max_components,
                    if account_source {
                        self.stats.components
                    } else {
                        0
                    },
                    attempted_components,
                ));
            }
            have_instructions |= record.flags & COMPOUND_HAVE_INSTRUCTIONS != 0;
            if first_unsupported.is_none() {
                first_unsupported = self.compound_unsupported_kind(record.flags);
            }
            position = record.end;
            if record.flags & COMPOUND_MORE == 0 {
                break;
            }
        }
        self.validate_compound_instruction_tail(glyph, position, have_instructions, glyph_id)?;
        if account_source {
            self.stats.components = self
                .stats
                .components
                .checked_add(component_count)
                .ok_or_else(|| self.error(FontErrorCode::NumericOverflow, Some(glyph_id)))?;
        }
        if let Some(kind) = first_unsupported {
            return Err(self.unsupported(kind, Some(glyph_id)));
        }
        Ok(())
    }

    fn validate_compound_instruction_tail(
        &mut self,
        glyph: &[u8],
        position: usize,
        have_instructions: bool,
        glyph_id: u16,
    ) -> ParseResult<()> {
        if !have_instructions {
            return Ok(());
        }
        let length = usize::from(
            read_u16(glyph, position)
                .ok_or_else(|| self.error(FontErrorCode::InvalidGlyph, Some(glyph_id)))?,
        );
        let end = position
            .checked_add(2)
            .and_then(|value| value.checked_add(length))
            .ok_or_else(|| self.error(FontErrorCode::NumericOverflow, Some(glyph_id)))?;
        if end > glyph.len() {
            return Err(self.error(FontErrorCode::InvalidGlyph, Some(glyph_id)));
        }
        self.charge(u64::try_from(length).unwrap_or(u64::MAX))
    }

    fn compound_record(
        &self,
        metadata: &Metadata,
        glyph: &[u8],
        position: usize,
        glyph_id: u16,
    ) -> ParseResult<CompoundRecord> {
        let flags = read_u16(glyph, position)
            .ok_or_else(|| self.error(FontErrorCode::InvalidGlyph, Some(glyph_id)))?;
        let component = read_u16(glyph, position + 2)
            .ok_or_else(|| self.error(FontErrorCode::InvalidGlyph, Some(glyph_id)))?;
        self.validate_compound_flag_shape(flags, glyph_id)?;

        let argument_bytes = if flags & COMPOUND_ARG_WORDS != 0 {
            4_usize
        } else {
            2_usize
        };
        let transform_bytes = if flags & COMPOUND_HAVE_TWO_BY_TWO != 0 {
            8_usize
        } else if flags & COMPOUND_HAVE_XY_SCALE != 0 {
            4_usize
        } else if flags & COMPOUND_HAVE_SCALE != 0 {
            2_usize
        } else {
            0_usize
        };
        let end = position
            .checked_add(4)
            .and_then(|value| value.checked_add(argument_bytes))
            .and_then(|value| value.checked_add(transform_bytes))
            .ok_or_else(|| self.error(FontErrorCode::NumericOverflow, Some(glyph_id)))?;
        if end > glyph.len() || component >= metadata.glyph_count {
            return Err(self.error(FontErrorCode::InvalidGlyph, Some(glyph_id)));
        }
        let (x, y) = if flags & COMPOUND_ARG_WORDS != 0 {
            (
                i32::from(
                    read_i16(glyph, position + 4)
                        .ok_or_else(|| self.error(FontErrorCode::InvalidGlyph, Some(glyph_id)))?,
                ),
                i32::from(
                    read_i16(glyph, position + 6)
                        .ok_or_else(|| self.error(FontErrorCode::InvalidGlyph, Some(glyph_id)))?,
                ),
            )
        } else {
            (
                i32::from(
                    *glyph
                        .get(position + 4)
                        .ok_or_else(|| self.error(FontErrorCode::InvalidGlyph, Some(glyph_id)))?
                        as i8,
                ),
                i32::from(
                    *glyph
                        .get(position + 5)
                        .ok_or_else(|| self.error(FontErrorCode::InvalidGlyph, Some(glyph_id)))?
                        as i8,
                ),
            )
        };
        Ok(CompoundRecord {
            flags,
            component,
            x,
            y,
            end,
        })
    }

    const fn compound_unsupported_kind(&self, flags: u16) -> Option<FontUnsupportedKind> {
        if flags & COMPOUND_ARGS_XY == 0 {
            Some(FontUnsupportedKind::CompoundPointAttachment)
        } else if flags & (COMPOUND_HAVE_SCALE | COMPOUND_HAVE_XY_SCALE | COMPOUND_HAVE_TWO_BY_TWO)
            != 0
        {
            Some(FontUnsupportedKind::CompoundTransform)
        } else {
            None
        }
    }

    fn validate_compound_flag_shape(&self, flags: u16, glyph_id: u16) -> ParseResult<()> {
        if flags & COMPOUND_SCALED_OFFSET != 0 && flags & COMPOUND_UNSCALED_OFFSET != 0 {
            return Err(self.error(FontErrorCode::InvalidGlyph, Some(glyph_id)));
        }
        let transform_count = u8::from(flags & COMPOUND_HAVE_SCALE != 0)
            + u8::from(flags & COMPOUND_HAVE_XY_SCALE != 0)
            + u8::from(flags & COMPOUND_HAVE_TWO_BY_TWO != 0);
        if transform_count > 1 || flags & !COMPOUND_KNOWN != 0 {
            return Err(self.error(FontErrorCode::InvalidGlyph, Some(glyph_id)));
        }
        Ok(())
    }

    fn validate_compound_bounds(
        &mut self,
        segments: &[OutlineSegment],
        declared: FontBounds,
        translate_x: i32,
        translate_y: i32,
        glyph_id: u16,
    ) -> ParseResult<()> {
        let mut actual = None;
        for segment in segments {
            self.charge(1)?;
            match *segment {
                OutlineSegment::MoveTo(point) | OutlineSegment::LineTo(point) => {
                    include_emitted_point(&mut actual, point);
                }
                OutlineSegment::QuadTo { control, end } => {
                    include_emitted_point(&mut actual, control);
                    include_emitted_point(&mut actual, end);
                }
                OutlineSegment::CloseContour => {}
            }
        }
        let Some((x_min, y_min, x_max, y_max)) = actual else {
            return Ok(());
        };
        let declared_x_min = i32::from(declared.x_min())
            .checked_mul(2)
            .and_then(|value| value.checked_add(translate_x))
            .ok_or_else(|| self.error(FontErrorCode::NumericOverflow, Some(glyph_id)))?;
        let declared_y_min = i32::from(declared.y_min())
            .checked_mul(2)
            .and_then(|value| value.checked_add(translate_y))
            .ok_or_else(|| self.error(FontErrorCode::NumericOverflow, Some(glyph_id)))?;
        let declared_x_max = i32::from(declared.x_max())
            .checked_mul(2)
            .and_then(|value| value.checked_add(translate_x))
            .ok_or_else(|| self.error(FontErrorCode::NumericOverflow, Some(glyph_id)))?;
        let declared_y_max = i32::from(declared.y_max())
            .checked_mul(2)
            .and_then(|value| value.checked_add(translate_y))
            .ok_or_else(|| self.error(FontErrorCode::NumericOverflow, Some(glyph_id)))?;
        if x_min < declared_x_min
            || y_min < declared_y_min
            || x_max > declared_x_max
            || y_max > declared_y_max
        {
            return Err(self.error(FontErrorCode::InvalidGlyph, Some(glyph_id)));
        }
        Ok(())
    }

    fn enter_glyph(
        &self,
        glyph_id: u16,
        depth: u16,
        stack: &mut [u16; HARD_MAX_COMPONENT_DEPTH as usize + 1],
    ) -> ParseResult<()> {
        if depth > self.limits.max_component_depth {
            return Err(self.resource(
                FontLimitKind::ComponentDepth,
                u64::from(self.limits.max_component_depth),
                u64::from(self.limits.max_component_depth),
                u64::from(depth),
            ));
        }
        let depth_index = usize::from(depth);
        if stack[..depth_index].contains(&glyph_id) {
            return Err(self.error(FontErrorCode::CompoundCycle, Some(glyph_id)));
        }
        stack[depth_index] = glyph_id;
        Ok(())
    }

    fn glyph_header(&self, glyph: &[u8], glyph_id: u16) -> ParseResult<GlyphHeader> {
        if glyph.len() < 10 {
            return Err(self.error(FontErrorCode::InvalidGlyph, Some(glyph_id)));
        }
        let contours = read_i16(glyph, 0)
            .ok_or_else(|| self.error(FontErrorCode::InvalidGlyph, Some(glyph_id)))?;
        let x_min = read_i16(glyph, 2)
            .ok_or_else(|| self.error(FontErrorCode::InvalidGlyph, Some(glyph_id)))?;
        let y_min = read_i16(glyph, 4)
            .ok_or_else(|| self.error(FontErrorCode::InvalidGlyph, Some(glyph_id)))?;
        let x_max = read_i16(glyph, 6)
            .ok_or_else(|| self.error(FontErrorCode::InvalidGlyph, Some(glyph_id)))?;
        let y_max = read_i16(glyph, 8)
            .ok_or_else(|| self.error(FontErrorCode::InvalidGlyph, Some(glyph_id)))?;
        if x_min > x_max || y_min > y_max {
            return Err(self.error(FontErrorCode::InvalidGlyph, Some(glyph_id)));
        }
        Ok(GlyphHeader {
            contours,
            bounds: FontBounds::new(x_min, y_min, x_max, y_max),
        })
    }

    fn glyph_range(&self, metadata: &Metadata, glyph_id: u16) -> ParseResult<&'a [u8]> {
        let start = self.loca_offset(metadata, glyph_id)?;
        let end = self.loca_offset(metadata, glyph_id + 1)?;
        if start > end || end > metadata.tables.glyf.length {
            return Err(self.error(FontErrorCode::InvalidLoca, Some(glyph_id)));
        }
        let glyf = table_slice(self.bytes, metadata.tables.glyf, FontErrorCode::InvalidLoca)?;
        glyf.get(start..end)
            .ok_or_else(|| self.error(FontErrorCode::InvalidLoca, Some(glyph_id)))
    }

    fn loca_offset(&self, metadata: &Metadata, index: u16) -> ParseResult<usize> {
        let loca = table_slice(self.bytes, metadata.tables.loca, FontErrorCode::InvalidLoca)?;
        if metadata.long_loca {
            let position = usize::from(index)
                .checked_mul(4)
                .ok_or_else(|| self.error(FontErrorCode::NumericOverflow, None))?;
            usize::try_from(
                read_u32(loca, position)
                    .ok_or_else(|| self.error(FontErrorCode::InvalidLoca, None))?,
            )
            .map_err(|_| self.error(FontErrorCode::NumericOverflow, None))
        } else {
            let position = usize::from(index)
                .checked_mul(2)
                .ok_or_else(|| self.error(FontErrorCode::NumericOverflow, None))?;
            usize::from(
                read_u16(loca, position)
                    .ok_or_else(|| self.error(FontErrorCode::InvalidLoca, None))?,
            )
            .checked_mul(2)
            .ok_or_else(|| self.error(FontErrorCode::NumericOverflow, None))
        }
    }

    fn advance_width(&self, metadata: &Metadata, glyph_id: u16) -> ParseResult<u16> {
        let metric_index = glyph_id.min(metadata.number_of_h_metrics - 1);
        let position = usize::from(metric_index)
            .checked_mul(4)
            .ok_or_else(|| self.error(FontErrorCode::NumericOverflow, Some(glyph_id)))?;
        let hmtx = table_slice(self.bytes, metadata.tables.hmtx, FontErrorCode::InvalidHmtx)?;
        read_u16(hmtx, position)
            .ok_or_else(|| self.error(FontErrorCode::InvalidHmtx, Some(glyph_id)))
    }

    fn charge(&mut self, amount: u64) -> ParseResult<()> {
        let attempted = self.stats.fuel.saturating_add(amount);
        if attempted > self.limits.max_fuel {
            return Err(self.resource(
                FontLimitKind::Fuel,
                self.limits.max_fuel,
                self.stats.fuel,
                attempted,
            ));
        }
        self.stats.fuel = attempted;
        while self.stats.fuel >= self.next_cancellation_fuel {
            self.check_cancelled()?;
            self.next_cancellation_fuel = self
                .next_cancellation_fuel
                .saturating_add(self.limits.cancellation_check_interval_fuel);
            if self.next_cancellation_fuel == u64::MAX {
                break;
            }
        }
        Ok(())
    }

    fn check_cancelled(&self) -> ParseResult<()> {
        if self.cancellation.is_cancelled() {
            Err(self.error(FontErrorCode::Cancelled, None))
        } else {
            Ok(())
        }
    }

    fn checked_limit_add(
        &self,
        kind: FontLimitKind,
        consumed: u64,
        addition: u64,
        limit: u64,
    ) -> ParseResult<u64> {
        let attempted = consumed.saturating_add(addition);
        if attempted > limit {
            Err(self.resource(kind, limit, consumed, attempted))
        } else {
            Ok(attempted)
        }
    }

    fn error(&self, code: FontErrorCode, glyph_id: Option<u16>) -> ParseStop {
        ParseStop::Error(FontError::for_code(code, glyph_id))
    }

    fn unsupported(&self, kind: FontUnsupportedKind, glyph_id: Option<u16>) -> ParseStop {
        ParseStop::Unsupported(FontUnsupported::new(kind, glyph_id))
    }

    fn resource(
        &self,
        kind: FontLimitKind,
        limit: u64,
        consumed: u64,
        attempted: u64,
    ) -> ParseStop {
        ParseStop::Error(FontError::resource(FontLimit::new(
            kind, limit, consumed, attempted,
        )))
    }
}

struct FlagIter<'a> {
    glyph: &'a [u8],
    position: usize,
    remaining: usize,
    repeated_flag: u8,
    repeat_remaining: usize,
    glyph_id: u16,
}

impl<'a> FlagIter<'a> {
    const fn new(glyph: &'a [u8], position: usize, points: usize, glyph_id: u16) -> Self {
        Self {
            glyph,
            position,
            remaining: points,
            repeated_flag: 0,
            repeat_remaining: 0,
            glyph_id,
        }
    }

    fn next(&mut self) -> ParseResult<Option<u8>> {
        if self.remaining == 0 {
            return Ok(None);
        }
        let flag = if self.repeat_remaining != 0 {
            self.repeat_remaining -= 1;
            self.repeated_flag
        } else {
            let flag = *self.glyph.get(self.position).ok_or_else(|| {
                ParseStop::Error(FontError::for_code(
                    FontErrorCode::InvalidGlyph,
                    Some(self.glyph_id),
                ))
            })?;
            self.position += 1;
            if flag & GLYPH_FLAG_RESERVED != 0 {
                return Err(ParseStop::Error(FontError::for_code(
                    FontErrorCode::InvalidGlyph,
                    Some(self.glyph_id),
                )));
            }
            if flag & GLYPH_FLAG_REPEAT != 0 {
                let repeats = usize::from(*self.glyph.get(self.position).ok_or_else(|| {
                    ParseStop::Error(FontError::for_code(
                        FontErrorCode::InvalidGlyph,
                        Some(self.glyph_id),
                    ))
                })?);
                self.position += 1;
                if repeats >= self.remaining {
                    return Err(ParseStop::Error(FontError::for_code(
                        FontErrorCode::InvalidGlyph,
                        Some(self.glyph_id),
                    )));
                }
                self.repeated_flag = flag;
                self.repeat_remaining = repeats;
            }
            flag
        };
        self.remaining -= 1;
        Ok(Some(flag))
    }

    const fn position(&self) -> usize {
        self.position
    }
}

fn directory_record(bytes: &[u8], record: usize) -> ParseResult<Table> {
    let offset = usize::try_from(read_u32(bytes, record + 8).ok_or_else(|| {
        ParseStop::Error(FontError::for_code(FontErrorCode::MalformedSfnt, None))
    })?)
    .map_err(|_| ParseStop::Error(FontError::for_code(FontErrorCode::NumericOverflow, None)))?;
    let length = usize::try_from(read_u32(bytes, record + 12).ok_or_else(|| {
        ParseStop::Error(FontError::for_code(FontErrorCode::MalformedSfnt, None))
    })?)
    .map_err(|_| ParseStop::Error(FontError::for_code(FontErrorCode::NumericOverflow, None)))?;
    Ok(Table { offset, length })
}

fn ranges_overlap(left: Table, right: Table) -> ParseResult<bool> {
    let left_end = left.offset.checked_add(left.length).ok_or_else(|| {
        ParseStop::Error(FontError::for_code(FontErrorCode::NumericOverflow, None))
    })?;
    let right_end = right.offset.checked_add(right.length).ok_or_else(|| {
        ParseStop::Error(FontError::for_code(FontErrorCode::NumericOverflow, None))
    })?;
    Ok(left.offset < right_end && right.offset < left_end)
}

fn table_slice(bytes: &[u8], table: Table, error_code: FontErrorCode) -> ParseResult<&[u8]> {
    let end = table.offset.checked_add(table.length).ok_or_else(|| {
        ParseStop::Error(FontError::for_code(FontErrorCode::NumericOverflow, None))
    })?;
    bytes
        .get(table.offset..end)
        .ok_or_else(|| ParseStop::Error(FontError::for_code(error_code, None)))
}

fn coordinate_bytes(flag: u8, short_mask: u8, same_mask: u8) -> usize {
    if flag & short_mask != 0 {
        1
    } else if flag & same_mask != 0 {
        0
    } else {
        2
    }
}

fn decode_coordinate(
    glyph: &[u8],
    position: &mut usize,
    flag: u8,
    short_mask: u8,
    same_mask: u8,
) -> Option<i32> {
    if flag & short_mask != 0 {
        let magnitude = i32::from(*glyph.get(*position)?);
        *position = position.checked_add(1)?;
        Some(if flag & same_mask != 0 {
            magnitude
        } else {
            -magnitude
        })
    } else if flag & same_mask != 0 {
        Some(0)
    } else {
        let value = i32::from(read_i16(glyph, *position)?);
        *position = position.checked_add(2)?;
        Some(value)
    }
}

fn translated_raw_point(
    point: RawPoint,
    translate_x: i32,
    translate_y: i32,
    glyph_id: u16,
) -> ParseResult<FontPoint> {
    let x = point
        .x
        .checked_mul(2)
        .and_then(|value| value.checked_add(translate_x))
        .ok_or_else(|| {
            ParseStop::Error(FontError::for_code(
                FontErrorCode::NumericOverflow,
                Some(glyph_id),
            ))
        })?;
    let y = point
        .y
        .checked_mul(2)
        .and_then(|value| value.checked_add(translate_y))
        .ok_or_else(|| {
            ParseStop::Error(FontError::for_code(
                FontErrorCode::NumericOverflow,
                Some(glyph_id),
            ))
        })?;
    Ok(FontPoint::new(
        FontCoordinate::from_half_units(x),
        FontCoordinate::from_half_units(y),
    ))
}

fn midpoint(left: FontPoint, right: FontPoint, glyph_id: u16) -> ParseResult<FontPoint> {
    let x = left
        .x()
        .half_units()
        .checked_add(right.x().half_units())
        .ok_or_else(|| {
            ParseStop::Error(FontError::for_code(
                FontErrorCode::NumericOverflow,
                Some(glyph_id),
            ))
        })?
        / 2;
    let y = left
        .y()
        .half_units()
        .checked_add(right.y().half_units())
        .ok_or_else(|| {
            ParseStop::Error(FontError::for_code(
                FontErrorCode::NumericOverflow,
                Some(glyph_id),
            ))
        })?
        / 2;
    Ok(FontPoint::new(
        FontCoordinate::from_half_units(x),
        FontCoordinate::from_half_units(y),
    ))
}

fn include_emitted_point(bounds: &mut Option<(i32, i32, i32, i32)>, point: FontPoint) {
    let x = point.x().half_units();
    let y = point.y().half_units();
    *bounds = Some(match *bounds {
        Some((x_min, y_min, x_max, y_max)) => {
            (x_min.min(x), y_min.min(y), x_max.max(x), y_max.max(y))
        }
        None => (x, y, x, y),
    });
}

fn push_segment(
    output: &mut Vec<OutlineSegment>,
    segment: OutlineSegment,
    glyph_id: u16,
) -> ParseResult<()> {
    if output.len() == output.capacity() {
        return Err(ParseStop::Error(FontError::for_code(
            FontErrorCode::InternalState,
            Some(glyph_id),
        )));
    }
    output.push(segment);
    Ok(())
}

fn retained_font_bytes(glyph_capacity: usize, segment_capacity: usize) -> ParseResult<u64> {
    let glyphs = capacity_bytes::<GlyphRecord>(glyph_capacity)?;
    let segments = capacity_bytes::<OutlineSegment>(segment_capacity)?;
    let arc_overhead = u64::try_from(
        2_usize
            .checked_mul(size_of::<Vec<GlyphRecord>>() + 2 * size_of::<usize>())
            .ok_or_else(|| {
                ParseStop::Error(FontError::for_code(FontErrorCode::NumericOverflow, None))
            })?,
    )
    .map_err(|_| ParseStop::Error(FontError::for_code(FontErrorCode::NumericOverflow, None)))?;
    glyphs
        .checked_add(segments)
        .and_then(|value| value.checked_add(arc_overhead))
        .ok_or_else(|| ParseStop::Error(FontError::for_code(FontErrorCode::NumericOverflow, None)))
}

fn retained_scratch_bytes(capacity: usize) -> ParseResult<u64> {
    retained_scratch_bytes_with_capacities(capacity, capacity)
}

fn retained_scratch_bytes_with_capacities(
    flag_capacity: usize,
    point_capacity: usize,
) -> ParseResult<u64> {
    capacity_bytes::<u8>(flag_capacity)?
        .checked_add(capacity_bytes::<RawPoint>(point_capacity)?)
        .ok_or_else(|| ParseStop::Error(FontError::for_code(FontErrorCode::NumericOverflow, None)))
}

fn capacity_bytes<T>(capacity: usize) -> ParseResult<u64> {
    let bytes = capacity.checked_mul(size_of::<T>()).ok_or_else(|| {
        ParseStop::Error(FontError::for_code(FontErrorCode::NumericOverflow, None))
    })?;
    u64::try_from(bytes)
        .map_err(|_| ParseStop::Error(FontError::for_code(FontErrorCode::NumericOverflow, None)))
}

fn try_reserve_exact<T>(
    values: &mut Vec<T>,
    capacity: usize,
    limit: u64,
    attempted: u64,
) -> ParseResult<()> {
    values.try_reserve_exact(capacity).map_err(|_| {
        ParseStop::Error(FontError::resource(FontLimit::new(
            FontLimitKind::Allocation,
            limit,
            0,
            attempted,
        )))
    })
}

const fn winansi_unicode(byte: u8) -> Option<u16> {
    Some(match byte {
        0x00..=0x1f => return None,
        0x20..=0x7e => byte as u16,
        0x7f | 0x81 | 0x8d | 0x8f | 0x90 | 0x95 | 0x9d => 0x2022,
        0x80 => 0x20ac,
        0x82 => 0x201a,
        0x83 => 0x0192,
        0x84 => 0x201e,
        0x85 => 0x2026,
        0x86 => 0x2020,
        0x87 => 0x2021,
        0x88 => 0x02c6,
        0x89 => 0x2030,
        0x8a => 0x0160,
        0x8b => 0x2039,
        0x8c => 0x0152,
        0x8e => 0x017d,
        0x91 => 0x2018,
        0x92 => 0x2019,
        0x93 => 0x201c,
        0x94 => 0x201d,
        0x96 => 0x2013,
        0x97 => 0x2014,
        0x98 => 0x02dc,
        0x99 => 0x2122,
        0x9a => 0x0161,
        0x9b => 0x203a,
        0x9c => 0x0153,
        0x9e => 0x017e,
        0x9f => 0x0178,
        0xa0 => 0x0020,
        0xad => 0x002d,
        0xa1..=0xff => byte as u16,
    })
}

fn read_u16(bytes: &[u8], offset: usize) -> Option<u16> {
    let value: [u8; 2] = bytes.get(offset..offset.checked_add(2)?)?.try_into().ok()?;
    Some(u16::from_be_bytes(value))
}

fn read_i16(bytes: &[u8], offset: usize) -> Option<i16> {
    let value: [u8; 2] = bytes.get(offset..offset.checked_add(2)?)?.try_into().ok()?;
    Some(i16::from_be_bytes(value))
}

fn read_u32(bytes: &[u8], offset: usize) -> Option<u32> {
    let value: [u8; 4] = bytes.get(offset..offset.checked_add(4)?)?.try_into().ok()?;
    Some(u32::from_be_bytes(value))
}

#[cfg(test)]
mod tests {
    use super::winansi_unicode;

    #[test]
    fn complete_pdf_winansi_mapping_handles_special_and_reassigned_codes() {
        assert_eq!(winansi_unicode(0x1f), None);
        assert_eq!(winansi_unicode(b'A'), Some(u16::from(b'A')));
        assert_eq!(winansi_unicode(0x7f), Some(0x2022));
        assert_eq!(winansi_unicode(0x80), Some(0x20ac));
        assert_eq!(winansi_unicode(0x8c), Some(0x0152));
        assert_eq!(winansi_unicode(0x95), Some(0x2022));
        assert_eq!(winansi_unicode(0xa0), Some(0x0020));
        assert_eq!(winansi_unicode(0xad), Some(0x002d));
        assert_eq!(winansi_unicode(0xff), Some(0x00ff));
    }
}
