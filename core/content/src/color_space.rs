use pdf_rs_bytes::{ByteSource, DataTicket, JobId, RequestPriority, ResumeCheckpoint, SmallRanges};
use pdf_rs_document::{
    AttestedObject, AttestedObjectJobContext, AttestedObjectPoll, DocumentCancellation,
    DocumentError, OpenAttestedObjectJob, PageColorSpaceLookupLimits, PageResourceScope,
    SharedAttestedRevisionIndex,
};
use pdf_rs_syntax::{ObjectRef, SyntaxObject};

/// Device component model selected by one supported PDF color-space definition.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ContentColorSpaceKind {
    /// One additive gray component.
    Gray,
    /// Three additive red, green, and blue components.
    Rgb,
    /// Four subtractive cyan, magenta, yellow, and black components.
    Cmyk,
}

impl ContentColorSpaceKind {
    pub(super) const fn components(self) -> u8 {
        match self {
            Self::Gray => 1,
            Self::Rgb => 3,
            Self::Cmyk => 4,
        }
    }
}

/// Runtime identity and checkpoint namespace for resumable named color-space access.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ContentColorSpaceJobContext {
    job: JobId,
    checkpoint_base: ResumeCheckpoint,
    priority: RequestPriority,
}

impl ContentColorSpaceJobContext {
    /// Creates a deterministic namespace for color-space definitions and referenced ICC profiles.
    pub const fn new(
        job: JobId,
        checkpoint_base: ResumeCheckpoint,
        priority: RequestPriority,
    ) -> Self {
        Self {
            job,
            checkpoint_base,
            priority,
        }
    }

    fn object_context(
        self,
        scope: ObjectRef,
        slot: u64,
        max_resources: u64,
    ) -> Option<AttestedObjectJobContext> {
        let stride = max_resources.checked_mul(4)?.checked_add(4)?;
        let scope_key = u64::from(scope.number())
            .checked_mul(u64::from(u16::MAX).checked_add(1)?)?
            .checked_add(u64::from(scope.generation()))?;
        let offset = scope_key
            .checked_mul(stride)?
            .checked_add(slot.checked_mul(2)?)?;
        Some(AttestedObjectJobContext::new(
            self.job,
            ResumeCheckpoint::new(self.checkpoint_base.value().checked_add(offset)?),
            ResumeCheckpoint::new(
                self.checkpoint_base
                    .value()
                    .checked_add(offset)?
                    .checked_add(1)?,
            ),
            self.priority,
        ))
    }
}

/// Proof authority and bounded lookup context for Page- or Form-local named color spaces.
#[derive(Clone, Debug)]
pub struct ContentColorSpaceAcquisitionProfile {
    authority: SharedAttestedRevisionIndex,
    lookup_limits: PageColorSpaceLookupLimits,
    context: ContentColorSpaceJobContext,
}

impl ContentColorSpaceAcquisitionProfile {
    /// Creates a dynamic named color-space profile bound to one attested revision.
    pub const fn new(
        authority: SharedAttestedRevisionIndex,
        lookup_limits: PageColorSpaceLookupLimits,
        context: ContentColorSpaceJobContext,
    ) -> Self {
        Self {
            authority,
            lookup_limits,
            context,
        }
    }

    /// Borrows the revision authority used to reopen selected definitions and ICC profiles.
    pub const fn authority(&self) -> &SharedAttestedRevisionIndex {
        &self.authority
    }
}

struct ColorSpaceResource {
    selector: ColorSpaceSelector,
    kind: ContentColorSpaceKind,
    _definition: AttestedObject,
    _icc_profile: Option<AttestedObject>,
}

#[derive(Eq, PartialEq)]
enum ColorSpaceSelector {
    Name(Vec<u8>),
    Reference(ObjectRef),
}

enum ActiveColorSpace {
    Definition {
        selector: ColorSpaceSelector,
        ordinal: u64,
        job: OpenAttestedObjectJob,
    },
    IccProfile {
        selector: ColorSpaceSelector,
        definition: AttestedObject,
        job: OpenAttestedObjectJob,
    },
}

pub(super) struct ContentColorSpaceRuntime {
    profile: ContentColorSpaceAcquisitionProfile,
    resources: Vec<ColorSpaceResource>,
    active: Option<ActiveColorSpace>,
}

impl ContentColorSpaceRuntime {
    pub(super) fn new(profile: ContentColorSpaceAcquisitionProfile) -> Self {
        Self {
            profile,
            resources: Vec::new(),
            active: None,
        }
    }

    pub(super) fn resolve(
        &mut self,
        scope: &PageResourceScope,
        name: &[u8],
        source: &dyn ByteSource,
        cancellation: &dyn DocumentCancellation,
    ) -> ContentColorSpaceRuntimePoll {
        if let Some(resource) = self
            .resources
            .iter()
            .find(|resource| resource.selector.matches_name(name))
        {
            return ContentColorSpaceRuntimePoll::Ready(resource.kind);
        }
        if self
            .active
            .as_ref()
            .is_some_and(|active| !active.selector().matches_name(name))
        {
            return ContentColorSpaceRuntimePoll::Internal;
        }
        if self.active.is_none() {
            let mut resolver = scope.color_space_resolver(self.profile.lookup_limits);
            let proof = match resolver.lookup_color_space(name, source, cancellation) {
                Ok(value) => value,
                Err(error) => return ContentColorSpaceRuntimePoll::Failed(error),
            };
            let authority = self.profile.authority.as_attested();
            if proof.snapshot() != authority.snapshot()
                || proof.revision_id() != authority.revision_id()
                || proof.revision_startxref() != authority.startxref()
            {
                return ContentColorSpaceRuntimePoll::Internal;
            }
            let mut retained_name = Vec::new();
            if retained_name.try_reserve_exact(name.len()).is_err() {
                return ContentColorSpaceRuntimePoll::ResourceLimit;
            }
            retained_name.extend_from_slice(name);
            if let Some(terminal) = self.start_definition(
                scope,
                ColorSpaceSelector::Name(retained_name),
                proof.target(),
            ) {
                return terminal;
            }
        }
        self.poll_active(scope, source, cancellation)
    }

    pub(super) fn resolve_reference(
        &mut self,
        scope: &PageResourceScope,
        reference: ObjectRef,
        source: &dyn ByteSource,
        cancellation: &dyn DocumentCancellation,
    ) -> ContentColorSpaceRuntimePoll {
        if let Some(resource) = self
            .resources
            .iter()
            .find(|resource| resource.selector.matches_reference(reference))
        {
            return ContentColorSpaceRuntimePoll::Ready(resource.kind);
        }
        if self
            .active
            .as_ref()
            .is_some_and(|active| !active.selector().matches_reference(reference))
        {
            return ContentColorSpaceRuntimePoll::Internal;
        }
        if self.active.is_none()
            && let Some(terminal) =
                self.start_definition(scope, ColorSpaceSelector::Reference(reference), reference)
        {
            return terminal;
        }
        self.poll_active(scope, source, cancellation)
    }

    fn start_definition(
        &mut self,
        scope: &PageResourceScope,
        selector: ColorSpaceSelector,
        reference: ObjectRef,
    ) -> Option<ContentColorSpaceRuntimePoll> {
        let ordinal = match u64::try_from(self.resources.len()) {
            Ok(value) if value < self.profile.lookup_limits.max_lookups() => value,
            _ => return Some(ContentColorSpaceRuntimePoll::ResourceLimit),
        };
        let Some(context) = self.profile.context.object_context(
            scope.defining_object(),
            ordinal.checked_mul(2).unwrap_or(u64::MAX),
            self.profile.lookup_limits.max_lookups(),
        ) else {
            return Some(ContentColorSpaceRuntimePoll::Internal);
        };
        let authority = self.profile.authority.as_attested();
        let job = match authority.open_object_with_attested_work_caps(reference, context) {
            Ok(value) => value,
            Err(error) => return Some(ContentColorSpaceRuntimePoll::Failed(error)),
        };
        self.active = Some(ActiveColorSpace::Definition {
            selector,
            ordinal,
            job,
        });
        None
    }

    fn poll_active(
        &mut self,
        scope: &PageResourceScope,
        source: &dyn ByteSource,
        cancellation: &dyn DocumentCancellation,
    ) -> ContentColorSpaceRuntimePoll {
        loop {
            let poll = match self
                .active
                .as_mut()
                .expect("an unresolved color space retains one active object job")
            {
                ActiveColorSpace::Definition { job, .. }
                | ActiveColorSpace::IccProfile { job, .. } => job.poll(source, cancellation),
            };
            match poll {
                AttestedObjectPoll::Pending {
                    ticket,
                    missing,
                    checkpoint,
                } => {
                    return ContentColorSpaceRuntimePoll::Pending {
                        ticket,
                        missing,
                        checkpoint,
                    };
                }
                AttestedObjectPoll::Failed(error) => {
                    self.active.take();
                    return ContentColorSpaceRuntimePoll::Failed(error);
                }
                AttestedObjectPoll::Ready(object) => {
                    let active = self
                        .active
                        .take()
                        .expect("ready color-space object retains its stage");
                    match active {
                        ActiveColorSpace::Definition {
                            selector, ordinal, ..
                        } => match classify_definition(&object) {
                            DefinitionOutcome::Ready(kind) => {
                                return self.publish(selector, kind, object, None);
                            }
                            DefinitionOutcome::IccBased(profile_reference) => {
                                let Some(context) = self.profile.context.object_context(
                                    scope.defining_object(),
                                    ordinal
                                        .checked_mul(2)
                                        .and_then(|value| value.checked_add(1))
                                        .unwrap_or(u64::MAX),
                                    self.profile.lookup_limits.max_lookups(),
                                ) else {
                                    return ContentColorSpaceRuntimePoll::Internal;
                                };
                                let authority = self.profile.authority.as_attested();
                                let job = match authority
                                    .open_object_with_attested_work_caps(profile_reference, context)
                                {
                                    Ok(value) => value,
                                    Err(error) => {
                                        return ContentColorSpaceRuntimePoll::Failed(error);
                                    }
                                };
                                self.active = Some(ActiveColorSpace::IccProfile {
                                    selector,
                                    definition: object,
                                    job,
                                });
                            }
                            DefinitionOutcome::Unsupported => {
                                return ContentColorSpaceRuntimePoll::Unsupported;
                            }
                        },
                        ActiveColorSpace::IccProfile {
                            selector,
                            definition,
                            ..
                        } => {
                            let Some(kind) = classify_icc_profile(&object) else {
                                return ContentColorSpaceRuntimePoll::Unsupported;
                            };
                            return self.publish(selector, kind, definition, Some(object));
                        }
                    }
                }
            }
        }
    }

    fn publish(
        &mut self,
        selector: ColorSpaceSelector,
        kind: ContentColorSpaceKind,
        definition: AttestedObject,
        icc_profile: Option<AttestedObject>,
    ) -> ContentColorSpaceRuntimePoll {
        if self.resources.try_reserve_exact(1).is_err() {
            return ContentColorSpaceRuntimePoll::ResourceLimit;
        }
        self.resources.push(ColorSpaceResource {
            selector,
            kind,
            _definition: definition,
            _icc_profile: icc_profile,
        });
        ContentColorSpaceRuntimePoll::Ready(kind)
    }
}

impl ActiveColorSpace {
    fn selector(&self) -> &ColorSpaceSelector {
        match self {
            Self::Definition { selector, .. } | Self::IccProfile { selector, .. } => selector,
        }
    }
}

impl ColorSpaceSelector {
    fn matches_name(&self, name: &[u8]) -> bool {
        match self {
            Self::Name(value) => value == name,
            Self::Reference(_) => false,
        }
    }

    const fn matches_reference(&self, reference: ObjectRef) -> bool {
        match self {
            Self::Name(_) => false,
            Self::Reference(value) => {
                value.number() == reference.number() && value.generation() == reference.generation()
            }
        }
    }
}

enum DefinitionOutcome {
    Ready(ContentColorSpaceKind),
    IccBased(ObjectRef),
    Unsupported,
}

fn classify_definition(object: &AttestedObject) -> DefinitionOutcome {
    let Some(value) = object.direct_value() else {
        return DefinitionOutcome::Unsupported;
    };
    match value.value() {
        SyntaxObject::Name(name) => device_space(name.bytes())
            .map_or(DefinitionOutcome::Unsupported, DefinitionOutcome::Ready),
        SyntaxObject::Array(array) if array.values().len() == 2 => {
            let values = array.values();
            match (values[0].value(), values[1].value()) {
                (SyntaxObject::Name(family), SyntaxObject::Reference(profile))
                    if family.bytes() == b"ICCBased" =>
                {
                    DefinitionOutcome::IccBased(*profile)
                }
                _ => DefinitionOutcome::Unsupported,
            }
        }
        _ => DefinitionOutcome::Unsupported,
    }
}

fn classify_icc_profile(object: &AttestedObject) -> Option<ContentColorSpaceKind> {
    let dictionary = object.stream_dictionary()?.value();
    let mut components = None;
    let mut alternate = None;
    for entry in dictionary.entries() {
        match entry.key().value().bytes() {
            b"N" => {
                if components.is_some() {
                    return None;
                }
                let SyntaxObject::Integer(value @ (1 | 3 | 4)) = entry.value().value() else {
                    return None;
                };
                components = Some(u8::try_from(*value).ok()?);
            }
            b"Alternate" => {
                if alternate.is_some() {
                    return None;
                }
                let SyntaxObject::Name(name) = entry.value().value() else {
                    return None;
                };
                alternate = device_space(name.bytes());
                alternate?;
            }
            b"Range" => return None,
            _ => {}
        }
    }
    let components = components?;
    let default = match components {
        1 => ContentColorSpaceKind::Gray,
        3 => ContentColorSpaceKind::Rgb,
        4 => ContentColorSpaceKind::Cmyk,
        _ => return None,
    };
    let kind = alternate.unwrap_or(default);
    (kind.components() == components).then_some(kind)
}

pub(super) fn device_space(name: &[u8]) -> Option<ContentColorSpaceKind> {
    match name {
        b"DeviceGray" | b"G" => Some(ContentColorSpaceKind::Gray),
        b"DeviceRGB" | b"RGB" => Some(ContentColorSpaceKind::Rgb),
        b"DeviceCMYK" | b"CMYK" => Some(ContentColorSpaceKind::Cmyk),
        _ => None,
    }
}

pub(super) enum ContentColorSpaceRuntimePoll {
    Ready(ContentColorSpaceKind),
    Pending {
        ticket: DataTicket,
        missing: SmallRanges,
        checkpoint: ResumeCheckpoint,
    },
    Unsupported,
    Failed(DocumentError),
    ResourceLimit,
    Internal,
}
