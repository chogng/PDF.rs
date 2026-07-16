use std::ops::Range;

pub fn foundational_font() -> Vec<u8> {
    build_font(vec![
        Vec::new(),
        triangle_glyph(),
        triangle_glyph(),
        compound_glyph(&[(1, 10, 20), (2, -10, -20)]),
    ])
}

pub fn build_font(glyphs: Vec<Vec<u8>>) -> Vec<u8> {
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

pub fn triangle_glyph() -> Vec<u8> {
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

pub fn quadratic_glyph() -> Vec<u8> {
    let mut glyph = Vec::new();
    push_i16(&mut glyph, 1);
    push_i16(&mut glyph, 0);
    push_i16(&mut glyph, 0);
    push_i16(&mut glyph, 100);
    push_i16(&mut glyph, 100);
    push_u16(&mut glyph, 2);
    push_u16(&mut glyph, 0);
    glyph.extend_from_slice(&[0x00, 0x00, 0x01]);
    for delta in [0_i16, 100, 0] {
        push_i16(&mut glyph, delta);
    }
    for delta in [0_i16, 0, 100] {
        push_i16(&mut glyph, delta);
    }
    glyph
}

pub fn contour_glyph(on_curve: &[bool]) -> Vec<u8> {
    assert!(!on_curve.is_empty());
    assert!(on_curve.len() <= usize::from(u16::MAX));
    let coordinates = (0..on_curve.len())
        .map(|index| {
            (
                (index as i16) * 10,
                if index.is_multiple_of(2) { 0 } else { 10 },
            )
        })
        .collect::<Vec<_>>();
    let mut glyph = Vec::new();
    push_i16(&mut glyph, 1);
    push_i16(&mut glyph, 0);
    push_i16(&mut glyph, 0);
    push_i16(&mut glyph, coordinates.last().unwrap().0);
    push_i16(&mut glyph, if coordinates.len() == 1 { 0 } else { 10 });
    push_u16(&mut glyph, (on_curve.len() - 1) as u16);
    push_u16(&mut glyph, 0);
    glyph.extend(on_curve.iter().map(|on| u8::from(*on)));
    let mut prior_x = 0_i16;
    for (x, _) in &coordinates {
        push_i16(&mut glyph, *x - prior_x);
        prior_x = *x;
    }
    let mut prior_y = 0_i16;
    for (_, y) in &coordinates {
        push_i16(&mut glyph, *y - prior_y);
        prior_y = *y;
    }
    glyph
}

pub fn compound_glyph(components: &[(u16, i16, i16)]) -> Vec<u8> {
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

pub fn byte_compound_glyph(glyph_id: u16, x: i8, y: i8) -> Vec<u8> {
    let mut glyph = Vec::new();
    push_i16(&mut glyph, -1);
    push_i16(&mut glyph, i16::from(x));
    push_i16(&mut glyph, i16::from(y));
    push_i16(&mut glyph, i16::from(x) + 100);
    push_i16(&mut glyph, i16::from(y) + 100);
    push_u16(&mut glyph, 0x0002);
    push_u16(&mut glyph, glyph_id);
    glyph.push(x as u8);
    glyph.push(y as u8);
    glyph
}

pub fn table_range(font: &[u8], tag: &[u8; 4]) -> Range<usize> {
    let count = usize::from(u16::from_be_bytes(font[4..6].try_into().unwrap()));
    for index in 0..count {
        let record = 12 + index * 16;
        if &font[record..record + 4] == tag {
            let offset = u32::from_be_bytes(font[record + 8..record + 12].try_into().unwrap());
            let length = u32::from_be_bytes(font[record + 12..record + 16].try_into().unwrap());
            let start = offset as usize;
            return start..start + length as usize;
        }
    }
    panic!("missing fixture table {tag:?}");
}

pub fn glyph_range(font: &[u8], glyph_id: u16) -> Range<usize> {
    let loca = table_range(font, b"loca");
    let glyf = table_range(font, b"glyf");
    let index = usize::from(glyph_id);
    let start = u32::from_be_bytes(
        font[loca.start + index * 4..loca.start + index * 4 + 4]
            .try_into()
            .unwrap(),
    ) as usize;
    let end = u32::from_be_bytes(
        font[loca.start + (index + 1) * 4..loca.start + (index + 1) * 4 + 4]
            .try_into()
            .unwrap(),
    ) as usize;
    glyf.start + start..glyf.start + end
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

pub fn set_u16(bytes: &mut [u8], offset: usize, value: u16) {
    bytes[offset..offset + 2].copy_from_slice(&value.to_be_bytes());
}

pub fn set_i16(bytes: &mut [u8], offset: usize, value: i16) {
    bytes[offset..offset + 2].copy_from_slice(&value.to_be_bytes());
}

pub fn set_u32(bytes: &mut [u8], offset: usize, value: u32) {
    bytes[offset..offset + 4].copy_from_slice(&value.to_be_bytes());
}
