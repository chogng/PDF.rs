use std::collections::{BTreeMap, VecDeque};
use std::num::NonZeroU32;
use std::sync::Arc;

use pdf_rs_bytes::{SourceIdentity, SourceRevision, SourceStableId};
use pdf_rs_engine::{
    ActorProgress, NativePolicyTask, NativeRasterTask, NativeTaskPoll, NativeWorkerConfig,
    NativeWorkerEvent, NativeWorkerLimitConfig, NativeWorkerRegistry, OpenCompletion, Reentry,
};
use pdf_rs_fast_raster::fast::{FastRasterPollBudget, NeverCancelled};
use pdf_rs_policy::{NeverCancelled as NeverCancelledPolicy, PolicyPollBudget};
use pdf_rs_protocol::{
    CapabilityProfileId, Command, CommandEnvelope, Correlation, DataPriority, DataTicket,
    ENDPOINT_CAPABILITY_SHARED_MEMORY, EndpointCapabilities, EndpointRole,
    EngineExecutionCapabilities, EngineHelloEvent, Event, FailDataCommand, HelloAcceptCommand,
    MAX_DATA_SEGMENT_BYTES, MAX_DATA_TICKET_BYTES, MAX_MESSAGE_BYTES, MAX_TRANSFER_SLOTS,
    MESSAGE_ID_HELLO, MESSAGE_ID_HELLO_ACCEPT, NeedDataEvent, OpenCommand, OutputProfile,
    PROTOCOL_MAJOR, PROTOCOL_MINOR, ProtocolHello, ProtocolValidator, ProvideDataCommand,
    ReadyEvent, RequestId, SCHEMA_HASH, SessionId, SourceDescriptor, SurfaceReadyEvent,
    SurfaceTransport, WorkerId,
};
use pdf_rs_scene::{
    BlendMode, CapabilityContext, CapabilityStatus, CommandSource, DeviceColor, FillRule,
    GraphicsCapability, GraphicsSceneBuilder, GraphicsSceneLimits, Matrix, PageGeometry,
    PageRotation, Paint, PathResource, PathSegment, Scene, SceneBinding, SceneBounds, ScenePoint,
    SceneRect, SceneScalar, SceneUnit,
};
use pdf_rs_surface::WorkerEpoch;
use pdf_rs_syntax::{InputExtent, ObjectRef, SyntaxInput, SyntaxLimits, SyntaxParser, SyntaxPoll};

use crate::{DesktopIpcError, DesktopIpcErrorCode, DesktopIpcLimits, error::error};

const DOCUMENT_REVISION: u64 = 1;
const REVISION_STARTXREF: u64 = 1;
const PAGE_INDEX: u32 = 0;
const NATIVE_POLICY_POLL_WORK_UNITS: u32 = 64;
const NATIVE_RASTER_POLL_WORK_UNITS: u32 = 64;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum NativeDesktopPhase {
    Starting,
    AwaitingAccept,
    Ready,
    Stopped,
}

pub(crate) struct DesktopNativeEvent {
    pub(crate) correlation: Correlation,
    pub(crate) event: Event,
    pub(crate) shared_region: Option<Vec<u8>>,
    pub(crate) shared_memory_budget: Option<u64>,
}

#[allow(
    clippy::large_enum_variant,
    reason = "the single event remains move-only and avoids an unbounded boxing allocation"
)]
pub(crate) enum DesktopNativePoll {
    Event(DesktopNativeEvent),
    Progressed,
    Idle,
}

#[derive(Clone)]
struct PendingSource {
    descriptor: SourceDescriptor,
    session: SessionId,
    request: RequestId,
    ticket: DataTicket,
}

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

pub(crate) struct DesktopNativeWorker {
    phase: NativeDesktopPhase,
    worker: WorkerId,
    worker_epoch: WorkerEpoch,
    limits: NativeWorkerLimitConfig,
    ipc_limits: DesktopIpcLimits,
    validator: ProtocolValidator,
    local_hello: ProtocolHello,
    handshake: Option<pdf_rs_protocol::CompatibleHandshake>,
    registry: Option<NativeWorkerRegistry>,
    queued: VecDeque<DesktopNativeEvent>,
    pending_sources: BTreeMap<SessionId, PendingSource>,
    pending_parse: VecDeque<PendingParse>,
    active_parse: Option<ActiveParse>,
    pending_parse_bytes: u64,
    pending_parse_byte_capacity: u64,
    surface_staging_byte_capacity: u64,
    active_policy_task: Option<NativePolicyTask>,
    active_raster_task: Option<NativeRasterTask>,
    pending_reentries: VecDeque<Reentry>,
    next_ticket: u64,
    #[cfg(test)]
    force_surface_budget_one_less: bool,
}

impl DesktopNativeWorker {
    pub(crate) fn new(
        worker: WorkerId,
        worker_epoch: WorkerEpoch,
        ipc_limits: DesktopIpcLimits,
    ) -> Result<Self, DesktopIpcError> {
        if worker.value() == 0 {
            return Err(error(DesktopIpcErrorCode::Authentication));
        }
        let limits = NativeWorkerLimitConfig::default();
        let protocol_limits = limits.protocol;
        let pending_parse_byte_capacity = limits
            .max_scene_bytes_per_open
            .min(MAX_DATA_TICKET_BYTES)
            .min(
                u64::try_from(ipc_limits.max_source_bytes())
                    .map_err(|_| error(DesktopIpcErrorCode::ResourceLimit))?,
            );
        let surface_staging_byte_capacity = limits.retained_raster_byte_capacity.min(
            u64::try_from(ipc_limits.max_capability_bytes())
                .map_err(|_| error(DesktopIpcErrorCode::ResourceLimit))?,
        );
        let local_hello = ProtocolHello {
            major: PROTOCOL_MAJOR,
            minor: PROTOCOL_MINOR,
            schema_hash: SCHEMA_HASH,
            endpoint_role: EndpointRole::Engine,
            capabilities: EndpointCapabilities {
                supported: ENDPOINT_CAPABILITY_SHARED_MEMORY,
                mandatory: ENDPOINT_CAPABILITY_SHARED_MEMORY,
            },
            max_message_bytes: MAX_MESSAGE_BYTES.min(protocol_limits.max_payload_bytes()),
            max_transfer_slots: MAX_TRANSFER_SLOTS
                .min(protocol_limits.max_transfer_slots())
                .min(
                    u16::try_from(ipc_limits.max_capabilities())
                        .map_err(|_| error(DesktopIpcErrorCode::ResourceLimit))?,
                ),
        };
        Ok(Self {
            phase: NativeDesktopPhase::Starting,
            worker,
            worker_epoch,
            limits,
            ipc_limits,
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
            surface_staging_byte_capacity,
            active_policy_task: None,
            active_raster_task: None,
            pending_reentries: VecDeque::new(),
            next_ticket: 1,
            #[cfg(test)]
            force_surface_budget_one_less: false,
        })
    }

    pub(crate) const fn phase(&self) -> NativeDesktopPhase {
        self.phase
    }

    pub(crate) const fn handshake(&self) -> Option<pdf_rs_protocol::CompatibleHandshake> {
        self.handshake
    }

    pub(crate) fn handle_command(
        &mut self,
        envelope: CommandEnvelope,
        transfers: &[Vec<u8>],
    ) -> Result<(), DesktopIpcError> {
        self.validator
            .validate_command_payload_correlation(
                &envelope,
                self.worker,
                envelope.correlation.session,
            )
            .map_err(|_| error(DesktopIpcErrorCode::InvalidFrame))?;
        match envelope.command {
            Command::Hello(command) => {
                self.handle_hello(envelope.correlation, command.hello, transfers)
            }
            Command::HelloAccept(command) => {
                self.handle_accept(envelope.correlation, command, transfers)
            }
            command => {
                if self.phase != NativeDesktopPhase::Ready {
                    return Err(error(DesktopIpcErrorCode::Lifecycle));
                }
                self.handle_ready_command(envelope.correlation, command, transfers)
            }
        }
    }

    pub(crate) fn poll(&mut self) -> Result<DesktopNativePoll, DesktopIpcError> {
        if let Some(event) = self.take_ready_event()? {
            return Ok(DesktopNativePoll::Event(event));
        }
        if let Some(reentry) = self.pending_reentries.pop_front() {
            if let Err(failure) = self.registry_mut()?.enqueue_reentry(reentry) {
                self.pending_reentries.push_front(failure.into_reentry());
                return Ok(DesktopNativePoll::Progressed);
            }
            return Ok(DesktopNativePoll::Progressed);
        }
        if self.active_parse.is_some() {
            self.poll_active_parse()?;
            return Ok(DesktopNativePoll::Progressed);
        }
        if let Some(pending) = self.pending_parse.pop_front() {
            self.active_parse = Some(ActiveParse {
                pending,
                phase: PendingParsePhase::ValidateHeader,
            });
            return Ok(DesktopNativePoll::Progressed);
        }
        if self.registry.is_none() {
            return Ok(DesktopNativePoll::Idle);
        }
        let progress = self
            .registry_mut()?
            .pump()
            .map_err(|_| error(DesktopIpcErrorCode::Lifecycle))?;
        if let Some(event) = self.take_ready_event()? {
            return Ok(DesktopNativePoll::Event(event));
        }
        if let Some(task) = self.active_policy_task.take() {
            let budget = PolicyPollBudget::new(
                NonZeroU32::new(NATIVE_POLICY_POLL_WORK_UNITS)
                    .ok_or_else(|| error(DesktopIpcErrorCode::ResourceLimit))?,
            )
            .map_err(|_| error(DesktopIpcErrorCode::ResourceLimit))?;
            match task.poll(budget, &NeverCancelledPolicy) {
                NativeTaskPoll::Pending(pending) => self.active_policy_task = Some(pending),
                NativeTaskPoll::Ready(reentry) => self.pending_reentries.push_back(reentry),
            }
            return Ok(DesktopNativePoll::Progressed);
        }
        if let Some(task) = self.active_raster_task.take() {
            let budget = FastRasterPollBudget::new(
                NonZeroU32::new(NATIVE_RASTER_POLL_WORK_UNITS)
                    .ok_or_else(|| error(DesktopIpcErrorCode::ResourceLimit))?,
            )
            .map_err(|_| error(DesktopIpcErrorCode::ResourceLimit))?;
            match task.poll(budget, &NeverCancelled) {
                NativeTaskPoll::Pending(pending) => self.active_raster_task = Some(pending),
                NativeTaskPoll::Ready(reentry) => self.pending_reentries.push_back(reentry),
            }
            return Ok(DesktopNativePoll::Progressed);
        }
        if let Some(task) = self.registry_mut()?.next_policy_task() {
            self.active_policy_task = Some(task);
            return Ok(DesktopNativePoll::Progressed);
        }
        if let Some(task) = self.registry_mut()?.next_raster_task() {
            self.active_raster_task = Some(task);
            return Ok(DesktopNativePoll::Progressed);
        }
        if progress == ActorProgress::Idle {
            Ok(DesktopNativePoll::Idle)
        } else {
            Ok(DesktopNativePoll::Progressed)
        }
    }

    fn take_ready_event(&mut self) -> Result<Option<DesktopNativeEvent>, DesktopIpcError> {
        if let Some(event) = self.queued.pop_front() {
            return Ok(Some(event));
        }
        let native = self
            .registry
            .as_mut()
            .and_then(NativeWorkerRegistry::next_event);
        native.map(|event| self.map_native_event(event)).transpose()
    }

    fn handle_hello(
        &mut self,
        correlation: Correlation,
        peer: ProtocolHello,
        transfers: &[Vec<u8>],
    ) -> Result<(), DesktopIpcError> {
        if self.phase != NativeDesktopPhase::Starting || !transfers.is_empty() {
            return Err(error(DesktopIpcErrorCode::Lifecycle));
        }
        self.validator
            .validate_correlation(MESSAGE_ID_HELLO, &correlation, self.worker, None)
            .map_err(|_| error(DesktopIpcErrorCode::InvalidFrame))?;
        let handshake = self
            .validator
            .validate_handshake(&self.local_hello, &peer)
            .map_err(|_| error(DesktopIpcErrorCode::InvalidFrame))?;
        self.handshake = Some(handshake);
        self.phase = NativeDesktopPhase::AwaitingAccept;
        self.queued.push_back(DesktopNativeEvent {
            correlation,
            event: Event::EngineHello(EngineHelloEvent {
                hello: self.local_hello.clone(),
                execution_capabilities: EngineExecutionCapabilities { supported: 0 },
            }),
            shared_region: None,
            shared_memory_budget: None,
        });
        Ok(())
    }

    fn handle_accept(
        &mut self,
        correlation: Correlation,
        command: HelloAcceptCommand,
        transfers: &[Vec<u8>],
    ) -> Result<(), DesktopIpcError> {
        if self.phase != NativeDesktopPhase::AwaitingAccept || !transfers.is_empty() {
            return Err(error(DesktopIpcErrorCode::Lifecycle));
        }
        self.validator
            .validate_correlation(MESSAGE_ID_HELLO_ACCEPT, &correlation, self.worker, None)
            .map_err(|_| error(DesktopIpcErrorCode::InvalidFrame))?;
        let handshake = self
            .handshake
            .ok_or_else(|| error(DesktopIpcErrorCode::Lifecycle))?;
        if command.negotiated_minor != handshake.minor() || command.schema_hash != SCHEMA_HASH {
            return Err(error(DesktopIpcErrorCode::InvalidFrame));
        }
        let config = NativeWorkerConfig::new(self.worker, self.worker_epoch, 1, self.limits)
            .map_err(|_| error(DesktopIpcErrorCode::Lifecycle))?;
        self.registry = Some(
            NativeWorkerRegistry::new(config).map_err(|_| error(DesktopIpcErrorCode::Lifecycle))?,
        );
        self.phase = NativeDesktopPhase::Ready;
        self.queued.push_back(DesktopNativeEvent {
            correlation,
            event: Event::Ready(ReadyEvent {
                worker: self.worker,
                negotiated_minor: handshake.minor(),
                schema_hash: SCHEMA_HASH,
                execution_capabilities: EngineExecutionCapabilities { supported: 0 },
                capability_profiles: vec![CapabilityProfileId::BaselineNative],
                output_profiles: vec![OutputProfile::Srgb],
            }),
            shared_region: None,
            shared_memory_budget: None,
        });
        Ok(())
    }

    fn handle_ready_command(
        &mut self,
        correlation: Correlation,
        command: Command,
        transfers: &[Vec<u8>],
    ) -> Result<(), DesktopIpcError> {
        match command {
            Command::Open(command) => self.open(correlation, command, transfers),
            Command::ProvideData(command) => self.provide_data(correlation, command, transfers),
            Command::FailData(command) => self.fail_data(correlation, command, transfers),
            Command::SetViewport(command) => {
                require_no_transfers(transfers)?;
                self.registry_mut()?
                    .set_viewport(&correlation, &command)
                    .map_err(|_| error(DesktopIpcErrorCode::Lifecycle))
            }
            Command::GetPageMetrics(command) => {
                require_no_transfers(transfers)?;
                self.registry_mut()?
                    .get_page_metrics(&correlation, &command)
                    .map_err(|_| error(DesktopIpcErrorCode::Lifecycle))
            }
            Command::Cancel(command) => {
                require_no_transfers(transfers)?;
                self.registry_mut()?
                    .cancel(&correlation, &command)
                    .map_err(|_| error(DesktopIpcErrorCode::Lifecycle))?;
                if let Some(session) = correlation.session {
                    self.discard_pending_open(session);
                }
                Ok(())
            }
            Command::ReleaseSurface(command) => {
                require_no_transfers(transfers)?;
                self.registry_mut()?
                    .release_surface(&correlation, &command)
                    .map_err(|_| error(DesktopIpcErrorCode::Lifecycle))
            }
            Command::CloseSession(command) => {
                require_no_transfers(transfers)?;
                self.registry_mut()?
                    .close_session(&correlation, &command)
                    .map_err(|_| error(DesktopIpcErrorCode::Lifecycle))?;
                if let Some(session) = correlation.session {
                    self.discard_pending_open(session);
                }
                Ok(())
            }
            Command::Shutdown(command) => {
                require_no_transfers(transfers)?;
                self.registry_mut()?
                    .shutdown(&correlation, &command)
                    .map_err(|_| error(DesktopIpcErrorCode::Lifecycle))?;
                self.pending_sources.clear();
                self.pending_parse.clear();
                self.active_parse = None;
                self.pending_parse_bytes = 0;
                self.pending_reentries.clear();
                Ok(())
            }
            Command::Hello(_) | Command::HelloAccept(_) => {
                Err(error(DesktopIpcErrorCode::InvalidFrame))
            }
        }
    }

    fn open(
        &mut self,
        correlation: Correlation,
        command: OpenCommand,
        transfers: &[Vec<u8>],
    ) -> Result<(), DesktopIpcError> {
        require_no_transfers(transfers)?;
        let request = correlation
            .request
            .ok_or_else(|| error(DesktopIpcErrorCode::InvalidFrame))?;
        let length = command
            .source
            .length
            .ok_or_else(|| error(DesktopIpcErrorCode::Source))?;
        if length == 0
            || length > MAX_DATA_TICKET_BYTES
            || length
                > u64::try_from(self.ipc_limits.max_source_bytes())
                    .map_err(|_| error(DesktopIpcErrorCode::ResourceLimit))?
        {
            return Err(error(DesktopIpcErrorCode::ResourceLimit));
        }
        let session = self
            .registry_mut()?
            .open(&correlation, &command)
            .map_err(|_| error(DesktopIpcErrorCode::Lifecycle))?;
        let ticket_value = self.next_ticket;
        self.next_ticket = ticket_value
            .checked_add(1)
            .ok_or_else(|| error(DesktopIpcErrorCode::ResourceLimit))?;
        let ticket = DataTicket::new(ticket_value);
        let mut ranges = Vec::new();
        let mut start = 0_u64;
        while start < length {
            let len = (length - start).min(MAX_DATA_SEGMENT_BYTES);
            if ranges.len() == self.ipc_limits.max_capabilities() {
                return Err(error(DesktopIpcErrorCode::ResourceLimit));
            }
            ranges.push(pdf_rs_protocol::ByteRange { start, len });
            start = start
                .checked_add(len)
                .ok_or_else(|| error(DesktopIpcErrorCode::ResourceLimit))?;
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
            .map_err(|_| error(DesktopIpcErrorCode::Lifecycle))?;
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
    ) -> Result<(), DesktopIpcError> {
        let session = correlation
            .session
            .ok_or_else(|| error(DesktopIpcErrorCode::InvalidFrame))?;
        let pending = self
            .pending_sources
            .get(&session)
            .cloned()
            .ok_or_else(|| error(DesktopIpcErrorCode::Source))?;
        if pending.session != session
            || command.ticket != pending.ticket
            || command.source != pending.descriptor.identity
            || command.segments.len() != transfers.len()
        {
            return Err(error(DesktopIpcErrorCode::Source));
        }
        let length = pending
            .descriptor
            .length
            .ok_or_else(|| error(DesktopIpcErrorCode::Source))?;
        let length_usize =
            usize::try_from(length).map_err(|_| error(DesktopIpcErrorCode::ResourceLimit))?;
        validate_full_source_segments(&command, transfers, length)?;
        let remaining = self
            .pending_parse_byte_capacity
            .checked_sub(self.pending_parse_bytes)
            .ok_or_else(|| error(DesktopIpcErrorCode::ResourceLimit))?;
        if length > remaining {
            return Err(error(DesktopIpcErrorCode::ResourceLimit));
        }
        let mut bytes = Vec::new();
        bytes
            .try_reserve_exact(length_usize)
            .map_err(|_| error(DesktopIpcErrorCode::ResourceLimit))?;
        let charged_bytes = u64::try_from(bytes.capacity())
            .map_err(|_| error(DesktopIpcErrorCode::ResourceLimit))?;
        let next_pending_bytes = self
            .pending_parse_bytes
            .checked_add(charged_bytes)
            .filter(|value| *value <= self.pending_parse_byte_capacity)
            .ok_or_else(|| error(DesktopIpcErrorCode::ResourceLimit))?;
        bytes.resize(length_usize, 0);
        for (segment, transfer) in command.segments.iter().zip(transfers) {
            let start = usize::try_from(segment.range.start)
                .map_err(|_| error(DesktopIpcErrorCode::ResourceLimit))?;
            let len = usize::try_from(segment.range.len)
                .map_err(|_| error(DesktopIpcErrorCode::ResourceLimit))?;
            let end = start
                .checked_add(len)
                .ok_or_else(|| error(DesktopIpcErrorCode::ResourceLimit))?;
            let target = bytes
                .get_mut(start..end)
                .filter(|target| target.len() == transfer.len())
                .ok_or_else(|| error(DesktopIpcErrorCode::Source))?;
            target.copy_from_slice(transfer);
        }
        let transfer_lengths = transfers
            .iter()
            .map(|transfer| {
                u64::try_from(transfer.len()).map_err(|_| error(DesktopIpcErrorCode::ResourceLimit))
            })
            .collect::<Result<Vec<_>, _>>()?;
        self.registry_mut()?
            .provide_data(&correlation, &command, &transfer_lengths)
            .map_err(|_| error(DesktopIpcErrorCode::Lifecycle))?;
        self.pending_parse_bytes = next_pending_bytes;
        self.pending_parse.push_back(PendingParse {
            session,
            bytes,
            charged_bytes,
        });
        Ok(())
    }

    fn fail_data(
        &mut self,
        correlation: Correlation,
        command: FailDataCommand,
        transfers: &[Vec<u8>],
    ) -> Result<(), DesktopIpcError> {
        require_no_transfers(transfers)?;
        self.registry_mut()?
            .fail_data(&correlation, &command)
            .map_err(|_| error(DesktopIpcErrorCode::Lifecycle))?;
        if let Some(session) = correlation.session {
            self.discard_pending_open(session);
        }
        Ok(())
    }

    fn poll_active_parse(&mut self) -> Result<(), DesktopIpcError> {
        let mut active = self
            .active_parse
            .take()
            .ok_or_else(|| error(DesktopIpcErrorCode::Lifecycle))?;
        match active.phase {
            PendingParsePhase::ValidateHeader => {
                let descriptor = self
                    .pending_sources
                    .get(&active.pending.session)
                    .ok_or_else(|| error(DesktopIpcErrorCode::Source))?
                    .descriptor
                    .clone();
                match parse_header_version(&descriptor, &active.pending.bytes) {
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
                    .ok_or_else(|| error(DesktopIpcErrorCode::Source))?
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
    ) -> Result<(), DesktopIpcError> {
        let session = active.pending.session;
        let pending = self
            .pending_sources
            .remove(&session)
            .ok_or_else(|| error(DesktopIpcErrorCode::Source))?;
        self.pending_parse_bytes = self
            .pending_parse_bytes
            .checked_sub(active.pending.charged_bytes)
            .ok_or_else(|| error(DesktopIpcErrorCode::ResourceLimit))?;
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
    ) -> Result<DesktopNativeEvent, DesktopIpcError> {
        let (correlation, event) = match native {
            NativeWorkerEvent::NeedData { correlation, event } => {
                (correlation, Event::NeedData(event))
            }
            NativeWorkerEvent::DocumentReady { correlation, event } => {
                (correlation, Event::DocumentReady(event))
            }
            NativeWorkerEvent::PageMetrics { correlation, event } => {
                (correlation, Event::PageMetrics(event))
            }
            NativeWorkerEvent::CapabilityReported {
                correlation, event, ..
            } => (correlation, Event::CapabilityReported(event)),
            NativeWorkerEvent::GenerationPlanned {
                correlation, event, ..
            } => (correlation, Event::GenerationPlanned(event)),
            NativeWorkerEvent::SurfaceReady(publication) => {
                return self.map_surface_publication(publication);
            }
            NativeWorkerEvent::SurfaceReclaimed { correlation, event } => {
                (correlation, Event::SurfaceReclaimed(event))
            }
            NativeWorkerEvent::GenerationCompleted { correlation, event } => {
                (correlation, Event::GenerationCompleted(event))
            }
            NativeWorkerEvent::RequestCancelled { correlation, event } => {
                if let Some(session) = correlation.session {
                    self.discard_pending_open(session);
                }
                (correlation, Event::RequestCancelled(event))
            }
            NativeWorkerEvent::RequestFailed { correlation, event } => {
                if let Some(session) = correlation.session {
                    self.discard_pending_open(session);
                }
                (correlation, Event::RequestFailed(event))
            }
            NativeWorkerEvent::CancelAcknowledged { correlation, event } => {
                (correlation, Event::CancelAcknowledged(event))
            }
            NativeWorkerEvent::SurfaceReleaseAcknowledged { correlation, event } => {
                (correlation, Event::SurfaceReleaseAcknowledged(event))
            }
            NativeWorkerEvent::CloseSessionAcknowledged { correlation, event } => {
                (correlation, Event::CloseSessionAcknowledged(event))
            }
            NativeWorkerEvent::SessionClosed { correlation, event } => {
                if let Some(session) = correlation.session {
                    self.discard_pending_open(session);
                }
                (correlation, Event::SessionClosed(event))
            }
            NativeWorkerEvent::ShutdownAcknowledged { correlation, event } => {
                (correlation, Event::ShutdownAcknowledged(event))
            }
            NativeWorkerEvent::WorkerStopped { correlation, event } => {
                self.phase = NativeDesktopPhase::Stopped;
                (correlation, Event::WorkerStopped(event))
            }
        };
        Ok(DesktopNativeEvent {
            correlation,
            event,
            shared_region: None,
            shared_memory_budget: None,
        })
    }

    fn map_surface_publication(
        &mut self,
        publication: pdf_rs_engine::SurfacePublication,
    ) -> Result<DesktopNativeEvent, DesktopIpcError> {
        let result = self.try_map_surface_publication(&publication);
        if result.is_err()
            && self
                .registry_mut()?
                .reclaim_undelivered_surface(&publication)
                .is_err()
        {
            return Err(error(DesktopIpcErrorCode::Lifecycle));
        }
        result
    }

    fn try_map_surface_publication(
        &mut self,
        publication: &pdf_rs_engine::SurfacePublication,
    ) -> Result<DesktopNativeEvent, DesktopIpcError> {
        let metadata = publication.event().metadata.clone();
        let byte_offset = usize::try_from(metadata.byte_offset)
            .map_err(|_| error(DesktopIpcErrorCode::ResourceLimit))?;
        let pixel_length = usize::try_from(metadata.byte_length)
            .map_err(|_| error(DesktopIpcErrorCode::ResourceLimit))?;
        let region_length = byte_offset
            .checked_add(pixel_length)
            .ok_or_else(|| error(DesktopIpcErrorCode::ResourceLimit))?;
        if region_length > self.ipc_limits.max_capability_bytes() {
            return Err(error(DesktopIpcErrorCode::ResourceLimit));
        }
        let logical_region_bytes =
            u64::try_from(region_length).map_err(|_| error(DesktopIpcErrorCode::ResourceLimit))?;
        let logical_coexistence = logical_region_bytes
            .checked_add(logical_region_bytes.max(metadata.byte_length))
            .ok_or_else(|| error(DesktopIpcErrorCode::ResourceLimit))?;
        if logical_coexistence > self.surface_staging_byte_capacity {
            return Err(error(DesktopIpcErrorCode::ResourceLimit));
        }
        let mut region = Vec::new();
        region
            .try_reserve_exact(region_length)
            .map_err(|_| error(DesktopIpcErrorCode::ResourceLimit))?;
        if region.capacity() > self.ipc_limits.max_capability_bytes() {
            return Err(error(DesktopIpcErrorCode::ResourceLimit));
        }
        region.resize(region_length, 0);
        let retained_region_capacity = u64::try_from(region.capacity())
            .map_err(|_| error(DesktopIpcErrorCode::ResourceLimit))?;
        #[cfg(test)]
        let staging_capacity = if self.force_surface_budget_one_less {
            retained_region_capacity
                .checked_add(logical_region_bytes)
                .and_then(|required| required.checked_sub(1))
                .ok_or_else(|| error(DesktopIpcErrorCode::ResourceLimit))?
        } else {
            self.surface_staging_byte_capacity
        };
        #[cfg(not(test))]
        let staging_capacity = self.surface_staging_byte_capacity;
        let import_capacity = staging_capacity
            .checked_sub(retained_region_capacity)
            .filter(|remaining| *remaining >= logical_region_bytes)
            .ok_or_else(|| error(DesktopIpcErrorCode::ResourceLimit))?;
        let imported = self
            .registry_mut()?
            .import_surface_bytes_bounded(
                publication,
                publication.transfer().clone(),
                import_capacity,
            )
            .map_err(|_| error(DesktopIpcErrorCode::Capability))?;
        let target = region
            .get_mut(byte_offset..region_length)
            .filter(|target| target.len() == imported.bytes().len())
            .ok_or_else(|| error(DesktopIpcErrorCode::Capability))?;
        target.copy_from_slice(imported.bytes());
        let correlation = publication.correlation().clone();
        Ok(DesktopNativeEvent {
            correlation,
            event: Event::SurfaceReady(SurfaceReadyEvent {
                metadata,
                transport: SurfaceTransport::SharedMemory {
                    slot: 0,
                    region_length: u64::try_from(region_length)
                        .map_err(|_| error(DesktopIpcErrorCode::ResourceLimit))?,
                },
            }),
            shared_region: Some(region),
            shared_memory_budget: Some(import_capacity),
        })
    }

    fn registry_mut(&mut self) -> Result<&mut NativeWorkerRegistry, DesktopIpcError> {
        self.registry
            .as_mut()
            .ok_or_else(|| error(DesktopIpcErrorCode::Lifecycle))
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
        self.pending_parse.retain(|pending| {
            if pending.session == session {
                self.pending_parse_bytes = self
                    .pending_parse_bytes
                    .saturating_sub(pending.charged_bytes);
                false
            } else {
                true
            }
        });
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

fn require_no_transfers(transfers: &[Vec<u8>]) -> Result<(), DesktopIpcError> {
    if transfers.is_empty() {
        Ok(())
    } else {
        Err(error(DesktopIpcErrorCode::Capability))
    }
}

fn validate_full_source_segments(
    command: &ProvideDataCommand,
    transfers: &[Vec<u8>],
    source_length: u64,
) -> Result<(), DesktopIpcError> {
    let mut expected_start = 0_u64;
    for (index, (segment, transfer)) in command.segments.iter().zip(transfers).enumerate() {
        let expected_length = (source_length - expected_start).min(MAX_DATA_SEGMENT_BYTES);
        if segment.slot
            != u16::try_from(index).map_err(|_| error(DesktopIpcErrorCode::ResourceLimit))?
            || segment.range.start != expected_start
            || segment.range.len != expected_length
            || segment.byte_length != expected_length
            || segment.role != pdf_rs_protocol::DataAttachmentRole::ImmutableRangeBytes
            || u64::try_from(transfer.len())
                .map_err(|_| error(DesktopIpcErrorCode::ResourceLimit))?
                != expected_length
        {
            return Err(error(DesktopIpcErrorCode::Source));
        }
        expected_start = expected_start
            .checked_add(expected_length)
            .ok_or_else(|| error(DesktopIpcErrorCode::ResourceLimit))?;
    }
    if expected_start != source_length {
        return Err(error(DesktopIpcErrorCode::Source));
    }
    Ok(())
}

fn parse_header_version(
    descriptor: &SourceDescriptor,
    bytes: &[u8],
) -> Result<(u8, u8), DesktopIpcError> {
    let source = source_identity(&descriptor.identity)?;
    let input = SyntaxInput::new(source, 0, bytes, InputExtent::KnownSourceEnd)
        .map_err(|_| error(DesktopIpcErrorCode::Source))?;
    let mut parser = SyntaxParser::new(input, SyntaxLimits::default())
        .map_err(|_| error(DesktopIpcErrorCode::Source))?;
    match parser.parse_header() {
        SyntaxPoll::Ready(header) => {
            let header = header.into_value();
            Ok((header.major(), header.minor()))
        }
        SyntaxPoll::NeedMore { .. } | SyntaxPoll::EndOfInput | SyntaxPoll::Failed(_) => {
            Err(error(DesktopIpcErrorCode::Source))
        }
    }
}

fn source_identity(
    source: &pdf_rs_protocol::SourceIdentity,
) -> Result<SourceIdentity, DesktopIpcError> {
    if source.revision == 0 || source.stable_id.iter().all(|byte| *byte == 0) {
        return Err(error(DesktopIpcErrorCode::Source));
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
) -> Result<Scene, DesktopIpcError> {
    if major == 0 || source_length == 0 {
        return Err(error(DesktopIpcErrorCode::Source));
    }
    let page = SceneRect::new([
        SceneScalar::ZERO,
        SceneScalar::ZERO,
        SceneScalar::from_scaled(16_000_000_000),
        SceneScalar::from_scaled(16_000_000_000),
    ])
    .map_err(|_| error(DesktopIpcErrorCode::Lifecycle))?;
    let binding = SceneBinding::new(
        source,
        REVISION_STARTXREF,
        PAGE_INDEX,
        ObjectRef::new(1, 0).map_err(|_| error(DesktopIpcErrorCode::Lifecycle))?,
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
            .map_err(|_| error(DesktopIpcErrorCode::Lifecycle))?;
        return builder
            .finish()
            .map_err(|_| error(DesktopIpcErrorCode::Lifecycle));
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
    .map_err(|_| error(DesktopIpcErrorCode::Lifecycle))?;
    let bounds = SceneBounds::finite(
        point(1_000_000_000, 1_000_000_000),
        point(15_000_000_000, 15_000_000_000),
    )
    .map_err(|_| error(DesktopIpcErrorCode::Lifecycle))?;
    let decoded_length =
        u64::try_from(source_length).map_err(|_| error(DesktopIpcErrorCode::ResourceLimit))?;
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
                ObjectRef::new(2, 0).map_err(|_| error(DesktopIpcErrorCode::Lifecycle))?,
                0,
                0,
                decoded_length,
                u32::from(minor),
            )
            .map_err(|_| error(DesktopIpcErrorCode::Lifecycle))?,
        )
        .map_err(|_| error(DesktopIpcErrorCode::Lifecycle))?;
    builder
        .finish()
        .map_err(|_| error(DesktopIpcErrorCode::Lifecycle))
}

#[cfg(test)]
mod tests {
    use super::{DesktopNativePoll, DesktopNativeWorker};
    use crate::{DesktopIpcErrorCode, DesktopIpcLimitConfig, DesktopIpcLimits};
    use pdf_rs_protocol::{
        Command, CommandEnvelope, Correlation, DataAttachmentRole, DataSegment,
        ENDPOINT_CAPABILITY_SHARED_MEMORY, EndpointCapabilities, EndpointRole, Event,
        GetPageMetricsCommand, HelloAcceptCommand, HelloCommand, MAX_MESSAGE_BYTES,
        MAX_TRANSFER_SLOTS, MESSAGE_ID_GET_PAGE_METRICS, MESSAGE_ID_HELLO, MESSAGE_ID_HELLO_ACCEPT,
        MESSAGE_ID_OPEN, MESSAGE_ID_PROVIDE_DATA, MESSAGE_ID_SET_VIEWPORT, OpenCommand,
        OutputProfile, PROTOCOL_MAJOR, PROTOCOL_MINOR, PageCoordinateSpace, PageGeometry,
        PageRotation, PageViewport, ProtocolHello, ProvideDataCommand, QualityPolicy, RequestId,
        SCHEMA_HASH, SetViewportCommand, SourceDescriptor, SourceIdentity, ViewportRequest,
        WorkerId,
    };
    use pdf_rs_surface::WorkerEpoch;

    const SOURCE_BYTES: &[u8] = b"%PDF-1.7\n1 0 obj\n<<>>\nendobj\n%%EOF\n";

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

    fn next_event(worker: &mut DesktopNativeWorker) -> super::DesktopNativeEvent {
        for _ in 0..1_024 {
            match worker.poll().expect("Native poll") {
                DesktopNativePoll::Event(event) => return event,
                DesktopNativePoll::Progressed => {}
                DesktopNativePoll::Idle => panic!("Native worker became idle before event"),
            }
        }
        panic!("Native event turn bound exhausted");
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

    fn worker_with_viewport() -> DesktopNativeWorker {
        let ipc_limits =
            DesktopIpcLimits::new(DesktopIpcLimitConfig::default()).expect("desktop limits");
        let mut worker = DesktopNativeWorker::new(
            WorkerId::new(1),
            WorkerEpoch::new(1).expect("epoch"),
            ipc_limits,
        )
        .expect("desktop Native worker");
        let host_hello = ProtocolHello {
            major: PROTOCOL_MAJOR,
            minor: PROTOCOL_MINOR,
            schema_hash: SCHEMA_HASH,
            endpoint_role: EndpointRole::Host,
            capabilities: EndpointCapabilities {
                supported: ENDPOINT_CAPABILITY_SHARED_MEMORY,
                mandatory: ENDPOINT_CAPABILITY_SHARED_MEMORY,
            },
            max_message_bytes: MAX_MESSAGE_BYTES,
            max_transfer_slots: MAX_TRANSFER_SLOTS,
        };
        worker
            .handle_command(
                command(
                    MESSAGE_ID_HELLO,
                    correlation(None, None, None),
                    Command::Hello(HelloCommand { hello: host_hello }),
                ),
                &[],
            )
            .expect("Hello");
        assert!(matches!(
            next_event(&mut worker).event,
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
            .expect("HelloAccept");
        assert!(matches!(next_event(&mut worker).event, Event::Ready(_)));
        let source = SourceDescriptor {
            identity: SourceIdentity {
                stable_id: [7; 32],
                revision: 1,
            },
            length: Some(u64::try_from(SOURCE_BYTES.len()).expect("source length")),
            validator: [9; 32],
        };
        worker
            .handle_command(
                command(
                    MESSAGE_ID_OPEN,
                    correlation(None, Some(RequestId::new(1)), None),
                    Command::Open(OpenCommand {
                        source: source.clone(),
                    }),
                ),
                &[],
            )
            .expect("Open");
        let need = next_event(&mut worker);
        let (session, need) = match (need.correlation.session, need.event) {
            (Some(session), Event::NeedData(need)) => (session, need),
            other => panic!("expected NeedData, got {other:?}"),
        };
        let segments = need
            .ranges
            .iter()
            .enumerate()
            .map(|(index, range)| DataSegment {
                range: range.clone(),
                slot: u16::try_from(index).expect("slot"),
                byte_length: range.len,
                role: DataAttachmentRole::ImmutableRangeBytes,
            })
            .collect::<Vec<_>>();
        let transfers = need
            .ranges
            .iter()
            .map(|range| {
                let start = usize::try_from(range.start).expect("range start");
                let end = usize::try_from(range.start + range.len).expect("range end");
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
            .expect("ProvideData");
        assert!(matches!(
            next_event(&mut worker).event,
            Event::DocumentReady(_)
        ));
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
            .expect("GetPageMetrics");
        let geometry = match next_event(&mut worker).event {
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
            .expect("SetViewport");
        worker
    }

    #[test]
    fn one_less_surface_staging_budget_reclaims_without_surface_ready() {
        let mut worker = worker_with_viewport();
        worker.force_surface_budget_one_less = true;
        let mut saw_surface_ready = false;
        let mut failure = None;
        for _ in 0..2_048 {
            match worker.poll() {
                Ok(DesktopNativePoll::Event(event)) => {
                    saw_surface_ready |= matches!(event.event, Event::SurfaceReady(_));
                }
                Ok(DesktopNativePoll::Progressed) => {}
                Ok(DesktopNativePoll::Idle) => panic!("viewport idled before Surface publication"),
                Err(value) => {
                    failure = Some(value);
                    break;
                }
            }
        }
        let failure = failure.expect("surface staging rejection");
        assert_eq!(failure.code(), DesktopIpcErrorCode::ResourceLimit);
        assert!(!saw_surface_ready);
        let resources = worker.registry.as_ref().expect("registry").resources();
        assert_eq!(resources.delivered_surface_leases(), 0);
        assert!(resources.surface().has_zero_surface_resources());
    }
}
