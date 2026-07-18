//! Length-bounded local stdio bridge from Electron main to PDF.rs Native viewer.

use std::collections::BTreeMap;
use std::fs;
use std::io::{self, BufRead, BufReader, BufWriter, Write};

use pdf_rs_viewer::{NativeDocument, NativeViewerErrorCode};

const MAX_COMMAND_BYTES: usize = 16 * 1024;
const MAX_PATH_BYTES: usize = 4 * 1024;

fn main() {
    if std::env::args().nth(1).as_deref() != Some("--stdio") {
        std::process::exit(64);
    }
    if run_stdio().is_err() {
        std::process::exit(70);
    }
}

fn run_stdio() -> io::Result<()> {
    let stdin = io::stdin();
    let stdout = io::stdout();
    let mut input = BufReader::new(stdin.lock());
    let mut output = BufWriter::new(stdout.lock());
    let mut documents = BTreeMap::<u64, NativeDocument>::new();
    let mut next_document = 1_u64;
    let mut line = Vec::new();
    loop {
        line.clear();
        let read = input.read_until(b'\n', &mut line)?;
        if read == 0 {
            break;
        }
        if line.len() > MAX_COMMAND_BYTES || line.last() != Some(&b'\n') {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "invalid command frame",
            ));
        }
        line.pop();
        let command = match std::str::from_utf8(&line) {
            Ok(command) => command,
            Err(_) => {
                write_error(&mut output, 0, "invalid-command")?;
                continue;
            }
        };
        let mut fields = command.split(' ');
        let method = fields.next().unwrap_or_default();
        let request = match parse_u64(fields.next()) {
            Some(request) if request > 0 => request,
            _ => {
                write_error(&mut output, 0, "invalid-command")?;
                continue;
            }
        };
        match method {
            "OPEN" => {
                let Some(path) = fields.next().and_then(decode_path) else {
                    write_error(&mut output, request, "invalid-path")?;
                    continue;
                };
                if fields.next().is_some() {
                    write_error(&mut output, request, "invalid-command")?;
                    continue;
                }
                let bytes = match fs::read(path) {
                    Ok(bytes) => bytes,
                    Err(_) => {
                        write_error(&mut output, request, "source")?;
                        continue;
                    }
                };
                let document = match NativeDocument::open(bytes) {
                    Ok(document) => document,
                    Err(error) => {
                        write_error(&mut output, request, error_code(error.code()))?;
                        continue;
                    }
                };
                let document_id = next_document;
                next_document = next_document.checked_add(1).ok_or_else(|| {
                    io::Error::new(io::ErrorKind::OutOfMemory, "document id exhausted")
                })?;
                let pages = document.page_count();
                documents.insert(document_id, document);
                writeln!(output, "OPENED {request} {document_id} {pages}")?;
                output.flush()?;
            }
            "RENDER" => {
                let Some(document_id) = parse_u64(fields.next()) else {
                    write_error(&mut output, request, "invalid-command")?;
                    continue;
                };
                let Some(page) = parse_u32(fields.next()) else {
                    write_error(&mut output, request, "invalid-command")?;
                    continue;
                };
                let Some(width) = parse_u32(fields.next()) else {
                    write_error(&mut output, request, "invalid-command")?;
                    continue;
                };
                if fields.next().is_some() {
                    write_error(&mut output, request, "invalid-command")?;
                    continue;
                }
                let Some(document) = documents.get_mut(&document_id) else {
                    write_error(&mut output, request, "unknown-document")?;
                    continue;
                };
                let surface = match document.render_page(page, width) {
                    Ok(surface) => surface,
                    Err(error) => {
                        write_error(&mut output, request, error_code(error.code()))?;
                        continue;
                    }
                };
                writeln!(
                    output,
                    "SURFACE {request} {document_id} {} {} {} {} {}",
                    surface.page_index(),
                    surface.width(),
                    surface.height(),
                    surface.stride(),
                    surface.pixels().len()
                )?;
                output.write_all(surface.pixels())?;
                output.write_all(b"\n")?;
                output.flush()?;
            }
            "CLOSE" => {
                let Some(document_id) = parse_u64(fields.next()) else {
                    write_error(&mut output, request, "invalid-command")?;
                    continue;
                };
                if fields.next().is_some() || documents.remove(&document_id).is_none() {
                    write_error(&mut output, request, "unknown-document")?;
                    continue;
                }
                writeln!(output, "CLOSED {request} {document_id}")?;
                output.flush()?;
            }
            "SHUTDOWN" => {
                if fields.next().is_some() {
                    write_error(&mut output, request, "invalid-command")?;
                    continue;
                }
                documents.clear();
                writeln!(output, "BYE {request}")?;
                output.flush()?;
                return Ok(());
            }
            _ => write_error(&mut output, request, "invalid-command")?,
        }
    }
    Ok(())
}

fn parse_u64(value: Option<&str>) -> Option<u64> {
    value?.parse().ok()
}

fn parse_u32(value: Option<&str>) -> Option<u32> {
    value?.parse().ok()
}

fn decode_path(hex: &str) -> Option<String> {
    if hex.is_empty() || hex.len() > MAX_PATH_BYTES.checked_mul(2)? || !hex.len().is_multiple_of(2) {
        return None;
    }
    let mut bytes = Vec::new();
    bytes.try_reserve_exact(hex.len() / 2).ok()?;
    for pair in hex.as_bytes().chunks_exact(2) {
        let upper = decode_nibble(pair[0])?;
        let lower = decode_nibble(pair[1])?;
        bytes.push((upper << 4) | lower);
    }
    String::from_utf8(bytes).ok()
}

fn decode_nibble(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        _ => None,
    }
}

fn write_error(output: &mut impl Write, request: u64, code: &str) -> io::Result<()> {
    writeln!(output, "ERROR {request} {code}")?;
    output.flush()
}

fn error_code(code: NativeViewerErrorCode) -> &'static str {
    match code {
        NativeViewerErrorCode::InvalidInput => "invalid-input",
        NativeViewerErrorCode::Source => "source",
        NativeViewerErrorCode::Document => "document",
        NativeViewerErrorCode::Content => "content",
        NativeViewerErrorCode::Unsupported => "unsupported",
        NativeViewerErrorCode::Render => "render",
        NativeViewerErrorCode::ResourceLimit => "resource-limit",
        NativeViewerErrorCode::Internal => "internal",
    }
}
