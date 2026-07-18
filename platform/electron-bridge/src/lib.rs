//! Length-bounded local stdio bridge from Electron main to PDF.rs Native viewer.

use std::collections::BTreeMap;
use std::fs;
use std::io::{self, BufRead, BufReader, BufWriter, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, SyncSender, TrySendError};
use std::sync::{Arc, Mutex};
use std::thread;

use pdf_rs_viewer::{NativeDocument, NativePageSurface, NativeRendererKind, NativeViewerErrorCode};

const MAX_COMMAND_BYTES: usize = 16 * 1024;
const MAX_PATH_BYTES: usize = 4 * 1024;
const MAX_SOURCE_BYTES: u64 = 256 * 1024 * 1024;
const MAX_PENDING_COMMANDS: usize = 32;
const MAX_PENDING_RESPONSES: usize = 2;
const FAST_CPU_CANARY_ENV: &str = "PDF_RS_FAST_CPU_CANARY_V1";
const FAST_CPU_CANARY_COHORT: &str = "m4-r0-basic-page-local-v1";

type ActiveRenders = Arc<Mutex<BTreeMap<u64, ActiveRender>>>;

struct ActiveRender {
    document_id: u64,
    cancellation: Arc<AtomicBool>,
}

enum WorkerCommand {
    Open {
        request: u64,
        path: String,
    },
    Render {
        request: u64,
        document_id: u64,
        page: u32,
        width: u32,
        cancellation: Arc<AtomicBool>,
    },
    Close {
        request: u64,
        document_id: u64,
    },
    Shutdown {
        request: u64,
    },
    Stop,
}

enum Response {
    Opened {
        request: u64,
        document_id: u64,
        pages: u32,
    },
    Surface {
        request: u64,
        document_id: u64,
        surface: NativePageSurface,
    },
    Cancelled {
        request: u64,
        target: u64,
    },
    Closed {
        request: u64,
        document_id: u64,
    },
    Bye {
        request: u64,
    },
    Error {
        request: u64,
        code: &'static str,
    },
}

/// Runs the fixed local stdio bridge command selected from the process arguments.
///
/// The returned value is a process exit code; the library never terminates its caller.
pub fn run_from_environment() -> u8 {
    if std::env::args().nth(1).as_deref() != Some("--stdio") {
        return 64;
    }
    if run_stdio().is_err() {
        return 70;
    }
    0
}

fn run_stdio() -> io::Result<()> {
    let active_renders = ActiveRenders::default();
    let (response_sender, response_receiver) = mpsc::sync_channel(MAX_PENDING_RESPONSES);
    let writer = thread::Builder::new()
        .name("pdf-rs-electron-output".into())
        .spawn(move || write_responses(response_receiver))?;
    let (command_sender, command_receiver) = mpsc::sync_channel(MAX_PENDING_COMMANDS);
    let worker_responses = response_sender.clone();
    let worker_renders = Arc::clone(&active_renders);
    let renderer = selected_renderer();
    let worker = thread::Builder::new()
        .name("pdf-rs-electron-render".into())
        .spawn(move || {
            run_worker(command_receiver, worker_responses, worker_renders, renderer);
        })?;

    let stdin = io::stdin();
    let mut input = BufReader::new(stdin.lock());
    let mut line = Vec::new();
    let mut orderly_shutdown = false;
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
                send_response(
                    &response_sender,
                    Response::Error {
                        request: 0,
                        code: "invalid-command",
                    },
                )?;
                continue;
            }
        };
        let mut fields = command.split(' ');
        let method = fields.next().unwrap_or_default();
        let request = match parse_u64(fields.next()) {
            Some(request) if request > 0 => request,
            _ => {
                send_response(
                    &response_sender,
                    Response::Error {
                        request: 0,
                        code: "invalid-command",
                    },
                )?;
                continue;
            }
        };
        match method {
            "OPEN" => {
                let Some(path) = fields.next().and_then(decode_path) else {
                    send_error(&response_sender, request, "invalid-path")?;
                    continue;
                };
                if fields.next().is_some() {
                    send_error(&response_sender, request, "invalid-command")?;
                    continue;
                }
                dispatch(
                    &command_sender,
                    WorkerCommand::Open { request, path },
                    &response_sender,
                    request,
                )?;
            }
            "RENDER" => {
                let Some(document_id) = parse_u64(fields.next()) else {
                    send_error(&response_sender, request, "invalid-command")?;
                    continue;
                };
                let Some(page) = parse_u32(fields.next()) else {
                    send_error(&response_sender, request, "invalid-command")?;
                    continue;
                };
                let Some(width) = parse_u32(fields.next()) else {
                    send_error(&response_sender, request, "invalid-command")?;
                    continue;
                };
                if fields.next().is_some() {
                    send_error(&response_sender, request, "invalid-command")?;
                    continue;
                }
                let cancellation = Arc::new(AtomicBool::new(false));
                if !register_render(
                    &active_renders,
                    request,
                    document_id,
                    Arc::clone(&cancellation),
                ) {
                    send_error(&response_sender, request, "invalid-command")?;
                    continue;
                }
                let command = WorkerCommand::Render {
                    request,
                    document_id,
                    page,
                    width,
                    cancellation,
                };
                if !dispatch(&command_sender, command, &response_sender, request)? {
                    remove_render(&active_renders, request);
                }
            }
            "CANCEL" => {
                let Some(target) = parse_u64(fields.next()) else {
                    send_error(&response_sender, request, "invalid-command")?;
                    continue;
                };
                if fields.next().is_some() {
                    send_error(&response_sender, request, "invalid-command")?;
                    continue;
                }
                if cancel_render(&active_renders, target) {
                    send_response(&response_sender, Response::Cancelled { request, target })?;
                } else {
                    send_error(&response_sender, request, "unknown-request")?;
                }
            }
            "CLOSE" => {
                let Some(document_id) = parse_u64(fields.next()) else {
                    send_error(&response_sender, request, "invalid-command")?;
                    continue;
                };
                if fields.next().is_some() {
                    send_error(&response_sender, request, "invalid-command")?;
                    continue;
                }
                cancel_document_renders(&active_renders, document_id);
                dispatch(
                    &command_sender,
                    WorkerCommand::Close {
                        request,
                        document_id,
                    },
                    &response_sender,
                    request,
                )?;
            }
            "SHUTDOWN" => {
                if fields.next().is_some() {
                    send_error(&response_sender, request, "invalid-command")?;
                    continue;
                }
                cancel_all_renders(&active_renders);
                command_sender
                    .send(WorkerCommand::Shutdown { request })
                    .map_err(|_| io::Error::new(io::ErrorKind::BrokenPipe, "worker stopped"))?;
                orderly_shutdown = true;
                break;
            }
            _ => send_error(&response_sender, request, "invalid-command")?,
        }
    }

    if !orderly_shutdown {
        cancel_all_renders(&active_renders);
        let _ = command_sender.send(WorkerCommand::Stop);
    }
    drop(command_sender);
    worker
        .join()
        .map_err(|_| io::Error::other("render worker panicked"))?;
    drop(response_sender);
    writer
        .join()
        .map_err(|_| io::Error::other("output worker panicked"))?
}

fn run_worker(
    commands: Receiver<WorkerCommand>,
    responses: SyncSender<Response>,
    active_renders: ActiveRenders,
    renderer: NativeRendererKind,
) {
    let mut documents = BTreeMap::<u64, NativeDocument>::new();
    let mut next_document = 1_u64;
    while let Ok(command) = commands.recv() {
        match command {
            WorkerCommand::Open { request, path } => {
                let bytes = match read_source(&path) {
                    Ok(bytes) => bytes,
                    Err(code) => {
                        let _ = responses.send(Response::Error { request, code });
                        continue;
                    }
                };
                let document = match NativeDocument::open(bytes) {
                    Ok(document) => document,
                    Err(error) => {
                        let _ = responses.send(Response::Error {
                            request,
                            code: error_code(error.code()),
                        });
                        continue;
                    }
                };
                let document_id = next_document;
                let Some(successor) = next_document.checked_add(1) else {
                    let _ = responses.send(Response::Error {
                        request,
                        code: "resource-limit",
                    });
                    continue;
                };
                next_document = successor;
                let pages = document.page_count();
                documents.insert(document_id, document);
                if responses
                    .send(Response::Opened {
                        request,
                        document_id,
                        pages,
                    })
                    .is_err()
                {
                    break;
                }
            }
            WorkerCommand::Render {
                request,
                document_id,
                page,
                width,
                cancellation,
            } => {
                let result = match documents.get_mut(&document_id) {
                    Some(document) => document.render_page_with_renderer_and_cancellation(
                        page,
                        width,
                        renderer,
                        cancellation.as_ref(),
                    ),
                    None => {
                        let publish = commit_render(&active_renders, request, &cancellation);
                        if responses
                            .send(Response::Error {
                                request,
                                code: if publish {
                                    "unknown-document"
                                } else {
                                    "cancelled"
                                },
                            })
                            .is_err()
                        {
                            break;
                        }
                        continue;
                    }
                };
                let publish = commit_render(&active_renders, request, &cancellation);
                let response = if !publish {
                    Response::Error {
                        request,
                        code: "cancelled",
                    }
                } else {
                    match result {
                        Ok(surface) => Response::Surface {
                            request,
                            document_id,
                            surface,
                        },
                        Err(error) => Response::Error {
                            request,
                            code: error_code(error.code()),
                        },
                    }
                };
                if responses.send(response).is_err() {
                    break;
                }
            }
            WorkerCommand::Close {
                request,
                document_id,
            } => {
                let response = if documents.remove(&document_id).is_some() {
                    Response::Closed {
                        request,
                        document_id,
                    }
                } else {
                    Response::Error {
                        request,
                        code: "unknown-document",
                    }
                };
                if responses.send(response).is_err() {
                    break;
                }
            }
            WorkerCommand::Shutdown { request } => {
                documents.clear();
                let _ = responses.send(Response::Bye { request });
                break;
            }
            WorkerCommand::Stop => break,
        }
    }
}

fn write_responses(responses: Receiver<Response>) -> io::Result<()> {
    let stdout = io::stdout();
    let mut output = BufWriter::new(stdout.lock());
    while let Ok(response) = responses.recv() {
        match response {
            Response::Opened {
                request,
                document_id,
                pages,
            } => writeln!(output, "OPENED {request} {document_id} {pages}")?,
            Response::Surface {
                request,
                document_id,
                surface,
            } => {
                writeln!(
                    output,
                    "SURFACE {request} {document_id} {} {} {} {} {} {}",
                    surface.page_index(),
                    surface.renderer().identifier(),
                    surface.width(),
                    surface.height(),
                    surface.stride(),
                    surface.pixels().len()
                )?;
                output.write_all(surface.pixels())?;
                output.write_all(b"\n")?;
            }
            Response::Cancelled { request, target } => {
                writeln!(output, "CANCELLED {request} {target}")?;
            }
            Response::Closed {
                request,
                document_id,
            } => writeln!(output, "CLOSED {request} {document_id}")?,
            Response::Bye { request } => writeln!(output, "BYE {request}")?,
            Response::Error { request, code } => writeln!(output, "ERROR {request} {code}")?,
        }
        output.flush()?;
    }
    Ok(())
}

fn dispatch(
    sender: &SyncSender<WorkerCommand>,
    command: WorkerCommand,
    responses: &SyncSender<Response>,
    request: u64,
) -> io::Result<bool> {
    match sender.try_send(command) {
        Ok(()) => Ok(true),
        Err(TrySendError::Full(_)) => {
            send_error(responses, request, "resource-limit")?;
            Ok(false)
        }
        Err(TrySendError::Disconnected(_)) => {
            send_error(responses, request, "bridge-closed")?;
            Ok(false)
        }
    }
}

fn send_error(
    responses: &SyncSender<Response>,
    request: u64,
    code: &'static str,
) -> io::Result<()> {
    send_response(responses, Response::Error { request, code })
}

fn send_response(responses: &SyncSender<Response>, response: Response) -> io::Result<()> {
    responses
        .send(response)
        .map_err(|_| io::Error::new(io::ErrorKind::BrokenPipe, "output stopped"))
}

fn read_source(path: &str) -> Result<Vec<u8>, &'static str> {
    let length = fs::metadata(path).map_err(|_| "source")?.len();
    if length == 0 || length > MAX_SOURCE_BYTES {
        return Err("resource-limit");
    }
    let bytes = fs::read(path).map_err(|_| "source")?;
    if u64::try_from(bytes.len()).ok() != Some(length) {
        return Err("source");
    }
    Ok(bytes)
}

fn register_render(
    active: &ActiveRenders,
    request: u64,
    document_id: u64,
    cancellation: Arc<AtomicBool>,
) -> bool {
    let Ok(mut active) = active.lock() else {
        return false;
    };
    if active.contains_key(&request) {
        return false;
    }
    active.insert(
        request,
        ActiveRender {
            document_id,
            cancellation,
        },
    );
    true
}

fn cancel_render(active: &ActiveRenders, request: u64) -> bool {
    let Ok(active) = active.lock() else {
        return false;
    };
    let Some(render) = active.get(&request) else {
        return false;
    };
    render.cancellation.store(true, Ordering::Release);
    true
}

fn cancel_document_renders(active: &ActiveRenders, document_id: u64) {
    let Ok(active) = active.lock() else {
        return;
    };
    for render in active.values() {
        if render.document_id == document_id {
            render.cancellation.store(true, Ordering::Release);
        }
    }
}

fn cancel_all_renders(active: &ActiveRenders) {
    let Ok(active) = active.lock() else {
        return;
    };
    for render in active.values() {
        render.cancellation.store(true, Ordering::Release);
    }
}

fn commit_render(active: &ActiveRenders, request: u64, cancellation: &Arc<AtomicBool>) -> bool {
    let Ok(mut active) = active.lock() else {
        return false;
    };
    let publish = active.get(&request).is_some_and(|render| {
        Arc::ptr_eq(&render.cancellation, cancellation)
            && !render.cancellation.load(Ordering::Acquire)
    });
    active.remove(&request);
    publish
}

fn remove_render(active: &ActiveRenders, request: u64) {
    if let Ok(mut active) = active.lock() {
        active.remove(&request);
    }
}

fn selected_renderer() -> NativeRendererKind {
    match std::env::var(FAST_CPU_CANARY_ENV) {
        Ok(value) if value == FAST_CPU_CANARY_COHORT => NativeRendererKind::FastCpu,
        _ => NativeRendererKind::ReferenceCpu,
    }
}

fn parse_u64(value: Option<&str>) -> Option<u64> {
    value?.parse().ok()
}

fn parse_u32(value: Option<&str>) -> Option<u32> {
    value?.parse().ok()
}

fn decode_path(hex: &str) -> Option<String> {
    if hex.is_empty() || hex.len() > MAX_PATH_BYTES.checked_mul(2)? || !hex.len().is_multiple_of(2)
    {
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

fn error_code(code: NativeViewerErrorCode) -> &'static str {
    match code {
        NativeViewerErrorCode::InvalidInput => "invalid-input",
        NativeViewerErrorCode::Source => "source",
        NativeViewerErrorCode::Document => "document",
        NativeViewerErrorCode::Content => "content",
        NativeViewerErrorCode::Unsupported => "unsupported",
        NativeViewerErrorCode::Render => "render",
        NativeViewerErrorCode::Cancelled => "cancelled",
        NativeViewerErrorCode::ResourceLimit => "resource-limit",
        NativeViewerErrorCode::Internal => "internal",
    }
}
