use std::fmt;

use pdf_rs_bytes::{ByteSource, DataTicket, JobId, RequestPriority, ResumeCheckpoint, SmallRanges};
use pdf_rs_document::{
    AttestedObject, AttestedObjectJobContext, AttestedObjectPoll, DocumentCancellation,
    DocumentError, OpenAttestedObjectJob, PageExtGStateLookupLimits, PageResourceScope,
    SharedAttestedRevisionIndex,
};
use pdf_rs_scene::{BlendMode, SceneUnit};
use pdf_rs_syntax::{ObjectRef, PdfReal, SyntaxObject};

/// Runtime-owned identity namespace for resumable ExtGState object access.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ContentExtGStateJobContext {
    job: JobId,
    checkpoint_base: ResumeCheckpoint,
    priority: RequestPriority,
}

impl ContentExtGStateJobContext {
    /// Creates a deterministic namespace whose per-resource identities are derived from scope and
    /// source-order ordinal.
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
        ordinal: u64,
        max_resources: u64,
    ) -> Option<AttestedObjectJobContext> {
        let stride = max_resources.checked_mul(3)?.checked_add(3)?;
        let scope_key = u64::from(scope.number())
            .checked_mul(u64::from(u16::MAX).checked_add(1)?)?
            .checked_add(u64::from(scope.generation()))?;
        let offset = scope_key
            .checked_mul(stride)?
            .checked_add(ordinal.checked_mul(3)?)?;
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

/// Proof authority and bounded lookup context for Page- or Form-local ExtGState resolution.
#[derive(Clone, Debug)]
pub struct ContentExtGStateAcquisitionProfile {
    authority: SharedAttestedRevisionIndex,
    lookup_limits: PageExtGStateLookupLimits,
    context: ContentExtGStateJobContext,
}

impl ContentExtGStateAcquisitionProfile {
    /// Creates a dynamic ExtGState profile bound to one attested revision.
    pub const fn new(
        authority: SharedAttestedRevisionIndex,
        lookup_limits: PageExtGStateLookupLimits,
        context: ContentExtGStateJobContext,
    ) -> Self {
        Self {
            authority,
            lookup_limits,
            context,
        }
    }

    /// Borrows the revision authority used to reopen selected state dictionaries.
    pub const fn authority(&self) -> &SharedAttestedRevisionIndex {
        &self.authority
    }

    /// Returns the per-resource name lookup limits.
    pub const fn lookup_limits(&self) -> PageExtGStateLookupLimits {
        self.lookup_limits
    }

    /// Returns the runtime-owned object-access namespace.
    pub const fn context(&self) -> ContentExtGStateJobContext {
        self.context
    }
}

/// Stable reason why a selected external graphics state is outside the registered subset.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ContentExtGStateErrorKind {
    /// The supplied resource name is empty or above the fixed retained-name ceiling.
    InvalidName,
    /// The selected indirect object is not one direct dictionary.
    InvalidDictionary,
    /// A structural key occurs more than once.
    DuplicateEntry,
    /// `/Type`, `/CA`, `/ca`, or `/BM` has a malformed value.
    InvalidValue,
    /// The dictionary selects an ExtGState entry outside the registered screen-raster subset.
    UnsupportedEntry,
}

/// Source-redacted external graphics-state construction error.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ContentExtGStateError {
    kind: ContentExtGStateErrorKind,
    reference: ObjectRef,
    offset: u64,
}

impl ContentExtGStateError {
    const fn new(kind: ContentExtGStateErrorKind, reference: ObjectRef, offset: u64) -> Self {
        Self {
            kind,
            reference,
            offset,
        }
    }

    /// Returns the stable rejected capability or syntax class.
    pub const fn kind(self) -> ContentExtGStateErrorKind {
        self.kind
    }

    /// Returns the selected indirect object identity.
    pub const fn reference(self) -> ObjectRef {
        self.reference
    }

    /// Returns the exact physical source offset associated with the rejection.
    pub const fn offset(self) -> u64 {
        self.offset
    }
}

/// One proof-retaining Page ExtGState resource in the supported alpha/blend subset.
pub struct ContentExtGStateResource {
    name: Vec<u8>,
    object: AttestedObject,
    stroking_alpha: Option<SceneUnit>,
    nonstroking_alpha: Option<SceneUnit>,
    blend_mode: Option<BlendMode>,
}

impl ContentExtGStateResource {
    /// Parses one reopened proof-bound ExtGState dictionary and retains its resource name.
    ///
    /// The registered subset accepts optional `/Type /ExtGState`, direct numeric `/CA` and `/ca`
    /// in the closed unit interval, and `/BM` names `Normal`, `Multiply`, or `Screen`. Standard
    /// screen-compatibility controls `/AIS`, `/OP`, `/op`, `/OPM`, and `/SA` are shape-validated
    /// while `/SMask` is accepted only when it explicitly selects `/None`. Other entries remain
    /// an explicit unsupported capability instead of being silently discarded.
    pub fn new(name: Vec<u8>, object: AttestedObject) -> Result<Self, ContentExtGStateError> {
        let reference = object.reference();
        let object_offset = object.object_span().start();
        if name.is_empty() || name.len() > 127 {
            return Err(ContentExtGStateError::new(
                ContentExtGStateErrorKind::InvalidName,
                reference,
                object_offset,
            ));
        }
        let Some(value) = object.direct_value() else {
            return Err(ContentExtGStateError::new(
                ContentExtGStateErrorKind::InvalidDictionary,
                reference,
                object_offset,
            ));
        };
        let SyntaxObject::Dictionary(dictionary) = value.value() else {
            return Err(ContentExtGStateError::new(
                ContentExtGStateErrorKind::InvalidDictionary,
                reference,
                value.span().start(),
            ));
        };

        let mut seen_type = false;
        let mut stroking_alpha = None;
        let mut nonstroking_alpha = None;
        let mut blend_mode = None;
        let mut alpha_is_shape = None;
        let mut stroking_overprint = None;
        let mut nonstroking_overprint = None;
        let mut overprint_mode = None;
        let mut stroke_adjustment = None;
        let mut soft_mask_none = false;
        for entry in dictionary.entries() {
            let key = entry.key().value().bytes();
            let offset = entry.value().span().start();
            match key {
                b"Type" => {
                    if seen_type {
                        return Err(duplicate(reference, offset));
                    }
                    seen_type = true;
                    match entry.value().value() {
                        SyntaxObject::Name(name) if name.bytes() == b"ExtGState" => {}
                        _ => return Err(invalid_value(reference, offset)),
                    }
                }
                b"CA" => {
                    if stroking_alpha.is_some() {
                        return Err(duplicate(reference, offset));
                    }
                    stroking_alpha = Some(
                        parse_unit(entry.value().value())
                            .ok_or_else(|| invalid_value(reference, offset))?,
                    );
                }
                b"ca" => {
                    if nonstroking_alpha.is_some() {
                        return Err(duplicate(reference, offset));
                    }
                    nonstroking_alpha = Some(
                        parse_unit(entry.value().value())
                            .ok_or_else(|| invalid_value(reference, offset))?,
                    );
                }
                b"BM" => {
                    if blend_mode.is_some() {
                        return Err(duplicate(reference, offset));
                    }
                    let SyntaxObject::Name(name) = entry.value().value() else {
                        return Err(invalid_value(reference, offset));
                    };
                    blend_mode = Some(match name.bytes() {
                        b"Normal" | b"Compatible" => BlendMode::Normal,
                        b"Multiply" => BlendMode::Multiply,
                        b"Screen" => BlendMode::Screen,
                        _ => {
                            return Err(ContentExtGStateError::new(
                                ContentExtGStateErrorKind::UnsupportedEntry,
                                reference,
                                offset,
                            ));
                        }
                    });
                }
                b"AIS" => {
                    parse_boolean_entry(
                        &mut alpha_is_shape,
                        entry.value().value(),
                        reference,
                        offset,
                    )?;
                }
                b"OP" => {
                    parse_boolean_entry(
                        &mut stroking_overprint,
                        entry.value().value(),
                        reference,
                        offset,
                    )?;
                }
                b"op" => {
                    parse_boolean_entry(
                        &mut nonstroking_overprint,
                        entry.value().value(),
                        reference,
                        offset,
                    )?;
                }
                b"OPM" => {
                    if overprint_mode.is_some() {
                        return Err(duplicate(reference, offset));
                    }
                    overprint_mode = Some(match entry.value().value() {
                        SyntaxObject::Integer(value @ (0 | 1)) => *value,
                        _ => return Err(invalid_value(reference, offset)),
                    });
                }
                b"SA" => {
                    parse_boolean_entry(
                        &mut stroke_adjustment,
                        entry.value().value(),
                        reference,
                        offset,
                    )?;
                }
                b"SMask" => {
                    if soft_mask_none {
                        return Err(duplicate(reference, offset));
                    }
                    match entry.value().value() {
                        SyntaxObject::Name(name) if name.bytes() == b"None" => {
                            soft_mask_none = true;
                        }
                        _ => {
                            return Err(ContentExtGStateError::new(
                                ContentExtGStateErrorKind::UnsupportedEntry,
                                reference,
                                offset,
                            ));
                        }
                    }
                }
                _ => {
                    return Err(ContentExtGStateError::new(
                        ContentExtGStateErrorKind::UnsupportedEntry,
                        reference,
                        offset,
                    ));
                }
            }
        }

        Ok(Self {
            name,
            object,
            stroking_alpha,
            nonstroking_alpha,
            blend_mode,
        })
    }

    /// Returns the retained decoded resource name.
    pub fn name(&self) -> &[u8] {
        &self.name
    }

    /// Borrows the reopened object proof retained by this resource.
    pub const fn object(&self) -> &AttestedObject {
        &self.object
    }

    /// Returns the optional stroking constant alpha.
    pub const fn stroking_alpha(&self) -> Option<SceneUnit> {
        self.stroking_alpha
    }

    /// Returns the optional nonstroking constant alpha.
    pub const fn nonstroking_alpha(&self) -> Option<SceneUnit> {
        self.nonstroking_alpha
    }

    /// Returns the optional supported blend mode.
    pub const fn blend_mode(&self) -> Option<BlendMode> {
        self.blend_mode
    }
}

impl fmt::Debug for ContentExtGStateResource {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ContentExtGStateResource")
            .field("name_len", &self.name.len())
            .field("name", &"[REDACTED]")
            .field("reference", &self.object.reference())
            .field("stroking_alpha", &self.stroking_alpha)
            .field("nonstroking_alpha", &self.nonstroking_alpha)
            .field("blend_mode", &self.blend_mode)
            .finish()
    }
}

/// Complete proof-bound ExtGState registry for one interpreted Page.
pub struct ContentExtGStateProfile {
    resources: Vec<ContentExtGStateResource>,
}

impl ContentExtGStateProfile {
    /// Validates unique names and one immutable source/revision across all resources.
    pub fn new(resources: Vec<ContentExtGStateResource>) -> Result<Self, ContentExtGStateError> {
        for (index, resource) in resources.iter().enumerate() {
            let reference = resource.object.reference();
            let offset = resource.object.object_span().start();
            if resources[index + 1..]
                .iter()
                .any(|candidate| candidate.name == resource.name)
            {
                return Err(ContentExtGStateError::new(
                    ContentExtGStateErrorKind::DuplicateEntry,
                    reference,
                    offset,
                ));
            }
            if let Some(first) = resources.first()
                && (resource.object.snapshot() != first.object.snapshot()
                    || resource.object.revision_id() != first.object.revision_id()
                    || resource.object.revision_startxref() != first.object.revision_startxref())
            {
                return Err(ContentExtGStateError::new(
                    ContentExtGStateErrorKind::InvalidDictionary,
                    reference,
                    offset,
                ));
            }
        }
        Ok(Self { resources })
    }

    pub(crate) fn find(&self, name: &[u8]) -> Option<&ContentExtGStateResource> {
        self.resources
            .iter()
            .find(|resource| resource.name() == name)
    }

    /// Returns proof-bound resources in registry order.
    pub fn resources(&self) -> &[ContentExtGStateResource] {
        &self.resources
    }
}

#[derive(Clone, Copy)]
pub(super) struct ResolvedExtGState {
    pub(super) stroking_alpha: Option<SceneUnit>,
    pub(super) nonstroking_alpha: Option<SceneUnit>,
    pub(super) blend_mode: Option<BlendMode>,
}

impl ResolvedExtGState {
    fn from_resource(resource: &ContentExtGStateResource) -> Self {
        Self {
            stroking_alpha: resource.stroking_alpha(),
            nonstroking_alpha: resource.nonstroking_alpha(),
            blend_mode: resource.blend_mode(),
        }
    }
}

struct ActiveExtGState {
    name: Vec<u8>,
    job: OpenAttestedObjectJob,
}

pub(super) struct ContentExtGStateRuntime {
    profile: ContentExtGStateAcquisitionProfile,
    resources: Vec<ContentExtGStateResource>,
    active: Option<ActiveExtGState>,
}

impl ContentExtGStateRuntime {
    pub(super) fn new(profile: ContentExtGStateAcquisitionProfile) -> Self {
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
    ) -> ContentExtGStateRuntimePoll {
        if let Some(resource) = self
            .resources
            .iter()
            .find(|resource| resource.name() == name)
        {
            return ContentExtGStateRuntimePoll::Ready(ResolvedExtGState::from_resource(resource));
        }
        if self
            .active
            .as_ref()
            .is_some_and(|active| active.name != name)
        {
            return ContentExtGStateRuntimePoll::Internal;
        }
        if self.active.is_none() {
            let ordinal = match u64::try_from(self.resources.len()) {
                Ok(value) if value < self.profile.lookup_limits.max_lookups() => value,
                _ => return ContentExtGStateRuntimePoll::ResourceLimit,
            };
            let mut resolver = scope.ext_gstate_resolver(self.profile.lookup_limits);
            let proof = match resolver.lookup_ext_gstate(name, source, cancellation) {
                Ok(value) => value,
                Err(error) => return ContentExtGStateRuntimePoll::Failed(error),
            };
            let authority = self.profile.authority.as_attested();
            if proof.snapshot() != authority.snapshot()
                || proof.revision_id() != authority.revision_id()
                || proof.revision_startxref() != authority.startxref()
            {
                return ContentExtGStateRuntimePoll::Internal;
            }
            let Some(context) = self.profile.context.object_context(
                scope.defining_object(),
                ordinal,
                self.profile.lookup_limits.max_lookups(),
            ) else {
                return ContentExtGStateRuntimePoll::Internal;
            };
            let job = match authority.open_object_with_attested_work_caps(proof.target(), context) {
                Ok(value) => value,
                Err(error) => return ContentExtGStateRuntimePoll::Failed(error),
            };
            let mut retained_name = Vec::new();
            if retained_name.try_reserve_exact(name.len()).is_err() {
                return ContentExtGStateRuntimePoll::ResourceLimit;
            }
            retained_name.extend_from_slice(name);
            self.active = Some(ActiveExtGState {
                name: retained_name,
                job,
            });
        }

        let poll = {
            let active = self
                .active
                .as_mut()
                .expect("an unresolved ExtGState installs one active object job");
            active.job.poll(source, cancellation)
        };
        match poll {
            AttestedObjectPoll::Ready(object) => {
                let active = self
                    .active
                    .take()
                    .expect("ready ExtGState retains its resource name");
                let resource = match ContentExtGStateResource::new(active.name, object) {
                    Ok(value) => value,
                    Err(_) => return ContentExtGStateRuntimePoll::Unsupported,
                };
                let resolved = ResolvedExtGState::from_resource(&resource);
                if self.resources.try_reserve_exact(1).is_err() {
                    return ContentExtGStateRuntimePoll::ResourceLimit;
                }
                self.resources.push(resource);
                ContentExtGStateRuntimePoll::Ready(resolved)
            }
            AttestedObjectPoll::Pending {
                ticket,
                missing,
                checkpoint,
            } => ContentExtGStateRuntimePoll::Pending {
                ticket,
                missing,
                checkpoint,
            },
            AttestedObjectPoll::Failed(error) => {
                self.active.take();
                ContentExtGStateRuntimePoll::Failed(error)
            }
        }
    }
}

pub(super) enum ContentExtGStateRuntimePoll {
    Ready(ResolvedExtGState),
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

fn duplicate(reference: ObjectRef, offset: u64) -> ContentExtGStateError {
    ContentExtGStateError::new(ContentExtGStateErrorKind::DuplicateEntry, reference, offset)
}

fn invalid_value(reference: ObjectRef, offset: u64) -> ContentExtGStateError {
    ContentExtGStateError::new(ContentExtGStateErrorKind::InvalidValue, reference, offset)
}

fn parse_boolean_entry(
    slot: &mut Option<bool>,
    value: &SyntaxObject,
    reference: ObjectRef,
    offset: u64,
) -> Result<(), ContentExtGStateError> {
    if slot.is_some() {
        return Err(duplicate(reference, offset));
    }
    let SyntaxObject::Boolean(value) = value else {
        return Err(invalid_value(reference, offset));
    };
    *slot = Some(*value);
    Ok(())
}

fn parse_unit(value: &SyntaxObject) -> Option<SceneUnit> {
    let (numerator, denominator) = match value {
        SyntaxObject::Integer(0) => (0_i128, 1_i128),
        SyntaxObject::Integer(1) => (1_i128, 1_i128),
        SyntaxObject::Integer(_) => return None,
        SyntaxObject::Real(real) => decimal_ratio(real)?,
        _ => return None,
    };
    if numerator < 0 || numerator > denominator || denominator <= 0 {
        return None;
    }
    let scaled = numerator
        .checked_mul(i128::from(u16::MAX))?
        .checked_add(denominator / 2)?
        / denominator;
    Some(SceneUnit::from_u16(u16::try_from(scaled).ok()?))
}

fn decimal_ratio(real: &PdfReal) -> Option<(i128, i128)> {
    let raw = real.raw();
    let (negative, unsigned) = match raw.first()? {
        b'-' => (true, raw.get(1..)?),
        b'+' => (false, raw.get(1..)?),
        _ => (false, raw),
    };
    let exponent_index = unsigned.iter().position(|byte| matches!(byte, b'e' | b'E'));
    let (mantissa, exponent) = match exponent_index {
        Some(index) => (
            unsigned.get(..index)?,
            parse_exponent(unsigned.get(index + 1..)?),
        ),
        None => (unsigned, Some(0)),
    };
    let exponent = exponent?;
    let mut digits = 0_i128;
    let mut fractional = 0_i32;
    let mut after_decimal = false;
    let mut saw_digit = false;
    for byte in mantissa {
        match byte {
            b'0'..=b'9' => {
                saw_digit = true;
                digits = digits
                    .checked_mul(10)?
                    .checked_add(i128::from(byte - b'0'))?;
                if after_decimal {
                    fractional = fractional.checked_add(1)?;
                }
            }
            b'.' if !after_decimal => after_decimal = true,
            _ => return None,
        }
    }
    if !saw_digit {
        return None;
    }
    let scale = fractional.checked_sub(exponent)?;
    let (mut numerator, denominator) = if scale >= 0 {
        (digits, pow10(u32::try_from(scale).ok()?)?)
    } else {
        (digits.checked_mul(pow10(scale.unsigned_abs())?)?, 1_i128)
    };
    if negative {
        numerator = numerator.checked_neg()?;
    }
    Some((numerator, denominator))
}

fn parse_exponent(bytes: &[u8]) -> Option<i32> {
    let text = std::str::from_utf8(bytes).ok()?;
    text.parse().ok()
}

fn pow10(exponent: u32) -> Option<i128> {
    let mut value = 1_i128;
    for _ in 0..exponent {
        value = value.checked_mul(10)?;
    }
    Some(value)
}
