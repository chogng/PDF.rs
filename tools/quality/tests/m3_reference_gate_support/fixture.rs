//! Deterministic, analytic PDF fixtures for the integrated M3 reference gate.
//!
//! The builder deliberately emits a traditional xref table and never consults
//! the filesystem or a platform font service. Object numbers are fixed so the
//! strict acquisition trace stays comparable across debug and release builds.

pub(super) const CATALOG_OBJECT_NUMBER: u32 = 1;
pub(super) const PAGES_OBJECT_NUMBER: u32 = 2;
pub(super) const PAGE_OBJECT_NUMBER: u32 = 3;
pub(super) const CONTENT_OBJECT_NUMBER: u32 = 4;
pub(super) const IMAGE_OBJECT_NUMBER: u32 = 5;
pub(super) const FONT_OBJECT_NUMBER: u32 = 8;
pub(super) const FONT_DESCRIPTOR_OBJECT_NUMBER: u32 = 9;
pub(super) const FONT_PROGRAM_OBJECT_NUMBER: u32 = 10;

pub(super) const IMAGE_RGB: [u8; 6] = [255, 0, 0, 0, 0, 255];
pub(super) const FOUNDATIONAL_ASCII_A_WIDTH: u32 = 500;

const XREF_SIZE: u32 = FONT_PROGRAM_OBJECT_NUMBER + 1;
const PDF_HEADER: &[u8] = b"%PDF-1.7\n";
const CATALOG_OBJECT: &[u8] = b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n";
const PAGES_OBJECT: &[u8] = b"2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 >>\nendobj\n";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct ImageSpec {
    pub(super) interpolate: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct FixtureSpec {
    pub(super) content: &'static [u8],
    pub(super) image: Option<ImageSpec>,
    pub(super) font: bool,
    pub(super) invalid_startxref: bool,
    pub(super) salt: u8,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct Fixture {
    pub(super) bytes: Vec<u8>,
    /// The byte offset where the real `xref` token begins.
    pub(super) startxref: u64,
    /// The value written to the trailer. It differs from `startxref` only for
    /// the deliberately malformed strict-open fixture.
    pub(super) advertised_startxref: u64,
    /// Source-identity salt selected by the caller. It does not affect bytes.
    pub(super) salt: u8,
}

pub(super) fn build_fixture(spec: FixtureSpec) -> Fixture {
    let resources = page_resources(spec.image.is_some(), spec.font);
    let mut bodies = vec![
        (CATALOG_OBJECT_NUMBER, CATALOG_OBJECT.to_vec()),
        (PAGES_OBJECT_NUMBER, PAGES_OBJECT.to_vec()),
        (PAGE_OBJECT_NUMBER, page_object(resources)),
        (CONTENT_OBJECT_NUMBER, content_object(spec.content)),
    ];

    if let Some(image) = spec.image {
        bodies.push((IMAGE_OBJECT_NUMBER, image_object(image)));
    }
    if spec.font {
        let program = foundational_font();
        bodies.extend(font_objects(&program));
    }
    bodies.sort_unstable_by_key(|(number, _)| *number);

    let mut bytes = PDF_HEADER.to_vec();
    let mut offsets = [None; XREF_SIZE as usize];
    for (number, body) in bodies {
        let index = usize::try_from(number).expect("fixture object number fits usize");
        assert!(
            index < offsets.len(),
            "fixture object number {number} exceeds the fixed xref"
        );
        assert!(
            offsets[index].is_none(),
            "duplicate fixture object number {number}"
        );
        let offset = u64::try_from(bytes.len()).expect("fixture offset fits u64");
        assert!(
            offset <= 9_999_999_999,
            "traditional xref offsets must fit ten decimal digits"
        );
        offsets[index] = Some(offset);
        bytes.extend_from_slice(&body);
    }

    let startxref = u64::try_from(bytes.len()).expect("fixture xref offset fits u64");
    bytes.extend_from_slice(format!("xref\n0 {XREF_SIZE}\n").as_bytes());
    for number in 0..XREF_SIZE {
        let row = if number == 0 {
            "0000000000 65535 f \n".to_owned()
        } else if let Some(offset) = offsets[number as usize] {
            format!("{offset:010} 00000 n \n")
        } else {
            "0000000000 00000 f \n".to_owned()
        };
        bytes.extend_from_slice(row.as_bytes());
    }

    let advertised_startxref = if spec.invalid_startxref {
        startxref
            .checked_add(1)
            .expect("fixture xref offset can be corrupted by one")
    } else {
        startxref
    };
    bytes.extend_from_slice(
        format!(
            "trailer\n<< /Size {XREF_SIZE} /Root {CATALOG_OBJECT_NUMBER} 0 R >>\n\
             startxref\n{advertised_startxref}\n%%EOF\n"
        )
        .as_bytes(),
    );

    Fixture {
        bytes,
        startxref,
        advertised_startxref,
        salt: spec.salt,
    }
}

fn page_resources(image: bool, font: bool) -> &'static [u8] {
    match (image, font) {
        (false, false) => b"<< >>",
        (true, false) => b"<< /XObject << /Im0 5 0 R >> >>",
        (false, true) => b"<< /Font << /F0 8 0 R >> >>",
        (true, true) => b"<< /XObject << /Im0 5 0 R >> /Font << /F0 8 0 R >> >>",
    }
}

fn page_object(resources: &[u8]) -> Vec<u8> {
    let mut object = format!(
        "{PAGE_OBJECT_NUMBER} 0 obj\n\
         << /Type /Page /Parent {PAGES_OBJECT_NUMBER} 0 R \
         /MediaBox [0 0 100 100] /Resources "
    )
    .into_bytes();
    object.extend_from_slice(resources);
    object.extend_from_slice(
        format!(" /Contents {CONTENT_OBJECT_NUMBER} 0 R >>\nendobj\n").as_bytes(),
    );
    object
}

fn content_object(content: &[u8]) -> Vec<u8> {
    let mut object = format!(
        "{CONTENT_OBJECT_NUMBER} 0 obj\n<< /Length {} >>\nstream\n",
        content.len()
    )
    .into_bytes();
    object.extend_from_slice(content);
    object.extend_from_slice(b"\nendstream\nendobj\n");
    object
}

fn image_object(spec: ImageSpec) -> Vec<u8> {
    let interpolate = if spec.interpolate {
        " /Interpolate true"
    } else {
        ""
    };
    let mut object = format!(
        "{IMAGE_OBJECT_NUMBER} 0 obj\n\
         << /Type /XObject /Subtype /Image /Width 2 /Height 1 \
         /ColorSpace /DeviceRGB /BitsPerComponent 8 /Length {}{interpolate} >>\n\
         stream\n",
        IMAGE_RGB.len()
    )
    .into_bytes();
    object.extend_from_slice(&IMAGE_RGB);
    object.extend_from_slice(b"\nendstream\nendobj\n");
    object
}

fn font_objects(program: &[u8]) -> [(u32, Vec<u8>); 3] {
    let font = format!(
        "{FONT_OBJECT_NUMBER} 0 obj\n\
         << /Type /Font /Subtype /TrueType /Encoding /WinAnsiEncoding \
         /FirstChar 32 /LastChar 126 /Widths [{}] \
         /FontDescriptor {FONT_DESCRIPTOR_OBJECT_NUMBER} 0 R >>\n\
         endobj\n",
        font_widths(FOUNDATIONAL_ASCII_A_WIDTH)
    )
    .into_bytes();
    let descriptor = format!(
        "{FONT_DESCRIPTOR_OBJECT_NUMBER} 0 obj\n\
         << /Type /FontDescriptor /FontFile2 {FONT_PROGRAM_OBJECT_NUMBER} 0 R >>\n\
         endobj\n"
    )
    .into_bytes();
    let mut program_object = format!(
        "{FONT_PROGRAM_OBJECT_NUMBER} 0 obj\n\
         << /Length {} /Length1 {} >>\nstream\n",
        program.len(),
        program.len()
    )
    .into_bytes();
    program_object.extend_from_slice(program);
    program_object.extend_from_slice(b"\nendstream\nendobj\n");

    [
        (FONT_OBJECT_NUMBER, font),
        (FONT_DESCRIPTOR_OBJECT_NUMBER, descriptor),
        (FONT_PROGRAM_OBJECT_NUMBER, program_object),
    ]
}

fn font_widths(ascii_a_width: u32) -> String {
    (0x20_u8..=0x7e)
        .map(|byte| if byte == b'A' { ascii_a_width } else { 600 })
        .map(|width| width.to_string())
        .collect::<Vec<_>>()
        .join(" ")
}

/// A tiny, analytic TrueType program. Printable ASCII maps to the first
/// triangle glyph, while two additional glyphs keep compound-glyph parsing
/// covered without relying on a host font.
pub(super) fn foundational_font() -> Vec<u8> {
    build_font(vec![
        Vec::new(),
        triangle_glyph(),
        triangle_glyph(),
        compound_glyph(&[(1, 10, 20), (2, -10, -20)]),
    ])
}

fn build_font(glyphs: Vec<Vec<u8>>) -> Vec<u8> {
    assert!(!glyphs.is_empty());
    assert!(glyphs.len() <= usize::from(u16::MAX));
    let glyph_count = glyphs.len() as u16;

    let mut head = vec![0_u8; 54];
    set_u32(&mut head, 0, 0x0001_0000);
    set_u32(&mut head, 12, 0x5f0f_3cf5);
    set_u16(&mut head, 18, 1_000);
    set_i16(&mut head, 50, 1);
    set_i16(&mut head, 52, 0);

    let mut hhea = vec![0_u8; 36];
    set_u32(&mut hhea, 0, 0x0001_0000);
    set_u16(&mut hhea, 34, glyph_count);

    let mut maxp = vec![0_u8; 32];
    set_u32(&mut maxp, 0, 0x0001_0000);
    set_u16(&mut maxp, 4, glyph_count);

    let mut loca = Vec::with_capacity((glyphs.len() + 1) * 4);
    let mut glyf = Vec::new();
    loca.extend_from_slice(&0_u32.to_be_bytes());
    for glyph in glyphs {
        glyf.extend_from_slice(&glyph);
        loca.extend_from_slice(&(glyf.len() as u32).to_be_bytes());
    }

    let mut hmtx = Vec::with_capacity(usize::from(glyph_count) * 4);
    for glyph in 0..glyph_count {
        hmtx.extend_from_slice(&(500_u16 + glyph).to_be_bytes());
        hmtx.extend_from_slice(&0_i16.to_be_bytes());
    }
    let cmap = ascii_cmap(if glyph_count > 1 { 1 } else { 0 });

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
    set_u16(&mut font, 4, tables.len() as u16);
    for (index, (tag, table)) in tables.into_iter().enumerate() {
        while !font.len().is_multiple_of(4) {
            font.push(0);
        }
        let offset = font.len();
        font.extend_from_slice(&table);
        let record = 12 + index * 16;
        font[record..record + 4].copy_from_slice(&tag);
        set_u32(&mut font, record + 8, offset as u32);
        set_u32(&mut font, record + 12, table.len() as u32);
    }
    font
}

fn triangle_glyph() -> Vec<u8> {
    let mut glyph = Vec::new();
    push_i16(&mut glyph, 1);
    push_i16(&mut glyph, 0);
    push_i16(&mut glyph, 0);
    push_i16(&mut glyph, 100);
    push_i16(&mut glyph, 100);
    push_u16(&mut glyph, 2);
    push_u16(&mut glyph, 0);
    glyph.extend_from_slice(&[0x01, 0x01, 0x01]);
    for delta in [0_i16, 100, -100] {
        push_i16(&mut glyph, delta);
    }
    for delta in [0_i16, 0, 100] {
        push_i16(&mut glyph, delta);
    }
    glyph
}

fn compound_glyph(components: &[(u16, i16, i16)]) -> Vec<u8> {
    assert!(!components.is_empty());
    let mut x_min = i16::MAX;
    let mut y_min = i16::MAX;
    let mut x_max = i16::MIN;
    let mut y_max = i16::MIN;
    for (_, x, y) in components {
        x_min = x_min.min(*x);
        y_min = y_min.min(*y);
        x_max = x_max.max(x.saturating_add(100));
        y_max = y_max.max(y.saturating_add(100));
    }

    let mut glyph = Vec::new();
    push_i16(&mut glyph, -1);
    push_i16(&mut glyph, x_min);
    push_i16(&mut glyph, y_min);
    push_i16(&mut glyph, x_max);
    push_i16(&mut glyph, y_max);
    for (index, (glyph_id, x, y)) in components.iter().enumerate() {
        let mut flags = 0x0001 | 0x0002;
        if index + 1 != components.len() {
            flags |= 0x0020;
        }
        push_u16(&mut glyph, flags);
        push_u16(&mut glyph, *glyph_id);
        push_i16(&mut glyph, *x);
        push_i16(&mut glyph, *y);
    }
    glyph
}

fn ascii_cmap(glyph_id: u16) -> Vec<u8> {
    let segment_count = 2_usize;
    let glyph_count = 95_usize;
    let format_length = 16 + segment_count * 8 + glyph_count * 2;
    let mut format = vec![0_u8; format_length];
    set_u16(&mut format, 0, 4);
    set_u16(&mut format, 2, format_length as u16);
    set_u16(&mut format, 6, (segment_count * 2) as u16);
    set_u16(&mut format, 14, 0x007e);
    set_u16(&mut format, 16, 0xffff);
    set_u16(&mut format, 20, 0x0020);
    set_u16(&mut format, 22, 0xffff);
    set_i16(&mut format, 24, 0);
    set_i16(&mut format, 26, 1);
    set_u16(&mut format, 28, 4);
    set_u16(&mut format, 30, 0);
    for index in 0..glyph_count {
        set_u16(&mut format, 32 + index * 2, glyph_id);
    }

    let mut cmap = vec![0_u8; 12];
    set_u16(&mut cmap, 0, 0);
    set_u16(&mut cmap, 2, 1);
    set_u16(&mut cmap, 4, 3);
    set_u16(&mut cmap, 6, 1);
    set_u32(&mut cmap, 8, 12);
    cmap.extend_from_slice(&format);
    cmap
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

    const CONTENT: &[u8] = b"0 0 m 2 0 l 2 1 l h f";

    #[test]
    fn fixture_is_deterministic_and_startxref_names_the_real_xref() {
        let spec = FixtureSpec {
            content: CONTENT,
            image: Some(ImageSpec { interpolate: false }),
            font: true,
            invalid_startxref: false,
            salt: 31,
        };
        let first = build_fixture(spec);
        let second = build_fixture(spec);

        assert_eq!(first, second);
        assert_eq!(
            &first.bytes[first.startxref as usize..first.startxref as usize + 5],
            b"xref\n"
        );
        assert_eq!(first.startxref, first.advertised_startxref);
        assert!(
            first
                .bytes
                .windows(IMAGE_RGB.len())
                .any(|bytes| bytes == IMAGE_RGB)
        );
        assert!(first.bytes.windows(4).any(|bytes| bytes == b"cmap"));
    }

    #[test]
    fn invalid_startxref_corrupts_only_the_advertised_anchor() {
        let valid = build_fixture(FixtureSpec {
            content: CONTENT,
            image: None,
            font: false,
            invalid_startxref: false,
            salt: 32,
        });
        let invalid = build_fixture(FixtureSpec {
            invalid_startxref: true,
            ..FixtureSpec {
                content: CONTENT,
                image: None,
                font: false,
                invalid_startxref: false,
                salt: 32,
            }
        });

        assert_eq!(valid.startxref, invalid.startxref);
        assert_eq!(invalid.advertised_startxref, invalid.startxref + 1);
        assert_eq!(
            &invalid.bytes[invalid.startxref as usize..invalid.startxref as usize + 5],
            b"xref\n"
        );
    }
}
