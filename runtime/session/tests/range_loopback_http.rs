use std::fmt::Write as _;
use std::io::{self, Cursor, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver, SyncSender};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use pdf_rs_bytes::{
    ByteRange, JobId, RangeResponse, ReadRequest, RequestPriority, ResumeCheckpoint, SmallRanges,
    SourceIdentity, SourceRevision, SourceSnapshot, SourceStableId, SourceValidator,
    SourceValidatorKind,
};
use pdf_rs_cache::{ReadyStoreEpoch, ReadyStoreSessionId};
use pdf_rs_document::{
    DocumentLimits, NeverCancelled, OpenStrictBaseRevisionJob, OutlineJobContext, OutlineLimits,
    PageTreeJobContext, PageTreeLimits, RevisionAttestationJobContext, RevisionAttestationLimits,
    RevisionId, StrictBaseOpenContext, StrictBaseOpenLimits,
};
use pdf_rs_object::ObjectLimits;
use pdf_rs_session::{
    M1OpeningParserAudit, M1RequestId, M1RequestIdentity, M1SessionCancel, M1SessionFailure,
    M1SessionIngress, M1SessionIngressRejectReason, M1SessionPhase, M1SessionResources,
    M1SessionRun, M1SessionWait, M1StrictDocumentSession, NeverCancelledRangeCoalescer,
    RangeCoalescerRequest, RangeRequestCoalescer, RangeRequestId, RangeResumeGeneration,
};
use pdf_rs_syntax::SyntaxLimits;
use pdf_rs_xref::{XrefJobContext, XrefLimits};

const IO_TIMEOUT: Duration = Duration::from_secs(5);
const MAX_HTTP_HEADER_BYTES: usize = 16 * 1024;
const MAX_HTTP_BODY_BYTES: usize = 1024 * 1024;
const MAX_HTTP_TOTAL_BYTES: usize = MAX_HTTP_HEADER_BYTES + 4 + MAX_HTTP_BODY_BYTES;
const VALID_TEST_ETAG: &str =
    "\"0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef\"";
const GENERATION: RangeResumeGeneration = RangeResumeGeneration::new(0x4a);
const OPEN_REQUEST: M1RequestIdentity =
    M1RequestIdentity::new(M1RequestId::new(0x4a01), JobId::new(0x4a02), GENERATION);
const PAGE_REQUEST: M1RequestIdentity =
    M1RequestIdentity::new(M1RequestId::new(0x4a11), JobId::new(0x4a12), GENERATION);
const OUTLINE_REQUEST: M1RequestIdentity =
    M1RequestIdentity::new(M1RequestId::new(0x4a21), JobId::new(0x4a22), GENERATION);

struct Fixture {
    bytes: Arc<Vec<u8>>,
    snapshot: SourceSnapshot,
    etag: String,
}

fn source_snapshot(revision: u64, tag_seed: u8, len: u64) -> SourceSnapshot {
    SourceSnapshot::new(
        SourceIdentity::new(
            SourceStableId::new([0xa7; 32]),
            SourceRevision::new(revision),
        ),
        Some(len),
        SourceValidator::new(SourceValidatorKind::StrongEntityTag, [tag_seed; 32]),
    )
}

fn strong_etag(snapshot: SourceSnapshot) -> String {
    assert_eq!(
        snapshot.validator().kind(),
        SourceValidatorKind::StrongEntityTag
    );
    let mut tag = String::with_capacity(66);
    tag.push('"');
    for byte in snapshot.validator().digest() {
        write!(&mut tag, "{byte:02x}").expect("writing to String is infallible");
    }
    tag.push('"');
    tag
}

fn fixture(revision: u64, tag_seed: u8) -> Fixture {
    let bodies: &[(u32, &[u8])] = &[
        (
            1,
            b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R /Outlines 4 0 R >>\nendobj\n",
        ),
        (
            2,
            b"2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 >>\nendobj\n",
        ),
        (3, b"3 0 obj\n<< /Type /Page /Parent 2 0 R >>\nendobj\n"),
        (
            4,
            b"4 0 obj\n<< /Type /Outlines /First 5 0 R /Last 5 0 R /Count 1 >>\nendobj\n",
        ),
        (
            5,
            b"5 0 obj\n<< /Title (Loopback) /Parent 4 0 R >>\nendobj\n",
        ),
    ];
    let mut bytes = b"%PDF-1.7\n".to_vec();
    let mut offsets = Vec::new();
    for &(number, body) in bodies {
        offsets.push((
            number,
            u64::try_from(bytes.len()).expect("fixture offset fits u64"),
        ));
        bytes.extend_from_slice(body);
    }
    let startxref = u64::try_from(bytes.len()).expect("fixture length fits u64");
    let size = 6_u32;
    bytes.extend_from_slice(format!("xref\n0 {size}\n").as_bytes());
    for number in 0..size {
        let row = if number == 0 {
            "0000000000 65535 f \n".to_owned()
        } else {
            let offset = offsets
                .iter()
                .find(|(candidate, _)| *candidate == number)
                .map(|(_, offset)| *offset)
                .expect("every nonzero object has an xref row");
            format!("{offset:010} 00000 n \n")
        };
        assert_eq!(row.len(), 20);
        bytes.extend_from_slice(row.as_bytes());
    }
    bytes.extend_from_slice(
        format!("trailer\n<< /Size {size} /Root 1 0 R >>\nstartxref\n{startxref}\n%%EOF\n")
            .as_bytes(),
    );
    let len = u64::try_from(bytes.len()).expect("fixture length fits u64");
    let snapshot = source_snapshot(revision, tag_seed, len);
    Fixture {
        bytes: Arc::new(bytes),
        snapshot,
        etag: strong_etag(snapshot),
    }
}

fn strict_job(fixture: &Fixture) -> OpenStrictBaseRevisionJob {
    OpenStrictBaseRevisionJob::new(
        fixture.snapshot,
        RevisionId::new(0x4a03),
        StrictBaseOpenContext::new(
            XrefJobContext::new(
                OPEN_REQUEST.job(),
                ResumeCheckpoint::new(0x4a04),
                ResumeCheckpoint::new(0x4a05),
            ),
            RevisionAttestationJobContext::new(
                OPEN_REQUEST.job(),
                ResumeCheckpoint::new(0x4a06),
                ResumeCheckpoint::new(0x4a07),
                ResumeCheckpoint::new(0x4a08),
                RequestPriority::Metadata,
            ),
        ),
        StrictBaseOpenLimits::new(
            XrefLimits::default(),
            DocumentLimits::default(),
            RevisionAttestationLimits::default(),
            ObjectLimits::default(),
            SyntaxLimits::default(),
        ),
    )
    .expect("loopback fixture is a valid strict-open job")
}

fn session(fixture: &Fixture) -> M1StrictDocumentSession {
    M1StrictDocumentSession::new(
        ReadyStoreSessionId::new(0x4a09),
        OPEN_REQUEST,
        strict_job(fixture),
        Default::default(),
        ReadyStoreEpoch::new(0x4a0a),
        Default::default(),
    )
    .expect("built-in session limits validate")
}

#[derive(Debug, Eq, PartialEq)]
struct ObservedRequest {
    range: ByteRange,
    if_range: String,
}

struct ResponseGate {
    request_seen: SyncSender<()>,
    release_response: Receiver<()>,
}

fn spawn_server(
    bytes: Arc<Vec<u8>>,
    current_etag: String,
    request_count: usize,
    mut gate: Option<ResponseGate>,
) -> (SocketAddr, JoinHandle<Vec<ObservedRequest>>) {
    let listener = TcpListener::bind(("127.0.0.1", 0)).expect("bind loopback HTTP fixture");
    let address = listener.local_addr().expect("read loopback address");
    let handle = thread::spawn(move || {
        let mut observed = Vec::with_capacity(request_count);
        for _ in 0..request_count {
            let (mut stream, peer) = listener.accept().expect("accept loopback client");
            assert!(peer.ip().is_loopback());
            stream
                .set_read_timeout(Some(IO_TIMEOUT))
                .expect("set server read timeout");
            stream
                .set_write_timeout(Some(IO_TIMEOUT))
                .expect("set server write timeout");
            let request = read_http_head(&mut stream).expect("read bounded HTTP request");
            let request_line = request.lines().next().expect("request line exists");
            assert_eq!(request_line, "GET /document.pdf HTTP/1.1");
            let range = parse_range_header(
                header_value(&request, "Range").expect("HTTP Range header is required"),
            )
            .expect("client emits a valid closed HTTP Range");
            let if_range = header_value(&request, "If-Range")
                .expect("HTTP If-Range header is required")
                .to_owned();
            assert!(is_strong_etag(&if_range));
            observed.push(ObservedRequest {
                range,
                if_range: if_range.clone(),
            });

            if let Some(response_gate) = gate.take() {
                response_gate
                    .request_seen
                    .send(())
                    .expect("announce received request");
                response_gate
                    .release_response
                    .recv_timeout(IO_TIMEOUT)
                    .expect("release delayed response");
            }

            if if_range == current_etag {
                write_partial_response(&mut stream, &bytes, range, &current_etag)
                    .expect("write partial loopback response");
            } else {
                write_full_response(&mut stream, &bytes, &current_etag)
                    .expect("write changed-source loopback response");
            }
        }
        observed
    });
    (address, handle)
}

fn read_http_head(stream: &mut TcpStream) -> io::Result<String> {
    let mut bytes = Vec::with_capacity(1024);
    let mut buffer = [0_u8; 512];
    while !bytes.windows(4).any(|window| window == b"\r\n\r\n") {
        let read = stream.read(&mut buffer)?;
        if read == 0 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "HTTP request ended before headers",
            ));
        }
        bytes.extend_from_slice(&buffer[..read]);
        if bytes.len() > MAX_HTTP_HEADER_BYTES {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "HTTP request headers exceed test ceiling",
            ));
        }
    }
    String::from_utf8(bytes)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "HTTP headers are not UTF-8"))
}

fn header_value<'a>(head: &'a str, name: &str) -> Option<&'a str> {
    head.lines().skip(1).find_map(|line| {
        let (candidate, value) = line.split_once(':')?;
        candidate.eq_ignore_ascii_case(name).then_some(value.trim())
    })
}

fn is_strong_etag(value: &str) -> bool {
    value.len() == 66
        && value.starts_with('"')
        && value.ends_with('"')
        && value.as_bytes()[1..65].iter().all(u8::is_ascii_hexdigit)
}

fn invalid_data(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

fn parse_range_header(value: &str) -> io::Result<ByteRange> {
    let range = value
        .strip_prefix("bytes=")
        .ok_or_else(|| invalid_data("Range unit is not bytes"))?;
    let (start, end) = range
        .split_once('-')
        .ok_or_else(|| invalid_data("Range is not closed"))?;
    if end.contains('-') {
        return Err(invalid_data("Range has more than one separator"));
    }
    let start = start
        .parse::<u64>()
        .map_err(|_| invalid_data("Range start is not numeric"))?;
    let end = end
        .parse::<u64>()
        .map_err(|_| invalid_data("Range end is not numeric"))?;
    let len = end
        .checked_sub(start)
        .and_then(|distance| distance.checked_add(1))
        .ok_or_else(|| invalid_data("Range is empty or wraps"))?;
    ByteRange::new(start, len).map_err(|_| invalid_data("Range geometry is invalid"))
}

fn write_partial_response(
    stream: &mut TcpStream,
    bytes: &[u8],
    range: ByteRange,
    etag: &str,
) -> io::Result<()> {
    let start = usize::try_from(range.start())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "Range start exceeds usize"))?;
    let end = usize::try_from(range.end_exclusive())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "Range end exceeds usize"))?;
    let body = bytes
        .get(start..end)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "Range exceeds fixture"))?;
    let last = range
        .end_exclusive()
        .checked_sub(1)
        .expect("Range is nonempty");
    write!(
        stream,
        "HTTP/1.1 206 Partial Content\r\nETag: {etag}\r\nAccept-Ranges: bytes\r\nContent-Range: bytes {}-{last}/{}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        range.start(),
        bytes.len(),
        body.len()
    )?;
    stream.write_all(body)
}

fn write_full_response(stream: &mut TcpStream, bytes: &[u8], etag: &str) -> io::Result<()> {
    write!(
        stream,
        "HTTP/1.1 200 OK\r\nETag: {etag}\r\nAccept-Ranges: bytes\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        bytes.len()
    )?;
    stream.write_all(bytes)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct HttpContentRange {
    range: ByteRange,
    total_len: u64,
}

#[derive(Debug)]
struct HttpResponse {
    status: u16,
    etag: String,
    content_range: Option<HttpContentRange>,
    body: Vec<u8>,
}

fn fetch_range(
    address: SocketAddr,
    range: ByteRange,
    if_range: &str,
    expected_source_len: u64,
) -> io::Result<HttpResponse> {
    assert!(address.ip().is_loopback());
    assert!(is_strong_etag(if_range));
    let mut stream = TcpStream::connect_timeout(&address, IO_TIMEOUT)?;
    stream.set_read_timeout(Some(IO_TIMEOUT))?;
    stream.set_write_timeout(Some(IO_TIMEOUT))?;
    let last = range
        .end_exclusive()
        .checked_sub(1)
        .expect("Range is nonempty");
    write!(
        stream,
        "GET /document.pdf HTTP/1.1\r\nHost: {address}\r\nRange: bytes={}-{last}\r\nIf-Range: {if_range}\r\nConnection: close\r\n\r\n",
        range.start()
    )?;
    stream.flush()?;

    let response = read_http_response(&mut stream)?;
    validate_http_response(response, range, expected_source_len)
}

fn read_http_response(reader: &mut impl Read) -> io::Result<HttpResponse> {
    let mut head_bytes = [0_u8; MAX_HTTP_HEADER_BYTES + 4];
    let mut stored_head_bytes = 0_usize;
    loop {
        if head_bytes[..stored_head_bytes].ends_with(b"\r\n\r\n") {
            break;
        }
        if stored_head_bytes == head_bytes.len() {
            return Err(invalid_data("HTTP response header exceeds hard ceiling"));
        }
        if reader.read(&mut head_bytes[stored_head_bytes..=stored_head_bytes])? == 0 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "HTTP response ended before headers",
            ));
        }
        stored_head_bytes += 1;
    }
    let head_len = stored_head_bytes
        .checked_sub(4)
        .ok_or_else(|| invalid_data("HTTP header length underflow"))?;
    if head_len > MAX_HTTP_HEADER_BYTES {
        return Err(invalid_data("HTTP response header exceeds hard ceiling"));
    }
    let head = std::str::from_utf8(&head_bytes[..head_len])
        .map_err(|_| invalid_data("response head is not UTF-8"))?;
    let status = head
        .lines()
        .next()
        .and_then(|line| line.split_ascii_whitespace().nth(1))
        .ok_or_else(|| invalid_data("missing HTTP status"))?
        .parse::<u16>()
        .map_err(|_| invalid_data("invalid HTTP status"))?;
    let etag = single_response_header(head, "ETag")?.to_owned();
    if !is_strong_etag(&etag) {
        return Err(invalid_data("response ETag is weak"));
    }
    let content_length = single_response_header(head, "Content-Length")?
        .parse::<usize>()
        .map_err(|_| invalid_data("invalid Content-Length"))?;
    if content_length > MAX_HTTP_BODY_BYTES {
        return Err(invalid_data("HTTP response body exceeds hard ceiling"));
    }
    let total_len = stored_head_bytes
        .checked_add(content_length)
        .ok_or_else(|| invalid_data("HTTP response total length overflow"))?;
    if total_len > MAX_HTTP_TOTAL_BYTES {
        return Err(invalid_data("HTTP response exceeds total hard ceiling"));
    }
    let mut body = Vec::new();
    body.try_reserve_exact(content_length)
        .map_err(|_| invalid_data("HTTP response body allocation failed"))?;
    let mut buffer = [0_u8; 4096];
    while body.len() < content_length {
        let remaining = content_length - body.len();
        let chunk_len = remaining.min(buffer.len());
        let read = reader.read(&mut buffer[..chunk_len])?;
        if read == 0 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "HTTP response body is shorter than Content-Length",
            ));
        }
        body.extend_from_slice(&buffer[..read]);
    }
    let mut surplus = [0_u8; 1];
    if reader.read(&mut surplus)? != 0 {
        return Err(invalid_data(
            "HTTP response body is longer than Content-Length",
        ));
    }
    let content_range = optional_response_header(head, "Content-Range")?
        .map(parse_content_range)
        .transpose()?;
    Ok(HttpResponse {
        status,
        etag,
        content_range,
        body,
    })
}

fn response_header_values<'a>(head: &'a str, name: &'a str) -> impl Iterator<Item = &'a str> {
    head.lines().skip(1).filter_map(move |line| {
        let (candidate, value) = line.split_once(':')?;
        candidate.eq_ignore_ascii_case(name).then_some(value.trim())
    })
}

fn single_response_header<'a>(head: &'a str, name: &'a str) -> io::Result<&'a str> {
    let mut values = response_header_values(head, name);
    let value = values
        .next()
        .ok_or_else(|| invalid_data("required HTTP response header is missing"))?;
    if values.next().is_some() {
        return Err(invalid_data("duplicate HTTP response header"));
    }
    Ok(value)
}

fn optional_response_header<'a>(head: &'a str, name: &'a str) -> io::Result<Option<&'a str>> {
    let mut values = response_header_values(head, name);
    let value = values.next();
    if values.next().is_some() {
        return Err(invalid_data("duplicate HTTP response header"));
    }
    Ok(value)
}

fn parse_content_range(value: &str) -> io::Result<HttpContentRange> {
    let range_and_len = value
        .strip_prefix("bytes ")
        .ok_or_else(|| invalid_data("Content-Range unit is not bytes"))?;
    let (range, source_len) = range_and_len
        .split_once('/')
        .ok_or_else(|| invalid_data("Content-Range lacks source length"))?;
    if source_len.contains('/') {
        return Err(invalid_data("Content-Range has multiple source lengths"));
    }
    let range = parse_range_header(&format!("bytes={range}"))?;
    let total_len = source_len
        .parse::<u64>()
        .map_err(|_| invalid_data("Content-Range source length is invalid"))?;
    if range.end_exclusive() > total_len {
        return Err(invalid_data("Content-Range exceeds source length"));
    }
    Ok(HttpContentRange { range, total_len })
}

fn validate_http_response(
    response: HttpResponse,
    requested: ByteRange,
    expected_source_len: u64,
) -> io::Result<HttpResponse> {
    let body_len = u64::try_from(response.body.len())
        .map_err(|_| invalid_data("response body length exceeds u64"))?;
    match response.status {
        206 => {
            let content_range = response
                .content_range
                .ok_or_else(|| invalid_data("206 response lacks Content-Range"))?;
            if content_range.range != requested {
                return Err(invalid_data("Content-Range does not match request"));
            }
            if content_range.total_len != expected_source_len {
                return Err(invalid_data("Content-Range source length changed"));
            }
            if body_len != requested.len() {
                return Err(invalid_data("partial response body does not match Range"));
            }
        }
        200 => {
            if response.content_range.is_some() {
                return Err(invalid_data("full response carries Content-Range"));
            }
            if body_len != expected_source_len {
                return Err(invalid_data(
                    "full response body does not match source length",
                ));
            }
        }
        _ => return Err(invalid_data("unexpected HTTP response status")),
    }
    Ok(response)
}

fn split_missing(
    snapshot: SourceSnapshot,
    missing: SmallRanges,
    next_id: &mut u64,
) -> Vec<RangeCoalescerRequest> {
    let mut requests = Vec::new();
    for range in missing.as_slice().iter().copied() {
        let pieces = if range.len() > 1 {
            let lower_len = range.len() / 2;
            vec![
                ByteRange::new(range.start(), lower_len).expect("lower split is valid"),
                ByteRange::new(range.start() + lower_len, range.len() - lower_len)
                    .expect("upper split is valid"),
            ]
        } else {
            vec![range]
        };
        for piece in pieces {
            *next_id = next_id.checked_add(1).expect("test request id fits u64");
            requests.push(RangeCoalescerRequest::new(
                RangeRequestId::new(*next_id),
                snapshot,
                ReadRequest::new(
                    piece,
                    RequestPriority::Metadata,
                    OPEN_REQUEST.job(),
                    ResumeCheckpoint::new(*next_id + 0x5000),
                ),
            ));
        }
    }
    requests
}

struct RoutedResponse {
    range: ByteRange,
    response: RangeResponse,
}

fn fetch_and_route(
    fixture: &Fixture,
    requests: &[RangeCoalescerRequest],
) -> (Vec<RoutedResponse>, Vec<ObservedRequest>, usize) {
    let plan = RangeRequestCoalescer::new(fixture.snapshot, 1, Default::default())
        .plan(requests, &NeverCancelledRangeCoalescer)
        .expect("bounded adjacent requests coalesce");
    let (address, server) = spawn_server(
        Arc::clone(&fixture.bytes),
        fixture.etag.clone(),
        plan.groups().len(),
        None,
    );
    let mut routed = Vec::with_capacity(plan.request_count());
    for group in plan.groups() {
        let host_response = fetch_range(
            address,
            group.range(),
            &fixture.etag,
            fixture.snapshot.len().expect("fixture length is known"),
        )
        .expect("fetch coalesced Range");
        assert_eq!(host_response.status, 206);
        assert_eq!(host_response.etag, fixture.etag);
        assert_eq!(
            host_response.content_range,
            Some(HttpContentRange {
                range: group.range(),
                total_len: fixture.snapshot.len().expect("fixture length is known"),
            })
        );
        for member in group.members() {
            let member_range = member.request().range();
            let relative_start = member_range
                .start()
                .checked_sub(group.range().start())
                .expect("coalesced member starts within its group");
            let start = usize::try_from(relative_start).expect("member offset fits usize");
            let len = usize::try_from(member_range.len()).expect("member length fits usize");
            let end = start.checked_add(len).expect("member slice end fits usize");
            let bytes = host_response
                .body
                .get(start..end)
                .expect("coalesced response contains every exact member")
                .to_vec();
            routed.push(RoutedResponse {
                range: member_range,
                response: RangeResponse::new(fixture.snapshot, member_range, bytes)
                    .expect("routed exact response geometry validates"),
            });
        }
    }
    let observed = server.join().expect("loopback server exits cleanly");
    let group_count = plan.groups().len();
    let expected_ranges: Vec<_> = plan.groups().iter().map(|group| group.range()).collect();
    let observed_ranges: Vec<_> = observed.iter().map(|request| request.range).collect();
    assert_eq!(observed_ranges, expected_ranges);
    assert!(
        observed
            .iter()
            .all(|request| request.if_range == fixture.etag)
    );
    (routed, observed, group_count)
}

fn page_context() -> PageTreeJobContext {
    PageTreeJobContext::new(
        PAGE_REQUEST.job(),
        ResumeCheckpoint::new(0x4a13),
        ResumeCheckpoint::new(0x4a14),
        RequestPriority::Metadata,
    )
}

fn outline_context() -> OutlineJobContext {
    OutlineJobContext::new(
        OUTLINE_REQUEST.job(),
        ResumeCheckpoint::new(0x4a23),
        ResumeCheckpoint::new(0x4a24),
        RequestPriority::Metadata,
    )
}

fn assert_active_open_wait(resources: M1SessionResources, expect_cached_bytes: bool) {
    assert_eq!(resources.opening_jobs(), 1);
    assert_eq!(resources.service_jobs(), 0);
    assert_eq!(resources.waiting_targets(), 1);
    assert_eq!(resources.held_completions(), 0);
    assert_eq!(resources.range_registrations(), 1);
    assert_eq!(resources.range_pending_tickets(), 1);
    if expect_cached_bytes {
        assert!(resources.cached_bytes() > 0);
    } else {
        assert_eq!(resources.cached_bytes(), 0);
    }
    assert!(resources.range_resident_bytes() > 0);
    assert_eq!(resources.cache_entries(), 0);
    assert_eq!(resources.cache_resident_bytes(), 0);
    assert_eq!(resources.index_handles(), 0);
    assert_eq!(resources.resident_bytes(), resources.range_resident_bytes());
}

fn assert_zero_resources(resources: M1SessionResources) {
    assert_eq!(resources.opening_jobs(), 0);
    assert_eq!(resources.service_jobs(), 0);
    assert_eq!(resources.waiting_targets(), 0);
    assert_eq!(resources.held_completions(), 0);
    assert_eq!(resources.range_registrations(), 0);
    assert_eq!(resources.range_pending_tickets(), 0);
    assert_eq!(resources.cached_bytes(), 0);
    assert_eq!(resources.range_resident_bytes(), 0);
    assert_eq!(resources.cache_entries(), 0);
    assert_eq!(resources.cache_resident_bytes(), 0);
    assert_eq!(resources.index_handles(), 0);
    assert_eq!(resources.resident_bytes(), 0);
}

#[test]
fn coalesced_if_range_responses_resume_strict_open_only_on_later_actor_turns() {
    let fixture = fixture(1, 0xb1);
    let mut session = session(&fixture);
    let mut next_id = 0_u64;
    let mut parser_turns = 0_usize;
    let mut exact_requests = 0_usize;
    let mut host_requests = 0_usize;
    let mut reverse_batches = 0_usize;
    let mut ingress_audit: Option<M1OpeningParserAudit> = None;

    loop {
        if let Some(expected) = ingress_audit {
            assert_eq!(session.opening_parser_audit(), Some(expected));
        }
        parser_turns += 1;
        let outcome = session.run_one(&NeverCancelled);
        if let Some(previous) = ingress_audit.take() {
            assert_ne!(
                session.opening_parser_audit(),
                Some(previous),
                "only the explicit actor turn may advance parser phase, stats, or checkpoint"
            );
        }
        match outcome {
            M1SessionRun::WaitingForData {
                owner: M1SessionWait::Opening(request),
                missing,
                ..
            } => {
                assert_eq!(request, OPEN_REQUEST);
                assert_eq!(session.phase(), M1SessionPhase::WaitingForData);
                let parser_audit_before_ingress = session
                    .opening_parser_audit()
                    .expect("opening retains parser audit state while waiting");
                assert!(parser_audit_before_ingress.waiting_checkpoint().is_some());
                let requests = split_missing(fixture.snapshot, missing, &mut next_id);
                let parser_turns_before_ingress = parser_turns;
                let opening_jobs_before_ingress = session.resources().opening_jobs();
                let (mut responses, _observed, groups) = fetch_and_route(&fixture, &requests);
                exact_requests += responses.len();
                host_requests += groups;
                responses.sort_by_key(|response| std::cmp::Reverse(response.range.start()));
                if responses.len() > 1 {
                    reverse_batches += 1;
                    assert!(
                        responses
                            .windows(2)
                            .all(|pair| pair[0].range.start() >= pair[1].range.start())
                    );
                }
                let response_count = responses.len();
                let mut wakes = 0_usize;
                for (index, response) in responses.into_iter().enumerate() {
                    let ingress = session.supply(response.response);
                    match ingress {
                        M1SessionIngress::Accepted { wake_scheduler, .. } => {
                            wakes += usize::from(wake_scheduler);
                            assert_eq!(wake_scheduler, index + 1 == response_count);
                        }
                        other => panic!("valid loopback response must be accepted: {other:?}"),
                    }
                    assert_eq!(parser_turns, parser_turns_before_ingress);
                    assert_eq!(
                        session.opening_parser_audit(),
                        Some(parser_audit_before_ingress),
                        "host ingress cannot mutate parser phase, cumulative stats, or checkpoint"
                    );
                    assert_eq!(
                        session.resources().opening_jobs(),
                        opening_jobs_before_ingress
                    );
                    assert_ne!(session.phase(), M1SessionPhase::Ready);
                }
                assert_eq!(wakes, 1);
                assert_eq!(session.phase(), M1SessionPhase::Opening);
                ingress_audit = Some(parser_audit_before_ingress);
            }
            M1SessionRun::Ready => {
                assert_eq!(session.opening_parser_audit(), None);
                break;
            }
            other => panic!("loopback strict-open must suspend or become Ready: {other:?}"),
        }
    }

    assert!(parser_turns > 1);
    assert!(reverse_batches > 0);
    assert!(exact_requests > host_requests);
    assert_eq!(session.phase(), M1SessionPhase::Ready);
    session
        .request_page_count(PAGE_REQUEST, page_context(), PageTreeLimits::default())
        .expect("admit page-count service");
    session
        .request_outline(OUTLINE_REQUEST, outline_context(), OutlineLimits::default())
        .expect("admit outline service");
    match session.run_one(&NeverCancelled) {
        M1SessionRun::PageCountReady { request, result } => {
            assert_eq!(request, PAGE_REQUEST);
            assert_eq!(result.page_count(), 1);
        }
        other => panic!("page-count service owns the first Ready turn: {other:?}"),
    }
    match session.run_one(&NeverCancelled) {
        M1SessionRun::OutlineReady { request, result } => {
            assert_eq!(request, OUTLINE_REQUEST);
            assert_eq!(result.items().len(), 1);
            assert_eq!(result.items()[0].title(), "Loopback");
        }
        other => panic!("outline service owns the second Ready turn: {other:?}"),
    }
}

#[test]
fn cancellation_rejects_a_response_released_after_the_http_request_started() {
    let fixture = fixture(1, 0xb2);
    let mut session = session(&fixture);
    let missing = match session.run_one(&NeverCancelled) {
        M1SessionRun::WaitingForData { missing, .. } => missing,
        other => panic!("empty session must request source bytes: {other:?}"),
    };
    let owned_before_cancel = session.resources();
    assert_active_open_wait(owned_before_cancel, false);
    assert!(session.opening_parser_audit().is_some());
    let mut next_id = 100_u64;
    let requests = split_missing(fixture.snapshot, missing, &mut next_id);
    let plan = RangeRequestCoalescer::new(fixture.snapshot, 1, Default::default())
        .plan(&requests, &NeverCancelledRangeCoalescer)
        .expect("initial request coalesces");
    let range = plan.groups()[0].range();
    let (seen_tx, seen_rx) = mpsc::sync_channel(1);
    let (release_tx, release_rx) = mpsc::sync_channel(1);
    let (address, server) = spawn_server(
        Arc::clone(&fixture.bytes),
        fixture.etag.clone(),
        1,
        Some(ResponseGate {
            request_seen: seen_tx,
            release_response: release_rx,
        }),
    );
    let if_range = fixture.etag.clone();
    let source_len = fixture.snapshot.len().expect("fixture length is known");
    let client = thread::spawn(move || fetch_range(address, range, &if_range, source_len));
    seen_rx
        .recv_timeout(IO_TIMEOUT)
        .expect("server observed Range before cancellation");

    assert_eq!(
        session.cancel_request(OPEN_REQUEST),
        M1SessionCancel::Cancelled {
            request: OPEN_REQUEST,
            service: None,
        }
    );
    assert_eq!(session.phase(), M1SessionPhase::Failed);
    assert_eq!(session.opening_parser_audit(), None);
    assert_zero_resources(session.resources());
    release_tx.send(()).expect("release delayed HTTP response");
    let late = client
        .join()
        .expect("loopback client thread exits")
        .expect("late HTTP response remains well formed");
    assert_eq!(late.status, 206);
    assert_eq!(late.etag, fixture.etag);
    assert_eq!(
        late.content_range,
        Some(HttpContentRange {
            range,
            total_len: source_len,
        })
    );
    let response = RangeResponse::new(fixture.snapshot, range, late.body)
        .expect("late response geometry validates");
    assert!(matches!(
        session.supply(response),
        M1SessionIngress::Rejected {
            phase: M1SessionPhase::Failed,
            reason: M1SessionIngressRejectReason::TerminalPhase,
        }
    ));
    let observed = server.join().expect("gated server exits cleanly");
    assert_eq!(
        observed,
        vec![ObservedRequest {
            range,
            if_range: fixture.etag.clone(),
        }]
    );
    assert_eq!(session.close(), pdf_rs_session::M1SessionClose::Queued);
    let report = match session.run_one(&NeverCancelled) {
        M1SessionRun::Closed(report) => report,
        other => panic!("cancelled opening closes without parser work: {other:?}"),
    };
    assert_eq!(report.previous_phase(), M1SessionPhase::Failed);
    assert_eq!(report.failure(), Some(M1SessionFailure::OpeningCancelled));
    assert_eq!(report.released_service_jobs(), 0);
    assert_eq!(report.released_waiting_targets(), 0);
    assert_eq!(report.released_held_completions(), 0);
    assert_eq!(report.released_index_handles(), 0);
    assert_eq!(report.cache(), None);
    assert_eq!(report.source(), None);
    let opening = report
        .opening()
        .expect("cancelled strict opening retains nested release evidence");
    assert_eq!(
        opening.previous_phase(),
        pdf_rs_session::StrictBaseOpenCoordinatorPhase::Cancelled
    );
    assert_eq!(opening.owner().released_jobs(), 0);
    assert_eq!(opening.owner().released_waiting_targets(), 0);
    let source = opening
        .source()
        .expect("cancelled opening closes its Range owner");
    assert_eq!(source.released_registrations(), 0);
    assert_eq!(source.released_pending_tickets(), 0);
    assert_eq!(source.released_ready_resumes(), 0);
    assert_eq!(source.released_queued_failures(), 0);
    assert_eq!(source.released_cached_bytes(), 0);
    assert_eq!(source.released_source_resident_bytes(), 0);
    assert_eq!(
        source.released_registration_metadata_bytes(),
        owned_before_cancel.range_resident_bytes()
    );
    assert_eq!(
        source.released_resident_bytes(),
        owned_before_cancel.range_resident_bytes()
    );
    assert_zero_resources(session.resources());
}

#[test]
fn changed_strong_etag_observation_fails_the_old_revision_closed() {
    let local = fixture(7, 0xb3);
    let changed = fixture(8, 0xb4);
    assert_eq!(local.bytes.as_slice(), changed.bytes.as_slice());
    assert_eq!(
        local.snapshot.identity().stable_id(),
        changed.snapshot.identity().stable_id()
    );
    assert_ne!(
        local.snapshot.identity().revision(),
        changed.snapshot.identity().revision()
    );
    assert_ne!(local.etag, changed.etag);

    let mut session = session(&local);
    let missing = match session.run_one(&NeverCancelled) {
        M1SessionRun::WaitingForData { missing, .. } => missing,
        other => panic!("empty session must request source bytes: {other:?}"),
    };
    let mut next_id = 200_u64;
    let requests = split_missing(local.snapshot, missing, &mut next_id);
    let plan = RangeRequestCoalescer::new(local.snapshot, 1, Default::default())
        .plan(&requests, &NeverCancelledRangeCoalescer)
        .expect("old-snapshot request coalesces");
    let range = plan.groups()[0].range();
    assert!(
        range.len() > 1,
        "fixture must permit an incomplete partial fill"
    );
    let partial_range = ByteRange::new(range.start(), 1).expect("one-byte partial Range is valid");
    let (partial_address, partial_server) =
        spawn_server(Arc::clone(&local.bytes), local.etag.clone(), 1, None);
    let partial = fetch_range(
        partial_address,
        partial_range,
        &local.etag,
        local.snapshot.len().expect("fixture length is known"),
    )
    .expect("old snapshot supplies one incomplete Range fragment");
    let audit_before_partial = session
        .opening_parser_audit()
        .expect("waiting opening exposes parser audit evidence");
    assert!(matches!(
        session.supply(
            RangeResponse::new(local.snapshot, partial_range, partial.body)
                .expect("partial response geometry validates")
        ),
        M1SessionIngress::Accepted {
            wake_scheduler: false,
            cached_bytes: 1,
        }
    ));
    assert_eq!(session.opening_parser_audit(), Some(audit_before_partial));
    assert_eq!(session.phase(), M1SessionPhase::WaitingForData);
    let partial_observed = partial_server
        .join()
        .expect("partial old-snapshot server exits cleanly");
    assert_eq!(
        partial_observed,
        vec![ObservedRequest {
            range: partial_range,
            if_range: local.etag.clone(),
        }]
    );
    let owned_before_change = session.resources();
    assert_active_open_wait(owned_before_change, true);
    assert_eq!(owned_before_change.cached_bytes(), 1);

    let (address, server) = spawn_server(Arc::clone(&changed.bytes), changed.etag.clone(), 1, None);
    let response = fetch_range(
        address,
        range,
        &local.etag,
        local.snapshot.len().expect("fixture length is known"),
    )
    .expect("changed HTTP entity returns a complete response");
    assert_eq!(response.status, 200);
    assert_eq!(response.etag, changed.etag);
    assert_eq!(response.content_range, None);
    assert_eq!(response.body, changed.bytes.as_slice());
    let observed = server.join().expect("changed-source server exits cleanly");
    assert_eq!(
        observed,
        vec![ObservedRequest {
            range,
            if_range: local.etag.clone(),
        }]
    );

    assert_eq!(
        session.observe_snapshot(changed.snapshot),
        M1SessionIngress::SourceChanged
    );
    assert_eq!(session.phase(), M1SessionPhase::Failed);
    assert_eq!(session.opening_parser_audit(), None);
    assert_zero_resources(session.resources());
    assert!(matches!(
        session.run_one(&NeverCancelled),
        M1SessionRun::AlreadyTerminal {
            phase: M1SessionPhase::Failed,
        }
    ));
    assert_eq!(session.close(), pdf_rs_session::M1SessionClose::Queued);
    match session.run_one(&NeverCancelled) {
        M1SessionRun::Closed(report) => {
            assert_eq!(report.previous_phase(), M1SessionPhase::Failed);
            assert!(matches!(
                report.failure(),
                Some(M1SessionFailure::SourceChanged(Some(_)))
            ));
            assert_eq!(report.released_service_jobs(), 0);
            assert_eq!(report.released_waiting_targets(), 0);
            assert_eq!(report.released_held_completions(), 0);
            assert_eq!(report.released_index_handles(), 0);
            assert_eq!(report.cache(), None);
            assert_eq!(report.source(), None);
            let opening = report
                .opening()
                .expect("source-changed opening retains nested release evidence");
            assert_eq!(
                opening.previous_phase(),
                pdf_rs_session::StrictBaseOpenCoordinatorPhase::SourceChanged
            );
            assert_eq!(opening.owner().released_jobs(), 0);
            assert_eq!(opening.owner().released_waiting_targets(), 0);
            let source = opening
                .source()
                .expect("source change retains Range release evidence");
            assert_eq!(source.released_registrations(), 1);
            assert_eq!(source.released_pending_tickets(), 1);
            assert_eq!(source.released_ready_resumes(), 0);
            assert_eq!(source.released_queued_failures(), 0);
            assert_eq!(source.released_cached_bytes(), 1);
            assert_eq!(
                source.released_resident_bytes(),
                owned_before_change.range_resident_bytes()
            );
            assert_eq!(
                source.released_registration_metadata_bytes()
                    + source.released_source_resident_bytes(),
                source.released_resident_bytes()
            );
            assert!(source.released_source_resident_bytes() >= 1);
            assert_zero_resources(session.resources());
        }
        other => panic!("changed source closes without parser work: {other:?}"),
    }
}

#[test]
fn bounded_http_parser_rejects_oversize_and_invalid_content_range_geometry() {
    assert!(is_strong_etag(VALID_TEST_ETAG));
    assert!(!is_strong_etag("W/\"strong\""));
    assert!(!is_strong_etag("\"strong\""));
    assert!(!is_strong_etag(
        "\"0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdeg\""
    ));

    let mut oversized_header = b"HTTP/1.1 206 Partial Content\r\nX-Pad: ".to_vec();
    oversized_header.resize(MAX_HTTP_HEADER_BYTES + 5, b'x');
    let error = read_http_response(&mut Cursor::new(oversized_header))
        .expect_err("unterminated oversized response header fails closed");
    assert_eq!(error.kind(), io::ErrorKind::InvalidData);

    let oversized_body = format!(
        "HTTP/1.1 206 Partial Content\r\nETag: {VALID_TEST_ETAG}\r\nContent-Range: bytes 0-0/1\r\nContent-Length: {}\r\n\r\n",
        MAX_HTTP_BODY_BYTES + 1,
    );
    let error = read_http_response(&mut Cursor::new(oversized_body.into_bytes()))
        .expect_err("declared body above the hard ceiling fails before allocation");
    assert_eq!(error.kind(), io::ErrorKind::InvalidData);

    let malformed = format!(
        "HTTP/1.1 206 Partial Content\r\nETag: {VALID_TEST_ETAG}\r\nContent-Range: bytes nope/10\r\nContent-Length: 1\r\n\r\nX"
    );
    let error = read_http_response(&mut Cursor::new(malformed.as_bytes()))
        .expect_err("malformed Content-Range fails closed");
    assert_eq!(error.kind(), io::ErrorKind::InvalidData);

    let requested = ByteRange::new(1, 1).expect("test request is valid");
    let wrong_range = format!(
        "HTTP/1.1 206 Partial Content\r\nETag: {VALID_TEST_ETAG}\r\nContent-Range: bytes 0-0/10\r\nContent-Length: 1\r\n\r\nX"
    );
    let parsed = read_http_response(&mut Cursor::new(wrong_range.as_bytes()))
        .expect("response is syntactically valid");
    let error = validate_http_response(parsed, requested, 10)
        .expect_err("Content-Range must equal the requested Range");
    assert_eq!(error.kind(), io::ErrorKind::InvalidData);

    let wrong_total = format!(
        "HTTP/1.1 206 Partial Content\r\nETag: {VALID_TEST_ETAG}\r\nContent-Range: bytes 1-1/11\r\nContent-Length: 1\r\n\r\nX"
    );
    let parsed = read_http_response(&mut Cursor::new(wrong_total.as_bytes()))
        .expect("response is syntactically valid");
    let error = validate_http_response(parsed, requested, 10)
        .expect_err("Content-Range total must equal the bound snapshot length");
    assert_eq!(error.kind(), io::ErrorKind::InvalidData);

    let wrong_body = format!(
        "HTTP/1.1 206 Partial Content\r\nETag: {VALID_TEST_ETAG}\r\nContent-Range: bytes 1-1/10\r\nContent-Length: 2\r\n\r\nXX"
    );
    let parsed = read_http_response(&mut Cursor::new(wrong_body.as_bytes()))
        .expect("response is syntactically valid");
    let error = validate_http_response(parsed, requested, 10)
        .expect_err("partial response body length must equal Content-Range length");
    assert_eq!(error.kind(), io::ErrorKind::InvalidData);
}
