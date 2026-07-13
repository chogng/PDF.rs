#![forbid(unsafe_code)]
#![deny(missing_docs)]

//! Self-authored child process used to test the deadline- and byte-limited supervisor.

use std::env;
use std::fs;
use std::io::{self, Read, Write};
use std::process::{Command, ExitCode, Stdio};
use std::thread;
use std::time::Duration;

const REQUEST_MAGIC: &[u8; 8] = b"PRSBREQ2";
const RESPONSE_MAGIC: &[u8; 8] = b"PRSBOBS2";
const SCHEMA_VERSION: u16 = 2;
const REQUEST_HEADER_LEN: usize = 96;
const RESPONSE_HEADER_LEN: usize = 112;
const FIXTURE_BYTE_LIMIT: usize = 8 * 1024 * 1024;

struct Request {
    page: u32,
    width: u32,
    height: u32,
    source_hash: [u8; 32],
    descriptor_identity: [u8; 32],
}

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(()) => ExitCode::from(7),
    }
}

fn run() -> Result<(), ()> {
    let arguments: Vec<String> = env::args().skip(1).collect();
    let mode = arguments.first().map(String::as_str).ok_or(())?;
    match mode {
        "ok" => write_produced_response(read_request()?, 2, 0),
        "unsupported" => {
            let request = read_request()?;
            write_response(request, 0, [1, 1, 0, 0], b"", b"", b"[]", &[0; 4])
        }
        "channel-failed" => {
            let request = read_request()?;
            write_response(request, 0, [2, 0, 0, 0], b"", b"[]", b"[]", &[0; 4])
        }
        "emit" => {
            let parse_bytes = parse_count(arguments.get(1))?;
            let stderr_bytes = parse_count(arguments.get(2))?;
            write_repeated(&mut io::stderr().lock(), b'E', stderr_bytes)?;
            write_produced_response(read_request()?, parse_bytes, 0)
        }
        "hang" => {
            let _ = read_request()?;
            thread::sleep(Duration::from_secs(5));
            Ok(())
        }
        "inherit-pipes" => {
            let request = read_request()?;
            Command::new(env::current_exe().map_err(|_| ())?)
                .arg("sleep-only")
                .stdin(Stdio::null())
                .stdout(Stdio::inherit())
                .stderr(Stdio::inherit())
                .spawn()
                .map_err(|_| ())?;
            write_produced_response(request, 2, 0)
        }
        "sleep-only" => {
            thread::sleep(Duration::from_millis(600));
            Ok(())
        }
        "protocol-fail" => {
            let request = read_request()?;
            write_response(request, 1, [2; 4], b"", b"", b"", b"")
        }
        "nonzero" => {
            let _ = read_request()?;
            io::stderr()
                .write_all(b"%PDF-private-stderr-canary")
                .map_err(|_| ())?;
            Err(())
        }
        "malformed" => {
            let _ = read_request()?;
            io::stdout().write_all(b"not-a-frame").map_err(|_| ())
        }
        "wrong-page" => {
            let mut request = read_request()?;
            request.page = request.page.checked_add(1).ok_or(())?;
            write_produced_response(request, 2, 0)
        }
        "inspect" => {
            if env::var_os("PATH").is_some()
                || env::var("PDF_RS_ALLOWED").as_deref() != Ok("yes")
                || arguments.get(1).map(String::as_str) != Some("; $(touch sentinel) spaced")
            {
                return Err(());
            }
            write_produced_response(read_request()?, 2, 0)
        }
        "mark" => {
            let marker = arguments.get(1).ok_or(())?;
            fs::write(marker, b"spawned").map_err(|_| ())?;
            write_produced_response(read_request()?, 2, 0)
        }
        _ => Err(()),
    }
}

fn read_request() -> Result<Request, ()> {
    let mut input = io::stdin().lock();
    let mut header = [0_u8; REQUEST_HEADER_LEN];
    input.read_exact(&mut header).map_err(|_| ())?;
    if &header[..8] != REQUEST_MAGIC || read_u16(&header, 8)? != SCHEMA_VERSION {
        return Err(());
    }
    let pdf_length = usize::try_from(read_u64(&header, 24)?).map_err(|_| ())?;
    if pdf_length > FIXTURE_BYTE_LIMIT {
        return Err(());
    }
    let mut pdf = Vec::new();
    pdf.try_reserve_exact(pdf_length).map_err(|_| ())?;
    pdf.resize(pdf_length, 0);
    input.read_exact(&mut pdf).map_err(|_| ())?;
    let mut extra = [0_u8; 1];
    if input.read(&mut extra).map_err(|_| ())? != 0 {
        return Err(());
    }
    Ok(Request {
        page: read_u32(&header, 12)?,
        width: read_u32(&header, 16)?,
        height: read_u32(&header, 20)?,
        source_hash: header[32..64].try_into().map_err(|_| ())?,
        descriptor_identity: header[64..96].try_into().map_err(|_| ())?,
    })
}

fn write_produced_response(
    request: Request,
    parse_bytes: usize,
    stderr_bytes: usize,
) -> Result<(), ()> {
    if !(2..=FIXTURE_BYTE_LIMIT).contains(&parse_bytes) || stderr_bytes > FIXTURE_BYTE_LIMIT {
        return Err(());
    }
    write_repeated(&mut io::stderr().lock(), b'E', stderr_bytes)?;
    let parse = json_payload(parse_bytes)?;
    let rgba_length = rgba_length(request.width, request.height)?;
    let rgba = filled(rgba_length, 0)?;
    write_response(request, 0, [0; 4], &parse, b"[]", b"[]", &rgba)
}

fn write_response(
    request: Request,
    outcome: u16,
    statuses: [u8; 4],
    parse: &[u8],
    scene: &[u8],
    text: &[u8],
    rgba: &[u8],
) -> Result<(), ()> {
    let payload_length = parse
        .len()
        .checked_add(scene.len())
        .and_then(|value| value.checked_add(text.len()))
        .and_then(|value| value.checked_add(rgba.len()))
        .ok_or(())?;
    let capacity = RESPONSE_HEADER_LEN.checked_add(payload_length).ok_or(())?;
    let mut response = Vec::new();
    response.try_reserve_exact(capacity).map_err(|_| ())?;
    response.extend_from_slice(RESPONSE_MAGIC);
    response.extend_from_slice(&SCHEMA_VERSION.to_be_bytes());
    response.extend_from_slice(&outcome.to_be_bytes());
    response.extend_from_slice(&statuses);
    response.extend_from_slice(&u32::try_from(parse.len()).map_err(|_| ())?.to_be_bytes());
    response.extend_from_slice(&u32::try_from(scene.len()).map_err(|_| ())?.to_be_bytes());
    response.extend_from_slice(&u32::try_from(text.len()).map_err(|_| ())?.to_be_bytes());
    response.extend_from_slice(&request.page.to_be_bytes());
    response.extend_from_slice(&request.width.to_be_bytes());
    response.extend_from_slice(&request.height.to_be_bytes());
    response.extend_from_slice(&u64::try_from(rgba.len()).map_err(|_| ())?.to_be_bytes());
    response.extend_from_slice(&request.source_hash);
    response.extend_from_slice(&request.descriptor_identity);
    response.extend_from_slice(parse);
    response.extend_from_slice(scene);
    response.extend_from_slice(text);
    response.extend_from_slice(rgba);
    debug_assert_eq!(response.len(), capacity);
    let mut output = io::stdout().lock();
    output.write_all(&response).map_err(|_| ())?;
    output.flush().map_err(|_| ())
}

fn write_repeated(output: &mut impl Write, byte: u8, mut length: usize) -> Result<(), ()> {
    let block = [byte; 8 * 1024];
    while length != 0 {
        let written = length.min(block.len());
        output.write_all(&block[..written]).map_err(|_| ())?;
        length -= written;
    }
    output.flush().map_err(|_| ())
}

fn filled(length: usize, byte: u8) -> Result<Vec<u8>, ()> {
    let mut output = Vec::new();
    output.try_reserve_exact(length).map_err(|_| ())?;
    output.resize(length, byte);
    Ok(output)
}

fn json_payload(length: usize) -> Result<Vec<u8>, ()> {
    if length == 2 {
        return Ok(b"{}".to_vec());
    }
    let mut output = filled(length, b'P')?;
    output[0] = b'"';
    output[length - 1] = b'"';
    Ok(output)
}

fn rgba_length(width: u32, height: u32) -> Result<usize, ()> {
    let length = u64::from(width)
        .checked_mul(u64::from(height))
        .and_then(|value| value.checked_mul(4))
        .ok_or(())?;
    let length = usize::try_from(length).map_err(|_| ())?;
    if length == 0 || length > FIXTURE_BYTE_LIMIT {
        return Err(());
    }
    Ok(length)
}

fn parse_count(value: Option<&String>) -> Result<usize, ()> {
    let value = value.ok_or(())?.parse::<usize>().map_err(|_| ())?;
    if value > FIXTURE_BYTE_LIMIT {
        return Err(());
    }
    Ok(value)
}

fn read_u16(bytes: &[u8], offset: usize) -> Result<u16, ()> {
    let value: [u8; 2] = bytes
        .get(offset..offset + 2)
        .ok_or(())?
        .try_into()
        .map_err(|_| ())?;
    Ok(u16::from_be_bytes(value))
}

fn read_u32(bytes: &[u8], offset: usize) -> Result<u32, ()> {
    let value: [u8; 4] = bytes
        .get(offset..offset + 4)
        .ok_or(())?
        .try_into()
        .map_err(|_| ())?;
    Ok(u32::from_be_bytes(value))
}

fn read_u64(bytes: &[u8], offset: usize) -> Result<u64, ()> {
    let value: [u8; 8] = bytes
        .get(offset..offset + 8)
        .ok_or(())?
        .try_into()
        .map_err(|_| ())?;
    Ok(u64::from_be_bytes(value))
}
