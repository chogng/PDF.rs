use std::io::{Read, Write};

use pdf_rs_protocol::{
    Command, EndpointCapabilities, EndpointRole, Event, HandshakeFrameDecoder,
    KNOWN_ENDPOINT_CAPABILITIES, MAX_MESSAGE_BYTES, MAX_TRANSFER_SLOTS, ProtocolHello,
    ProtocolLimits, ProtocolValidator, SequenceTracker, WorkerId,
};
use pdf_rs_surface::WorkerEpoch;

use crate::{
    DesktopCapability, DesktopIpcError, DesktopIpcErrorCode, DesktopIpcLimits, DesktopLaunchAuth,
    error::error,
};

const MAGIC: [u8; 4] = *b"PD09";
const VERSION: u8 = 1;
const FIXED_HEADER_BYTES: usize = 4 + 1 + 1 + 2 + 8 + 4 + 8 + 8 + 32 + 4 + 2;
const CAPABILITY_BYTES: usize = 8 + 1 + 1 + 2 + 8 + 8 + 8;

/// Direction bound into each per-launch authenticated desktop record.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum DesktopDirection {
    /// Host-to-child traffic.
    HostToWorker = 1,
    /// Child-to-Host traffic.
    WorkerToHost = 2,
}

/// Immutable record fields bound by the launch authenticator.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DesktopRecordBinding {
    /// Bound traffic direction.
    pub direction: DesktopDirection,
    /// Operating-system PID of the record sender.
    pub sender_pid: u32,
    /// Exact worker lifetime epoch.
    pub worker_epoch: WorkerEpoch,
    /// Monotonic outer transport sequence.
    pub sequence: u64,
}

impl DesktopDirection {
    fn decode(value: u8) -> Result<Self, DesktopIpcError> {
        match value {
            1 => Ok(Self::HostToWorker),
            2 => Ok(Self::WorkerToHost),
            _ => Err(error(DesktopIpcErrorCode::InvalidFrame)),
        }
    }
}

/// One complete outer desktop record before canonical protocol payload dispatch.
#[derive(Eq, PartialEq)]
pub struct DesktopWireRecord {
    direction: DesktopDirection,
    launch: crate::DesktopLaunchId,
    sender_pid: u32,
    worker_epoch: WorkerEpoch,
    sequence: u64,
    frame: Vec<u8>,
    capabilities: Vec<DesktopCapability>,
    token: [u8; 32],
}

impl DesktopWireRecord {
    /// Returns immutable canonical payload bytes.
    pub fn frame(&self) -> &[u8] {
        &self.frame
    }
    /// Returns the immutable capability descriptor table.
    pub fn capabilities(&self) -> &[DesktopCapability] {
        &self.capabilities
    }
    /// Returns the outer record sequence.
    pub const fn sequence(&self) -> u64 {
        self.sequence
    }
    /// Returns the worker epoch bound to the record.
    pub const fn worker_epoch(&self) -> WorkerEpoch {
        self.worker_epoch
    }
    /// Consumes a validated record and returns only its canonical payload.
    pub fn into_frame(self) -> Vec<u8> {
        self.frame
    }
    /// Creates a record whose credentials are bound to one launch.
    pub fn new(
        auth: &DesktopLaunchAuth,
        binding: DesktopRecordBinding,
        frame: Vec<u8>,
        capabilities: Vec<DesktopCapability>,
        limits: DesktopIpcLimits,
    ) -> Result<Self, DesktopIpcError> {
        if binding.sender_pid == 0
            || binding.sequence == 0
            || frame.is_empty()
            || capabilities.len() > limits.max_capabilities()
        {
            return Err(error(DesktopIpcErrorCode::InvalidFrame));
        }
        validate_capabilities(
            binding.direction,
            binding.worker_epoch,
            &capabilities,
            limits,
        )?;
        let record = Self {
            direction: binding.direction,
            launch: auth.launch(),
            sender_pid: binding.sender_pid,
            worker_epoch: binding.worker_epoch,
            sequence: binding.sequence,
            frame,
            capabilities,
            token: *auth.token(),
        };
        if record.encoded_len()? > limits.max_record_bytes() {
            return Err(error(DesktopIpcErrorCode::ResourceLimit));
        }
        Ok(record)
    }

    /// Writes one length-prefixed exact record; all lengths are checked before allocation.
    pub fn write_to(
        &self,
        writer: &mut impl Write,
        limits: DesktopIpcLimits,
    ) -> Result<(), DesktopIpcError> {
        validate_capabilities(
            self.direction,
            self.worker_epoch,
            &self.capabilities,
            limits,
        )?;
        let length = self.encoded_len()?;
        if length > limits.max_record_bytes() {
            return Err(error(DesktopIpcErrorCode::ResourceLimit));
        }
        writer
            .write_all(
                &u32::try_from(length)
                    .map_err(|_| error(DesktopIpcErrorCode::ResourceLimit))?
                    .to_le_bytes(),
            )
            .map_err(|_| error(DesktopIpcErrorCode::Disconnected))?;
        writer
            .write_all(&MAGIC)
            .map_err(|_| error(DesktopIpcErrorCode::Disconnected))?;
        writer
            .write_all(&[VERSION, self.direction as u8])
            .map_err(|_| error(DesktopIpcErrorCode::Disconnected))?;
        writer
            .write_all(&0_u16.to_le_bytes())
            .map_err(|_| error(DesktopIpcErrorCode::Disconnected))?;
        writer
            .write_all(&self.launch.value().to_le_bytes())
            .map_err(|_| error(DesktopIpcErrorCode::Disconnected))?;
        writer
            .write_all(&self.sender_pid.to_le_bytes())
            .map_err(|_| error(DesktopIpcErrorCode::Disconnected))?;
        writer
            .write_all(&self.worker_epoch.value().to_le_bytes())
            .map_err(|_| error(DesktopIpcErrorCode::Disconnected))?;
        writer
            .write_all(&self.sequence.to_le_bytes())
            .map_err(|_| error(DesktopIpcErrorCode::Disconnected))?;
        writer
            .write_all(&self.token)
            .map_err(|_| error(DesktopIpcErrorCode::Disconnected))?;
        writer
            .write_all(
                &u32::try_from(self.frame.len())
                    .map_err(|_| error(DesktopIpcErrorCode::ResourceLimit))?
                    .to_le_bytes(),
            )
            .map_err(|_| error(DesktopIpcErrorCode::Disconnected))?;
        writer
            .write_all(
                &u16::try_from(self.capabilities.len())
                    .map_err(|_| error(DesktopIpcErrorCode::ResourceLimit))?
                    .to_le_bytes(),
            )
            .map_err(|_| error(DesktopIpcErrorCode::Disconnected))?;
        writer
            .write_all(&self.frame)
            .map_err(|_| error(DesktopIpcErrorCode::Disconnected))?;
        for capability in &self.capabilities {
            write_capability(writer, *capability)?;
        }
        writer
            .flush()
            .map_err(|_| error(DesktopIpcErrorCode::Disconnected))
    }

    /// Reads and authenticates the fixed header before allocating any payload.
    ///
    /// This is the production receive path.  It deliberately leaves sequence
    /// advancement to the caller, which must first validate canonical payload
    /// semantics and every imported out-of-band descriptor.
    pub fn read_authenticated_from(
        reader: &mut impl Read,
        limits: DesktopIpcLimits,
        auth: &DesktopLaunchAuth,
        expected_sender_pid: u32,
        direction: DesktopDirection,
        epoch: WorkerEpoch,
        last_sequence: &mut Option<u64>,
    ) -> Result<Self, DesktopIpcError> {
        let mut prefix = [0_u8; 4];
        reader
            .read_exact(&mut prefix)
            .map_err(|_| error(DesktopIpcErrorCode::Disconnected))?;
        let length = usize::try_from(u32::from_le_bytes(prefix))
            .map_err(|_| error(DesktopIpcErrorCode::InvalidFrame))?;
        if length < FIXED_HEADER_BYTES || length > limits.max_record_bytes() {
            return Err(error(DesktopIpcErrorCode::InvalidFrame));
        }
        let mut header = [0_u8; FIXED_HEADER_BYTES];
        reader
            .read_exact(&mut header)
            .map_err(|_| error(DesktopIpcErrorCode::Disconnected))?;
        let (record, frame_length, capability_count) = decode_header(&header, limits)?;
        record.authenticate(auth, expected_sender_pid, direction, epoch, last_sequence)?;
        validate_record_length(length, frame_length, capability_count)?;
        finish_read(reader, record, frame_length, capability_count, limits)
    }

    /// Prepares peer identity, token, direction, epoch, and monotonic outer sequence.
    /// The caller must commit only after canonical frame and OOB resources validate.
    pub fn authenticate(
        &self,
        auth: &DesktopLaunchAuth,
        expected_sender_pid: u32,
        direction: DesktopDirection,
        epoch: WorkerEpoch,
        last_sequence: &mut Option<u64>,
    ) -> Result<(), DesktopIpcError> {
        if self.launch != auth.launch()
            || self.sender_pid != expected_sender_pid
            || self.direction != direction
            || self.worker_epoch != epoch
            || !auth.matches(&self.token)
        {
            return Err(error(DesktopIpcErrorCode::Authentication));
        }
        if last_sequence.is_some_and(|last| self.sequence <= last) || self.sequence == 0 {
            return Err(error(DesktopIpcErrorCode::Sequence));
        }
        Ok(())
    }

    /// Commits an already prepared outer sequence after all dependent validation succeeds.
    pub fn commit_outer_sequence(
        &self,
        last_sequence: &mut Option<u64>,
    ) -> Result<(), DesktopIpcError> {
        if last_sequence.is_some_and(|last| self.sequence <= last) || self.sequence == 0 {
            return Err(error(DesktopIpcErrorCode::Sequence));
        }
        *last_sequence = Some(self.sequence);
        Ok(())
    }

    fn encoded_len(&self) -> Result<usize, DesktopIpcError> {
        FIXED_HEADER_BYTES
            .checked_add(self.frame.len())
            .and_then(|value| {
                value.checked_add(self.capabilities.len().checked_mul(CAPABILITY_BYTES)?)
            })
            .ok_or_else(|| error(DesktopIpcErrorCode::ResourceLimit))
    }
}

/// Validates the generated canonical frame and only then commits its protocol sequence.
pub fn validate_host_hello_command(
    frame: &[u8],
    transfer_slots: usize,
    worker: WorkerId,
    sequence: &mut SequenceTracker,
) -> Result<(), DesktopIpcError> {
    let pending = HandshakeFrameDecoder::new(ProtocolLimits::default())
        .prepare(frame, transfer_slots, sequence)
        .map_err(|_| error(DesktopIpcErrorCode::InvalidFrame))?;
    let command = pending
        .decode_command()
        .map_err(|_| error(DesktopIpcErrorCode::InvalidFrame))?;
    let Command::Hello(hello) = command.command else {
        return Err(error(DesktopIpcErrorCode::InvalidFrame));
    };
    let local = ProtocolHello {
        major: pdf_rs_protocol::PROTOCOL_MAJOR,
        minor: pdf_rs_protocol::PROTOCOL_MINOR,
        schema_hash: pdf_rs_protocol::SCHEMA_HASH,
        endpoint_role: EndpointRole::Engine,
        capabilities: EndpointCapabilities {
            supported: KNOWN_ENDPOINT_CAPABILITIES,
            mandatory: 0,
        },
        max_message_bytes: MAX_MESSAGE_BYTES,
        max_transfer_slots: MAX_TRANSFER_SLOTS,
    };
    let validator = ProtocolValidator::new(ProtocolLimits::default());
    validator
        .validate_handshake(&local, &hello.hello)
        .map_err(|_| error(DesktopIpcErrorCode::InvalidFrame))?;
    validator
        .validate_correlation(
            command.header.message_type,
            &command.correlation,
            worker,
            None,
        )
        .map_err(|_| error(DesktopIpcErrorCode::InvalidFrame))?;
    pending
        .commit(sequence)
        .map_err(|_| error(DesktopIpcErrorCode::Sequence))?;
    Ok(())
}

/// Validates the Engine's generated `EngineHello` event before Host dispatch.
pub fn validate_engine_hello_event(
    frame: &[u8],
    transfer_slots: usize,
    worker: WorkerId,
    sequence: &mut SequenceTracker,
) -> Result<(), DesktopIpcError> {
    let pending = HandshakeFrameDecoder::new(ProtocolLimits::default())
        .prepare(frame, transfer_slots, sequence)
        .map_err(|_| error(DesktopIpcErrorCode::InvalidFrame))?;
    let event = pending
        .decode_event()
        .map_err(|_| error(DesktopIpcErrorCode::InvalidFrame))?;
    let Event::EngineHello(engine_hello) = event.event else {
        return Err(error(DesktopIpcErrorCode::InvalidFrame));
    };
    let local = ProtocolHello {
        major: pdf_rs_protocol::PROTOCOL_MAJOR,
        minor: pdf_rs_protocol::PROTOCOL_MINOR,
        schema_hash: pdf_rs_protocol::SCHEMA_HASH,
        endpoint_role: EndpointRole::Host,
        capabilities: EndpointCapabilities {
            supported: KNOWN_ENDPOINT_CAPABILITIES,
            mandatory: 0,
        },
        max_message_bytes: MAX_MESSAGE_BYTES,
        max_transfer_slots: MAX_TRANSFER_SLOTS,
    };
    let validator = ProtocolValidator::new(ProtocolLimits::default());
    validator
        .validate_handshake(&local, &engine_hello.hello)
        .map_err(|_| error(DesktopIpcErrorCode::InvalidFrame))?;
    validator
        .validate_correlation(event.header.message_type, &event.correlation, worker, None)
        .map_err(|_| error(DesktopIpcErrorCode::InvalidFrame))?;
    pending
        .commit(sequence)
        .map_err(|_| error(DesktopIpcErrorCode::Sequence))?;
    Ok(())
}

fn write_capability(
    writer: &mut impl Write,
    capability: DesktopCapability,
) -> Result<(), DesktopIpcError> {
    writer
        .write_all(&capability.id().to_le_bytes())
        .map_err(|_| error(DesktopIpcErrorCode::Disconnected))?;
    writer
        .write_all(&[capability.class() as u8, capability.rights() as u8, 0, 0])
        .map_err(|_| error(DesktopIpcErrorCode::Disconnected))?;
    writer
        .write_all(&capability.owner().value().to_le_bytes())
        .map_err(|_| error(DesktopIpcErrorCode::Disconnected))?;
    writer
        .write_all(&capability.worker_epoch().value().to_le_bytes())
        .map_err(|_| error(DesktopIpcErrorCode::Disconnected))?;
    writer
        .write_all(&capability.byte_length().to_le_bytes())
        .map_err(|_| error(DesktopIpcErrorCode::Disconnected))
}

fn finish_read(
    reader: &mut impl Read,
    mut record: DesktopWireRecord,
    frame_length: usize,
    capability_count: usize,
    limits: DesktopIpcLimits,
) -> Result<DesktopWireRecord, DesktopIpcError> {
    let mut frame = Vec::new();
    frame
        .try_reserve_exact(frame_length)
        .map_err(|_| error(DesktopIpcErrorCode::ResourceLimit))?;
    frame.resize(frame_length, 0);
    reader
        .read_exact(&mut frame)
        .map_err(|_| error(DesktopIpcErrorCode::Disconnected))?;
    let mut capabilities = Vec::new();
    capabilities
        .try_reserve_exact(capability_count)
        .map_err(|_| error(DesktopIpcErrorCode::ResourceLimit))?;
    for _ in 0..capability_count {
        let mut bytes = [0_u8; CAPABILITY_BYTES];
        reader
            .read_exact(&mut bytes)
            .map_err(|_| error(DesktopIpcErrorCode::Disconnected))?;
        let capability = read_capability(&bytes)?;
        if capabilities
            .iter()
            .any(|existing: &DesktopCapability| existing.id() == capability.id())
        {
            return Err(error(DesktopIpcErrorCode::Capability));
        }
        capabilities.push(capability);
    }
    record.frame = frame;
    record.capabilities = capabilities;
    validate_capabilities(
        record.direction,
        record.worker_epoch,
        &record.capabilities,
        limits,
    )?;
    Ok(record)
}

fn validate_capabilities(
    direction: DesktopDirection,
    worker_epoch: WorkerEpoch,
    capabilities: &[DesktopCapability],
    limits: DesktopIpcLimits,
) -> Result<(), DesktopIpcError> {
    if capabilities.len() > limits.max_capabilities() {
        return Err(error(DesktopIpcErrorCode::ResourceLimit));
    }
    let mut total = 0_u64;
    for descriptor in capabilities {
        let expected = match direction {
            DesktopDirection::HostToWorker => crate::CapabilityClass::SourceSegment,
            DesktopDirection::WorkerToHost => crate::CapabilityClass::SurfaceRegion,
        };
        if descriptor.class() != expected || descriptor.worker_epoch() != worker_epoch {
            return Err(error(DesktopIpcErrorCode::Capability));
        }
        total = total
            .checked_add(descriptor.byte_length())
            .ok_or_else(|| error(DesktopIpcErrorCode::ResourceLimit))?;
    }
    if total
        > u64::try_from(limits.max_capability_bytes())
            .map_err(|_| error(DesktopIpcErrorCode::ResourceLimit))?
    {
        return Err(error(DesktopIpcErrorCode::ResourceLimit));
    }
    Ok(())
}

fn validate_record_length(
    length: usize,
    frame_length: usize,
    capability_count: usize,
) -> Result<(), DesktopIpcError> {
    let expected = FIXED_HEADER_BYTES
        .checked_add(frame_length)
        .and_then(|value| value.checked_add(capability_count.checked_mul(CAPABILITY_BYTES)?))
        .ok_or_else(|| error(DesktopIpcErrorCode::InvalidFrame))?;
    if length != expected {
        return Err(error(DesktopIpcErrorCode::InvalidFrame));
    }
    Ok(())
}

impl core::fmt::Debug for DesktopWireRecord {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter
            .debug_struct("DesktopWireRecord")
            .field("direction", &self.direction)
            .field("launch", &self.launch)
            .field("sender_pid", &self.sender_pid)
            .field("worker_epoch", &self.worker_epoch)
            .field("sequence", &self.sequence)
            .field("frame_length", &self.frame.len())
            .field("capability_count", &self.capabilities.len())
            .field("token", &"[REDACTED]")
            .finish()
    }
}

fn decode_header(
    body: &[u8; FIXED_HEADER_BYTES],
    limits: DesktopIpcLimits,
) -> Result<(DesktopWireRecord, usize, usize), DesktopIpcError> {
    if body.get(..4) != Some(&MAGIC) || body.get(4) != Some(&VERSION) {
        return Err(error(DesktopIpcErrorCode::InvalidFrame));
    }
    let direction = DesktopDirection::decode(
        *body
            .get(5)
            .ok_or_else(|| error(DesktopIpcErrorCode::InvalidFrame))?,
    )?;
    if body.get(6..8) != Some(&[0, 0]) {
        return Err(error(DesktopIpcErrorCode::InvalidFrame));
    }
    let launch = crate::DesktopLaunchId::from_bootstrap(u64::from_le_bytes(read(body, 8)?))
        .ok_or_else(|| error(DesktopIpcErrorCode::InvalidFrame))?;
    let sender_pid = u32::from_le_bytes(read(body, 16)?);
    let worker_epoch = WorkerEpoch::new(u64::from_le_bytes(read(body, 20)?))
        .ok_or_else(|| error(DesktopIpcErrorCode::InvalidFrame))?;
    let sequence = u64::from_le_bytes(read(body, 28)?);
    let token: [u8; 32] = body
        .get(36..68)
        .and_then(|value| value.try_into().ok())
        .ok_or_else(|| error(DesktopIpcErrorCode::InvalidFrame))?;
    let frame_length = usize::try_from(u32::from_le_bytes(read(body, 68)?))
        .map_err(|_| error(DesktopIpcErrorCode::InvalidFrame))?;
    let capability_count = usize::from(u16::from_le_bytes(read(body, 72)?));
    if sender_pid == 0 || sequence == 0 || capability_count > limits.max_capabilities() {
        return Err(error(DesktopIpcErrorCode::InvalidFrame));
    }
    Ok((
        DesktopWireRecord {
            direction,
            launch,
            sender_pid,
            worker_epoch,
            sequence,
            frame: Vec::new(),
            capabilities: Vec::new(),
            token,
        },
        frame_length,
        capability_count,
    ))
}

fn read<const N: usize>(body: &[u8], offset: usize) -> Result<[u8; N], DesktopIpcError> {
    body.get(offset..offset + N)
        .and_then(|value| value.try_into().ok())
        .ok_or_else(|| error(DesktopIpcErrorCode::InvalidFrame))
}

fn read_capability(bytes: &[u8]) -> Result<DesktopCapability, DesktopIpcError> {
    let id = u64::from_le_bytes(read(bytes, 0)?);
    let class = match bytes.get(8).copied() {
        Some(1) => crate::CapabilityClass::SourceSegment,
        Some(2) => crate::CapabilityClass::SurfaceRegion,
        _ => return Err(error(DesktopIpcErrorCode::Capability)),
    };
    if bytes.get(9) != Some(&1) || bytes.get(10..12) != Some(&[0, 0]) {
        return Err(error(DesktopIpcErrorCode::Capability));
    }
    let owner = pdf_rs_protocol::SessionId::new(u64::from_le_bytes(read(bytes, 12)?));
    let epoch = WorkerEpoch::new(u64::from_le_bytes(read(bytes, 20)?))
        .ok_or_else(|| error(DesktopIpcErrorCode::Capability))?;
    DesktopCapability::new(
        id,
        class,
        crate::CapabilityRights::ReadOnly,
        owner,
        epoch,
        u64::from_le_bytes(read(bytes, 28)?),
    )
}
