use std::collections::{BTreeMap, VecDeque};
use std::fmt;
use std::num::NonZeroU32;
use std::sync::Arc;

use pdf_rs_bytes::{SourceIdentity, SourceRevision, SourceStableId};
use pdf_rs_engine::{
    NativePolicyTask, NativeRasterTask, NativeTaskPoll, NativeWorkerConfig, NativeWorkerEvent,
    NativeWorkerLimitConfig, NativeWorkerRegistry, OpenCompletion, Reentry,
};
use pdf_rs_policy::{NeverCancelled as NeverCancelledPolicy, PolicyPollBudget};
use pdf_rs_protocol::{
    CapabilityProfileId, Command, CommandEnvelope, Correlation, DataPriority, DataTicket,
    ENDPOINT_CAPABILITY_TRANSFERABLE_ARRAY_BUFFER, EndpointCapabilities, EndpointRole,
    EngineExecutionCapabilities, EngineHelloEvent, Event, FailDataCommand, HelloAcceptCommand,
    MAX_DATA_SEGMENT_BYTES, MAX_DATA_TICKET_BYTES, MAX_MESSAGE_BYTES, MAX_TRANSFER_SLOTS,
    MESSAGE_ID_HELLO, MESSAGE_ID_HELLO_ACCEPT, NeedDataEvent, OpenCommand, OutputProfile,
    PROTOCOL_MAJOR, PROTOCOL_MINOR, ProtocolHello, ProtocolValidator, ProvideDataCommand,
    ReadyEvent, RequestId, SCHEMA_HASH, SessionId, SourceDescriptor, SurfaceReadyEvent,
    SurfaceTransport, WorkerId,
};
use pdf_rs_raster::fast::{FastRasterPollBudget, NeverCancelled};
use pdf_rs_scene::{
    BlendMode, CapabilityContext, CapabilityStatus, CommandSource, DeviceColor, FillRule,
    GraphicsCapability, GraphicsSceneBuilder, GraphicsSceneLimits, Matrix, PageGeometry,
    PageRotation, Paint, PathResource, PathSegment, Scene, SceneBinding, SceneBounds, ScenePoint,
    SceneRect, SceneScalar, SceneUnit,
};
use pdf_rs_surface::WorkerEpoch;
use pdf_rs_syntax::{InputExtent, ObjectRef, SyntaxInput, SyntaxLimits, SyntaxParser, SyntaxPoll};

const DOCUMENT_REVISION: u64 = 1;
const REVISION_STARTXREF: u64 = 1;
const PAGE_INDEX: u32 = 0;
const MAX_BROWSER_SURFACE_COPY_BYTES: u64 = 64 * 1024 * 1024;
const NATIVE_POLICY_POLL_WORK_UNITS: u32 = 64;
const NATIVE_RASTER_POLL_WORK_UNITS: u32 = 64;

/// Stable adapter-layer rejection categories.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NativeBrowserWorkerError {
    /// A command or handshake value violated the generated protocol.
    Protocol,
    /// The command is not legal in the current adapter lifecycle.
    InvalidLifecycle,
    /// Immutable source metadata, bytes, or revision identity drifted.
    Source,
    /// Source or message work exceeded an explicit hard ceiling.
    Limit,
    /// PDF syntax parsing failed before a complete immutable Scene existed.
    Parse,
    /// Immutable Scene construction failed.
    Scene,
    /// Native scheduling, policy, raster, cache, or lifecycle work failed.
    Engine,
    /// A one-shot Native Surface could not be acquired for browser transfer.
    Surface,
}

/// Browser-facing lifecycle for one exact Native Worker epoch.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NativeBrowserWorkerPhase {
    /// Waiting for the exact generated Host Hello transcript.
    Starting,
    /// Hello is compatible and the exact Host accept is required.
    AwaitingAccept,
    /// The Native registry exists and accepts product commands.
    Ready,
    /// Shutdown completed and no further work is accepted.
    Stopped,
}

/// One generated protocol event plus browser-owned out-of-band byte transfers.
#[derive(Eq, PartialEq)]
pub struct BrowserNativeWorkerEvent {
    correlation: Correlation,
    event: Event,
    transfers: Vec<Vec<u8>>,
}

impl fmt::Debug for BrowserNativeWorkerEvent {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let transfer_lengths = self.transfers.iter().map(Vec::len).collect::<Vec<_>>();
        formatter
            .debug_struct("BrowserNativeWorkerEvent")
            .field("correlation", &self.correlation)
            .field("event", &self.event)
            .field("transfer_lengths", &transfer_lengths)
            .finish()
    }
}

impl BrowserNativeWorkerEvent {
    /// Returns the exact protocol correlation.
    pub const fn correlation(&self) -> &Correlation {
        &self.correlation
    }

    /// Returns the generated protocol event.
    pub const fn event(&self) -> &Event {
        &self.event
    }

    /// Returns immutable browser transfer payloads in protocol slot order.
    pub fn transfers(&self) -> &[Vec<u8>] {
        &self.transfers
    }

    /// Consumes the event into its protocol and transfer components.
    pub fn into_parts(self) -> (Correlation, Event, Vec<Vec<u8>>) {
        (self.correlation, self.event, self.transfers)
    }
}

#[derive(Clone, Debug)]
struct PendingSource {
    descriptor: SourceDescriptor,
    session: SessionId,
    request: RequestId,
    ticket: DataTicket,
}

/// Complete immutable source bytes admitted for a later explicit parse turn.
struct PendingParse {
    session: SessionId,
    bytes: Vec<u8>,
    charged_bytes: u64,
}

enum PendingParsePhase {
    ValidateHeader,
    BuildScene { major: u8, minor: u8 },
}

struct ActiveParse {
    pending: PendingParse,
    phase: PendingParsePhase,
}

/// Single-writer adapter from generated browser commands into the Native engine registry.
pub struct NativeBrowserWorker {
    phase: NativeBrowserWorkerPhase,
    worker: WorkerId,
    worker_epoch: WorkerEpoch,
    renderer_epoch: u32,
    limits: NativeWorkerLimitConfig,
    validator: ProtocolValidator,
    local_hello: ProtocolHello,
    handshake: Option<pdf_rs_protocol::CompatibleHandshake>,
    registry: Option<NativeWorkerRegistry>,
    queued: VecDeque<BrowserNativeWorkerEvent>,
    pending_sources: BTreeMap<SessionId, PendingSource>,
    pending_parse: VecDeque<PendingParse>,
    active_parse: Option<ActiveParse>,
    pending_parse_bytes: u64,
    pending_parse_byte_capacity: u64,
    browser_surface_copy_byte_capacity: u64,
    active_policy_task: Option<NativePolicyTask>,
    active_raster_task: Option<NativeRasterTask>,
    pending_reentries: VecDeque<Reentry>,
    next_ticket: u64,
}

impl NativeBrowserWorker {
    /// Creates an inert adapter. The Native registry is created only after exact negotiation.
    pub fn new(
        worker: WorkerId,
        worker_epoch: WorkerEpoch,
        renderer_epoch: u32,
        limits: NativeWorkerLimitConfig,
    ) -> Result<Self, NativeBrowserWorkerError> {
        let browser_surface_copy_byte_capacity = limits
            .retained_raster_byte_capacity
            .min(MAX_BROWSER_SURFACE_COPY_BYTES);
        Self::new_with_browser_surface_copy_budget(
            worker,
            worker_epoch,
            renderer_epoch,
            limits,
            browser_surface_copy_byte_capacity,
        )
    }

    /// Creates an adapter with an explicit hard ceiling for the simultaneous
    /// browser destination, transfer table, and imported Surface byte copy.
    pub fn new_with_browser_surface_copy_budget(
        worker: WorkerId,
        worker_epoch: WorkerEpoch,
        renderer_epoch: u32,
        limits: NativeWorkerLimitConfig,
        browser_surface_copy_byte_capacity: u64,
    ) -> Result<Self, NativeBrowserWorkerError> {
        if worker.value() == 0 || renderer_epoch == 0 {
            return Err(NativeBrowserWorkerError::Protocol);
        }
        if browser_surface_copy_byte_capacity == 0
            || browser_surface_copy_byte_capacity > MAX_BROWSER_SURFACE_COPY_BYTES
            || browser_surface_copy_byte_capacity > limits.retained_raster_byte_capacity
        {
            return Err(NativeBrowserWorkerError::Limit);
        }
        let protocol_limits = limits.protocol;
        let pending_parse_byte_capacity =
            limits.max_scene_bytes_per_open.min(MAX_DATA_TICKET_BYTES);
        let local_hello = ProtocolHello {
            major: PROTOCOL_MAJOR,
            minor: PROTOCOL_MINOR,
            schema_hash: SCHEMA_HASH,
            endpoint_role: EndpointRole::Engine,
            capabilities: EndpointCapabilities {
                supported: ENDPOINT_CAPABILITY_TRANSFERABLE_ARRAY_BUFFER,
                mandatory: ENDPOINT_CAPABILITY_TRANSFERABLE_ARRAY_BUFFER,
            },
            max_message_bytes: MAX_MESSAGE_BYTES.min(protocol_limits.max_payload_bytes()),
            max_transfer_slots: MAX_TRANSFER_SLOTS.min(protocol_limits.max_transfer_slots()),
        };
        Ok(Self {
            phase: NativeBrowserWorkerPhase::Starting,
            worker,
            worker_epoch,
            renderer_epoch,
            limits,
            validator: ProtocolValidator::new(protocol_limits),
            local_hello,
            handshake: None,
            registry: None,
            queued: VecDeque::new(),
            pending_sources: BTreeMap::new(),
            pending_parse: VecDeque::new(),
            active_parse: None,
            pending_parse_bytes: 0,
            pending_parse_byte_capacity,
            browser_surface_copy_byte_capacity,
            active_policy_task: None,
            active_raster_task: None,
            pending_reentries: VecDeque::new(),
            next_ticket: 1,
        })
    }

    /// Creates the fixed production mailbox identity used by one Wasm instance.
    pub fn production_default() -> Result<Self, NativeBrowserWorkerError> {
        Self::new(
            WorkerId::new(1),
            WorkerEpoch::new(1).ok_or(NativeBrowserWorkerError::Protocol)?,
            1,
            NativeWorkerLimitConfig::default(),
        )
    }

    /// Returns the current adapter lifecycle.
    pub const fn phase(&self) -> NativeBrowserWorkerPhase {
        self.phase
    }

    /// Returns the exact compatible handshake after Hello validation.
    pub const fn handshake(&self) -> Option<pdf_rs_protocol::CompatibleHandshake> {
        self.handshake
    }

    /// Validates and admits one decoded generated command and its actual OOB bytes.
    pub fn handle_command(
        &mut self,
        envelope: CommandEnvelope,
        transfers: &[Vec<u8>],
    ) -> Result<(), NativeBrowserWorkerError> {
        self.validator
            .validate_command_payload_correlation(
                &envelope,
                self.worker,
                envelope.correlation.session,
            )
            .map_err(|_| NativeBrowserWorkerError::Protocol)?;
        match envelope.command {
            Command::Hello(command) => {
                self.handle_hello(envelope.correlation, command.hello, transfers)
            }
            Command::HelloAccept(command) => {
                self.handle_accept(envelope.correlation, command, transfers)
            }
            command => {
                if self.phase != NativeBrowserWorkerPhase::Ready {
                    return Err(NativeBrowserWorkerError::InvalidLifecycle);
                }
                self.handle_ready_command(envelope.correlation, command, transfers)
            }
        }
    }

    /// Pumps bounded Native actor work and returns at most one protocol event.
    pub fn next_event(
        &mut self,
    ) -> Result<Option<BrowserNativeWorkerEvent>, NativeBrowserWorkerError> {
        if let Some(event) = self.take_ready_event()? {
            return Ok(Some(event));
        }
        if let Some(reentry) = self.pending_reentries.pop_front() {
            if let Err(error) = self.registry_mut()?.enqueue_reentry(reentry) {
                self.pending_reentries.push_front(error.into_reentry());
                self.registry_mut()?
                    .pump()
                    .map_err(|_| NativeBrowserWorkerError::Engine)?;
                return self.take_ready_event();
            }
            return Ok(None);
        }
        if self.active_parse.is_some() {
            self.poll_active_parse()?;
            return Ok(None);
        }
        if let Some(pending) = self.pending_parse.pop_front() {
            self.active_parse = Some(ActiveParse {
                pending,
                phase: PendingParsePhase::ValidateHeader,
            });
            return Ok(None);
        }
        self.registry_mut()?
            .pump()
            .map_err(|_| NativeBrowserWorkerError::Engine)?;
        if let Some(event) = self.take_ready_event()? {
            return Ok(Some(event));
        }
        if let Some(task) = self.active_policy_task.take() {
            let budget = PolicyPollBudget::new(
                NonZeroU32::new(NATIVE_POLICY_POLL_WORK_UNITS)
                    .ok_or(NativeBrowserWorkerError::Limit)?,
            )
            .map_err(|_| NativeBrowserWorkerError::Limit)?;
            match task.poll(budget, &NeverCancelledPolicy) {
                NativeTaskPoll::Pending(pending) => {
                    self.active_policy_task = Some(pending);
                    return Ok(None);
                }
                NativeTaskPoll::Ready(reentry) => {
                    self.pending_reentries.push_back(reentry);
                    return Ok(None);
                }
            }
        }
        if let Some(task) = self.active_raster_task.take() {
            let budget = FastRasterPollBudget::new(
                NonZeroU32::new(NATIVE_RASTER_POLL_WORK_UNITS)
                    .ok_or(NativeBrowserWorkerError::Limit)?,
            )
            .map_err(|_| NativeBrowserWorkerError::Limit)?;
            match task.poll(budget, &NeverCancelled) {
                NativeTaskPoll::Pending(pending) => {
                    self.active_raster_task = Some(pending);
                    return Ok(None);
                }
                NativeTaskPoll::Ready(reentry) => {
                    self.pending_reentries.push_back(reentry);
                    return Ok(None);
                }
            }
        }
        let task = self.registry_mut()?.next_policy_task();
        if let Some(task) = task {
            debug_assert!(self.pending_reentries.is_empty());
            self.active_policy_task = Some(task);
            return Ok(None);
        }
        let task = self.registry_mut()?.next_raster_task();
        if let Some(task) = task {
            debug_assert!(self.pending_reentries.is_empty());
            self.active_raster_task = Some(task);
        }
        Ok(None)
    }

    /// Removes only an already-produced event without running parser, Scene,
    /// scheduler, raster, cache, or lifecycle work.
    ///
    /// The Wasm dispatch path uses this operation so a browser message callback
    /// can commit validated input and return, while product work starts only on
    /// a later explicit poll.
    pub fn take_ready_event(
        &mut self,
    ) -> Result<Option<BrowserNativeWorkerEvent>, NativeBrowserWorkerError> {
        if let Some(event) = self.queued.pop_front() {
            return Ok(Some(event));
        }
        let native = self
            .registry
            .as_mut()
            .and_then(NativeWorkerRegistry::next_event);
        if let Some(native) = native {
            return self.map_native_event(native).map(Some);
        }
        Ok(None)
    }

    /// Reports whether the adapter can be dropped without abandoning Native
    /// Session, parser, scheduler, cache, Surface, or queued event ownership.
    pub fn can_dispose(&self) -> bool {
        if !self.queued.is_empty()
            || !self.pending_sources.is_empty()
            || !self.pending_parse.is_empty()
            || self.active_parse.is_some()
            || self.active_policy_task.is_some()
            || self.active_raster_task.is_some()
            || !self.pending_reentries.is_empty()
        {
            return false;
        }
        match self.registry.as_ref() {
            None => matches!(
                self.phase,
                NativeBrowserWorkerPhase::Starting | NativeBrowserWorkerPhase::AwaitingAccept
            ),
            Some(registry) => {
                self.phase == NativeBrowserWorkerPhase::Stopped
                    && registry.resources().has_zero_live_resources()
            }
        }
    }

    pub(crate) fn reclaim_undelivered_event(
        &mut self,
        event: &BrowserNativeWorkerEvent,
    ) -> Result<(), NativeBrowserWorkerError> {
        let Event::SurfaceReady(surface) = event.event() else {
            return Ok(());
        };
        self.registry_mut()?
            .reclaim_undelivered_surface_identity(event.correlation(), &surface.metadata)
            .map_err(|_| NativeBrowserWorkerError::Engine)
    }

    #[cfg(test)]
    pub(crate) fn resources(&self) -> Option<pdf_rs_engine::NativeWorkerResources> {
        self.registry.as_ref().map(NativeWorkerRegistry::resources)
    }

    fn handle_hello(
        &mut self,
        correlation: Correlation,
        peer: ProtocolHello,
        transfers: &[Vec<u8>],
    ) -> Result<(), NativeBrowserWorkerError> {
        if self.phase != NativeBrowserWorkerPhase::Starting || !transfers.is_empty() {
            return Err(NativeBrowserWorkerError::InvalidLifecycle);
        }
        self.validator
            .validate_correlation(MESSAGE_ID_HELLO, &correlation, self.worker, None)
            .map_err(|_| NativeBrowserWorkerError::Protocol)?;
        let handshake = self
            .validator
            .validate_handshake(&self.local_hello, &peer)
            .map_err(|_| NativeBrowserWorkerError::Protocol)?;
        self.handshake = Some(handshake);
        self.phase = NativeBrowserWorkerPhase::AwaitingAccept;
        self.queued.push_back(BrowserNativeWorkerEvent {
            correlation,
            event: Event::EngineHello(EngineHelloEvent {
                hello: self.local_hello.clone(),
                execution_capabilities: EngineExecutionCapabilities { supported: 0 },
            }),
            transfers: Vec::new(),
        });
        Ok(())
    }

    fn handle_accept(
        &mut self,
        correlation: Correlation,
        command: HelloAcceptCommand,
        transfers: &[Vec<u8>],
    ) -> Result<(), NativeBrowserWorkerError> {
        if self.phase != NativeBrowserWorkerPhase::AwaitingAccept || !transfers.is_empty() {
            return Err(NativeBrowserWorkerError::InvalidLifecycle);
        }
        self.validator
            .validate_correlation(MESSAGE_ID_HELLO_ACCEPT, &correlation, self.worker, None)
            .map_err(|_| NativeBrowserWorkerError::Protocol)?;
        let handshake = self
            .handshake
            .ok_or(NativeBrowserWorkerError::InvalidLifecycle)?;
        if command.negotiated_minor != handshake.minor() || command.schema_hash != SCHEMA_HASH {
            return Err(NativeBrowserWorkerError::Protocol);
        }
        let config = NativeWorkerConfig::new(
            self.worker,
            self.worker_epoch,
            self.renderer_epoch,
            self.limits,
        )
        .map_err(|_| NativeBrowserWorkerError::Engine)?;
        self.registry =
            Some(NativeWorkerRegistry::new(config).map_err(|_| NativeBrowserWorkerError::Engine)?);
        self.phase = NativeBrowserWorkerPhase::Ready;
        self.queued.push_back(BrowserNativeWorkerEvent {
            correlation,
            event: Event::Ready(ReadyEvent {
                worker: self.worker,
                negotiated_minor: handshake.minor(),
                schema_hash: SCHEMA_HASH,
                execution_capabilities: EngineExecutionCapabilities { supported: 0 },
                capability_profiles: vec![CapabilityProfileId::BaselineNative],
                output_profiles: vec![OutputProfile::Srgb],
            }),
            transfers: Vec::new(),
        });
        Ok(())
    }

    fn handle_ready_command(
        &mut self,
        correlation: Correlation,
        command: Command,
        transfers: &[Vec<u8>],
    ) -> Result<(), NativeBrowserWorkerError> {
        match command {
            Command::Open(command) => self.open(correlation, command, transfers),
            Command::ProvideData(command) => self.provide_data(correlation, command, transfers),
            Command::FailData(command) => self.fail_data(correlation, command, transfers),
            Command::SetViewport(command) => {
                if !transfers.is_empty() {
                    return Err(NativeBrowserWorkerError::Protocol);
                }
                self.registry_mut()?
                    .set_viewport(&correlation, &command)
                    .map_err(|_| NativeBrowserWorkerError::Engine)
            }
            Command::GetPageMetrics(command) => {
                if !transfers.is_empty() {
                    return Err(NativeBrowserWorkerError::Protocol);
                }
                self.registry_mut()?
                    .get_page_metrics(&correlation, &command)
                    .map_err(|_| NativeBrowserWorkerError::Engine)
            }
            Command::Cancel(command) => {
                if !transfers.is_empty() {
                    return Err(NativeBrowserWorkerError::Protocol);
                }
                self.registry_mut()?
                    .cancel(&correlation, &command)
                    .map_err(|_| NativeBrowserWorkerError::Engine)?;
                if let Some(session) = correlation.session {
                    self.discard_pending_open(session);
                }
                Ok(())
            }
            Command::ReleaseSurface(command) => {
                if !transfers.is_empty() {
                    return Err(NativeBrowserWorkerError::Protocol);
                }
                self.registry_mut()?
                    .release_surface(&correlation, &command)
                    .map_err(|_| NativeBrowserWorkerError::Engine)
            }
            Command::CloseSession(command) => {
                if !transfers.is_empty() {
                    return Err(NativeBrowserWorkerError::Protocol);
                }
                self.registry_mut()?
                    .close_session(&correlation, &command)
                    .map_err(|_| NativeBrowserWorkerError::Engine)?;
                if let Some(session) = correlation.session {
                    self.discard_pending_open(session);
                }
                Ok(())
            }
            Command::Shutdown(command) => {
                if !transfers.is_empty() {
                    return Err(NativeBrowserWorkerError::Protocol);
                }
                self.registry_mut()?
                    .shutdown(&correlation, &command)
                    .map_err(|_| NativeBrowserWorkerError::Engine)?;
                self.pending_sources.clear();
                self.pending_parse.clear();
                self.pending_parse_bytes = 0;
                self.pending_reentries.clear();
                Ok(())
            }
            Command::Hello(_) | Command::HelloAccept(_) => Err(NativeBrowserWorkerError::Protocol),
        }
    }

    fn open(
        &mut self,
        correlation: Correlation,
        command: OpenCommand,
        transfers: &[Vec<u8>],
    ) -> Result<(), NativeBrowserWorkerError> {
        if !transfers.is_empty() {
            return Err(NativeBrowserWorkerError::Protocol);
        }
        let request = correlation
            .request
            .ok_or(NativeBrowserWorkerError::Protocol)?;
        let length = command
            .source
            .length
            .ok_or(NativeBrowserWorkerError::Source)?;
        if length == 0 || length > MAX_DATA_TICKET_BYTES {
            return Err(NativeBrowserWorkerError::Limit);
        }
        let session = self
            .registry_mut()?
            .open(&correlation, &command)
            .map_err(|_| NativeBrowserWorkerError::Engine)?;
        let ticket_value = self.next_ticket;
        self.next_ticket = ticket_value
            .checked_add(1)
            .ok_or(NativeBrowserWorkerError::Limit)?;
        let ticket = DataTicket::new(ticket_value);
        let mut ranges = Vec::new();
        let mut start = 0_u64;
        while start < length {
            let len = (length - start).min(MAX_DATA_SEGMENT_BYTES);
            ranges.push(pdf_rs_protocol::ByteRange { start, len });
            start = start
                .checked_add(len)
                .ok_or(NativeBrowserWorkerError::Limit)?;
        }
        let event_correlation = Correlation {
            worker: self.worker,
            session: Some(session),
            request: Some(request),
            generation: None,
        };
        let worker_epoch = self.worker_epoch;
        self.registry_mut()?
            .enqueue_reentry(Reentry::NeedData {
                worker_epoch,
                correlation: event_correlation,
                event: NeedDataEvent {
                    ticket,
                    source: command.source.identity.clone(),
                    ranges,
                    priority: DataPriority::VisiblePage,
                    checkpoint: ticket_value,
                },
            })
            .map_err(|_| NativeBrowserWorkerError::Engine)?;
        self.pending_sources.insert(
            session,
            PendingSource {
                descriptor: command.source,
                session,
                request,
                ticket,
            },
        );
        Ok(())
    }

    fn provide_data(
        &mut self,
        correlation: Correlation,
        command: ProvideDataCommand,
        transfers: &[Vec<u8>],
    ) -> Result<(), NativeBrowserWorkerError> {
        let session = correlation
            .session
            .ok_or(NativeBrowserWorkerError::Protocol)?;
        let pending = self
            .pending_sources
            .get(&session)
            .cloned()
            .ok_or(NativeBrowserWorkerError::Source)?;
        if pending.session != session
            || command.ticket != pending.ticket
            || command.source != pending.descriptor.identity
            || command.segments.len() != transfers.len()
        {
            return Err(NativeBrowserWorkerError::Source);
        }
        let length = pending
            .descriptor
            .length
            .ok_or(NativeBrowserWorkerError::Source)?;
        let length_usize = usize::try_from(length).map_err(|_| NativeBrowserWorkerError::Limit)?;
        self.validate_full_source_segments(&command, transfers, length)?;
        let remaining_parse_bytes = self
            .pending_parse_byte_capacity
            .checked_sub(self.pending_parse_bytes)
            .ok_or(NativeBrowserWorkerError::Limit)?;
        if u64::try_from(length_usize).map_err(|_| NativeBrowserWorkerError::Limit)?
            > remaining_parse_bytes
        {
            return Err(NativeBrowserWorkerError::Limit);
        }
        let mut bytes = Vec::new();
        bytes
            .try_reserve_exact(length_usize)
            .map_err(|_| NativeBrowserWorkerError::Limit)?;
        let charged_bytes =
            u64::try_from(bytes.capacity()).map_err(|_| NativeBrowserWorkerError::Limit)?;
        let next_pending_bytes = self
            .pending_parse_bytes
            .checked_add(charged_bytes)
            .ok_or(NativeBrowserWorkerError::Limit)?;
        if next_pending_bytes > self.pending_parse_byte_capacity {
            return Err(NativeBrowserWorkerError::Limit);
        }
        bytes.resize(length_usize, 0);
        for (segment, transfer) in command.segments.iter().zip(transfers) {
            let start = usize::try_from(segment.range.start)
                .map_err(|_| NativeBrowserWorkerError::Limit)?;
            let len =
                usize::try_from(segment.range.len).map_err(|_| NativeBrowserWorkerError::Limit)?;
            let end = start
                .checked_add(len)
                .ok_or(NativeBrowserWorkerError::Limit)?;
            if end > bytes.len() || transfer.len() != len {
                return Err(NativeBrowserWorkerError::Source);
            }
            bytes[start..end].copy_from_slice(transfer);
        }
        let transfer_lengths = transfers
            .iter()
            .map(|transfer| {
                u64::try_from(transfer.len()).map_err(|_| NativeBrowserWorkerError::Limit)
            })
            .collect::<Result<Vec<_>, _>>()?;
        self.registry_mut()?
            .provide_data(&correlation, &command, &transfer_lengths)
            .map_err(|_| NativeBrowserWorkerError::Engine)?;
        self.pending_parse_bytes = next_pending_bytes;
        self.pending_parse.push_back(PendingParse {
            session,
            bytes,
            charged_bytes,
        });
        Ok(())
    }

    fn validate_full_source_segments(
        &self,
        command: &ProvideDataCommand,
        transfers: &[Vec<u8>],
        source_length: u64,
    ) -> Result<(), NativeBrowserWorkerError> {
        let mut expected_start = 0_u64;
        for (index, (segment, transfer)) in command.segments.iter().zip(transfers).enumerate() {
            let expected_length = (source_length - expected_start).min(MAX_DATA_SEGMENT_BYTES);
            if segment.slot != u16::try_from(index).map_err(|_| NativeBrowserWorkerError::Limit)?
                || segment.range.start != expected_start
                || segment.range.len != expected_length
                || segment.byte_length != expected_length
                || segment.role != pdf_rs_protocol::DataAttachmentRole::ImmutableRangeBytes
                || u64::try_from(transfer.len()).map_err(|_| NativeBrowserWorkerError::Limit)?
                    != expected_length
            {
                return Err(NativeBrowserWorkerError::Source);
            }
            expected_start = expected_start
                .checked_add(expected_length)
                .ok_or(NativeBrowserWorkerError::Limit)?;
        }
        if expected_start != source_length {
            return Err(NativeBrowserWorkerError::Source);
        }
        Ok(())
    }

    fn fail_data(
        &mut self,
        correlation: Correlation,
        command: FailDataCommand,
        transfers: &[Vec<u8>],
    ) -> Result<(), NativeBrowserWorkerError> {
        if !transfers.is_empty() {
            return Err(NativeBrowserWorkerError::Protocol);
        }
        self.registry_mut()?
            .fail_data(&correlation, &command)
            .map_err(|_| NativeBrowserWorkerError::Engine)?;
        if let Some(session) = correlation.session {
            self.discard_pending_open(session);
        }
        Ok(())
    }

    fn parse_header_version(
        &self,
        descriptor: &SourceDescriptor,
        bytes: &[u8],
    ) -> Result<(u8, u8), NativeBrowserWorkerError> {
        let source = source_identity(&descriptor.identity)?;
        let input = SyntaxInput::new(source, 0, bytes, InputExtent::KnownSourceEnd)
            .map_err(|_| NativeBrowserWorkerError::Parse)?;
        let mut parser = SyntaxParser::new(input, SyntaxLimits::default())
            .map_err(|_| NativeBrowserWorkerError::Parse)?;
        let header = match parser.parse_header() {
            SyntaxPoll::Ready(header) => header.into_value(),
            SyntaxPoll::NeedMore { .. } | SyntaxPoll::EndOfInput | SyntaxPoll::Failed(_) => {
                return Err(NativeBrowserWorkerError::Parse);
            }
        };
        Ok((header.major(), header.minor()))
    }

    fn poll_active_parse(&mut self) -> Result<(), NativeBrowserWorkerError> {
        let mut active = self
            .active_parse
            .take()
            .ok_or(NativeBrowserWorkerError::InvalidLifecycle)?;
        match active.phase {
            PendingParsePhase::ValidateHeader => {
                let descriptor = self
                    .pending_sources
                    .get(&active.pending.session)
                    .ok_or(NativeBrowserWorkerError::Source)?
                    .descriptor
                    .clone();
                match self.parse_header_version(&descriptor, &active.pending.bytes) {
                    Ok((major, minor)) => {
                        active.phase = PendingParsePhase::BuildScene { major, minor };
                        self.active_parse = Some(active);
                    }
                    Err(_) => self.complete_active_parse(active, None)?,
                }
            }
            PendingParsePhase::BuildScene { major, minor } => {
                let descriptor = self
                    .pending_sources
                    .get(&active.pending.session)
                    .ok_or(NativeBrowserWorkerError::Source)?
                    .descriptor
                    .clone();
                let scene = source_identity(&descriptor.identity)
                    .and_then(|source| {
                        build_fixture_scene(source, major, minor, active.pending.bytes.len())
                    })
                    .ok();
                self.complete_active_parse(active, scene)?;
            }
        }
        Ok(())
    }

    fn complete_active_parse(
        &mut self,
        active: ActiveParse,
        scene: Option<Scene>,
    ) -> Result<(), NativeBrowserWorkerError> {
        let session = active.pending.session;
        let pending = self
            .pending_sources
            .remove(&session)
            .ok_or(NativeBrowserWorkerError::Source)?;
        self.pending_parse_bytes = self
            .pending_parse_bytes
            .saturating_sub(active.pending.charged_bytes);
        let completion = match scene {
            Some(scene) => OpenCompletion::Ready {
                worker: self.worker,
                worker_epoch: self.worker_epoch,
                session,
                request: pending.request,
                document_revision: DOCUMENT_REVISION,
                scenes: vec![Arc::new(scene)],
            },
            None => OpenCompletion::Failed {
                worker: self.worker,
                worker_epoch: self.worker_epoch,
                session,
                request: pending.request,
            },
        };
        self.pending_reentries.push_back(Reentry::Open(completion));
        Ok(())
    }

    fn map_native_event(
        &mut self,
        native: NativeWorkerEvent,
    ) -> Result<BrowserNativeWorkerEvent, NativeBrowserWorkerError> {
        let (correlation, event, transfers) = match native {
            NativeWorkerEvent::NeedData { correlation, event } => {
                (correlation, Event::NeedData(event), Vec::new())
            }
            NativeWorkerEvent::DocumentReady { correlation, event } => {
                (correlation, Event::DocumentReady(event), Vec::new())
            }
            NativeWorkerEvent::PageMetrics { correlation, event } => {
                (correlation, Event::PageMetrics(event), Vec::new())
            }
            NativeWorkerEvent::CapabilityReported {
                correlation, event, ..
            } => (correlation, Event::CapabilityReported(event), Vec::new()),
            NativeWorkerEvent::GenerationPlanned {
                correlation, event, ..
            } => (correlation, Event::GenerationPlanned(event), Vec::new()),
            NativeWorkerEvent::SurfaceReady(publication) => {
                return self.map_surface_publication(publication);
            }
            NativeWorkerEvent::SurfaceReclaimed { correlation, event } => {
                (correlation, Event::SurfaceReclaimed(event), Vec::new())
            }
            NativeWorkerEvent::GenerationCompleted { correlation, event } => {
                (correlation, Event::GenerationCompleted(event), Vec::new())
            }
            NativeWorkerEvent::RequestCancelled { correlation, event } => {
                if let Some(session) = correlation.session {
                    self.discard_pending_open(session);
                }
                (correlation, Event::RequestCancelled(event), Vec::new())
            }
            NativeWorkerEvent::RequestFailed { correlation, event } => {
                if let Some(session) = correlation.session {
                    self.discard_pending_open(session);
                }
                (correlation, Event::RequestFailed(event), Vec::new())
            }
            NativeWorkerEvent::CancelAcknowledged { correlation, event } => {
                (correlation, Event::CancelAcknowledged(event), Vec::new())
            }
            NativeWorkerEvent::SurfaceReleaseAcknowledged { correlation, event } => (
                correlation,
                Event::SurfaceReleaseAcknowledged(event),
                Vec::new(),
            ),
            NativeWorkerEvent::CloseSessionAcknowledged { correlation, event } => (
                correlation,
                Event::CloseSessionAcknowledged(event),
                Vec::new(),
            ),
            NativeWorkerEvent::SessionClosed { correlation, event } => {
                if let Some(session) = correlation.session {
                    self.discard_pending_open(session);
                }
                (correlation, Event::SessionClosed(event), Vec::new())
            }
            NativeWorkerEvent::ShutdownAcknowledged { correlation, event } => {
                (correlation, Event::ShutdownAcknowledged(event), Vec::new())
            }
            NativeWorkerEvent::WorkerStopped { correlation, event } => {
                self.phase = NativeBrowserWorkerPhase::Stopped;
                (correlation, Event::WorkerStopped(event), Vec::new())
            }
        };
        Ok(BrowserNativeWorkerEvent {
            correlation,
            event,
            transfers,
        })
    }

    fn map_surface_publication(
        &mut self,
        publication: pdf_rs_engine::SurfacePublication,
    ) -> Result<BrowserNativeWorkerEvent, NativeBrowserWorkerError> {
        let result = self.try_map_surface_publication(&publication);
        if result.is_err()
            && self
                .registry_mut()?
                .reclaim_undelivered_surface(&publication)
                .is_err()
        {
            return Err(NativeBrowserWorkerError::Engine);
        }
        result
    }

    fn try_map_surface_publication(
        &mut self,
        publication: &pdf_rs_engine::SurfacePublication,
    ) -> Result<BrowserNativeWorkerEvent, NativeBrowserWorkerError> {
        let published_metadata = &publication.event().metadata;
        let byte_offset = usize::try_from(published_metadata.byte_offset)
            .map_err(|_| NativeBrowserWorkerError::Limit)?;
        let pixel_length = usize::try_from(published_metadata.byte_length)
            .map_err(|_| NativeBrowserWorkerError::Limit)?;
        let buffer_length = byte_offset
            .checked_add(pixel_length)
            .ok_or(NativeBrowserWorkerError::Limit)?;
        let buffer_length_u64 =
            u64::try_from(buffer_length).map_err(|_| NativeBrowserWorkerError::Limit)?;
        let logical_copy_minimum = buffer_length_u64
            .checked_add(
                u64::try_from(std::mem::size_of::<Vec<u8>>())
                    .map_err(|_| NativeBrowserWorkerError::Limit)?,
            )
            .and_then(|retained| retained.checked_add(published_metadata.byte_length))
            .ok_or(NativeBrowserWorkerError::Limit)?;
        if logical_copy_minimum > self.browser_surface_copy_byte_capacity {
            return Err(NativeBrowserWorkerError::Limit);
        }

        let mut browser_bytes = Vec::new();
        browser_bytes
            .try_reserve_exact(buffer_length)
            .map_err(|_| NativeBrowserWorkerError::Limit)?;
        let browser_retained_capacity =
            u64::try_from(browser_bytes.capacity()).map_err(|_| NativeBrowserWorkerError::Limit)?;
        browser_bytes.resize(buffer_length, 0);
        let destination = browser_bytes
            .get_mut(byte_offset..buffer_length)
            .filter(|bytes| bytes.len() == pixel_length)
            .ok_or(NativeBrowserWorkerError::Limit)?;

        let correlation = publication.correlation().clone();
        let metadata = published_metadata.clone();
        let event = Event::SurfaceReady(SurfaceReadyEvent {
            metadata,
            transport: SurfaceTransport::BrowserArrayBuffer {
                slot: 0,
                buffer_length: buffer_length_u64,
            },
        });
        let mut browser_transfers = Vec::new();
        browser_transfers
            .try_reserve_exact(1)
            .map_err(|_| NativeBrowserWorkerError::Limit)?;
        let outer_retained_capacity = browser_transfers
            .capacity()
            .checked_mul(std::mem::size_of::<Vec<u8>>())
            .and_then(|capacity| u64::try_from(capacity).ok())
            .ok_or(NativeBrowserWorkerError::Limit)?;
        let preimport_retained_capacity = browser_retained_capacity
            .checked_add(outer_retained_capacity)
            .ok_or(NativeBrowserWorkerError::Limit)?;
        let import_capacity = self
            .browser_surface_copy_byte_capacity
            .checked_sub(preimport_retained_capacity)
            .filter(|remaining| *remaining > 0)
            .ok_or(NativeBrowserWorkerError::Limit)?;
        let transfer = publication.transfer().clone();

        let imported = self
            .registry_mut()?
            .import_surface_bytes_bounded(publication, transfer, import_capacity)
            .map_err(|_| NativeBrowserWorkerError::Surface)?;

        // The engine import contract returns exactly metadata.byte_length
        // immutable bytes and enforces its actual Vec capacity against
        // import_capacity. Everything below is allocation-free and infallible.
        for (target, source) in destination.iter_mut().zip(imported.bytes()) {
            *target = *source;
        }
        browser_transfers.push(browser_bytes);
        Ok(BrowserNativeWorkerEvent {
            correlation,
            event,
            transfers: browser_transfers,
        })
    }

    fn registry_mut(&mut self) -> Result<&mut NativeWorkerRegistry, NativeBrowserWorkerError> {
        self.registry
            .as_mut()
            .ok_or(NativeBrowserWorkerError::InvalidLifecycle)
    }

    fn discard_pending_open(&mut self, session: SessionId) {
        self.pending_sources.remove(&session);
        if self
            .active_parse
            .as_ref()
            .is_some_and(|active| active.pending.session == session)
            && let Some(active) = self.active_parse.take()
        {
            self.pending_parse_bytes = self
                .pending_parse_bytes
                .saturating_sub(active.pending.charged_bytes);
        }
        let mut retained = VecDeque::new();
        while let Some(pending) = self.pending_parse.pop_front() {
            if pending.session == session {
                self.pending_parse_bytes = self
                    .pending_parse_bytes
                    .saturating_sub(pending.charged_bytes);
            } else {
                retained.push_back(pending);
            }
        }
        self.pending_parse = retained;
        self.pending_reentries.retain(|reentry| {
            !matches!(
                reentry,
                Reentry::Open(
                    OpenCompletion::Ready {
                        session: candidate,
                        ..
                    } | OpenCompletion::Failed {
                        session: candidate,
                        ..
                    }
                ) if *candidate == session
            )
        });
    }
}

fn source_identity(
    source: &pdf_rs_protocol::SourceIdentity,
) -> Result<SourceIdentity, NativeBrowserWorkerError> {
    if source.revision == 0 || source.stable_id.iter().all(|byte| *byte == 0) {
        return Err(NativeBrowserWorkerError::Source);
    }
    Ok(SourceIdentity::new(
        SourceStableId::new(source.stable_id),
        SourceRevision::new(source.revision),
    ))
}

fn build_fixture_scene(
    source: SourceIdentity,
    major: u8,
    minor: u8,
    source_length: usize,
) -> Result<Scene, NativeBrowserWorkerError> {
    if major == 0 || source_length == 0 {
        return Err(NativeBrowserWorkerError::Parse);
    }
    let page = SceneRect::new([
        SceneScalar::ZERO,
        SceneScalar::ZERO,
        SceneScalar::from_scaled(16_000_000_000),
        SceneScalar::from_scaled(16_000_000_000),
    ])
    .map_err(|_| NativeBrowserWorkerError::Scene)?;
    let binding = SceneBinding::new(
        source,
        REVISION_STARTXREF,
        PAGE_INDEX,
        ObjectRef::new(1, 0).map_err(|_| NativeBrowserWorkerError::Scene)?,
    );
    let mut builder = GraphicsSceneBuilder::new_v2(
        binding,
        PageGeometry::new(page, page, PageRotation::Degrees0),
        GraphicsSceneLimits::default(),
    );
    if major == 2 {
        builder
            .add_requirement(
                GraphicsCapability::PathFill,
                0,
                CapabilityContext::Scene,
                Vec::new(),
                CapabilityStatus::Unsupported,
            )
            .map_err(|_| NativeBrowserWorkerError::Scene)?;
        return builder
            .finish()
            .map_err(|_| NativeBrowserWorkerError::Scene);
    }
    let scalar = |value| SceneScalar::from_scaled(value);
    let point = |x, y| ScenePoint::new(scalar(x), scalar(y));
    let path = PathResource::new(vec![
        PathSegment::MoveTo(point(1_000_000_000, 1_000_000_000)),
        PathSegment::LineTo(point(15_000_000_000, 1_000_000_000)),
        PathSegment::LineTo(point(15_000_000_000, 15_000_000_000)),
        PathSegment::LineTo(point(1_000_000_000, 15_000_000_000)),
        PathSegment::ClosePath,
    ])
    .map_err(|_| NativeBrowserWorkerError::Scene)?;
    let bounds = SceneBounds::finite(
        point(1_000_000_000, 1_000_000_000),
        point(15_000_000_000, 15_000_000_000),
    )
    .map_err(|_| NativeBrowserWorkerError::Scene)?;
    let decoded_length =
        u64::try_from(source_length).map_err(|_| NativeBrowserWorkerError::Limit)?;
    let operator_index = u32::from(minor);
    builder
        .append_fill(
            path,
            FillRule::Nonzero,
            Paint::new(
                DeviceColor::Gray(SceneUnit::ZERO),
                SceneUnit::ONE,
                BlendMode::Normal,
            ),
            Matrix::IDENTITY,
            bounds,
            CommandSource::new(
                ObjectRef::new(2, 0).map_err(|_| NativeBrowserWorkerError::Scene)?,
                0,
                0,
                decoded_length,
                operator_index,
            )
            .map_err(|_| NativeBrowserWorkerError::Scene)?,
        )
        .map_err(|_| NativeBrowserWorkerError::Scene)?;
    builder
        .finish()
        .map_err(|_| NativeBrowserWorkerError::Scene)
}

#[cfg(test)]
mod tests {
    use super::{build_fixture_scene, source_identity};
    use pdf_rs_protocol::SourceIdentity as WireSourceIdentity;

    #[test]
    fn fixture_scene_is_native_nonblank_graphics() {
        let source = source_identity(&WireSourceIdentity {
            stable_id: [7; 32],
            revision: 1,
        })
        .unwrap();
        let scene = build_fixture_scene(source, 1, 7, 64).unwrap();
        let graphics = scene.graphics().unwrap();
        assert_eq!(graphics.commands().len(), 1);
        assert!(graphics.is_supported());
    }
}
