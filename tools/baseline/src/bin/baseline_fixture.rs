#![forbid(unsafe_code)]
#![deny(missing_docs)]

//! Self-authored child process used to test the deadline- and byte-limited supervisor.

use std::env;
use std::fs;
use std::io::{self, Read, Write};
use std::process::{Command, ExitCode, Stdio};
use std::thread;
use std::time::Duration;

use pdf_rs_baseline::{
    AdapterRequest, AdapterResponseChannels, BaselineChannel, decode_adapter_request,
    encode_adapter_failure, encode_adapter_response,
};

const FIXTURE_BYTE_LIMIT: usize = 8 * 1024 * 1024;
const FIXTURE_REQUEST_FRAME_LIMIT: usize = FIXTURE_BYTE_LIMIT + 96;
const FIXTURE_RESPONSE_FRAME_LIMIT: u64 = 32 * 1024 * 1024;

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(()) => ExitCode::from(7),
    }
}

fn run() -> Result<(), ()> {
    let arguments: Vec<String> = env::args().skip(1).collect();
    let environment_mode = env::var("PDF_RS_BASELINE_FIXTURE_MODE").ok();
    let executable_mode = executable_mode();
    let mode = arguments
        .first()
        .map(String::as_str)
        .or(environment_mode.as_deref())
        .or(executable_mode.as_deref())
        .ok_or(())?;
    match mode {
        "ok" => write_produced_response(read_request()?, 2, 0),
        "unsupported" => {
            let request = read_request()?;
            write_channels(
                &request,
                AdapterResponseChannels::new(
                    BaselineChannel::Unsupported,
                    BaselineChannel::Unsupported,
                    BaselineChannel::Produced(b"[]"),
                    BaselineChannel::Produced(&[0; 4]),
                ),
            )
        }
        "channel-failed" => {
            let request = read_request()?;
            write_channels(
                &request,
                AdapterResponseChannels::new(
                    BaselineChannel::Failed,
                    BaselineChannel::Produced(b"[]"),
                    BaselineChannel::Produced(b"[]"),
                    BaselineChannel::Produced(&[0; 4]),
                ),
            )
        }
        "pixel-only" => {
            let request = read_request()?;
            let rgba = filled(rgba_length(request.width(), request.height())?, 0)?;
            write_channels(
                &request,
                AdapterResponseChannels::new(
                    BaselineChannel::Unsupported,
                    BaselineChannel::Unsupported,
                    BaselineChannel::Unsupported,
                    BaselineChannel::Produced(&rgba),
                ),
            )
        }
        "pixel-failed" => {
            let request = read_request()?;
            write_channels(
                &request,
                AdapterResponseChannels::new(
                    BaselineChannel::Unsupported,
                    BaselineChannel::Unsupported,
                    BaselineChannel::Unsupported,
                    BaselineChannel::Failed,
                ),
            )
        }
        "pixel-parse-failed" => write_pixel_profile_violation(read_request()?, 0),
        "pixel-scene-failed" => write_pixel_profile_violation(read_request()?, 1),
        "pixel-text-failed" => write_pixel_profile_violation(read_request()?, 2),
        "pixel-unsupported" => {
            let request = read_request()?;
            write_channels(
                &request,
                AdapterResponseChannels::new(
                    BaselineChannel::Unsupported,
                    BaselineChannel::Unsupported,
                    BaselineChannel::Unsupported,
                    BaselineChannel::Unsupported,
                ),
            )
        }
        "pixel-only-marker" => {
            fs::write("spawned", b"spawned").map_err(|_| ())?;
            let request = read_request()?;
            let rgba = filled(rgba_length(request.width(), request.height())?, 0)?;
            write_channels(
                &request,
                AdapterResponseChannels::new(
                    BaselineChannel::Unsupported,
                    BaselineChannel::Unsupported,
                    BaselineChannel::Unsupported,
                    BaselineChannel::Produced(&rgba),
                ),
            )
        }
        "profile-violation" => {
            let request = read_request()?;
            write_produced_response(request, 2, 0)
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
            let response =
                encode_adapter_failure(&request, FIXTURE_RESPONSE_FRAME_LIMIT).map_err(|_| ())?;
            write_frame(&response)
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
            let request = read_request()?;
            let mut response = produced_response(&request, 2)?;
            let wrong_page = request.page().checked_add(1).ok_or(())?;
            response[28..32].copy_from_slice(&wrong_page.to_be_bytes());
            write_frame(&response)
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

fn executable_mode() -> Option<String> {
    env::current_exe()
        .ok()?
        .file_name()?
        .to_str()?
        .strip_prefix("pdf-rs-baseline-fixture-")
        .map(str::to_owned)
}

fn read_request() -> Result<AdapterRequest, ()> {
    let limit = u64::try_from(FIXTURE_REQUEST_FRAME_LIMIT).map_err(|_| ())?;
    let mut input = io::stdin().lock().take(limit.saturating_add(1));
    let mut frame = Vec::new();
    input.read_to_end(&mut frame).map_err(|_| ())?;
    if frame.len() > FIXTURE_REQUEST_FRAME_LIMIT {
        return Err(());
    }
    decode_adapter_request(frame, limit).map_err(|_| ())
}

fn write_produced_response(
    request: AdapterRequest,
    parse_bytes: usize,
    stderr_bytes: usize,
) -> Result<(), ()> {
    if !(2..=FIXTURE_BYTE_LIMIT).contains(&parse_bytes) || stderr_bytes > FIXTURE_BYTE_LIMIT {
        return Err(());
    }
    write_repeated(&mut io::stderr().lock(), b'E', stderr_bytes)?;
    let response = produced_response(&request, parse_bytes)?;
    write_frame(&response)
}

fn produced_response(request: &AdapterRequest, parse_bytes: usize) -> Result<Vec<u8>, ()> {
    let parse = json_payload(parse_bytes)?;
    let rgba_length = rgba_length(request.width(), request.height())?;
    let rgba = filled(rgba_length, 0)?;
    encode_adapter_response(
        request,
        AdapterResponseChannels::new(
            BaselineChannel::Produced(&parse),
            BaselineChannel::Produced(b"[]"),
            BaselineChannel::Produced(b"[]"),
            BaselineChannel::Produced(&rgba),
        ),
        FIXTURE_RESPONSE_FRAME_LIMIT,
    )
    .map_err(|_| ())
}

fn write_channels(
    request: &AdapterRequest,
    channels: AdapterResponseChannels<'_>,
) -> Result<(), ()> {
    let response =
        encode_adapter_response(request, channels, FIXTURE_RESPONSE_FRAME_LIMIT).map_err(|_| ())?;
    write_frame(&response)
}

fn write_pixel_profile_violation(request: AdapterRequest, failed_channel: usize) -> Result<(), ()> {
    let rgba = filled(rgba_length(request.width(), request.height())?, 0)?;
    let parse = if failed_channel == 0 {
        BaselineChannel::Failed
    } else {
        BaselineChannel::Unsupported
    };
    let scene = if failed_channel == 1 {
        BaselineChannel::Failed
    } else {
        BaselineChannel::Unsupported
    };
    let text = if failed_channel == 2 {
        BaselineChannel::Failed
    } else {
        BaselineChannel::Unsupported
    };
    write_channels(
        &request,
        AdapterResponseChannels::new(parse, scene, text, BaselineChannel::Produced(&rgba)),
    )
}

fn write_frame(response: &[u8]) -> Result<(), ()> {
    let mut output = io::stdout().lock();
    output.write_all(response).map_err(|_| ())?;
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
