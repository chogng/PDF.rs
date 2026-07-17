use std::fmt;

use crate::{
    BrowserTransferKind, CanvasId, CapabilityDecisionHash, CorrelationRequirement, EndpointRole,
    FrameMessagePolicy, KNOWN_ENDPOINT_CAPABILITIES, MAX_MESSAGE_BYTES, MAX_TRANSFER_SLOTS,
    MESSAGE_ID_PROVIDE_DATA, MESSAGE_ID_SET_VIEWPORT, MIN_COMPATIBLE_MINOR, MemoryEpoch,
    NativeBackend, PROVIDE_DATA_COMMAND_SEGMENTS_MAX_COUNT, PlatformHandle, ProtocolError,
    ProtocolErrorCode, ProtocolHello, ProtocolLimits, ProvideDataCommand, RenderConfigHash,
    RenderPlanHash, RenderPlanId, RendererEpoch, SCHEMA_HASH, SceneHash, SessionId,
    SetViewportCommand, SurfaceMetadata, SurfaceRegion, SurfaceTransport,
    VIEWPORT_REQUEST_VISIBLE_PAGES_MAX_COUNT, ViewportRequest, WorkerId, descriptor_by_id,
};

/// Negotiated schema relationship between compatible endpoints.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum HandshakeCompatibility {
    /// Both endpoints advertise the exact generated schema hash.
    ExactSchema,
    /// The endpoints share a major and negotiated capabilities but use compatible minor schemas.
    CompatibleMinor,
}

/// Validated result of one Host-to-Engine handshake.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CompatibleHandshake {
    minor: u16,
    compatibility: HandshakeCompatibility,
    capabilities: u64,
    max_message_bytes: u32,
    max_transfer_slots: u16,
}

impl CompatibleHandshake {
    /// Returns the negotiated protocol minor.
    pub const fn minor(self) -> u16 {
        self.minor
    }

    /// Returns whether schema hashes were exact or minor-compatible.
    pub const fn compatibility(self) -> HandshakeCompatibility {
        self.compatibility
    }

    /// Returns the intersection of known endpoint capabilities.
    pub const fn capabilities(self) -> u64 {
        self.capabilities
    }

    /// Returns the negotiated message-size ceiling.
    pub const fn max_message_bytes(self) -> u32 {
        self.max_message_bytes
    }

    /// Returns the negotiated transfer-slot ceiling.
    pub const fn max_transfer_slots(self) -> u16 {
        self.max_transfer_slots
    }
}

/// Trusted render identity copied from one accepted Native render plan.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SurfaceRenderIdentity {
    render_config: RenderConfigHash,
    renderer_epoch: RendererEpoch,
    plan_id: RenderPlanId,
    plan_hash: RenderPlanHash,
    scene_hash: SceneHash,
    decision_hash: CapabilityDecisionHash,
    backend: NativeBackend,
}

impl SurfaceRenderIdentity {
    /// Creates an identity from an accepted semantic render plan.
    pub const fn new(
        render_config: RenderConfigHash,
        renderer_epoch: RendererEpoch,
        plan_id: RenderPlanId,
        plan_hash: RenderPlanHash,
        scene_hash: SceneHash,
        decision_hash: CapabilityDecisionHash,
        backend: NativeBackend,
    ) -> Self {
        Self {
            render_config,
            renderer_epoch,
            plan_id,
            plan_hash,
            scene_hash,
            decision_hash,
            backend,
        }
    }

    /// Returns the exact render-configuration identity.
    pub const fn render_config(self) -> RenderConfigHash {
        self.render_config
    }

    /// Returns the renderer implementation epoch.
    pub const fn renderer_epoch(self) -> RendererEpoch {
        self.renderer_epoch
    }

    /// Returns the non-reusable render-plan identity.
    pub const fn plan_id(self) -> RenderPlanId {
        self.plan_id
    }

    /// Returns the canonical render-plan hash.
    pub const fn plan_hash(self) -> RenderPlanHash {
        self.plan_hash
    }

    /// Returns the Native Scene hash consumed by the plan.
    pub const fn scene_hash(self) -> SceneHash {
        self.scene_hash
    }

    /// Returns the supported CapabilityDecision hash consumed by the plan.
    pub const fn decision_hash(self) -> CapabilityDecisionHash {
        self.decision_hash
    }

    /// Returns the exact selected Native backend.
    pub const fn backend(self) -> NativeBackend {
        self.backend
    }
}

/// Trusted render-plan binding for one expected Surface placement.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SurfacePlanBinding {
    region: SurfaceRegion,
    render: SurfaceRenderIdentity,
}

impl SurfacePlanBinding {
    /// Binds an accepted render identity to its exact planned placement.
    pub const fn new(region: SurfaceRegion, render: SurfaceRenderIdentity) -> Self {
        Self { region, render }
    }

    /// Borrows the expected placement.
    pub const fn region(&self) -> &SurfaceRegion {
        &self.region
    }

    /// Returns the expected render identity.
    pub const fn render(&self) -> SurfaceRenderIdentity {
        self.render
    }
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum SurfaceResourceBinding {
    None,
    OffscreenCanvas {
        canvas: CanvasId,
        region_bytes: u64,
    },
    BrowserTransfer {
        slot: u16,
        kind: BrowserTransferKind,
        region_bytes: u64,
    },
    SharedMemory {
        handle: PlatformHandle,
        region_bytes: u64,
    },
    LocalMemory {
        memory_epoch: MemoryEpoch,
        region_bytes: u64,
    },
}

/// Trusted receiver context for validating one generated Surface envelope.
#[derive(Clone, Eq, PartialEq)]
pub struct SurfaceValidationContext {
    worker: WorkerId,
    session: SessionId,
    generation: u64,
    plan: SurfacePlanBinding,
    transfer_slots: usize,
    resource: SurfaceResourceBinding,
}

impl SurfaceValidationContext {
    /// Creates a receiver context with no imported or registered Surface resource.
    pub fn new(
        worker: WorkerId,
        session: SessionId,
        generation: u64,
        plan: SurfacePlanBinding,
        transfer_slots: usize,
    ) -> Self {
        Self {
            worker,
            session,
            generation,
            plan,
            transfer_slots,
            resource: SurfaceResourceBinding::None,
        }
    }

    /// Records the registered canvas and its receiver-known accessible byte extent.
    pub const fn with_offscreen_canvas(mut self, canvas: CanvasId, region_bytes: u64) -> Self {
        self.resource = SurfaceResourceBinding::OffscreenCanvas {
            canvas,
            region_bytes,
        };
        self
    }

    /// Records one actual browser transfer slot, kind, and byte extent.
    pub const fn with_browser_transfer(
        mut self,
        slot: u16,
        kind: BrowserTransferKind,
        region_bytes: u64,
    ) -> Self {
        self.resource = SurfaceResourceBinding::BrowserTransfer {
            slot,
            kind,
            region_bytes,
        };
        self
    }

    /// Records the shared-memory handle and accessible region imported with the frame.
    pub const fn with_shared_memory(mut self, handle: PlatformHandle, region_bytes: u64) -> Self {
        self.resource = SurfaceResourceBinding::SharedMemory {
            handle,
            region_bytes,
        };
        self
    }

    /// Records the same-worker memory epoch and accessible linear-memory bytes.
    pub const fn with_local_memory(mut self, memory_epoch: MemoryEpoch, region_bytes: u64) -> Self {
        self.resource = SurfaceResourceBinding::LocalMemory {
            memory_epoch,
            region_bytes,
        };
        self
    }

    /// Returns the expected worker identity.
    pub const fn worker(&self) -> WorkerId {
        self.worker
    }

    /// Returns the expected session identity.
    pub const fn session(&self) -> SessionId {
        self.session
    }

    /// Returns the current viewport generation.
    pub const fn generation(&self) -> u64 {
        self.generation
    }

    /// Borrows the exact accepted render-plan binding.
    pub const fn plan(&self) -> &SurfacePlanBinding {
        &self.plan
    }

    /// Returns the actual out-of-band transfer table length.
    pub const fn transfer_slots(&self) -> usize {
        self.transfer_slots
    }
}

impl fmt::Debug for SurfaceValidationContext {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let (resource_kind, region_bytes) = match self.resource {
            SurfaceResourceBinding::None => ("none", None),
            SurfaceResourceBinding::OffscreenCanvas { region_bytes, .. } => {
                ("offscreen-canvas", Some(region_bytes))
            }
            SurfaceResourceBinding::BrowserTransfer { region_bytes, .. } => {
                ("browser-transfer", Some(region_bytes))
            }
            SurfaceResourceBinding::SharedMemory { region_bytes, .. } => {
                ("shared-memory", Some(region_bytes))
            }
            SurfaceResourceBinding::LocalMemory { region_bytes, .. } => {
                ("local-memory", Some(region_bytes))
            }
        };
        formatter
            .debug_struct("SurfaceValidationContext")
            .field("worker", &self.worker)
            .field("session", &self.session)
            .field("generation", &self.generation)
            .field("plan", &self.plan)
            .field("transfer_slots", &self.transfer_slots)
            .field("resource_kind", &resource_kind)
            .field("resource_region_bytes", &region_bytes)
            .field("platform_handle", &"[REDACTED]")
            .finish()
    }
}

/// Surface metadata and transport accepted for one exact receiver context.
#[derive(Clone, Eq, PartialEq)]
pub struct ValidatedSurface {
    metadata: SurfaceMetadata,
    transport: SurfaceTransport,
    layout_bytes: u64,
}

impl ValidatedSurface {
    /// Borrows the validated generated metadata.
    pub const fn metadata(&self) -> &SurfaceMetadata {
        &self.metadata
    }

    /// Borrows the validated generated transport without transferring ownership.
    pub const fn transport(&self) -> &SurfaceTransport {
        &self.transport
    }

    /// Returns exact `stride * height` bytes required by the Surface layout.
    pub const fn layout_bytes(&self) -> u64 {
        self.layout_bytes
    }
}

impl fmt::Debug for ValidatedSurface {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ValidatedSurface")
            .field("metadata", &self.metadata)
            .field("layout_bytes", &self.layout_bytes)
            .field("transport", &"[REDACTED]")
            .field("platform_handle", &"[REDACTED]")
            .finish()
    }
}

/// Handwritten validator over the generated Engine protocol registry and value types.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ProtocolValidator {
    limits: ProtocolLimits,
}

impl ProtocolValidator {
    /// Creates a validator under already validated hard ceilings.
    pub const fn new(limits: ProtocolLimits) -> Self {
        Self { limits }
    }

    /// Returns the validator's hard ceilings.
    pub const fn limits(self) -> ProtocolLimits {
        self.limits
    }

    /// Selects frame policy only from the generated descriptor registry.
    pub fn frame_policy(&self, message_type: u16) -> Result<FrameMessagePolicy, ProtocolError> {
        let descriptor = descriptor_by_id(message_type)
            .ok_or_else(|| ProtocolError::for_code(ProtocolErrorCode::UnknownMessage))?;
        FrameMessagePolicy::new(
            descriptor.id,
            descriptor.allowed_flags,
            descriptor.max_payload_bytes,
            descriptor.min_transfer_slots,
            descriptor.max_transfer_slots,
        )
    }

    /// Validates Host/Engine version, schema, endpoint limits, and mandatory capabilities.
    pub fn validate_handshake(
        &self,
        local: &ProtocolHello,
        peer: &ProtocolHello,
    ) -> Result<CompatibleHandshake, ProtocolError> {
        if local.major != peer.major || local.major != crate::PROTOCOL_MAJOR {
            return Err(ProtocolError::for_code(ProtocolErrorCode::UnsupportedMajor));
        }
        if local.minor != crate::PROTOCOL_MINOR
            || peer.minor < MIN_COMPATIBLE_MINOR
            || peer.minor > crate::PROTOCOL_MINOR
        {
            return Err(ProtocolError::for_code(ProtocolErrorCode::UnsupportedMinor));
        }
        if !opposite_roles(&local.endpoint_role, &peer.endpoint_role) {
            return Err(ProtocolError::for_code(
                ProtocolErrorCode::InvalidEndpointRole,
            ));
        }
        if local.schema_hash != SCHEMA_HASH {
            return Err(ProtocolError::for_code(
                ProtocolErrorCode::IncompatibleSchema,
            ));
        }
        if local.max_message_bytes == 0
            || peer.max_message_bytes == 0
            || local.max_message_bytes > MAX_MESSAGE_BYTES
            || peer.max_message_bytes > MAX_MESSAGE_BYTES
            || local.max_transfer_slots == 0
            || peer.max_transfer_slots == 0
            || local.max_transfer_slots > MAX_TRANSFER_SLOTS
            || peer.max_transfer_slots > MAX_TRANSFER_SLOTS
        {
            return Err(ProtocolError::for_code(
                ProtocolErrorCode::InvalidEndpointLimits,
            ));
        }

        let local_mandatory = local.capabilities.mandatory;
        let peer_mandatory = peer.capabilities.mandatory;
        if local_mandatory & !KNOWN_ENDPOINT_CAPABILITIES != 0
            || peer_mandatory & !KNOWN_ENDPOINT_CAPABILITIES != 0
        {
            return Err(ProtocolError::for_code(
                ProtocolErrorCode::UnknownMandatoryCapability,
            ));
        }
        if local_mandatory & !local.capabilities.supported != 0
            || peer_mandatory & !peer.capabilities.supported != 0
        {
            return Err(ProtocolError::for_code(
                ProtocolErrorCode::InvalidEndpointCapabilities,
            ));
        }
        if local_mandatory & !peer.capabilities.supported != 0
            || peer_mandatory & !local.capabilities.supported != 0
        {
            return Err(ProtocolError::for_code(
                ProtocolErrorCode::MissingMandatoryCapability,
            ));
        }

        let minor = peer.minor;
        let compatibility = if local.schema_hash == peer.schema_hash {
            if peer.minor != crate::PROTOCOL_MINOR {
                return Err(ProtocolError::for_code(
                    ProtocolErrorCode::IncompatibleSchema,
                ));
            }
            HandshakeCompatibility::ExactSchema
        } else if peer.minor == crate::PROTOCOL_MINOR {
            return Err(ProtocolError::for_code(
                ProtocolErrorCode::IncompatibleSchema,
            ));
        } else {
            HandshakeCompatibility::CompatibleMinor
        };
        let max_message_bytes = local
            .max_message_bytes
            .min(peer.max_message_bytes)
            .min(self.limits.max_payload_bytes());
        let max_transfer_slots = local
            .max_transfer_slots
            .min(peer.max_transfer_slots)
            .min(self.limits.max_transfer_slots());
        if max_message_bytes == 0 || max_transfer_slots == 0 {
            return Err(ProtocolError::for_code(
                ProtocolErrorCode::InvalidEndpointLimits,
            ));
        }
        Ok(CompatibleHandshake {
            minor,
            compatibility,
            capabilities: local.capabilities.supported
                & peer.capabilities.supported
                & KNOWN_ENDPOINT_CAPABILITIES,
            max_message_bytes,
            max_transfer_slots,
        })
    }

    /// Validates generated correlation shape and exact receiver ownership.
    pub fn validate_correlation(
        &self,
        message_type: u16,
        correlation: &crate::Correlation,
        worker: WorkerId,
        session: Option<SessionId>,
    ) -> Result<(), ProtocolError> {
        let descriptor = descriptor_by_id(message_type)
            .ok_or_else(|| ProtocolError::for_code(ProtocolErrorCode::UnknownMessage))?;
        if correlation.worker != worker
            || correlation.worker.value() == 0
            || correlation
                .session
                .is_some_and(|identity| identity.value() == 0)
            || correlation
                .request
                .is_some_and(|identity| identity.value() == 0)
            || correlation.generation == Some(0)
            || session.is_some_and(|expected| correlation.session != Some(expected))
        {
            return Err(ProtocolError::for_code(
                ProtocolErrorCode::InvalidCorrelation,
            ));
        }
        if descriptor.correlation_shape.worker != CorrelationRequirement::Required {
            return Err(ProtocolError::for_code(
                ProtocolErrorCode::InvalidGeneratedDescriptor,
            ));
        }
        if !correlation_requirement_met(
            correlation.session.is_some(),
            descriptor.correlation_shape.session,
        ) || !correlation_requirement_met(
            correlation.request.is_some(),
            descriptor.correlation_shape.request,
        ) || !correlation_requirement_met(
            correlation.generation.is_some(),
            descriptor.correlation_shape.generation,
        ) {
            return Err(ProtocolError::for_code(
                ProtocolErrorCode::InvalidCorrelation,
            ));
        }
        Ok(())
    }

    /// Validates one complete canonical viewport before render planning or allocation.
    pub fn validate_viewport_request(
        &self,
        viewport: &ViewportRequest,
    ) -> Result<(), ProtocolError> {
        if viewport.generation == 0
            || viewport.document_revision == 0
            || viewport.zoom_numerator == 0
            || viewport.zoom_denominator == 0
            || viewport.device_scale_milli == 0
            || greatest_common_divisor(viewport.zoom_numerator, viewport.zoom_denominator) != 1
            || viewport.visible_pages.len() > VIEWPORT_REQUEST_VISIBLE_PAGES_MAX_COUNT
        {
            return Err(ProtocolError::for_code(ProtocolErrorCode::InvalidViewport));
        }
        for (index, page) in viewport.visible_pages.iter().enumerate() {
            if !digest_is_nonzero(&page.geometry.identity)
                || page.geometry.media_box_width_milli_points == 0
                || page.geometry.media_box_height_milli_points == 0
                || page.geometry.crop_box_width_milli_points == 0
                || page.geometry.crop_box_height_milli_points == 0
                || page.clip_width_milli_points == 0
                || page.clip_height_milli_points == 0
                || viewport.visible_pages[..index].iter().any(|earlier| {
                    earlier.page_index == page.page_index
                        || earlier.geometry.identity == page.geometry.identity
                })
            {
                return Err(ProtocolError::for_code(ProtocolErrorCode::InvalidViewport));
            }
        }
        Ok(())
    }

    /// Validates SetViewport correlation and payload as one indivisible command contract.
    pub fn validate_set_viewport(
        &self,
        correlation: &crate::Correlation,
        command: &SetViewportCommand,
        worker: WorkerId,
        session: SessionId,
    ) -> Result<(), ProtocolError> {
        self.validate_correlation(MESSAGE_ID_SET_VIEWPORT, correlation, worker, Some(session))?;
        if correlation.generation != Some(command.viewport.generation) {
            return Err(ProtocolError::for_code(
                ProtocolErrorCode::InvalidCorrelation,
            ));
        }
        self.validate_viewport_request(&command.viewport)
    }

    /// Validates one ProvideData command against its actual received transfer byte lengths.
    pub fn validate_provide_data(
        &self,
        correlation: &crate::Correlation,
        command: &ProvideDataCommand,
        worker: WorkerId,
        session: SessionId,
        transfer_lengths: &[u64],
    ) -> Result<(), ProtocolError> {
        self.validate_correlation(MESSAGE_ID_PROVIDE_DATA, correlation, worker, Some(session))?;
        if command.segments.is_empty()
            || command.segments.len() > PROVIDE_DATA_COMMAND_SEGMENTS_MAX_COUNT
            || command.segments.len() != transfer_lengths.len()
            || transfer_lengths.len() > usize::from(self.limits.max_transfer_slots())
        {
            return Err(ProtocolError::for_code(
                ProtocolErrorCode::InvalidTransferCount,
            ));
        }

        for (index, segment) in command.segments.iter().enumerate() {
            if segment.range.len == 0 || segment.range.len != segment.byte_length {
                return Err(ProtocolError::for_code(ProtocolErrorCode::InvalidDataRange));
            }
            segment
                .range
                .start
                .checked_add(segment.range.len)
                .ok_or_else(|| ProtocolError::for_code(ProtocolErrorCode::NumericOverflow))?;

            let slot = usize::from(segment.slot);
            if slot != index || transfer_lengths[slot] != segment.byte_length {
                return Err(ProtocolError::for_code(
                    ProtocolErrorCode::InvalidTransferBinding,
                ));
            }
        }
        Ok(())
    }

    /// Validates Surface owner, generation, epoch, layout, format, range, and transfer binding.
    pub fn validate_surface(
        &self,
        metadata: &SurfaceMetadata,
        transport: &SurfaceTransport,
        context: &SurfaceValidationContext,
    ) -> Result<ValidatedSurface, ProtocolError> {
        if metadata.id.value() == 0
            || context.worker.value() == 0
            || context.session.value() == 0
            || metadata.owner.worker.value() == 0
            || metadata.owner.session.value() == 0
            || metadata.owner.worker != context.worker
            || metadata.owner.session != context.session
        {
            return Err(ProtocolError::for_code(
                ProtocolErrorCode::InvalidSurfaceOwner,
            ));
        }
        if metadata.generation == 0
            || context.generation == 0
            || metadata.generation != context.generation
            || metadata.renderer_epoch.value() == 0
            || metadata.renderer_epoch != context.plan.render.renderer_epoch
        {
            return Err(ProtocolError::for_code(
                ProtocolErrorCode::InvalidSurfaceEpoch,
            ));
        }
        let expected_render = context.plan.render;
        if expected_render.plan_id.value() == 0
            || metadata.plan_id.value() == 0
            || !digest_is_nonzero(expected_render.render_config.digest())
            || !digest_is_nonzero(expected_render.plan_hash.digest())
            || !digest_is_nonzero(expected_render.scene_hash.digest())
            || !digest_is_nonzero(expected_render.decision_hash.digest())
            || metadata.render_config != expected_render.render_config
            || metadata.plan_id != expected_render.plan_id
            || metadata.plan_hash != expected_render.plan_hash
            || metadata.scene_hash != expected_render.scene_hash
            || metadata.decision_hash != expected_render.decision_hash
            || metadata.backend != expected_render.backend
        {
            return Err(ProtocolError::for_code(
                ProtocolErrorCode::InvalidSurfacePlan,
            ));
        }
        if metadata.region != context.plan.region
            || metadata.region.width == 0
            || metadata.region.height == 0
        {
            return Err(ProtocolError::for_code(
                ProtocolErrorCode::InvalidSurfaceRegion,
            ));
        }
        if metadata.width == 0
            || metadata.height == 0
            || metadata.width > self.limits.max_surface_dimension()
            || metadata.height > self.limits.max_surface_dimension()
        {
            return Err(ProtocolError::for_code(
                ProtocolErrorCode::InvalidSurfaceLayout,
            ));
        }
        let bytes_per_pixel = match metadata.format {
            crate::PixelFormat::Rgba8 => 4_u64,
        };
        match metadata.alpha {
            crate::AlphaMode::Straight | crate::AlphaMode::Premultiplied => {}
        }
        let minimum_stride = u64::from(metadata.width)
            .checked_mul(bytes_per_pixel)
            .ok_or_else(|| ProtocolError::for_code(ProtocolErrorCode::NumericOverflow))?;
        let stride = u64::from(metadata.stride);
        if stride < minimum_stride
            || !stride.is_multiple_of(bytes_per_pixel)
            || stride > self.limits.max_surface_stride_bytes()
        {
            return Err(ProtocolError::for_code(
                ProtocolErrorCode::InvalidSurfaceLayout,
            ));
        }
        let layout_bytes = stride
            .checked_mul(u64::from(metadata.height))
            .ok_or_else(|| ProtocolError::for_code(ProtocolErrorCode::NumericOverflow))?;
        if layout_bytes > self.limits.max_surface_bytes() {
            return Err(ProtocolError::for_code(
                ProtocolErrorCode::InvalidSurfaceLayout,
            ));
        }

        if context.transfer_slots > usize::from(self.limits.max_transfer_slots()) {
            return Err(ProtocolError::for_code(
                ProtocolErrorCode::InvalidSurfaceSlot,
            ));
        }
        match (&context.resource, transport) {
            (
                SurfaceResourceBinding::OffscreenCanvas {
                    canvas: expected_canvas,
                    region_bytes,
                },
                SurfaceTransport::OffscreenCanvasCommit {
                    canvas,
                    region_length,
                },
            ) => {
                if canvas.value() == 0 || canvas != expected_canvas || context.transfer_slots != 0 {
                    return Err(ProtocolError::for_code(
                        ProtocolErrorCode::InvalidSurfaceSlot,
                    ));
                }
                validate_surface_range(
                    metadata.byte_offset,
                    metadata.byte_length,
                    layout_bytes,
                    *region_length,
                    *region_bytes,
                    self.limits.max_surface_bytes(),
                )?;
            }
            (
                SurfaceResourceBinding::BrowserTransfer {
                    slot: expected_slot,
                    kind: expected_kind,
                    region_bytes,
                },
                SurfaceTransport::BrowserTransfer {
                    slot,
                    transfer_kind,
                    transfer_length,
                },
            ) => {
                if context.transfer_slots != 1
                    || slot != expected_slot
                    || transfer_kind != expected_kind
                    || usize::from(*slot) >= context.transfer_slots
                {
                    return Err(ProtocolError::for_code(
                        ProtocolErrorCode::InvalidSurfaceSlot,
                    ));
                }
                validate_surface_range(
                    metadata.byte_offset,
                    metadata.byte_length,
                    layout_bytes,
                    *transfer_length,
                    *region_bytes,
                    self.limits.max_surface_bytes(),
                )?;
            }
            (
                SurfaceResourceBinding::SharedMemory {
                    handle: expected_handle,
                    region_bytes,
                },
                SurfaceTransport::SharedMemory {
                    handle,
                    region_length,
                    release_token,
                },
            ) => {
                if context.transfer_slots != 1
                    || handle != expected_handle
                    || handle.value() == 0
                    || *release_token == 0
                {
                    return Err(ProtocolError::for_code(
                        ProtocolErrorCode::InvalidSurfaceSlot,
                    ));
                }
                validate_surface_range(
                    metadata.byte_offset,
                    metadata.byte_length,
                    layout_bytes,
                    *region_length,
                    *region_bytes,
                    self.limits.max_surface_bytes(),
                )?;
            }
            (
                SurfaceResourceBinding::LocalMemory {
                    memory_epoch: expected_epoch,
                    region_bytes,
                },
                SurfaceTransport::LocalMemory {
                    region_length,
                    memory_epoch,
                },
            ) => {
                if context.transfer_slots != 0
                    || memory_epoch.value() == 0
                    || memory_epoch != expected_epoch
                {
                    return Err(ProtocolError::for_code(
                        ProtocolErrorCode::InvalidSurfaceEpoch,
                    ));
                }
                validate_surface_range(
                    metadata.byte_offset,
                    metadata.byte_length,
                    layout_bytes,
                    *region_length,
                    *region_bytes,
                    self.limits.max_surface_bytes(),
                )?;
            }
            _ => {
                return Err(ProtocolError::for_code(
                    ProtocolErrorCode::InvalidSurfaceSlot,
                ));
            }
        }
        Ok(ValidatedSurface {
            metadata: metadata.clone(),
            transport: transport.clone(),
            layout_bytes,
        })
    }
}

fn correlation_requirement_met(present: bool, requirement: CorrelationRequirement) -> bool {
    match requirement {
        CorrelationRequirement::Required => present,
        CorrelationRequirement::Optional => true,
        CorrelationRequirement::Forbidden => !present,
    }
}

fn opposite_roles(local: &EndpointRole, peer: &EndpointRole) -> bool {
    matches!(
        (local, peer),
        (EndpointRole::Host, EndpointRole::Engine) | (EndpointRole::Engine, EndpointRole::Host)
    )
}

fn validate_surface_range(
    offset: u64,
    len: u64,
    layout_bytes: u64,
    declared_region_bytes: u64,
    actual_region_bytes: u64,
    maximum_addressable_bytes: u64,
) -> Result<(), ProtocolError> {
    let end = offset
        .checked_add(len)
        .ok_or_else(|| ProtocolError::for_code(ProtocolErrorCode::NumericOverflow))?;
    if len != layout_bytes
        || declared_region_bytes == 0
        || declared_region_bytes != actual_region_bytes
        || end > declared_region_bytes
        || end > maximum_addressable_bytes
    {
        return Err(ProtocolError::for_code(
            ProtocolErrorCode::InvalidSurfaceRange,
        ));
    }
    Ok(())
}

fn digest_is_nonzero(digest: &[u8; 32]) -> bool {
    digest.iter().any(|byte| *byte != 0)
}

fn greatest_common_divisor(mut left: u32, mut right: u32) -> u32 {
    while right != 0 {
        let remainder = left % right;
        left = right;
        right = remainder;
    }
    left
}
