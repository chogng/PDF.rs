#[cfg(any(target_arch = "wasm32", test))]
use std::cell::RefCell;
#[cfg(any(target_arch = "wasm32", test))]
use std::panic::{AssertUnwindSafe, catch_unwind};

use pdf_rs_protocol::{
    CommandEnvelope, DesktopFrameDecoder, ENVELOPE_HEADER_BYTES, Event, EventEnvelope,
    HandshakeFrameDecoder, MAX_MESSAGE_BYTES, MAX_TRANSFER_SLOTS, MESSAGE_ID_CANCEL_ACKNOWLEDGED,
    MESSAGE_ID_CAPABILITY_REPORTED, MESSAGE_ID_CLOSE_SESSION_ACKNOWLEDGED, MESSAGE_ID_DATA_FAILED,
    MESSAGE_ID_DOCUMENT_READY, MESSAGE_ID_ENGINE_HELLO, MESSAGE_ID_GENERATION_COMPLETED,
    MESSAGE_ID_GENERATION_PLANNED, MESSAGE_ID_NEED_DATA, MESSAGE_ID_PAGE_METRICS,
    MESSAGE_ID_PROTOCOL_FAULT, MESSAGE_ID_READY, MESSAGE_ID_REQUEST_CANCELLED,
    MESSAGE_ID_REQUEST_FAILED, MESSAGE_ID_SESSION_CLOSED, MESSAGE_ID_SHUTDOWN_ACKNOWLEDGED,
    MESSAGE_ID_SURFACE_READY, MESSAGE_ID_SURFACE_RECLAIMED,
    MESSAGE_ID_SURFACE_RELEASE_ACKNOWLEDGED, MESSAGE_ID_WORKER_FAULT, MESSAGE_ID_WORKER_STOPPED,
    PROTOCOL_MAJOR, PROTOCOL_MINOR, PayloadCodecLimits, ProtocolLimits, ProtocolValidator,
    SequenceTracker, SurfaceTransport, encode_cancel_acknowledged_event_payload,
    encode_capability_reported_event_payload, encode_close_session_acknowledged_event_payload,
    encode_correlation_payload, encode_data_failed_event_payload,
    encode_document_ready_event_payload, encode_engine_hello_event_payload,
    encode_generation_completed_event_payload, encode_generation_planned_event_payload,
    encode_need_data_event_payload, encode_page_metrics_event_payload,
    encode_protocol_fault_event_payload, encode_ready_event_payload,
    encode_request_cancelled_event_payload, encode_request_failed_event_payload,
    encode_session_closed_event_payload, encode_shutdown_acknowledged_event_payload,
    encode_surface_ready_event_payload, encode_surface_reclaimed_event_payload,
    encode_surface_release_acknowledged_event_payload, encode_worker_fault_event_payload,
    encode_worker_stopped_event_payload,
};

use crate::{NativeBrowserWorker, NativeBrowserWorkerError, NativeBrowserWorkerPhase};

#[cfg(any(target_arch = "wasm32", test))]
const ABI_STATUS_OK: u32 = 0;
#[cfg(any(target_arch = "wasm32", test))]
const ABI_STATUS_REJECTED: u32 = 0xfffe;
// Reserved for internal unwinds in unwind-capable embeddings. The production
// Wasm build aborts on panic, so a panic traps instead of returning this value.
#[cfg(any(target_arch = "wasm32", test))]
const ABI_STATUS_INTERNAL_UNWIND: u32 = 0xffff;
const ABI_POLL_OUTPUT: u32 = 1;
const ABI_POLL_PENDING: u32 = 2;
#[cfg(target_arch = "wasm32")]
const INVALID_POINTER: u32 = u32::MAX;
const MAX_MAILBOX_INPUT_RETAINED_BYTES: u64 = 64 * 1024 * 1024;

/// Stable Wasm mailbox failure categories.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NativeWorkerMailboxError {
    /// A caller length, slot count, or index exceeded a generated hard limit.
    Limit,
    /// The canonical protocol frame or sequence was invalid.
    Protocol,
    /// The Native adapter rejected the decoded command or actor work.
    Native(NativeBrowserWorkerError),
    /// Output encoding or browser transfer binding failed closed.
    Output,
    /// The mailbox has been irreversibly shut down.
    Closed,
}

/// Same-instance bounded mailbox used by generated JavaScript glue.
///
/// Its raw addresses are exported only on `wasm32` and are consumed immediately
/// by glue in the same Worker and `WebAssembly.Memory`. They never enter a
/// generated protocol frame or browser `postMessage`.
pub struct NativeWorkerMailbox {
    worker: Option<NativeBrowserWorker>,
    input: Vec<u8>,
    input_transfers: Vec<Vec<u8>>,
    input_retained_byte_capacity: u64,
    output: Vec<u8>,
    output_transfers: Vec<Vec<u8>>,
    input_sequence: SequenceTracker,
    next_output_sequence: u64,
    memory_epoch: u32,
    closed: bool,
    #[cfg(test)]
    fail_next_surface_output_after_dequeue: bool,
    #[cfg(test)]
    fail_next_native_poll: bool,
}

impl NativeWorkerMailbox {
    /// Creates one mailbox around an unnegotiated Native adapter.
    pub fn new(worker: NativeBrowserWorker) -> Self {
        Self {
            worker: Some(worker),
            input: Vec::new(),
            input_transfers: Vec::new(),
            input_retained_byte_capacity: MAX_MAILBOX_INPUT_RETAINED_BYTES,
            output: Vec::new(),
            output_transfers: Vec::new(),
            input_sequence: SequenceTracker::new(),
            next_output_sequence: 1,
            memory_epoch: 1,
            closed: false,
            #[cfg(test)]
            fail_next_surface_output_after_dequeue: false,
            #[cfg(test)]
            fail_next_native_poll: false,
        }
    }

    /// Creates one mailbox for an explicitly supplied Worker identity and epochs.
    pub fn for_identity(
        worker: pdf_rs_protocol::WorkerId,
        worker_epoch: pdf_rs_surface::WorkerEpoch,
        renderer_epoch: u32,
    ) -> Result<Self, NativeWorkerMailboxError> {
        NativeBrowserWorker::new(
            worker,
            worker_epoch,
            renderer_epoch,
            pdf_rs_engine::NativeWorkerLimitConfig::default(),
        )
        .map(Self::new)
        .map_err(NativeWorkerMailboxError::Native)
    }

    #[cfg(test)]
    fn test_default() -> Result<Self, NativeWorkerMailboxError> {
        NativeBrowserWorker::test_default()
            .map(Self::new)
            .map_err(NativeWorkerMailboxError::Native)
    }

    /// Resizes the canonical control input and returns its same-instance bytes.
    pub fn prepare_input(&mut self, length: usize) -> Result<&mut [u8], NativeWorkerMailboxError> {
        self.ensure_open()?;
        let next_memory_epoch = self.next_memory_epoch()?;
        let maximum = usize::try_from(MAX_MESSAGE_BYTES)
            .map_err(|_| NativeWorkerMailboxError::Limit)?
            .checked_add(ENVELOPE_HEADER_BYTES)
            .ok_or(NativeWorkerMailboxError::Limit)?;
        if length < ENVELOPE_HEADER_BYTES || length > maximum {
            return Err(NativeWorkerMailboxError::Limit);
        }
        if length <= self.input.capacity() {
            self.ensure_input_retained_capacity()?;
            self.input.resize(length, 0);
        } else {
            let transaction_minimum = self
                .input_retained_capacity()?
                .checked_add(u64::try_from(length).map_err(|_| NativeWorkerMailboxError::Limit)?)
                .ok_or(NativeWorkerMailboxError::Limit)?;
            if transaction_minimum > self.input_retained_byte_capacity {
                return Err(NativeWorkerMailboxError::Limit);
            }
            let mut replacement = Vec::new();
            resize_bounded(&mut replacement, length)?;
            let replacement_capacity = byte_capacity(replacement.capacity())?;
            let transaction_retained = self
                .input_retained_capacity()?
                .checked_add(replacement_capacity)
                .ok_or(NativeWorkerMailboxError::Limit)?;
            if transaction_retained > self.input_retained_byte_capacity {
                return Err(NativeWorkerMailboxError::Limit);
            }
            self.input = replacement;
        }
        self.memory_epoch = next_memory_epoch;
        Ok(&mut self.input)
    }

    /// Resizes one contiguous OOB input slot and returns its same-instance bytes.
    pub fn prepare_transfer(
        &mut self,
        index: usize,
        length: usize,
    ) -> Result<&mut [u8], NativeWorkerMailboxError> {
        self.ensure_open()?;
        let next_memory_epoch = self.next_memory_epoch()?;
        if index >= usize::from(MAX_TRANSFER_SLOTS)
            || length
                > usize::try_from(MAX_MESSAGE_BYTES).map_err(|_| NativeWorkerMailboxError::Limit)?
            || index > self.input_transfers.len()
        {
            return Err(NativeWorkerMailboxError::Limit);
        }
        let existing_capacity = self
            .input_transfers
            .get(index)
            .map(Vec::capacity)
            .unwrap_or(0);
        if index < self.input_transfers.len() && length <= existing_capacity {
            self.ensure_input_retained_capacity()?;
            self.input_transfers
                .get_mut(index)
                .ok_or(NativeWorkerMailboxError::Limit)?
                .resize(length, 0);
        } else {
            let replacement_minimum =
                u64::try_from(length).map_err(|_| NativeWorkerMailboxError::Limit)?;
            let table_minimum = if index == self.input_transfers.len() {
                retained_capacity::<Vec<u8>>(index + 1)?
            } else {
                0
            };
            let transaction_minimum = self
                .input_retained_capacity()?
                .checked_add(replacement_minimum)
                .and_then(|retained| retained.checked_add(table_minimum))
                .ok_or(NativeWorkerMailboxError::Limit)?;
            if transaction_minimum > self.input_retained_byte_capacity {
                return Err(NativeWorkerMailboxError::Limit);
            }
            let mut replacement = Vec::new();
            resize_bounded(&mut replacement, length)?;
            let replacement_capacity = byte_capacity(replacement.capacity())?;
            let transaction_retained = self
                .input_retained_capacity()?
                .checked_add(replacement_capacity)
                .ok_or(NativeWorkerMailboxError::Limit)?;
            if transaction_retained > self.input_retained_byte_capacity {
                return Err(NativeWorkerMailboxError::Limit);
            }
            if index == self.input_transfers.len() {
                let mut replacement_table = Vec::new();
                replacement_table
                    .try_reserve_exact(index + 1)
                    .map_err(|_| NativeWorkerMailboxError::Limit)?;
                let table_capacity = retained_capacity::<Vec<u8>>(replacement_table.capacity())?;
                let transaction_retained = transaction_retained
                    .checked_add(table_capacity)
                    .ok_or(NativeWorkerMailboxError::Limit)?;
                if transaction_retained > self.input_retained_byte_capacity {
                    return Err(NativeWorkerMailboxError::Limit);
                }
                replacement_table.append(&mut self.input_transfers);
                replacement_table.push(replacement);
                self.input_transfers = replacement_table;
            } else {
                self.input_transfers[index] = replacement;
            }
        }
        self.memory_epoch = next_memory_epoch;
        self.input_transfers
            .get_mut(index)
            .map(Vec::as_mut_slice)
            .ok_or(NativeWorkerMailboxError::Limit)
    }

    /// Decodes and dispatches one complete canonical command frame.
    pub fn dispatch(
        &mut self,
        input_length: usize,
        transfer_count: usize,
    ) -> Result<(), NativeWorkerMailboxError> {
        self.ensure_open()?;
        let next_memory_epoch = self.next_memory_epoch()?;
        if input_length != self.input.len()
            || transfer_count > usize::from(MAX_TRANSFER_SLOTS)
            || transfer_count != self.input_transfers.len()
        {
            return Err(NativeWorkerMailboxError::Limit);
        }
        let worker = self
            .worker
            .as_mut()
            .ok_or(NativeWorkerMailboxError::Closed)?;
        let phase = worker.phase();
        let pending = match phase {
            NativeBrowserWorkerPhase::Starting | NativeBrowserWorkerPhase::AwaitingAccept => {
                HandshakeFrameDecoder::new(ProtocolLimits::default())
                    .prepare(&self.input, transfer_count, &self.input_sequence)
                    .map_err(|_| NativeWorkerMailboxError::Protocol)?
            }
            NativeBrowserWorkerPhase::Ready => {
                let handshake = worker
                    .handshake()
                    .ok_or(NativeWorkerMailboxError::Protocol)?;
                DesktopFrameDecoder::for_handshake(handshake)
                    .prepare(&self.input, transfer_count, &self.input_sequence)
                    .map_err(|_| NativeWorkerMailboxError::Protocol)?
            }
            NativeBrowserWorkerPhase::Stopped => {
                return Err(NativeWorkerMailboxError::Closed);
            }
        };
        let envelope: CommandEnvelope = pending
            .decode_command()
            .map_err(|_| NativeWorkerMailboxError::Protocol)?;
        worker
            .handle_command(envelope, &self.input_transfers[..transfer_count])
            .map_err(NativeWorkerMailboxError::Native)?;
        pending
            .commit(&mut self.input_sequence)
            .map_err(|_| NativeWorkerMailboxError::Protocol)?;
        self.input_transfers.clear();
        self.memory_epoch = next_memory_epoch;
        self.refresh_output(false)
    }

    /// Pumps at most one bounded Native actor turn and returns its ABI readiness bitmask.
    ///
    /// Bit zero reports a staged output frame. Bit one reports additional
    /// immediately runnable Native work. Zero means the Worker is idle and
    /// should not be polled again until the Host dispatches another command.
    pub fn poll(&mut self) -> Result<u32, NativeWorkerMailboxError> {
        self.ensure_open()?;
        self.refresh_output(true)?;
        Ok(poll_state(
            !self.output.is_empty(),
            self.worker
                .as_ref()
                .is_some_and(NativeBrowserWorker::has_pending_work),
        ))
    }

    /// Returns the current canonical output frame, or an empty slice when idle.
    pub fn output(&self) -> &[u8] {
        &self.output
    }

    /// Returns browser-owned OOB output payloads in protocol slot order.
    pub fn output_transfers(&self) -> &[Vec<u8>] {
        &self.output_transfers
    }

    /// Returns the monotonic same-instance memory-view epoch.
    pub const fn memory_epoch(&self) -> u32 {
        self.memory_epoch
    }

    /// Irreversibly drops the Native actor and all source, cache, and Surface ownership.
    pub fn shutdown(&mut self) -> Result<(), NativeWorkerMailboxError> {
        if self.closed {
            return Ok(());
        }
        if !self
            .worker
            .as_ref()
            .is_some_and(NativeBrowserWorker::can_dispose)
        {
            return Err(NativeWorkerMailboxError::Native(
                NativeBrowserWorkerError::InvalidLifecycle,
            ));
        }
        self.closed = true;
        self.worker.take();
        self.input.clear();
        self.input_transfers.clear();
        self.output.clear();
        self.output_transfers.clear();
        self.memory_epoch = self.memory_epoch.saturating_add(1);
        Ok(())
    }

    fn ensure_open(&self) -> Result<(), NativeWorkerMailboxError> {
        if self.closed {
            Err(NativeWorkerMailboxError::Closed)
        } else {
            Ok(())
        }
    }

    fn clear_output(&mut self) {
        self.output.clear();
        self.output_transfers.clear();
    }

    fn next_memory_epoch(&self) -> Result<u32, NativeWorkerMailboxError> {
        self.memory_epoch
            .checked_add(1)
            .ok_or(NativeWorkerMailboxError::Limit)
    }

    fn input_retained_capacity(&self) -> Result<u64, NativeWorkerMailboxError> {
        // The hard ceiling covers the control buffer, the transfer-table
        // allocation, and every inner byte buffer by allocator capacity.
        // Growth paths additionally charge both the old and replacement
        // allocations until the final infallible ownership swap.
        let outer = retained_capacity::<Vec<u8>>(self.input_transfers.capacity())?;
        self.input_transfers.iter().try_fold(
            byte_capacity(self.input.capacity())?
                .checked_add(outer)
                .ok_or(NativeWorkerMailboxError::Limit)?,
            |retained, transfer| {
                retained
                    .checked_add(byte_capacity(transfer.capacity())?)
                    .ok_or(NativeWorkerMailboxError::Limit)
            },
        )
    }

    fn ensure_input_retained_capacity(&self) -> Result<(), NativeWorkerMailboxError> {
        if self.input_retained_capacity()? > self.input_retained_byte_capacity {
            Err(NativeWorkerMailboxError::Limit)
        } else {
            Ok(())
        }
    }

    fn refresh_output(&mut self, run_work_turn: bool) -> Result<(), NativeWorkerMailboxError> {
        let next_memory_epoch = self.next_memory_epoch()?;
        #[cfg(test)]
        if run_work_turn && self.fail_next_native_poll {
            self.fail_next_native_poll = false;
            return Err(NativeWorkerMailboxError::Native(
                NativeBrowserWorkerError::Engine,
            ));
        }
        let event = {
            let worker = self
                .worker
                .as_mut()
                .ok_or(NativeWorkerMailboxError::Closed)?;
            if run_work_turn {
                worker.next_event()
            } else {
                worker.take_ready_event()
            }
            .map_err(NativeWorkerMailboxError::Native)?
        };
        let Some(event) = event else {
            self.clear_output();
            self.memory_epoch = next_memory_epoch;
            return Ok(());
        };

        #[cfg(test)]
        let injected_failure = self.fail_next_surface_output_after_dequeue
            && matches!(event.event(), Event::SurfaceReady(_));
        #[cfg(not(test))]
        let injected_failure = false;
        #[cfg(test)]
        if injected_failure {
            self.fail_next_surface_output_after_dequeue = false;
        }

        let staged = if injected_failure {
            Err(NativeWorkerMailboxError::Output)
        } else {
            stage_output(&event, self.next_output_sequence, self.memory_epoch)
        };
        let (staged_output, next_sequence, next_memory_epoch) = match staged {
            Ok(staged) => staged,
            Err(error) => {
                self.worker
                    .as_mut()
                    .ok_or(NativeWorkerMailboxError::Closed)?
                    .reclaim_undelivered_event(&event)
                    .map_err(NativeWorkerMailboxError::Native)?;
                return Err(error);
            }
        };
        let (_, _, transfers) = event.into_parts();
        self.output = staged_output;
        self.output_transfers = transfers;
        self.next_output_sequence = next_sequence;
        self.memory_epoch = next_memory_epoch;
        Ok(())
    }
}

fn byte_capacity(capacity: usize) -> Result<u64, NativeWorkerMailboxError> {
    u64::try_from(capacity).map_err(|_| NativeWorkerMailboxError::Limit)
}

fn retained_capacity<T>(capacity: usize) -> Result<u64, NativeWorkerMailboxError> {
    capacity
        .checked_mul(std::mem::size_of::<T>())
        .and_then(|bytes| u64::try_from(bytes).ok())
        .ok_or(NativeWorkerMailboxError::Limit)
}

fn stage_output(
    event: &crate::BrowserNativeWorkerEvent,
    sequence: u64,
    memory_epoch: u32,
) -> Result<(Vec<u8>, u64, u32), NativeWorkerMailboxError> {
    validate_transfer_binding(event.event(), event.transfers())?;
    let next_sequence = sequence
        .checked_add(1)
        .ok_or(NativeWorkerMailboxError::Limit)?;
    let next_memory_epoch = memory_epoch
        .checked_add(1)
        .ok_or(NativeWorkerMailboxError::Limit)?;
    let message_type = event_message_id(event.event());
    let mut payload =
        encode_correlation_payload(event.correlation(), PayloadCodecLimits::protocol_default())
            .map_err(|_| NativeWorkerMailboxError::Output)?;
    let event_bytes = encode_event_value(event.event())?;
    payload
        .try_reserve_exact(event_bytes.len())
        .map_err(|_| NativeWorkerMailboxError::Limit)?;
    payload.extend_from_slice(&event_bytes);
    let payload_length =
        u32::try_from(payload.len()).map_err(|_| NativeWorkerMailboxError::Limit)?;
    if payload_length > MAX_MESSAGE_BYTES {
        return Err(NativeWorkerMailboxError::Limit);
    }
    let header = pdf_rs_protocol::EnvelopeHeader {
        major: PROTOCOL_MAJOR,
        minor: PROTOCOL_MINOR,
        message_type,
        flags: 0,
        payload_len: payload_length,
        sequence,
    };
    let envelope = EventEnvelope {
        header: header.clone(),
        correlation: event.correlation().clone(),
        event: event.event().clone(),
    };
    ProtocolValidator::new(ProtocolLimits::default())
        .validate_event_payload_correlation(
            &envelope,
            envelope.correlation.worker,
            envelope.correlation.session,
        )
        .map_err(|_| NativeWorkerMailboxError::Output)?;
    let output_length = ENVELOPE_HEADER_BYTES
        .checked_add(payload.len())
        .ok_or(NativeWorkerMailboxError::Limit)?;
    let mut output = Vec::new();
    output
        .try_reserve_exact(output_length)
        .map_err(|_| NativeWorkerMailboxError::Limit)?;
    encode_header(&header, &mut output);
    output.extend_from_slice(&payload);
    Ok((output, next_sequence, next_memory_epoch))
}

fn resize_bounded(bytes: &mut Vec<u8>, length: usize) -> Result<(), NativeWorkerMailboxError> {
    if length > bytes.capacity() {
        bytes
            .try_reserve_exact(length - bytes.len())
            .map_err(|_| NativeWorkerMailboxError::Limit)?;
    }
    bytes.resize(length, 0);
    Ok(())
}

const fn poll_state(has_output: bool, has_pending_work: bool) -> u32 {
    (if has_output { ABI_POLL_OUTPUT } else { 0 })
        | (if has_pending_work {
            ABI_POLL_PENDING
        } else {
            0
        })
}

fn event_message_id(event: &Event) -> u16 {
    match event {
        Event::Ready(_) => MESSAGE_ID_READY,
        Event::NeedData(_) => MESSAGE_ID_NEED_DATA,
        Event::DocumentReady(_) => MESSAGE_ID_DOCUMENT_READY,
        Event::CapabilityReported(_) => MESSAGE_ID_CAPABILITY_REPORTED,
        Event::SurfaceReady(_) => MESSAGE_ID_SURFACE_READY,
        Event::RequestCancelled(_) => MESSAGE_ID_REQUEST_CANCELLED,
        Event::RequestFailed(_) => MESSAGE_ID_REQUEST_FAILED,
        Event::SessionClosed(_) => MESSAGE_ID_SESSION_CLOSED,
        Event::WorkerStopped(_) => MESSAGE_ID_WORKER_STOPPED,
        Event::WorkerFault(_) => MESSAGE_ID_WORKER_FAULT,
        Event::ProtocolFault(_) => MESSAGE_ID_PROTOCOL_FAULT,
        Event::SurfaceReclaimed(_) => MESSAGE_ID_SURFACE_RECLAIMED,
        Event::EngineHello(_) => MESSAGE_ID_ENGINE_HELLO,
        Event::DataFailed(_) => MESSAGE_ID_DATA_FAILED,
        Event::PageMetrics(_) => MESSAGE_ID_PAGE_METRICS,
        Event::GenerationPlanned(_) => MESSAGE_ID_GENERATION_PLANNED,
        Event::GenerationCompleted(_) => MESSAGE_ID_GENERATION_COMPLETED,
        Event::CancelAcknowledged(_) => MESSAGE_ID_CANCEL_ACKNOWLEDGED,
        Event::SurfaceReleaseAcknowledged(_) => MESSAGE_ID_SURFACE_RELEASE_ACKNOWLEDGED,
        Event::CloseSessionAcknowledged(_) => MESSAGE_ID_CLOSE_SESSION_ACKNOWLEDGED,
        Event::ShutdownAcknowledged(_) => MESSAGE_ID_SHUTDOWN_ACKNOWLEDGED,
    }
}

fn encode_event_value(event: &Event) -> Result<Vec<u8>, NativeWorkerMailboxError> {
    let limits = PayloadCodecLimits::protocol_default();
    let result = match event {
        Event::Ready(value) => encode_ready_event_payload(value, limits),
        Event::NeedData(value) => encode_need_data_event_payload(value, limits),
        Event::DocumentReady(value) => encode_document_ready_event_payload(value, limits),
        Event::CapabilityReported(value) => encode_capability_reported_event_payload(value, limits),
        Event::SurfaceReady(value) => encode_surface_ready_event_payload(value, limits),
        Event::RequestCancelled(value) => encode_request_cancelled_event_payload(value, limits),
        Event::RequestFailed(value) => encode_request_failed_event_payload(value, limits),
        Event::SessionClosed(value) => encode_session_closed_event_payload(value, limits),
        Event::WorkerStopped(value) => encode_worker_stopped_event_payload(value, limits),
        Event::WorkerFault(value) => encode_worker_fault_event_payload(value, limits),
        Event::ProtocolFault(value) => encode_protocol_fault_event_payload(value, limits),
        Event::SurfaceReclaimed(value) => encode_surface_reclaimed_event_payload(value, limits),
        Event::EngineHello(value) => encode_engine_hello_event_payload(value, limits),
        Event::DataFailed(value) => encode_data_failed_event_payload(value, limits),
        Event::PageMetrics(value) => encode_page_metrics_event_payload(value, limits),
        Event::GenerationPlanned(value) => encode_generation_planned_event_payload(value, limits),
        Event::GenerationCompleted(value) => {
            encode_generation_completed_event_payload(value, limits)
        }
        Event::CancelAcknowledged(value) => encode_cancel_acknowledged_event_payload(value, limits),
        Event::SurfaceReleaseAcknowledged(value) => {
            encode_surface_release_acknowledged_event_payload(value, limits)
        }
        Event::CloseSessionAcknowledged(value) => {
            encode_close_session_acknowledged_event_payload(value, limits)
        }
        Event::ShutdownAcknowledged(value) => {
            encode_shutdown_acknowledged_event_payload(value, limits)
        }
    };
    result.map_err(|_| NativeWorkerMailboxError::Output)
}

fn validate_transfer_binding(
    event: &Event,
    transfers: &[Vec<u8>],
) -> Result<(), NativeWorkerMailboxError> {
    match event {
        Event::SurfaceReady(event) => {
            let SurfaceTransport::BrowserArrayBuffer {
                slot,
                buffer_length,
            } = event.transport
            else {
                return Err(NativeWorkerMailboxError::Output);
            };
            if slot != 0
                || transfers.len() != 1
                || u64::try_from(transfers[0].len()).ok() != Some(buffer_length)
            {
                return Err(NativeWorkerMailboxError::Output);
            }
        }
        _ if !transfers.is_empty() => return Err(NativeWorkerMailboxError::Output),
        _ => {}
    }
    Ok(())
}

fn encode_header(header: &pdf_rs_protocol::EnvelopeHeader, output: &mut Vec<u8>) {
    output.extend_from_slice(&header.major.to_le_bytes());
    output.extend_from_slice(&header.minor.to_le_bytes());
    output.extend_from_slice(&header.message_type.to_le_bytes());
    output.extend_from_slice(&header.flags.to_le_bytes());
    output.extend_from_slice(&header.payload_len.to_le_bytes());
    output.extend_from_slice(&header.sequence.to_le_bytes());
}

#[cfg(target_arch = "wasm32")]
fn pointer_of(bytes: &mut [u8]) -> u32 {
    if bytes.is_empty() {
        return 0;
    }
    u32::try_from(bytes.as_mut_ptr() as usize).unwrap_or(INVALID_POINTER)
}

#[cfg(any(target_arch = "wasm32", test))]
thread_local! {
    static WASM_MAILBOX: RefCell<Option<NativeWorkerMailbox>> =
        const { RefCell::new(None) };
}

#[cfg(any(target_arch = "wasm32", test))]
fn with_mailbox_mut<T>(operation: impl FnOnce(&mut NativeWorkerMailbox) -> T) -> Option<T> {
    WASM_MAILBOX.with(|mailbox| {
        let mut mailbox = mailbox.try_borrow_mut().ok()?;
        let mailbox = mailbox.as_mut()?;
        Some(operation(mailbox))
    })
}

#[cfg(any(target_arch = "wasm32", test))]
fn abi_status(
    operation: impl FnOnce(&mut NativeWorkerMailbox) -> Result<(), NativeWorkerMailboxError>,
) -> u32 {
    match catch_unwind(AssertUnwindSafe(|| with_mailbox_mut(operation))) {
        Ok(Some(Ok(()))) => ABI_STATUS_OK,
        Ok(Some(Err(_))) | Ok(None) => ABI_STATUS_REJECTED,
        Err(_) => ABI_STATUS_INTERNAL_UNWIND,
    }
}

#[cfg(any(target_arch = "wasm32", test))]
fn abi_initialize(
    worker_low: u32,
    worker_high: u32,
    worker_epoch_low: u32,
    worker_epoch_high: u32,
    renderer_epoch: u32,
) -> u32 {
    match catch_unwind(AssertUnwindSafe(|| {
        WASM_MAILBOX.with(|mailbox| {
            let mut mailbox = mailbox
                .try_borrow_mut()
                .map_err(|_| NativeWorkerMailboxError::Closed)?;
            if mailbox.is_some() {
                return Err(NativeWorkerMailboxError::Closed);
            }
            let worker = join_u64(worker_low, worker_high);
            let worker_epoch = join_u64(worker_epoch_low, worker_epoch_high);
            if worker == 0 || renderer_epoch == 0 {
                return Err(NativeWorkerMailboxError::Native(
                    NativeBrowserWorkerError::Protocol,
                ));
            }
            let worker_epoch = pdf_rs_surface::WorkerEpoch::new(worker_epoch).ok_or(
                NativeWorkerMailboxError::Native(NativeBrowserWorkerError::Protocol),
            )?;
            let initialized = NativeWorkerMailbox::for_identity(
                pdf_rs_protocol::WorkerId::new(worker),
                worker_epoch,
                renderer_epoch,
            )?;
            *mailbox = Some(initialized);
            Ok(())
        })
    })) {
        Ok(Ok(())) => ABI_STATUS_OK,
        Ok(Err(_)) => ABI_STATUS_REJECTED,
        Err(_) => ABI_STATUS_INTERNAL_UNWIND,
    }
}

#[cfg(any(target_arch = "wasm32", test))]
fn join_u64(low: u32, high: u32) -> u64 {
    u64::from(low) | (u64::from(high) << 32)
}

#[cfg(any(target_arch = "wasm32", test))]
fn abi_poll() -> u32 {
    match catch_unwind(AssertUnwindSafe(|| {
        with_mailbox_mut(NativeWorkerMailbox::poll)
    })) {
        Ok(Some(Ok(state))) => state,
        Ok(Some(Err(_))) | Ok(None) => ABI_STATUS_REJECTED,
        Err(_) => ABI_STATUS_INTERNAL_UNWIND,
    }
}

/// Initializes the same-instance mailbox exactly once with its supervisor identity.
///
/// `0` and `0xfffe` mean success and rejection. `0xffff` is reserved for an
/// internal unwind in unwind-capable embeddings. A production Wasm panic traps
/// and therefore does not return an ABI status.
#[cfg(target_arch = "wasm32")]
pub fn wasm_initialize(
    worker_low: u32,
    worker_high: u32,
    worker_epoch_low: u32,
    worker_epoch_high: u32,
    renderer_epoch: u32,
) -> u32 {
    abi_initialize(
        worker_low,
        worker_high,
        worker_epoch_low,
        worker_epoch_high,
        renderer_epoch,
    )
}

/// Resizes the same-Wasm-memory command input and returns its local address.
#[cfg(target_arch = "wasm32")]
pub fn wasm_prepare_input(length: u32) -> u32 {
    match catch_unwind(AssertUnwindSafe(|| {
        with_mailbox_mut(|mailbox| {
            mailbox
                .prepare_input(length as usize)
                .map(pointer_of)
                .unwrap_or(INVALID_POINTER)
        })
    })) {
        Ok(Some(pointer)) => pointer,
        Ok(None) | Err(_) => INVALID_POINTER,
    }
}

/// Resizes one same-Wasm-memory OOB input and returns its local address.
#[cfg(target_arch = "wasm32")]
pub fn wasm_prepare_transfer(index: u32, length: u32) -> u32 {
    match catch_unwind(AssertUnwindSafe(|| {
        with_mailbox_mut(|mailbox| {
            mailbox
                .prepare_transfer(index as usize, length as usize)
                .map(pointer_of)
                .unwrap_or(INVALID_POINTER)
        })
    })) {
        Ok(Some(pointer)) => pointer,
        Ok(None) | Err(_) => INVALID_POINTER,
    }
}

/// Dispatches one prepared command.
///
/// `0` and `0xfffe` mean success and rejection. `0xffff` is reserved for an
/// internal unwind in unwind-capable embeddings. A production Wasm panic traps
/// and therefore does not return an ABI status.
#[cfg(target_arch = "wasm32")]
pub fn wasm_dispatch(length: u32, transfer_count: u32) -> u32 {
    abi_status(|mailbox| mailbox.dispatch(length as usize, transfer_count as usize))
}

/// Pumps one bounded turn and returns idle/output/pending/both as `0`, `1`, `2`, or `3`.
///
/// `0xfffe` means rejection. `0xffff` is reserved for an internal unwind in
/// unwind-capable embeddings. A production Wasm panic traps and therefore does
/// not return an ABI status.
#[cfg(target_arch = "wasm32")]
pub fn wasm_poll() -> u32 {
    abi_poll()
}

/// Returns the same-instance local address of the current output frame.
#[cfg(target_arch = "wasm32")]
pub fn wasm_output_pointer() -> u32 {
    with_mailbox_mut(|mailbox| pointer_of(mailbox.output.as_mut_slice())).unwrap_or(INVALID_POINTER)
}

/// Returns the byte length of the current output frame.
#[cfg(target_arch = "wasm32")]
pub fn wasm_output_length() -> u32 {
    with_mailbox_mut(|mailbox| u32::try_from(mailbox.output.len()).unwrap_or(MAX_MESSAGE_BYTES))
        .unwrap_or(0)
}

/// Returns the number of current browser-owned OOB output buffers.
#[cfg(target_arch = "wasm32")]
pub fn wasm_transfer_count() -> u32 {
    with_mailbox_mut(|mailbox| u32::try_from(mailbox.output_transfers.len()).unwrap_or(u32::MAX))
        .unwrap_or(0)
}

/// Returns the local address of one current OOB output buffer.
#[cfg(target_arch = "wasm32")]
pub fn wasm_transfer_pointer(index: u32) -> u32 {
    with_mailbox_mut(|mailbox| {
        mailbox
            .output_transfers
            .get_mut(index as usize)
            .map(|bytes| pointer_of(bytes.as_mut_slice()))
            .unwrap_or(INVALID_POINTER)
    })
    .unwrap_or(INVALID_POINTER)
}

/// Returns the byte length of one current OOB output buffer.
#[cfg(target_arch = "wasm32")]
pub fn wasm_transfer_length(index: u32) -> u32 {
    with_mailbox_mut(|mailbox| {
        mailbox
            .output_transfers
            .get(index as usize)
            .and_then(|bytes| u32::try_from(bytes.len()).ok())
            .unwrap_or(u32::MAX)
    })
    .unwrap_or(u32::MAX)
}

/// Returns the monotonic same-instance memory-view epoch.
#[cfg(target_arch = "wasm32")]
pub fn wasm_memory_epoch() -> u32 {
    with_mailbox_mut(|mailbox| mailbox.memory_epoch()).unwrap_or(0)
}

/// Irreversibly releases the mailbox and every Native-owned resource.
#[cfg(target_arch = "wasm32")]
pub fn wasm_shutdown() -> u32 {
    abi_status(NativeWorkerMailbox::shutdown)
}

#[cfg(test)]
mod tests {
    use super::{
        ABI_POLL_OUTPUT, ABI_POLL_PENDING, ABI_STATUS_INTERNAL_UNWIND, ABI_STATUS_OK,
        ABI_STATUS_REJECTED, NativeWorkerMailbox, NativeWorkerMailboxError, abi_initialize,
        abi_poll, abi_status, join_u64, poll_state, resize_bounded,
    };
    use crate::{BrowserNativeWorkerEvent, NativeBrowserWorker};
    use pdf_rs_protocol::{
        CapabilityProfileId, CloseSessionCommand, Command, CommandEnvelope, Correlation,
        DataAttachmentRole, DataSegment, ENDPOINT_CAPABILITY_TRANSFERABLE_ARRAY_BUFFER,
        EndpointCapabilities, EndpointRole, Event, GetPageMetricsCommand, HelloAcceptCommand,
        HelloCommand, MAX_MESSAGE_BYTES, MAX_TRANSFER_SLOTS, MESSAGE_ID_CLOSE_SESSION,
        MESSAGE_ID_GET_PAGE_METRICS, MESSAGE_ID_HELLO, MESSAGE_ID_HELLO_ACCEPT, MESSAGE_ID_OPEN,
        MESSAGE_ID_PROVIDE_DATA, MESSAGE_ID_SET_VIEWPORT, MESSAGE_ID_SHUTDOWN, OpenCommand,
        OutputProfile, PROTOCOL_MAJOR, PROTOCOL_MINOR, PageCoordinateSpace, PageGeometry,
        PageRotation, PageViewport, ProtocolHello, ProvideDataCommand, QualityPolicy, RequestId,
        SCHEMA_HASH, SetViewportCommand, ShutdownCommand, SourceDescriptor, SourceIdentity,
        ViewportRequest, WorkerId,
    };

    const SOURCE_BYTES: &[u8] = b"%PDF-1.7\n1 0 obj\n<<>>\nendobj\n%%EOF\n";
    const MAX_TEST_POLL_TURNS: usize = 4_096;

    fn correlation(
        session: Option<pdf_rs_protocol::SessionId>,
        request: Option<RequestId>,
        generation: Option<u64>,
    ) -> Correlation {
        Correlation {
            worker: WorkerId::new(1),
            session,
            request,
            generation,
        }
    }

    fn command(message_type: u16, correlation: Correlation, command: Command) -> CommandEnvelope {
        CommandEnvelope {
            header: pdf_rs_protocol::EnvelopeHeader {
                major: PROTOCOL_MAJOR,
                minor: PROTOCOL_MINOR,
                message_type,
                flags: 0,
                payload_len: 0,
                sequence: 1,
            },
            correlation,
            command,
        }
    }

    fn next_until(
        worker: &mut NativeBrowserWorker,
        predicate: impl Fn(&Event) -> bool,
    ) -> BrowserNativeWorkerEvent {
        for _ in 0..MAX_TEST_POLL_TURNS {
            if let Some(event) = worker.next_event().unwrap()
                && predicate(event.event())
            {
                return event;
            }
            assert!(
                worker.has_pending_work(),
                "Native actor became idle before producing the expected event"
            );
        }
        panic!("Native actor exceeded the bounded test poll budget");
    }

    fn negotiated_worker() -> NativeBrowserWorker {
        let mut worker = NativeBrowserWorker::test_default().unwrap();
        worker
            .handle_command(
                command(
                    MESSAGE_ID_HELLO,
                    correlation(None, None, None),
                    Command::Hello(HelloCommand {
                        hello: ProtocolHello {
                            major: PROTOCOL_MAJOR,
                            minor: PROTOCOL_MINOR,
                            schema_hash: SCHEMA_HASH,
                            endpoint_role: EndpointRole::Host,
                            capabilities: EndpointCapabilities {
                                supported: ENDPOINT_CAPABILITY_TRANSFERABLE_ARRAY_BUFFER,
                                mandatory: ENDPOINT_CAPABILITY_TRANSFERABLE_ARRAY_BUFFER,
                            },
                            max_message_bytes: MAX_MESSAGE_BYTES,
                            max_transfer_slots: MAX_TRANSFER_SLOTS,
                        },
                    }),
                ),
                &[],
            )
            .unwrap();
        assert!(matches!(
            worker.take_ready_event().unwrap().unwrap().event(),
            Event::EngineHello(_)
        ));
        worker
            .handle_command(
                command(
                    MESSAGE_ID_HELLO_ACCEPT,
                    correlation(None, None, None),
                    Command::HelloAccept(HelloAcceptCommand {
                        negotiated_minor: PROTOCOL_MINOR,
                        schema_hash: SCHEMA_HASH,
                    }),
                ),
                &[],
            )
            .unwrap();
        assert!(matches!(
            worker.take_ready_event().unwrap().unwrap().event(),
            Event::Ready(event)
                if event.capability_profiles == vec![CapabilityProfileId::BaselineNative]
                    && event.output_profiles == vec![OutputProfile::Srgb]
        ));
        worker
    }

    fn worker_with_pending_surface() -> (NativeBrowserWorker, pdf_rs_protocol::SessionId) {
        let mut worker = negotiated_worker();
        worker
            .handle_command(
                command(
                    MESSAGE_ID_OPEN,
                    correlation(None, Some(RequestId::new(1)), None),
                    Command::Open(OpenCommand {
                        source: SourceDescriptor {
                            identity: SourceIdentity {
                                stable_id: [7; 32],
                                revision: 1,
                            },
                            length: Some(u64::try_from(SOURCE_BYTES.len()).unwrap()),
                            validator: [9; 32],
                        },
                    }),
                ),
                &[],
            )
            .unwrap();
        let need = next_until(&mut worker, |event| matches!(event, Event::NeedData(_)));
        let (session, need) = match (need.correlation(), need.event()) {
            (
                Correlation {
                    session: Some(session),
                    ..
                },
                Event::NeedData(need),
            ) => (*session, need.clone()),
            other => panic!("expected NeedData, got {other:?}"),
        };
        let segments = need
            .ranges
            .iter()
            .enumerate()
            .map(|(index, range)| DataSegment {
                range: range.clone(),
                slot: u16::try_from(index).unwrap(),
                byte_length: range.len,
                role: DataAttachmentRole::ImmutableRangeBytes,
            })
            .collect::<Vec<_>>();
        let transfers = need
            .ranges
            .iter()
            .map(|range| {
                let start = usize::try_from(range.start).unwrap();
                let end = usize::try_from(range.start + range.len).unwrap();
                SOURCE_BYTES[start..end].to_vec()
            })
            .collect::<Vec<_>>();
        worker
            .handle_command(
                command(
                    MESSAGE_ID_PROVIDE_DATA,
                    correlation(Some(session), None, None),
                    Command::ProvideData(ProvideDataCommand {
                        ticket: need.ticket,
                        source: need.source,
                        segments,
                    }),
                ),
                &transfers,
            )
            .unwrap();
        next_until(&mut worker, |event| {
            matches!(event, Event::DocumentReady(_))
        });
        worker
            .handle_command(
                command(
                    MESSAGE_ID_GET_PAGE_METRICS,
                    correlation(Some(session), Some(RequestId::new(2)), None),
                    Command::GetPageMetrics(GetPageMetricsCommand {
                        document_revision: 1,
                        start_index: 0,
                        max_count: 1,
                    }),
                ),
                &[],
            )
            .unwrap();
        let metrics = next_until(&mut worker, |event| matches!(event, Event::PageMetrics(_)));
        let geometry = match metrics.event() {
            Event::PageMetrics(metrics) => metrics.pages[0].geometry.clone(),
            other => panic!("expected PageMetrics, got {other:?}"),
        };
        worker
            .handle_command(
                command(
                    MESSAGE_ID_SET_VIEWPORT,
                    correlation(Some(session), None, Some(1)),
                    Command::SetViewport(viewport(geometry)),
                ),
                &[],
            )
            .unwrap();
        (worker, session)
    }

    fn viewport(geometry: PageGeometry) -> SetViewportCommand {
        SetViewportCommand {
            viewport: ViewportRequest {
                generation: 1,
                document_revision: 1,
                annotation_revision: 1,
                zoom_numerator: 1,
                zoom_denominator: 1,
                visible_pages: vec![PageViewport {
                    page_index: 0,
                    coordinate_space: PageCoordinateSpace::PdfPointsBottomLeft,
                    geometry,
                    clip_x_milli_points: 0,
                    clip_y_milli_points: 0,
                    clip_width_milli_points: 16_000,
                    clip_height_milli_points: 16_000,
                }],
                quality: QualityPolicy::Full,
                output_profile: OutputProfile::Srgb,
                device_scale_milli: 1_000,
                rotation: PageRotation::Degrees0,
                optional_content_id: 1,
            },
        }
    }

    #[test]
    fn shutdown_is_irreversible_and_idempotent() {
        let mut mailbox = NativeWorkerMailbox::test_default().unwrap();
        mailbox.shutdown().unwrap();
        mailbox.shutdown().unwrap();
        assert!(mailbox.poll().is_err());
    }

    #[test]
    fn shutdown_rejects_a_live_native_registry() {
        let mut mailbox = NativeWorkerMailbox::test_default().unwrap();
        let input = mailbox.prepare_input(20).unwrap();
        input[12] = 1;
        assert!(mailbox.dispatch(20, 0).is_err());
        assert!(mailbox.shutdown().is_ok());
    }

    #[test]
    fn abi_status_values_and_poll_bits_are_stable() {
        assert_eq!(ABI_STATUS_OK, 0);
        assert_eq!(ABI_STATUS_REJECTED, 0xfffe);
        assert_eq!(ABI_STATUS_INTERNAL_UNWIND, 0xffff);
        assert_eq!(join_u64(0x89ab_cdef, 0x0123_4567), 0x0123_4567_89ab_cdef);
        assert_eq!(poll_state(false, false), 0);
        assert_eq!(poll_state(true, false), ABI_POLL_OUTPUT);
        assert_eq!(poll_state(false, true), ABI_POLL_PENDING);
        assert_eq!(poll_state(true, true), ABI_POLL_OUTPUT | ABI_POLL_PENDING);
    }

    #[test]
    fn wasm_mailbox_requires_one_nonzero_explicit_initialization() {
        std::thread::spawn(|| {
            assert_eq!(abi_poll(), ABI_STATUS_REJECTED);
            assert_eq!(abi_status(|_| Ok(())), ABI_STATUS_REJECTED);
            assert_eq!(abi_initialize(0, 0, 1, 0, 1), ABI_STATUS_REJECTED);
            assert_eq!(abi_initialize(1, 0, 0, 0, 1), ABI_STATUS_REJECTED);
            assert_eq!(abi_initialize(1, 0, 1, 0, 0), ABI_STATUS_REJECTED);

            assert_eq!(abi_initialize(0, 7, 0, 9, 11), ABI_STATUS_OK);
            assert_eq!(abi_status(|_| Ok(())), ABI_STATUS_OK);
            assert_eq!(abi_initialize(1, 0, 1, 0, 1), ABI_STATUS_REJECTED);
            assert_eq!(abi_status(NativeWorkerMailbox::shutdown), ABI_STATUS_OK);
            assert_eq!(abi_initialize(1, 0, 1, 0, 1), ABI_STATUS_REJECTED);
        })
        .join()
        .unwrap();
    }

    #[test]
    fn mailbox_poll_reports_idle_without_requiring_a_host_spin_limit() {
        let mut mailbox = NativeWorkerMailbox::new(negotiated_worker());
        assert_eq!(mailbox.poll(), Ok(0));
    }

    #[test]
    fn aggregate_input_transfer_capacity_has_exact_and_one_less_boundaries() {
        let input_length = super::ENVELOPE_HEADER_BYTES;
        let first_length = 4_097;
        let second_length = 8_193;
        let mut first_probe = Vec::new();
        resize_bounded(&mut first_probe, first_length).unwrap();
        let mut second_probe = Vec::new();
        resize_bounded(&mut second_probe, second_length).unwrap();
        let mut first_table_probe = Vec::<Vec<u8>>::new();
        first_table_probe.try_reserve_exact(1).unwrap();
        let mut second_table_probe = Vec::<Vec<u8>>::new();
        second_table_probe.try_reserve_exact(2).unwrap();

        let mut exact = NativeWorkerMailbox::test_default().unwrap();
        exact.prepare_input(input_length).unwrap();
        let exact_capacity = u64::try_from(exact.input.capacity()).unwrap()
            + u64::try_from(first_probe.capacity()).unwrap()
            + u64::try_from(first_table_probe.capacity() * std::mem::size_of::<Vec<u8>>()).unwrap()
            + u64::try_from(second_probe.capacity()).unwrap()
            + u64::try_from(second_table_probe.capacity() * std::mem::size_of::<Vec<u8>>())
                .unwrap();
        exact.input_retained_byte_capacity = exact_capacity;
        exact.prepare_transfer(0, first_length).unwrap();
        exact.prepare_transfer(1, second_length).unwrap();
        assert!(exact.input_retained_capacity().unwrap() <= exact_capacity);

        let mut one_less = NativeWorkerMailbox::test_default().unwrap();
        one_less.prepare_input(input_length).unwrap();
        one_less.input_retained_byte_capacity = exact_capacity - 1;
        one_less.prepare_transfer(0, first_length).unwrap();
        assert_eq!(
            one_less.prepare_transfer(1, second_length),
            Err(NativeWorkerMailboxError::Limit)
        );
        assert_eq!(one_less.input_transfers.len(), 1);
        assert!(
            one_less.input_retained_capacity().unwrap() <= one_less.input_retained_byte_capacity
        );
    }

    #[test]
    fn input_transfer_slot_limit_is_exact() {
        let mut mailbox = NativeWorkerMailbox::test_default().unwrap();
        mailbox.prepare_input(super::ENVELOPE_HEADER_BYTES).unwrap();
        for index in 0..usize::from(MAX_TRANSFER_SLOTS) {
            mailbox.prepare_transfer(index, 0).unwrap();
        }
        assert_eq!(
            mailbox.prepare_transfer(usize::from(MAX_TRANSFER_SLOTS), 0),
            Err(NativeWorkerMailboxError::Limit)
        );
    }

    #[test]
    fn aggregate_preflight_rejects_without_slot_or_epoch_mutation() {
        let mut mailbox = NativeWorkerMailbox::test_default().unwrap();
        mailbox.prepare_input(super::ENVELOPE_HEADER_BYTES).unwrap();
        mailbox.input_retained_byte_capacity = mailbox.input_retained_capacity().unwrap() + 32;
        let epoch = mailbox.memory_epoch();
        assert_eq!(
            mailbox.prepare_transfer(0, 4_096),
            Err(NativeWorkerMailboxError::Limit)
        );
        assert!(mailbox.input_transfers.is_empty());
        assert_eq!(mailbox.memory_epoch(), epoch);
    }

    #[test]
    fn exhausted_memory_epoch_never_mutates_prepared_input_or_transfers() {
        let mut input = NativeWorkerMailbox::test_default().unwrap();
        input.prepare_input(super::ENVELOPE_HEADER_BYTES).unwrap();
        let original = input.input.clone();
        input.memory_epoch = u32::MAX;
        assert_eq!(
            input.prepare_input(super::ENVELOPE_HEADER_BYTES + 1),
            Err(NativeWorkerMailboxError::Limit)
        );
        assert_eq!(input.input, original);
        assert_eq!(input.memory_epoch(), u32::MAX);

        let mut transfers = NativeWorkerMailbox::test_default().unwrap();
        transfers
            .prepare_input(super::ENVELOPE_HEADER_BYTES)
            .unwrap();
        transfers.prepare_transfer(0, 1).unwrap();
        let original_capacity = transfers.input_retained_capacity().unwrap();
        transfers.memory_epoch = u32::MAX;
        assert_eq!(
            transfers.prepare_transfer(1, 1),
            Err(NativeWorkerMailboxError::Limit)
        );
        assert_eq!(transfers.input_transfers.len(), 1);
        assert_eq!(
            transfers.input_retained_capacity().unwrap(),
            original_capacity
        );
        assert_eq!(transfers.memory_epoch(), u32::MAX);
    }

    #[test]
    fn failed_surface_output_rolls_back_lease_resources_and_sequence() {
        let (worker, session) = worker_with_pending_surface();
        let mut mailbox = NativeWorkerMailbox::new(worker);
        mailbox.fail_next_surface_output_after_dequeue = true;

        let mut rejected_sequence = None;
        for _ in 0..MAX_TEST_POLL_TURNS {
            let sequence = mailbox.next_output_sequence;
            match mailbox.poll() {
                Ok(state) => assert_ne!(
                    state & ABI_POLL_PENDING,
                    0,
                    "Native actor became idle before the pending Surface output"
                ),
                Err(NativeWorkerMailboxError::Output) => {
                    assert_eq!(mailbox.next_output_sequence, sequence);
                    rejected_sequence = Some(sequence);
                    break;
                }
                Err(error) => panic!("unexpected mailbox failure: {error:?}"),
            }
        }
        let rejected_sequence =
            rejected_sequence.expect("Surface output exceeded the bounded test poll budget");
        assert_ne!(rejected_sequence, 0);
        assert!(mailbox.output().is_empty());
        assert!(mailbox.output_transfers().is_empty());

        let worker = mailbox.worker.as_ref().unwrap();
        let resources = worker.resources().unwrap();
        assert_eq!(resources.delivered_surface_leases(), 0);
        assert!(resources.surface().has_zero_surface_resources());

        let worker = mailbox.worker.as_mut().unwrap();
        worker
            .handle_command(
                command(
                    MESSAGE_ID_CLOSE_SESSION,
                    correlation(Some(session), None, None),
                    Command::CloseSession(CloseSessionCommand {}),
                ),
                &[],
            )
            .unwrap();
        next_until(worker, |event| matches!(event, Event::SessionClosed(_)));
        worker
            .handle_command(
                command(
                    MESSAGE_ID_SHUTDOWN,
                    correlation(None, None, None),
                    Command::Shutdown(ShutdownCommand { deadline_ms: 1_000 }),
                ),
                &[],
            )
            .unwrap();
        next_until(worker, |event| matches!(event, Event::WorkerStopped(_)));
        assert!(worker.resources().unwrap().has_zero_live_resources());
        assert!(worker.can_dispose());
    }

    #[test]
    fn rejected_dispatch_and_native_poll_preserve_staged_surface_output() {
        let (worker, session) = worker_with_pending_surface();
        let mut mailbox = NativeWorkerMailbox::new(worker);
        let mut staged_surface = false;
        for _ in 0..MAX_TEST_POLL_TURNS {
            let state = mailbox.poll().unwrap();
            assert_eq!(state & ABI_POLL_OUTPUT != 0, !mailbox.output().is_empty());
            if !mailbox.output_transfers().is_empty() {
                staged_surface = true;
                break;
            }
            assert_ne!(
                state & ABI_POLL_PENDING,
                0,
                "Native actor became idle before the pending Surface output"
            );
        }
        assert!(
            staged_surface,
            "Surface output exceeded the bounded test poll budget"
        );
        assert_eq!(mailbox.output_transfers().len(), 1);
        let output = mailbox.output().to_vec();
        let transfers = mailbox.output_transfers().to_vec();
        let sequence = mailbox.next_output_sequence;

        assert_eq!(mailbox.dispatch(1, 0), Err(NativeWorkerMailboxError::Limit));
        assert_eq!(mailbox.output(), output);
        assert_eq!(mailbox.output_transfers(), transfers);
        assert_eq!(mailbox.next_output_sequence, sequence);

        mailbox.fail_next_native_poll = true;
        assert_eq!(
            mailbox.poll(),
            Err(NativeWorkerMailboxError::Native(
                crate::NativeBrowserWorkerError::Engine
            ))
        );
        assert_eq!(mailbox.output(), output);
        assert_eq!(mailbox.output_transfers(), transfers);
        assert_eq!(mailbox.next_output_sequence, sequence);
        let resources = mailbox.worker.as_ref().unwrap().resources().unwrap();
        assert_eq!(resources.delivered_surface_leases(), 1);
        assert!(!resources.surface().has_zero_surface_resources());

        let worker = mailbox.worker.as_mut().unwrap();
        worker
            .handle_command(
                command(
                    MESSAGE_ID_CLOSE_SESSION,
                    correlation(Some(session), None, None),
                    Command::CloseSession(CloseSessionCommand {}),
                ),
                &[],
            )
            .unwrap();
        next_until(worker, |event| matches!(event, Event::SessionClosed(_)));
        worker
            .handle_command(
                command(
                    MESSAGE_ID_SHUTDOWN,
                    correlation(None, None, None),
                    Command::Shutdown(ShutdownCommand { deadline_ms: 1_000 }),
                ),
                &[],
            )
            .unwrap();
        next_until(worker, |event| matches!(event, Event::WorkerStopped(_)));
        assert!(worker.resources().unwrap().has_zero_live_resources());
    }
}
