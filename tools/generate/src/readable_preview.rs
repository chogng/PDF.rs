//! Deterministic two-page readable fixture for the M4 desktop preview.

use std::collections::BTreeMap;

use pdf_rs_digest::sha256;

use super::{
    GenerateError, PdfOutput, append_metadata, append_object_header, append_plain_object,
    append_stream_object, checked_pdf_length, checked_xref_offset,
};

const OUTPUT_LIMIT: usize = 4 * 1024 * 1024;
const OBJECT_COUNT: usize = 9;
const PROFILE_SOURCE: &[u8] = b"m4.readable-preview.v1:two-page-letter:block-font";

const PAGE_ONE_CONTENT: &[u8] = br#"0.96 g 0 0 612 792 re f
0.08 0.11 0.19 rg 0 630 612 162 re f
1 g BT /F0 58 Tf 54 690 Td (PDF.RS) Tj ET
0.72 0.84 1 rg BT /F0 18 Tf 56 650 Td (NATIVE PDF PREVIEW) Tj ET
0.12 0.15 0.22 rg BT /F0 27 Tf 56 564 Td (RENDERED BY PDF.RS) Tj ET
0.32 g BT /F0 15 Tf 56 526 Td (PARSING  SCENE  RASTER  PIXELS) Tj ET
0.90 0.94 0.98 rg 56 370 152 108 re f
0.94 0.91 0.86 rg 230 370 152 108 re f
0.91 0.95 0.89 rg 404 370 152 108 re f
0.10 0.17 0.28 rg BT /F0 22 Tf 74 430 Td (PARSER) Tj ET
0.38 g BT /F0 12 Tf 74 402 Td (STRICT INPUT) Tj ET
0.30 0.20 0.12 rg BT /F0 22 Tf 248 430 Td (SCENE) Tj ET
0.38 g BT /F0 12 Tf 248 402 Td (NATIVE MODEL) Tj ET
0.13 0.28 0.16 rg BT /F0 22 Tf 422 430 Td (RASTER) Tj ET
0.38 g BT /F0 12 Tf 422 402 Td (RGBA OUTPUT) Tj ET
0.82 g 56 314 500 2 re f
0.18 g BT /F0 17 Tf 56 266 Td (THIS PAGE IS RENDERED BY RUST.) Tj ET
0.42 g BT /F0 13 Tf 56 228 Td (ELECTRON ONLY PRESENTS THE FINAL PIXELS.) Tj ET
0.72 0.20 0.14 rg 56 150 92 10 re f
0.22 g BT /F0 12 Tf 56 108 Td (PAGE 1 / 2) Tj ET
"#;

const PAGE_TWO_CONTENT: &[u8] = br#"0.97 g 0 0 612 792 re f
0.72 0.20 0.14 rg 0 650 612 142 re f
1 g BT /F0 48 Tf 54 704 Td (PAGE TWO) Tj ET
1 g BT /F0 17 Tf 56 668 Td (THE READING LOOP IS LIVE) Tj ET
0.13 0.16 0.22 rg BT /F0 30 Tf 56 578 Td (NAVIGATION WORKS) Tj ET
0.38 g BT /F0 15 Tf 56 538 Td (ZOOM  RESIZE  CLOSE  REOPEN) Tj ET
0.88 0.91 0.95 rg 56 402 112 82 re f
0.90 0.94 0.88 rg 186 402 112 82 re f
0.96 0.90 0.84 rg 316 402 112 82 re f
0.92 0.88 0.94 rg 446 402 110 82 re f
0.12 0.17 0.25 rg BT /F0 18 Tf 72 438 Td (OPEN) Tj ET
0.12 0.24 0.15 rg BT /F0 18 Tf 198 438 Td (PARSE) Tj ET
0.32 0.19 0.10 rg BT /F0 18 Tf 328 438 Td (SCENE) Tj ET
0.26 0.15 0.30 rg BT /F0 18 Tf 456 438 Td (PIXELS) Tj ET
0.72 0.20 0.14 rg 168 439 18 6 re f
0.72 0.20 0.14 rg 298 439 18 6 re f
0.72 0.20 0.14 rg 428 439 18 6 re f
0.18 g BT /F0 17 Tf 56 322 Td (EVERY VISIBLE MARK COMES FROM PDF.RS.) Tj ET
0.42 g BT /F0 13 Tf 56 284 Td (NO BROWSER PDF PLUGIN. NO EXTERNAL ENGINE.) Tj ET
0.82 g 56 224 500 2 re f
0.18 g BT /F0 15 Tf 56 174 Td (USE THE TOOLBAR TO RETURN TO PAGE ONE.) Tj ET
0.72 0.20 0.14 rg 56 120 92 10 re f
0.22 g BT /F0 12 Tf 56 78 Td (PAGE 2 / 2) Tj ET
"#;

/// Generates the deterministic readable two-page PDF used by the M4 Electron preview.
pub fn generate_readable_preview_pdf() -> Result<Vec<u8>, GenerateError> {
    let font = readable_font();
    let mut output = PdfOutput::new(OUTPUT_LIMIT);
    output.append(b"%PDF-1.7\n%\xe2\xe3\xcf\xd3\n")?;
    let source_hash = sha256(PROFILE_SOURCE).map_err(|_| super::hash_failed())?;
    append_metadata(&mut output, &source_hash)?;
    output.append(b"% profile=m4.readable-preview.v1\n")?;

    let mut offsets = Vec::with_capacity(OBJECT_COUNT);
    append_plain_object(
        &mut output,
        &mut offsets,
        1,
        b"<< /Type /Catalog /Pages 2 0 R >>\n",
    )?;
    append_plain_object(
        &mut output,
        &mut offsets,
        2,
        b"<< /Type /Pages /Kids [3 0 R 5 0 R] /Count 2 >>\n",
    )?;
    append_plain_object(
        &mut output,
        &mut offsets,
        3,
        b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] \
          /Resources << /Font << /F0 7 0 R >> >> /Contents 4 0 R >>\n",
    )?;
    append_stream_object(&mut output, &mut offsets, 4, PAGE_ONE_CONTENT)?;
    append_plain_object(
        &mut output,
        &mut offsets,
        5,
        b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] \
          /Resources << /Font << /F0 7 0 R >> >> /Contents 6 0 R >>\n",
    )?;
    append_stream_object(&mut output, &mut offsets, 6, PAGE_TWO_CONTENT)?;

    let widths = (0x20_u8..=0x7e)
        .map(|_| "600")
        .collect::<Vec<_>>()
        .join(" ");
    append_plain_object(
        &mut output,
        &mut offsets,
        7,
        format!(
            "<< /Type /Font /Subtype /TrueType /BaseFont /PdfRsBlock \
             /Encoding /WinAnsiEncoding /FirstChar 32 /LastChar 126 \
             /Widths [{widths}] /FontDescriptor 8 0 R >>\n"
        )
        .as_bytes(),
    )?;
    append_plain_object(
        &mut output,
        &mut offsets,
        8,
        b"<< /Type /FontDescriptor /FontName /PdfRsBlock /Flags 4 \
          /FontBBox [0 0 500 700] /ItalicAngle 0 /Ascent 700 /Descent 0 \
          /CapHeight 700 /StemV 80 /FontFile2 9 0 R >>\n",
    )?;
    append_font_program(&mut output, &mut offsets, &font)?;

    let startxref = checked_xref_offset(output.len())?;
    let xref_size = checked_pdf_length(
        offsets
            .len()
            .checked_add(1)
            .ok_or_else(super::object_count_overflow)?,
    )?;
    output.append(b"xref\n")?;
    output.append_text(&format!("0 {xref_size}\n"))?;
    output.append(b"0000000000 65535 f \n")?;
    for offset in offsets {
        output.append_text(&format!("{offset:010} 00000 n \n"))?;
    }
    output.append(b"trailer\n")?;
    output.append_text(&format!("<< /Size {xref_size} /Root 1 0 R >>\n"))?;
    output.append(b"startxref\n")?;
    output.append_text(&format!("{startxref}\n%%EOF\n"))?;
    Ok(output.finish())
}

fn append_font_program(
    output: &mut PdfOutput,
    offsets: &mut Vec<u64>,
    font: &[u8],
) -> Result<(), GenerateError> {
    let length = checked_pdf_length(font.len())?;
    append_object_header(output, offsets, 9)?;
    output.append_text(&format!("<< /Length {length} /Length1 {length} >>\n"))?;
    output.append(b"stream\n")?;
    output.append(font)?;
    output.append(b"\nendstream\nendobj\n")
}

fn readable_font() -> Vec<u8> {
    let mut glyphs = vec![Vec::new()];
    let mut glyph_ids = [0_u16; 95];
    let mut known = BTreeMap::<u8, u16>::new();

    for byte in 0x20_u8..=0x7e {
        if byte == b' ' {
            continue;
        }
        let canonical = if byte.is_ascii_lowercase() {
            byte.to_ascii_uppercase()
        } else {
            byte
        };
        let glyph_id = if let Some(glyph_id) = known.get(&canonical) {
            *glyph_id
        } else {
            let glyph_id = u16::try_from(glyphs.len()).expect("preview glyph count fits u16");
            glyphs.push(block_glyph(pattern(canonical)));
            known.insert(canonical, glyph_id);
            glyph_id
        };
        glyph_ids[usize::from(byte - 0x20)] = glyph_id;
    }

    build_font(glyphs, glyph_ids)
}

fn build_font(glyphs: Vec<Vec<u8>>, glyph_ids: [u16; 95]) -> Vec<u8> {
    let glyph_count = u16::try_from(glyphs.len()).expect("preview glyph count fits u16");

    let mut head = vec![0_u8; 54];
    set_u32(&mut head, 0, 0x0001_0000);
    set_u32(&mut head, 12, 0x5f0f_3cf5);
    set_u16(&mut head, 18, 1_000);
    set_i16(&mut head, 50, 1);

    let mut hhea = vec![0_u8; 36];
    set_u32(&mut hhea, 0, 0x0001_0000);
    set_i16(&mut hhea, 4, 700);
    set_i16(&mut hhea, 6, 0);
    set_u16(&mut hhea, 34, glyph_count);

    let mut maxp = vec![0_u8; 32];
    set_u32(&mut maxp, 0, 0x0001_0000);
    set_u16(&mut maxp, 4, glyph_count);

    let mut loca = Vec::with_capacity((glyphs.len() + 1) * 4);
    let mut glyf = Vec::new();
    loca.extend_from_slice(&0_u32.to_be_bytes());
    for glyph in glyphs {
        glyf.extend_from_slice(&glyph);
        loca.extend_from_slice(
            &u32::try_from(glyf.len())
                .expect("preview glyph table fits u32")
                .to_be_bytes(),
        );
    }

    let mut hmtx = Vec::with_capacity(usize::from(glyph_count) * 4);
    for _ in 0..glyph_count {
        hmtx.extend_from_slice(&600_u16.to_be_bytes());
        hmtx.extend_from_slice(&0_i16.to_be_bytes());
    }
    let cmap = ascii_cmap(glyph_ids);

    let tables = [
        (*b"head", head),
        (*b"hhea", hhea),
        (*b"maxp", maxp),
        (*b"loca", loca),
        (*b"glyf", glyf),
        (*b"hmtx", hmtx),
        (*b"cmap", cmap),
    ];
    let directory_len = 12 + tables.len() * 16;
    let mut font = vec![0_u8; directory_len];
    set_u32(&mut font, 0, 0x0001_0000);
    set_u16(
        &mut font,
        4,
        u16::try_from(tables.len()).expect("table count fits u16"),
    );
    for (index, (tag, table)) in tables.into_iter().enumerate() {
        while !font.len().is_multiple_of(4) {
            font.push(0);
        }
        let offset = font.len();
        font.extend_from_slice(&table);
        let record = 12 + index * 16;
        font[record..record + 4].copy_from_slice(&tag);
        set_u32(
            &mut font,
            record + 8,
            u32::try_from(offset).expect("font offset fits u32"),
        );
        set_u32(
            &mut font,
            record + 12,
            u32::try_from(table.len()).expect("font table length fits u32"),
        );
    }
    font
}

fn block_glyph(rows: [u8; 7]) -> Vec<u8> {
    let mut cells = [[false; 5]; 7];
    for (row, bits) in rows.into_iter().enumerate() {
        for column in 0..5 {
            cells[row][column] = bits & (1 << (4 - column)) != 0;
        }
    }

    let mut points = Vec::<(i16, i16)>::new();
    let mut endpoints = Vec::<u16>::new();
    for row in 0..7 {
        for column in 0..5 {
            if !cells[row][column] {
                continue;
            }
            let (width, height) = largest_rectangle(&cells, row, column);
            for covered_row in cells.iter_mut().skip(row).take(height) {
                for covered in covered_row.iter_mut().skip(column).take(width) {
                    *covered = false;
                }
            }
            let x_min = i16::try_from(column * 100).expect("block x fits i16");
            let x_max = i16::try_from((column + width) * 100 - 20).expect("block x fits i16");
            let y_min = i16::try_from((7 - row - height) * 100).expect("block y fits i16");
            let y_max = i16::try_from((7 - row) * 100 - 20).expect("block y fits i16");
            points.extend_from_slice(&[
                (x_min, y_min),
                (x_max, y_min),
                (x_max, y_max),
                (x_min, y_max),
            ]);
            endpoints.push(u16::try_from(points.len() - 1).expect("point index fits u16"));
        }
    }
    if points.is_empty() {
        return Vec::new();
    }

    let x_min = points.iter().map(|point| point.0).min().expect("points");
    let y_min = points.iter().map(|point| point.1).min().expect("points");
    let x_max = points.iter().map(|point| point.0).max().expect("points");
    let y_max = points.iter().map(|point| point.1).max().expect("points");
    let mut glyph = Vec::new();
    push_i16(
        &mut glyph,
        i16::try_from(endpoints.len()).expect("contour count fits i16"),
    );
    for bound in [x_min, y_min, x_max, y_max] {
        push_i16(&mut glyph, bound);
    }
    for endpoint in endpoints {
        push_u16(&mut glyph, endpoint);
    }
    push_u16(&mut glyph, 0);
    glyph.extend(std::iter::repeat_n(0x01, points.len()));

    let mut prior_x = 0_i16;
    for (x, _) in &points {
        push_i16(
            &mut glyph,
            x.checked_sub(prior_x).expect("x delta fits i16"),
        );
        prior_x = *x;
    }
    let mut prior_y = 0_i16;
    for (_, y) in &points {
        push_i16(
            &mut glyph,
            y.checked_sub(prior_y).expect("y delta fits i16"),
        );
        prior_y = *y;
    }
    glyph
}

fn largest_rectangle(cells: &[[bool; 5]; 7], row: usize, column: usize) -> (usize, usize) {
    let mut best = (1_usize, 1_usize);
    for height in 1..=7 - row {
        for width in 1..=5 - column {
            let complete = cells
                .iter()
                .skip(row)
                .take(height)
                .all(|candidate| candidate.iter().skip(column).take(width).all(|cell| *cell));
            if !complete {
                continue;
            }
            let area = width * height;
            let best_area = best.0 * best.1;
            if area > best_area || (area == best_area && height > best.1) {
                best = (width, height);
            }
        }
    }
    best
}

fn ascii_cmap(glyph_ids: [u16; 95]) -> Vec<u8> {
    let segment_count = 2_usize;
    let format_length = 16 + segment_count * 8 + glyph_ids.len() * 2;
    let mut format = vec![0_u8; format_length];
    set_u16(&mut format, 0, 4);
    set_u16(
        &mut format,
        2,
        u16::try_from(format_length).expect("format length fits u16"),
    );
    set_u16(&mut format, 6, 4);
    set_u16(&mut format, 14, 0x007e);
    set_u16(&mut format, 16, 0xffff);
    set_u16(&mut format, 20, 0x0020);
    set_u16(&mut format, 22, 0xffff);
    set_i16(&mut format, 24, 0);
    set_i16(&mut format, 26, 1);
    set_u16(&mut format, 28, 4);
    set_u16(&mut format, 30, 0);
    for (index, glyph_id) in glyph_ids.into_iter().enumerate() {
        set_u16(&mut format, 32 + index * 2, glyph_id);
    }

    let mut cmap = vec![0_u8; 12];
    set_u16(&mut cmap, 2, 1);
    set_u16(&mut cmap, 4, 3);
    set_u16(&mut cmap, 6, 1);
    set_u32(&mut cmap, 8, 12);
    cmap.extend_from_slice(&format);
    cmap
}

fn pattern(byte: u8) -> [u8; 7] {
    match byte {
        b'A' => [14, 17, 17, 31, 17, 17, 17],
        b'B' => [30, 17, 17, 30, 17, 17, 30],
        b'C' => [14, 17, 16, 16, 16, 17, 14],
        b'D' => [30, 17, 17, 17, 17, 17, 30],
        b'E' => [31, 16, 16, 30, 16, 16, 31],
        b'F' => [31, 16, 16, 30, 16, 16, 16],
        b'G' => [14, 17, 16, 23, 17, 17, 15],
        b'H' => [17, 17, 17, 31, 17, 17, 17],
        b'I' => [31, 4, 4, 4, 4, 4, 31],
        b'J' => [7, 2, 2, 2, 18, 18, 12],
        b'K' => [17, 18, 20, 24, 20, 18, 17],
        b'L' => [16, 16, 16, 16, 16, 16, 31],
        b'M' => [17, 27, 21, 21, 17, 17, 17],
        b'N' => [17, 25, 21, 19, 17, 17, 17],
        b'O' => [14, 17, 17, 17, 17, 17, 14],
        b'P' => [30, 17, 17, 30, 16, 16, 16],
        b'Q' => [14, 17, 17, 17, 21, 18, 13],
        b'R' => [30, 17, 17, 30, 20, 18, 17],
        b'S' => [15, 16, 16, 14, 1, 1, 30],
        b'T' => [31, 4, 4, 4, 4, 4, 4],
        b'U' => [17, 17, 17, 17, 17, 17, 14],
        b'V' => [17, 17, 17, 17, 17, 10, 4],
        b'W' => [17, 17, 17, 21, 21, 21, 10],
        b'X' => [17, 17, 10, 4, 10, 17, 17],
        b'Y' => [17, 17, 10, 4, 4, 4, 4],
        b'Z' => [31, 1, 2, 4, 8, 16, 31],
        b'0' => [14, 17, 19, 21, 25, 17, 14],
        b'1' => [4, 12, 4, 4, 4, 4, 14],
        b'2' => [14, 17, 1, 2, 4, 8, 31],
        b'3' => [30, 1, 1, 14, 1, 1, 30],
        b'4' => [2, 6, 10, 18, 31, 2, 2],
        b'5' => [31, 16, 16, 30, 1, 1, 30],
        b'6' => [14, 16, 16, 30, 17, 17, 14],
        b'7' => [31, 1, 2, 4, 8, 8, 8],
        b'8' => [14, 17, 17, 14, 17, 17, 14],
        b'9' => [14, 17, 17, 15, 1, 1, 14],
        b'.' => [0, 0, 0, 0, 0, 12, 12],
        b',' => [0, 0, 0, 0, 0, 12, 8],
        b':' => [0, 12, 12, 0, 12, 12, 0],
        b'-' => [0, 0, 0, 31, 0, 0, 0],
        b'/' => [1, 1, 2, 4, 8, 16, 16],
        b'+' => [0, 4, 4, 31, 4, 4, 0],
        b'_' => [0, 0, 0, 0, 0, 0, 31],
        b'!' => [4, 4, 4, 4, 4, 0, 4],
        b'?' => [14, 17, 1, 2, 4, 0, 4],
        _ => [31, 17, 17, 17, 17, 17, 31],
    }
}

fn push_u16(bytes: &mut Vec<u8>, value: u16) {
    bytes.extend_from_slice(&value.to_be_bytes());
}

fn push_i16(bytes: &mut Vec<u8>, value: i16) {
    bytes.extend_from_slice(&value.to_be_bytes());
}

fn set_u16(bytes: &mut [u8], offset: usize, value: u16) {
    bytes[offset..offset + 2].copy_from_slice(&value.to_be_bytes());
}

fn set_i16(bytes: &mut [u8], offset: usize, value: i16) {
    bytes[offset..offset + 2].copy_from_slice(&value.to_be_bytes());
}

fn set_u32(bytes: &mut [u8], offset: usize, value: u32) {
    bytes[offset..offset + 4].copy_from_slice(&value.to_be_bytes());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn readable_preview_is_deterministic_and_has_two_pages() {
        let first = generate_readable_preview_pdf().expect("preview generation succeeds");
        let second = generate_readable_preview_pdf().expect("preview replay succeeds");
        assert_eq!(first, second);
        assert!(first.starts_with(b"%PDF-1.7\n"));
        assert!(first.windows(8).any(|window| window == b"/Count 2"));
        let title = b"(NAVIGATION WORKS)";
        assert!(first.windows(title.len()).any(|window| window == title));
        let marker = b"startxref\n";
        let start = first
            .windows(marker.len())
            .position(|window| window == marker)
            .expect("startxref exists")
            + marker.len();
        let end = first[start..]
            .iter()
            .position(|byte| *byte == b'\n')
            .expect("startxref line ends")
            + start;
        let xref = std::str::from_utf8(&first[start..end])
            .expect("startxref is ASCII")
            .parse::<usize>()
            .expect("startxref is numeric");
        assert_eq!(&first[xref..xref + 5], b"xref\n");
    }
}
