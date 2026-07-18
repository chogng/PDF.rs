use std::io::Write;

use pdf_rs_viewer::{NativeDocument, NativeViewerErrorCode};

const REAL_WORLD_BOUNDARY_PADDING: usize = 20_000;

#[test]
fn minimal_traditional_pdf_still_opens() {
    let document =
        NativeDocument::open(traditional_pdf(0, None)).expect("minimal traditional PDF opens");
    assert_eq!(document.page_count(), 1);
}

#[test]
fn acquired_xref_representations_publish_page_counts() {
    for bytes in [xref_stream_pdf(true), incremental_pdf()] {
        let mut document =
            NativeDocument::open(bytes).expect("source-acquired xref representation opens");
        assert_eq!(document.page_count(), 1);
        let error = document
            .render_page(0, 100)
            .expect_err("acquired page rendering is the next compatibility boundary");
        assert_eq!(error.code(), NativeViewerErrorCode::Unsupported);
    }
}

#[test]
fn uncompressed_single_revision_xref_stream_reuses_strict_rendering() {
    let mut document = NativeDocument::open(xref_stream_pdf(false))
        .expect("traditional-equivalent xref stream opens through strict attestation");
    assert_eq!(document.page_count(), 1);
    let surface = document
        .render_page(0, 100)
        .expect("strict-attested blank page renders");
    assert_eq!((surface.width(), surface.height()), (100, 100));
}

#[test]
fn indirect_ext_gstate_applies_constant_alpha() {
    let mut document =
        NativeDocument::open(ext_gstate_pdf()).expect("proof-bound ExtGState PDF opens");
    let surface = document
        .render_page(0, 100)
        .expect("supported ExtGState alpha renders");
    let center = &surface.pixels()[((50 * 100 + 50) * 4)..][..4];
    assert_eq!(center[0], 255);
    assert!((126..=129).contains(&center[1]));
    assert!((126..=129).contains(&center[2]));
    assert_eq!(center[3], 255);
}

#[test]
fn oversized_valid_stream_boundary_remains_a_resource_limit() {
    let error = NativeDocument::open(traditional_pdf(REAL_WORLD_BOUNDARY_PADDING, None))
        .err()
        .expect("the fixed object-boundary budget rejects oversized padding");
    assert_eq!(error.code(), NativeViewerErrorCode::ResourceLimit);
}

fn ext_gstate_pdf() -> Vec<u8> {
    let mut pdf = b"%PDF-1.7\n%\x80\x81\x82\x83\n".to_vec();
    let mut offsets = Vec::new();
    append_object(
        &mut pdf,
        &mut offsets,
        1,
        b"<< /Type /Catalog /Pages 2 0 R >>",
    );
    append_object(
        &mut pdf,
        &mut offsets,
        2,
        b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>",
    );
    append_object(
        &mut pdf,
        &mut offsets,
        3,
        b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 100 100] /Resources << /ExtGState << /Fade 5 0 R >> >> /Contents 4 0 R >>",
    );
    append_object(
        &mut pdf,
        &mut offsets,
        4,
        b"<< /Length 34 >>\nstream\n/Fade gs 1 0 0 rg 0 0 100 100 re f\nendstream",
    );
    append_object(&mut pdf, &mut offsets, 5, b"<< /ca 0.5 /CA 1 >>");

    let xref_offset = pdf.len();
    pdf.extend_from_slice(b"xref\n0 6\n0000000000 65535 f \n");
    for offset in offsets {
        writeln!(pdf, "{offset:010} 00000 n ").expect("xref row");
    }
    write!(
        pdf,
        "trailer\n<< /Size 6 /Root 1 0 R >>\nstartxref\n{xref_offset}\n%%EOF\n"
    )
    .expect("traditional trailer");
    pdf
}

fn traditional_pdf(boundary_padding: usize, previous: Option<u64>) -> Vec<u8> {
    let mut pdf = b"%PDF-1.7\n%\x80\x81\x82\x83\n".to_vec();
    let mut offsets = Vec::new();
    append_object(
        &mut pdf,
        &mut offsets,
        1,
        b"<< /Type /Catalog /Pages 2 0 R >>",
    );
    append_object(
        &mut pdf,
        &mut offsets,
        2,
        b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>",
    );
    append_object(
        &mut pdf,
        &mut offsets,
        3,
        b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 100 100] /Resources <<>> /Contents 4 0 R >>",
    );

    offsets.push(pdf.len());
    pdf.extend_from_slice(b"4 0 obj\n<< /Length 0 >>\nstream\n\nendstream\n");
    pdf.resize(pdf.len() + boundary_padding, b' ');
    pdf.extend_from_slice(b"endobj\n");

    let xref_offset = pdf.len();
    pdf.extend_from_slice(b"xref\n0 5\n0000000000 65535 f \n");
    for offset in offsets {
        writeln!(pdf, "{offset:010} 00000 n ").expect("xref row");
    }
    pdf.extend_from_slice(b"trailer\n<< /Size 5 /Root 1 0 R");
    if let Some(previous) = previous {
        write!(pdf, " /Prev {previous}").expect("incremental trailer");
    }
    write!(pdf, " >>\nstartxref\n{xref_offset}\n%%EOF\n").expect("traditional trailer");
    pdf
}

fn xref_stream_pdf(include_self_row: bool) -> Vec<u8> {
    let mut pdf = b"%PDF-1.7\n%\x80\x81\x82\x83\n".to_vec();
    let mut offsets = Vec::new();
    append_object(
        &mut pdf,
        &mut offsets,
        1,
        b"<< /Type /Catalog /Pages 2 0 R >>",
    );
    append_object(
        &mut pdf,
        &mut offsets,
        2,
        b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>",
    );
    append_object(
        &mut pdf,
        &mut offsets,
        3,
        b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 100 100] /Resources <<>> /Contents 4 0 R >>",
    );
    append_object(
        &mut pdf,
        &mut offsets,
        4,
        b"<< /Length 0 >>\nstream\n\nendstream",
    );
    let xref_offset = pdf.len();
    if include_self_row {
        offsets.push(xref_offset);
    }
    let mut payload = Vec::new();
    append_xref_stream_entry(&mut payload, 0, 0, u16::MAX);
    for offset in offsets {
        append_xref_stream_entry(
            &mut payload,
            1,
            u32::try_from(offset).expect("fixture offset fits u32"),
            0,
        );
    }
    write!(
        pdf,
        "5 0 obj\n<< /Type /XRef /Size 6 /Root 1 0 R /W [1 4 2]{} /Length {} >>\nstream\n",
        if include_self_row {
            ""
        } else {
            " /Index [0 5]"
        },
        payload.len()
    )
    .expect("xref stream fixture");
    pdf.extend_from_slice(&payload);
    write!(
        pdf,
        "\nendstream\nendobj\nstartxref\n{xref_offset}\n%%EOF\n"
    )
    .expect("xref stream trailer");
    pdf
}

fn incremental_pdf() -> Vec<u8> {
    let mut pdf = traditional_pdf(0, None);
    let marker = b"startxref\n";
    let marker_start = pdf
        .windows(marker.len())
        .rposition(|window| window == marker)
        .expect("base startxref");
    let value_start = marker_start + marker.len();
    let value_end = pdf[value_start..]
        .iter()
        .position(|byte| *byte == b'\n')
        .map(|offset| value_start + offset)
        .expect("base startxref end");
    let previous = std::str::from_utf8(&pdf[value_start..value_end])
        .expect("ASCII base xref")
        .parse::<u64>()
        .expect("numeric base xref");
    let object_offset = pdf.len();
    pdf.extend_from_slice(b"5 0 obj\n42\nendobj\n");
    let xref_offset = pdf.len();
    write!(
        pdf,
        "xref\n5 1\n{object_offset:010} 00000 n \n\
         trailer\n<< /Size 6 /Root 1 0 R /Prev {previous} >>\n\
         startxref\n{xref_offset}\n%%EOF\n"
    )
    .expect("incremental revision");
    pdf
}

fn append_xref_stream_entry(payload: &mut Vec<u8>, kind: u8, field_two: u32, field_three: u16) {
    payload.push(kind);
    payload.extend_from_slice(&field_two.to_be_bytes());
    payload.extend_from_slice(&field_three.to_be_bytes());
}

fn append_object(pdf: &mut Vec<u8>, offsets: &mut Vec<usize>, number: u32, body: &[u8]) {
    offsets.push(pdf.len());
    writeln!(pdf, "{number} 0 obj").expect("object header");
    pdf.extend_from_slice(body);
    pdf.extend_from_slice(b"\nendobj\n");
}
