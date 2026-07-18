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
fn unsupported_xref_representations_remain_capability_outcomes() {
    let xref_stream = NativeDocument::open(xref_stream_pdf())
        .err()
        .expect("xref stream stays outside the strict viewer profile");
    assert_eq!(xref_stream.code(), NativeViewerErrorCode::Unsupported);

    let incremental = NativeDocument::open(traditional_pdf(0, Some(9)))
        .err()
        .expect("incremental revision stays outside the strict viewer profile");
    assert_eq!(incremental.code(), NativeViewerErrorCode::Unsupported);
}

#[test]
fn oversized_valid_stream_boundary_remains_a_resource_limit() {
    let error = NativeDocument::open(traditional_pdf(REAL_WORLD_BOUNDARY_PADDING, None))
        .err()
        .expect("the fixed object-boundary budget rejects oversized padding");
    assert_eq!(error.code(), NativeViewerErrorCode::ResourceLimit);
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

fn xref_stream_pdf() -> Vec<u8> {
    let mut pdf = b"%PDF-1.7\n%\x80\x81\x82\x83\n".to_vec();
    let xref_offset = pdf.len();
    write!(
        pdf,
        "4 0 obj\n<< /Type /XRef /Size 5 /Root 1 0 R /W [1 2 1] /Length 0 >>\n\
         stream\n\nendstream\nendobj\nstartxref\n{xref_offset}\n%%EOF\n"
    )
    .expect("xref stream fixture");
    pdf
}

fn append_object(pdf: &mut Vec<u8>, offsets: &mut Vec<usize>, number: u32, body: &[u8]) {
    offsets.push(pdf.len());
    writeln!(pdf, "{number} 0 obj").expect("object header");
    pdf.extend_from_slice(body);
    pdf.extend_from_slice(b"\nendobj\n");
}
