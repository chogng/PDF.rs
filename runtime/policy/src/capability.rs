use pdf_rs_bytes::SourceIdentity;
pub use pdf_rs_protocol::CapabilityProfileId;
use pdf_rs_scene::{
    CapabilityContext as SceneCapabilityContext, CapabilityRequirement as SceneRequirement,
    CapabilityStatus as SceneCapabilityStatus, GraphicsCapability, GraphicsResource, GraphicsScene,
    Scene, SceneCanonicalObserver, SceneErrorCode, SceneVersion,
};

use crate::canonical_hash::CanonicalHasher;
use crate::{CapabilityDecisionHash, PolicyError, PolicyLimitKind, PolicyLimits, SceneHash};

const DECISION_SCHEMA_VERSION: u16 = 1;
const M3_REFERENCE_PROFILE_VERSION: u32 = 1;
const M3_REFERENCE_POLICY_VERSION: u32 = 1;

/// Cooperative cancellation observed by product capability evaluation and render planning.
pub trait PolicyCancellation: Send + Sync {
    /// Returns whether the caller no longer needs the result.
    fn is_cancelled(&self) -> bool;
}

/// Cancellation token that never cancels.
#[derive(Clone, Copy, Debug, Default)]
pub struct NeverCancelled;

impl PolicyCancellation for NeverCancelled {
    fn is_cancelled(&self) -> bool {
        false
    }
}

/// One immutable versioned product capability policy.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CapabilityProfile {
    id: CapabilityProfileId,
    profile_version: u32,
    policy_version: u32,
}

impl CapabilityProfile {
    /// Returns the first Native profile, independently matching the registered M3 Reference
    /// requirement predicate without depending on or invoking the Reference renderer.
    pub const fn m3_reference_v1() -> Self {
        Self {
            id: CapabilityProfileId::BaselineNative,
            profile_version: M3_REFERENCE_PROFILE_VERSION,
            policy_version: M3_REFERENCE_POLICY_VERSION,
        }
    }

    /// Returns the stable profile identifier.
    pub const fn id(self) -> CapabilityProfileId {
        self.id
    }

    /// Returns the semantic profile version.
    pub const fn profile_version(self) -> u32 {
        self.profile_version
    }

    /// Returns the product policy version.
    pub const fn policy_version(self) -> u32 {
        self.policy_version
    }

    fn directly_supports(self, requirement: &SceneRequirement) -> bool {
        if requirement.status() == SceneCapabilityStatus::Unsupported {
            return false;
        }
        let parameter = requirement.parameter();
        match requirement.capability() {
            GraphicsCapability::PathFill | GraphicsCapability::Clip => parameter <= 1,
            GraphicsCapability::PathStroke => parameter == 0,
            GraphicsCapability::DeviceColor => matches!(parameter, 1 | 3 | 4),
            GraphicsCapability::ConstantAlpha => parameter <= u64::from(u16::MAX),
            GraphicsCapability::Blend => matches!(parameter, 0..=2),
            GraphicsCapability::Image => {
                let components = parameter & 0xff;
                let bits = (parameter >> 8) & 0xff;
                let interpolate = (parameter >> 16) & 1;
                parameter >> 17 == 0
                    && matches!(components, 1 | 3 | 4)
                    && bits == 8
                    && interpolate == 0
            }
            GraphicsCapability::Glyph => parameter != 0,
            GraphicsCapability::SoftMask | GraphicsCapability::IsolatedGroup => false,
        }
    }
}

impl Default for CapabilityProfile {
    fn default() -> Self {
        Self::m3_reference_v1()
    }
}

/// Product support outcome, distinct from operation errors and cancellation.
#[repr(u8)]
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum CapabilityStatus {
    /// The complete graph belongs to the selected profile.
    Supported = 1,
    /// At least one well-formed requirement is outside the selected profile or its dependency
    /// closure.
    Unsupported = 2,
    /// The graph is malformed or violates an explicit product policy invariant.
    Rejected = 3,
}

/// Whether a retained canonical prefix contains the complete evaluated collection.
#[repr(u8)]
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum CollectionCompleteness {
    /// Retained length equals the exact total.
    Complete = 1,
    /// Retained length is a strict canonical prefix of the exact total.
    Truncated = 2,
}

/// Stable rejection reason for malformed graphs and explicit policy prohibitions.
#[repr(u32)]
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum CapabilityRejectionCode {
    /// Requirement IDs are not contiguous canonical zero-based IDs.
    NonCanonicalRequirementId = 1,
    /// A requirement context refers to a missing command or resource.
    InvalidContext = 2,
    /// Dependencies are duplicate, forward, self, descending, or otherwise noncanonical.
    InvalidDependencyGraph = 3,
    /// One requirement exceeds the generated protocol's bounded dependency fanout.
    DependencyFanoutProhibited = 4,
    /// A graphics resource table is not in canonical ID order.
    NonCanonicalResourceId = 5,
    /// The product raster policy only accepts the graphics-capable Scene schema.
    UnsupportedSceneSchema = 6,
}

/// Canonical scope of a decision, requirement, or contributor.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum CapabilityScope {
    /// Whole page.
    Page {
        /// Zero-based logical page index.
        page: u32,
    },
    /// One Scene command.
    Command {
        /// Zero-based logical page index.
        page: u32,
        /// Zero-based graphics command index.
        command: u32,
    },
    /// One Scene resource.
    Resource {
        /// Zero-based logical page index.
        page: u32,
        /// Zero-based graphics resource identifier.
        resource: u32,
    },
}

/// Content-redacted structured location for one capability fact.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct CapabilityLocation {
    page_index: Option<u32>,
    object_number: Option<u32>,
    object_generation: Option<u16>,
    source_offset: Option<u64>,
    command_index: Option<u32>,
    resource_id: Option<u32>,
}

impl CapabilityLocation {
    fn page(scene: &Scene) -> Self {
        let object = scene.binding().page_object();
        Self {
            page_index: Some(scene.binding().page_index()),
            object_number: Some(object.number()),
            object_generation: Some(object.generation()),
            source_offset: None,
            command_index: None,
            resource_id: None,
        }
    }

    /// Returns the logical page index.
    pub const fn page_index(self) -> Option<u32> {
        self.page_index
    }

    /// Returns the defining object number when known.
    pub const fn object_number(self) -> Option<u32> {
        self.object_number
    }

    /// Returns the defining object generation when known.
    pub const fn object_generation(self) -> Option<u16> {
        self.object_generation
    }

    /// Returns an absolute source offset when one is known.
    ///
    /// Decoded content-stream offsets are deliberately not misrepresented as absolute source
    /// offsets, so the first profile leaves this field empty.
    pub const fn source_offset(self) -> Option<u64> {
        self.source_offset
    }

    /// Returns the graphics command index when applicable.
    pub const fn command_index(self) -> Option<u32> {
        self.command_index
    }

    /// Returns the graphics resource identifier when applicable.
    pub const fn resource_id(self) -> Option<u32> {
        self.resource_id
    }
}

/// Why one missing requirement contributed to the product outcome.
#[repr(u8)]
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum CapabilityContributorKind {
    /// The Scene requirement itself is outside the selected profile.
    SceneRequirement = 1,
    /// The requirement is directly supported but one canonical dependency is missing.
    PolicyDependencyClosure = 3,
}

/// One canonical contributor to an Unsupported or Rejected decision.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CapabilityContributor {
    id: u32,
    kind: CapabilityContributorKind,
    code: u32,
    location: Option<CapabilityLocation>,
}

impl CapabilityContributor {
    /// Returns the zero-based canonical contributor ID.
    pub const fn id(self) -> u32 {
        self.id
    }

    /// Returns the contributor family.
    pub const fn kind(self) -> CapabilityContributorKind {
        self.kind
    }

    /// Returns the stable capability, dependency, or rejection code.
    pub const fn code(self) -> u32 {
        self.code
    }

    /// Returns the optional bounded structured location.
    pub const fn location(self) -> Option<CapabilityLocation> {
        self.location
    }
}

/// One missing requirement retained in canonical requirement-ID order.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MissingCapabilityRequirement {
    id: u32,
    capability: GraphicsCapability,
    parameter: u64,
    dependencies: Vec<u32>,
    scope: CapabilityScope,
    contributor_ids: Vec<u32>,
    location: Option<CapabilityLocation>,
}

impl MissingCapabilityRequirement {
    /// Returns the Scene requirement ID.
    pub const fn id(&self) -> u32 {
        self.id
    }

    /// Returns the typed Scene capability.
    pub const fn capability(&self) -> GraphicsCapability {
        self.capability
    }

    /// Returns the capability-specific canonical parameter.
    pub const fn parameter(&self) -> u64 {
        self.parameter
    }

    /// Borrows dependency IDs in strict canonical order.
    pub fn dependencies(&self) -> &[u32] {
        &self.dependencies
    }

    /// Returns the requirement scope.
    pub const fn scope(&self) -> CapabilityScope {
        self.scope
    }

    /// Borrows canonical contributor IDs.
    pub fn contributor_ids(&self) -> &[u32] {
        &self.contributor_ids
    }

    /// Returns the optional bounded structured location.
    pub const fn location(&self) -> Option<CapabilityLocation> {
        self.location
    }
}

/// Complete immutable subject bound to one capability decision.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CapabilitySubject {
    source: SourceIdentity,
    document_revision: u64,
    revision_startxref: u64,
    page_index: u32,
    page_object_number: u32,
    page_object_generation: u16,
    scene_schema_major: u16,
    scene_schema_minor: u16,
    scene_hash: SceneHash,
}

impl CapabilitySubject {
    /// Returns the immutable source identity.
    pub const fn source(self) -> SourceIdentity {
        self.source
    }

    /// Returns the product document revision.
    pub const fn document_revision(self) -> u64 {
        self.document_revision
    }

    /// Returns the exact xref revision anchor.
    pub const fn revision_startxref(self) -> u64 {
        self.revision_startxref
    }

    /// Returns the logical page index.
    pub const fn page_index(self) -> u32 {
        self.page_index
    }

    /// Returns the Page object number.
    pub const fn page_object_number(self) -> u32 {
        self.page_object_number
    }

    /// Returns the Page object generation.
    pub const fn page_object_generation(self) -> u16 {
        self.page_object_generation
    }

    /// Returns the incompatible Scene schema generation.
    pub const fn scene_schema_major(self) -> u16 {
        self.scene_schema_major
    }

    /// Returns the compatible Scene schema revision.
    pub const fn scene_schema_minor(self) -> u16 {
        self.scene_schema_minor
    }

    /// Returns the complete canonical Scene digest.
    pub const fn scene_hash(self) -> SceneHash {
        self.scene_hash
    }
}

/// Complete bounded and auditable product capability decision.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CapabilityDecision {
    schema_version: u16,
    status: CapabilityStatus,
    profile: CapabilityProfile,
    subject: CapabilitySubject,
    missing: Vec<MissingCapabilityRequirement>,
    missing_total: u32,
    missing_completeness: CollectionCompleteness,
    contributors: Vec<CapabilityContributor>,
    contributors_total: u32,
    contributors_completeness: CollectionCompleteness,
    locations_total: u32,
    locations_completeness: CollectionCompleteness,
    evaluated_requirements: u32,
    evaluated_dependencies: u32,
    evaluated_parameters: u32,
    evaluated_commands: u32,
    evaluated_resources: u32,
    scope: CapabilityScope,
    location: Option<CapabilityLocation>,
    rejection_code: Option<CapabilityRejectionCode>,
    hash: CapabilityDecisionHash,
}

impl CapabilityDecision {
    /// Returns the decision schema version.
    pub const fn schema_version(&self) -> u16 {
        self.schema_version
    }

    /// Returns the product support status.
    pub const fn status(&self) -> CapabilityStatus {
        self.status
    }

    /// Returns the selected profile and policy versions.
    pub const fn profile(&self) -> CapabilityProfile {
        self.profile
    }

    /// Returns the complete decision subject.
    pub const fn subject(&self) -> CapabilitySubject {
        self.subject
    }

    /// Borrows the retained canonical missing prefix.
    pub fn missing(&self) -> &[MissingCapabilityRequirement] {
        &self.missing
    }

    /// Returns the exact missing count after complete evaluation.
    pub const fn missing_total(&self) -> u32 {
        self.missing_total
    }

    /// Returns missing-list completeness.
    pub const fn missing_completeness(&self) -> CollectionCompleteness {
        self.missing_completeness
    }

    /// Borrows the retained canonical contributor prefix.
    pub fn contributors(&self) -> &[CapabilityContributor] {
        &self.contributors
    }

    /// Returns the exact contributor count.
    pub const fn contributors_total(&self) -> u32 {
        self.contributors_total
    }

    /// Returns contributor-list completeness.
    pub const fn contributors_completeness(&self) -> CollectionCompleteness {
        self.contributors_completeness
    }

    /// Returns the exact number of available structured locations.
    pub const fn locations_total(&self) -> u32 {
        self.locations_total
    }

    /// Returns structured-location completeness.
    pub const fn locations_completeness(&self) -> CollectionCompleteness {
        self.locations_completeness
    }

    /// Returns the complete evaluated requirement count.
    pub const fn evaluated_requirements(&self) -> u32 {
        self.evaluated_requirements
    }

    /// Returns the complete evaluated dependency-edge count.
    pub const fn evaluated_dependencies(&self) -> u32 {
        self.evaluated_dependencies
    }

    /// Returns the complete evaluated parameter count.
    pub const fn evaluated_parameters(&self) -> u32 {
        self.evaluated_parameters
    }

    /// Returns the complete audited graphics-command count.
    pub const fn evaluated_commands(&self) -> u32 {
        self.evaluated_commands
    }

    /// Returns the complete audited graphics-resource count.
    pub const fn evaluated_resources(&self) -> u32 {
        self.evaluated_resources
    }

    /// Returns the most specific canonical outcome scope.
    pub const fn scope(&self) -> CapabilityScope {
        self.scope
    }

    /// Returns the optional bounded top-level location.
    pub const fn location(&self) -> Option<CapabilityLocation> {
        self.location
    }

    /// Returns the stable rejection code only for Rejected decisions.
    pub const fn rejection_code(&self) -> Option<CapabilityRejectionCode> {
        self.rejection_code
    }

    /// Returns the complete typed canonical decision digest.
    pub const fn hash(&self) -> CapabilityDecisionHash {
        self.hash
    }

    /// Builds the exact generated protocol value bound by [`Self::hash`].
    pub fn protocol_projection(&self) -> Result<pdf_rs_protocol::CapabilityDecision, PolicyError> {
        crate::protocol_projection::capability_decision(self)
    }

    fn seal(mut self, work: &mut CancellationWork<'_>) -> Result<Self, PolicyError> {
        work.check()?;
        self.hash = CapabilityDecisionHash::new(hash_decision(&self, work)?);
        work.check()?;
        Ok(self)
    }
}

/// Bounded evaluator for one immutable product capability profile.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CapabilityEvaluator {
    profile: CapabilityProfile,
    limits: PolicyLimits,
}

impl CapabilityEvaluator {
    /// Creates an evaluator from an immutable profile and validated limits.
    pub const fn new(profile: CapabilityProfile, limits: PolicyLimits) -> Self {
        Self { profile, limits }
    }

    /// Evaluates the complete Scene graph and dependency closure before raster allocation.
    ///
    /// `document_revision` is an explicit product revision distinct from the immutable host source
    /// revision already carried by the Scene binding.
    pub fn evaluate(
        self,
        scene: &Scene,
        document_revision: u64,
        cancellation: &dyn PolicyCancellation,
    ) -> Result<CapabilityDecision, PolicyError> {
        self.evaluate_inner(scene, document_revision, cancellation, None)
    }

    fn evaluate_inner(
        self,
        scene: &Scene,
        document_revision: u64,
        cancellation: &dyn PolicyCancellation,
        dependency_override: Option<DependencyOverride<'_>>,
    ) -> Result<CapabilityDecision, PolicyError> {
        if document_revision == 0 {
            return Err(PolicyError::invalid_document_revision());
        }
        let mut work = CancellationWork::new(cancellation, self.limits.cancellation_interval())?;
        let cardinalities =
            preflight_cardinalities(scene, self.limits, dependency_override, &mut work)?;
        let subject = subject_for_scene(scene, document_revision, &mut work)?;
        let page_scope = CapabilityScope::Page {
            page: subject.page_index(),
        };
        let page_location = CapabilityLocation::page(scene);
        let Some(graphics) = scene
            .graphics()
            .filter(|_| scene.version() == SceneVersion::V2_0)
        else {
            return rejected(
                self.profile,
                subject,
                CapabilityRejectionCode::UnsupportedSceneSchema,
                page_scope,
                retained_location(0, self.limits, page_location),
                cardinalities.requirements,
                cardinalities.dependencies,
                cardinalities.parameters,
                cardinalities.commands,
                cardinalities.resources,
                self.limits,
            )?
            .seal(&mut work);
        };

        let requirements = cardinalities.requirements;
        let dependencies = cardinalities.dependencies;
        let commands = cardinalities.commands;
        let resources = cardinalities.resources;

        for (index, entry) in graphics.resources().iter().enumerate() {
            work.step()?;
            if entry.id().value()
                != u32::try_from(index).map_err(|_| PolicyError::numeric_overflow())?
            {
                return rejected(
                    self.profile,
                    subject,
                    CapabilityRejectionCode::NonCanonicalResourceId,
                    page_scope,
                    retained_location(0, self.limits, page_location),
                    requirements,
                    dependencies,
                    requirements,
                    commands,
                    resources,
                    self.limits,
                )?
                .seal(&mut work);
            }
        }
        for _ in graphics.commands() {
            work.step()?;
        }

        let capacity =
            usize::try_from(requirements).map_err(|_| PolicyError::numeric_overflow())?;
        let mut effective = Vec::<bool>::new();
        effective
            .try_reserve_exact(capacity)
            .map_err(|_| PolicyError::allocation())?;
        let mut causes = Vec::<MissingCause>::new();
        causes
            .try_reserve_exact(capacity)
            .map_err(|_| PolicyError::allocation())?;

        for (index, requirement) in graphics.requirements().iter().enumerate() {
            let canonical_id = u32::try_from(index).map_err(|_| PolicyError::numeric_overflow())?;
            let dependency_values = dependency_values(index, requirement, dependency_override);
            let fallback_scope = page_scope;
            if requirement.id().value() != canonical_id {
                return rejected(
                    self.profile,
                    subject,
                    CapabilityRejectionCode::NonCanonicalRequirementId,
                    fallback_scope,
                    retained_location(0, self.limits, page_location),
                    requirements,
                    dependencies,
                    requirements,
                    commands,
                    resources,
                    self.limits,
                )?
                .seal(&mut work);
            }
            let Some((scope, location)) = context_for(scene, graphics, requirement.context())
            else {
                return rejected(
                    self.profile,
                    subject,
                    CapabilityRejectionCode::InvalidContext,
                    fallback_scope,
                    retained_location(0, self.limits, page_location),
                    requirements,
                    dependencies,
                    requirements,
                    commands,
                    resources,
                    self.limits,
                )?
                .seal(&mut work);
            };
            let dependency_count = u32::try_from(dependency_values.len())
                .map_err(|_| PolicyError::numeric_overflow())?;
            if dependency_count > self.limits.max_dependencies_per_requirement() {
                return rejected(
                    self.profile,
                    subject,
                    CapabilityRejectionCode::DependencyFanoutProhibited,
                    scope,
                    retained_location(0, self.limits, location),
                    requirements,
                    dependencies,
                    requirements,
                    commands,
                    resources,
                    self.limits,
                )?
                .seal(&mut work);
            }

            let mut first_missing_dependency = None;
            if !dependencies_are_canonical(
                canonical_id,
                (0..dependency_values.len()).map(|index| dependency_values.value(index)),
            ) {
                return rejected(
                    self.profile,
                    subject,
                    CapabilityRejectionCode::InvalidDependencyGraph,
                    scope,
                    retained_location(0, self.limits, location),
                    requirements,
                    dependencies,
                    requirements,
                    commands,
                    resources,
                    self.limits,
                )?
                .seal(&mut work);
            }
            for index in 0..dependency_values.len() {
                let value = dependency_values.value(index);
                let offset = usize::try_from(value).map_err(|_| PolicyError::numeric_overflow())?;
                if !effective[offset] && first_missing_dependency.is_none() {
                    first_missing_dependency = Some(value);
                }
                work.step()?;
            }
            let direct = self.profile.directly_supports(requirement);
            let is_supported = direct && first_missing_dependency.is_none();
            effective.push(is_supported);
            causes.push(if is_supported {
                MissingCause::Supported
            } else if direct {
                MissingCause::Dependency(
                    first_missing_dependency.ok_or_else(PolicyError::identity_mismatch)?,
                )
            } else {
                MissingCause::Direct(capability_code(requirement.capability()))
            });
            debug_assert_eq!(effective[index], is_supported);
            work.step()?;
        }

        build_evaluated_decision(
            self.profile,
            self.limits,
            scene,
            graphics,
            subject,
            page_scope,
            requirements,
            dependencies,
            commands,
            resources,
            &effective,
            &causes,
            &mut work,
        )?
        .seal(&mut work)
    }

    #[cfg(test)]
    fn evaluate_with_dependency_override(
        self,
        scene: &Scene,
        document_revision: u64,
        requirement_index: usize,
        dependencies: &[u32],
    ) -> Result<CapabilityDecision, PolicyError> {
        self.evaluate_inner(
            scene,
            document_revision,
            &NeverCancelled,
            Some(DependencyOverride {
                requirement_index,
                dependencies,
            }),
        )
    }

    /// Returns the selected profile.
    pub const fn profile(self) -> CapabilityProfile {
        self.profile
    }

    /// Returns the validated evaluator limits.
    pub const fn limits(self) -> PolicyLimits {
        self.limits
    }
}

impl Default for CapabilityEvaluator {
    fn default() -> Self {
        Self::new(CapabilityProfile::default(), PolicyLimits::default())
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum MissingCause {
    Supported,
    Direct(u32),
    Dependency(u32),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct GraphCardinalities {
    requirements: u32,
    dependencies: u32,
    parameters: u32,
    commands: u32,
    resources: u32,
}

#[derive(Clone, Copy)]
struct DependencyOverride<'a> {
    requirement_index: usize,
    dependencies: &'a [u32],
}

#[derive(Clone, Copy)]
enum DependencyValues<'a> {
    Scene(&'a [pdf_rs_scene::CapabilityRequirementId]),
    Override(&'a [u32]),
}

impl DependencyValues<'_> {
    fn len(self) -> usize {
        match self {
            Self::Scene(values) => values.len(),
            Self::Override(values) => values.len(),
        }
    }

    fn value(self, index: usize) -> u32 {
        match self {
            Self::Scene(values) => values[index].value(),
            Self::Override(values) => values[index],
        }
    }
}

fn dependency_values<'a>(
    requirement_index: usize,
    requirement: &'a SceneRequirement,
    dependency_override: Option<DependencyOverride<'a>>,
) -> DependencyValues<'a> {
    dependency_override
        .filter(|value| value.requirement_index == requirement_index)
        .map_or_else(
            || DependencyValues::Scene(requirement.dependencies()),
            |value| DependencyValues::Override(value.dependencies),
        )
}

fn preflight_cardinalities(
    scene: &Scene,
    limits: PolicyLimits,
    dependency_override: Option<DependencyOverride<'_>>,
    work: &mut CancellationWork<'_>,
) -> Result<GraphCardinalities, PolicyError> {
    let Some(graphics) = scene.graphics() else {
        return Ok(GraphCardinalities {
            requirements: 0,
            dependencies: 0,
            parameters: 0,
            commands: u32::try_from(scene.commands().len())
                .map_err(|_| PolicyError::numeric_overflow())?,
            resources: u32::try_from(scene.resources().len())
                .map_err(|_| PolicyError::numeric_overflow())?,
        });
    };

    let requirements = u32::try_from(graphics.requirements().len())
        .map_err(|_| PolicyError::numeric_overflow())?;
    let parameters = requirements;
    ensure_limit(
        PolicyLimitKind::Requirements,
        limits.max_requirements(),
        requirements,
    )?;
    ensure_limit(
        PolicyLimitKind::Parameters,
        limits.max_parameters(),
        parameters,
    )?;

    let mut dependencies = 0_u32;
    for (index, requirement) in graphics.requirements().iter().enumerate() {
        let dependency_values = dependency_values(index, requirement, dependency_override);
        dependencies = dependencies
            .checked_add(
                u32::try_from(dependency_values.len())
                    .map_err(|_| PolicyError::numeric_overflow())?,
            )
            .ok_or_else(PolicyError::numeric_overflow)?;
        work.step()?;
    }
    ensure_limit(
        PolicyLimitKind::Dependencies,
        limits.max_dependencies(),
        dependencies,
    )?;

    Ok(GraphCardinalities {
        requirements,
        dependencies,
        parameters,
        commands: u32::try_from(graphics.commands().len())
            .map_err(|_| PolicyError::numeric_overflow())?,
        resources: u32::try_from(graphics.resources().len())
            .map_err(|_| PolicyError::numeric_overflow())?,
    })
}

#[allow(
    clippy::too_many_arguments,
    reason = "decision construction receives independently audited cardinalities and identities"
)]
fn build_evaluated_decision(
    profile: CapabilityProfile,
    limits: PolicyLimits,
    scene: &Scene,
    graphics: &GraphicsScene,
    subject: CapabilitySubject,
    page_scope: CapabilityScope,
    requirements: u32,
    dependencies: u32,
    commands: u32,
    resources: u32,
    effective: &[bool],
    causes: &[MissingCause],
    work: &mut CancellationWork<'_>,
) -> Result<CapabilityDecision, PolicyError> {
    let mut missing_total = 0_u32;
    for is_supported in effective {
        if !is_supported {
            missing_total = missing_total
                .checked_add(1)
                .ok_or_else(PolicyError::numeric_overflow)?;
        }
        work.step()?;
    }
    let retained_missing = missing_total.min(limits.max_missing_retained());
    let retained_contributors = missing_total.min(limits.max_contributors_retained());
    let retained_locations = missing_total.min(limits.max_locations_retained());

    let mut missing = Vec::new();
    missing
        .try_reserve_exact(
            usize::try_from(retained_missing).map_err(|_| PolicyError::numeric_overflow())?,
        )
        .map_err(|_| PolicyError::allocation())?;
    let mut contributors = Vec::new();
    contributors
        .try_reserve_exact(
            usize::try_from(retained_contributors).map_err(|_| PolicyError::numeric_overflow())?,
        )
        .map_err(|_| PolicyError::allocation())?;

    let mut missing_ordinal = 0_u32;
    let mut first_scope = page_scope;
    let mut first_location = None;
    for (index, (requirement, is_supported)) in graphics
        .requirements()
        .iter()
        .zip(effective.iter().copied())
        .enumerate()
    {
        if is_supported {
            work.step()?;
            continue;
        }
        let (scope, available_location) = context_for(scene, graphics, requirement.context())
            .ok_or_else(PolicyError::identity_mismatch)?;
        let location = if missing_ordinal < retained_locations {
            Some(available_location)
        } else {
            None
        };
        if missing_ordinal == 0 {
            first_scope = scope;
            first_location = location;
        }
        let cause = causes
            .get(index)
            .copied()
            .ok_or_else(PolicyError::identity_mismatch)?;
        let (kind, code) = match cause {
            MissingCause::Direct(code) => (CapabilityContributorKind::SceneRequirement, code),
            MissingCause::Dependency(id) => {
                (CapabilityContributorKind::PolicyDependencyClosure, id)
            }
            MissingCause::Supported => return Err(PolicyError::identity_mismatch()),
        };

        if missing_ordinal < retained_contributors {
            contributors.push(CapabilityContributor {
                id: missing_ordinal,
                kind,
                code,
                location,
            });
        }
        if missing_ordinal < retained_missing {
            let mut dependency_ids = Vec::new();
            dependency_ids
                .try_reserve_exact(requirement.dependencies().len())
                .map_err(|_| PolicyError::allocation())?;
            dependency_ids.extend(
                requirement
                    .dependencies()
                    .iter()
                    .map(|dependency| dependency.value()),
            );
            for _ in requirement.dependencies() {
                work.step()?;
            }
            let mut contributor_ids = Vec::new();
            if missing_ordinal < retained_contributors {
                contributor_ids
                    .try_reserve_exact(1)
                    .map_err(|_| PolicyError::allocation())?;
                contributor_ids.push(missing_ordinal);
            }
            missing.push(MissingCapabilityRequirement {
                id: requirement.id().value(),
                capability: requirement.capability(),
                parameter: requirement.parameter(),
                dependencies: dependency_ids,
                scope,
                contributor_ids,
                location,
            });
        }
        missing_ordinal = missing_ordinal
            .checked_add(1)
            .ok_or_else(PolicyError::numeric_overflow)?;
        work.step()?;
    }
    if missing_ordinal != missing_total {
        return Err(PolicyError::identity_mismatch());
    }

    let status = if missing_total == 0 {
        CapabilityStatus::Supported
    } else {
        CapabilityStatus::Unsupported
    };
    Ok(CapabilityDecision {
        schema_version: DECISION_SCHEMA_VERSION,
        status,
        profile,
        subject,
        missing,
        missing_total,
        missing_completeness: completeness(retained_missing, missing_total),
        contributors,
        contributors_total: missing_total,
        contributors_completeness: completeness(retained_contributors, missing_total),
        locations_total: missing_total,
        locations_completeness: completeness(u32::from(first_location.is_some()), missing_total),
        evaluated_requirements: requirements,
        evaluated_dependencies: dependencies,
        evaluated_parameters: requirements,
        evaluated_commands: commands,
        evaluated_resources: resources,
        scope: if status == CapabilityStatus::Supported {
            page_scope
        } else {
            first_scope
        },
        location: if status == CapabilityStatus::Supported {
            None
        } else {
            first_location
        },
        rejection_code: None,
        hash: CapabilityDecisionHash::new([0; 32]),
    })
}

#[allow(
    clippy::too_many_arguments,
    reason = "rejection records every independently audited graph cardinality"
)]
fn rejected(
    profile: CapabilityProfile,
    subject: CapabilitySubject,
    code: CapabilityRejectionCode,
    scope: CapabilityScope,
    location: Option<CapabilityLocation>,
    requirements: u32,
    dependencies: u32,
    parameters: u32,
    commands: u32,
    resources: u32,
    limits: PolicyLimits,
) -> Result<CapabilityDecision, PolicyError> {
    let retain_contributor = limits.max_contributors_retained() != 0;
    let contributor = CapabilityContributor {
        id: 0,
        kind: CapabilityContributorKind::PolicyDependencyClosure,
        code: code as u32,
        location,
    };
    let mut contributors = Vec::new();
    if retain_contributor {
        contributors
            .try_reserve_exact(1)
            .map_err(|_| PolicyError::allocation())?;
        contributors.push(contributor);
    }
    Ok(CapabilityDecision {
        schema_version: DECISION_SCHEMA_VERSION,
        status: CapabilityStatus::Rejected,
        profile,
        subject,
        missing: Vec::new(),
        missing_total: 0,
        missing_completeness: CollectionCompleteness::Complete,
        contributors,
        contributors_total: 1,
        contributors_completeness: if retain_contributor {
            CollectionCompleteness::Complete
        } else {
            CollectionCompleteness::Truncated
        },
        locations_total: 1,
        locations_completeness: if location.is_some() {
            CollectionCompleteness::Complete
        } else {
            CollectionCompleteness::Truncated
        },
        evaluated_requirements: requirements,
        evaluated_dependencies: dependencies,
        evaluated_parameters: parameters,
        evaluated_commands: commands,
        evaluated_resources: resources,
        scope,
        location,
        rejection_code: Some(code),
        hash: CapabilityDecisionHash::new([0; 32]),
    })
}

fn retained_location(
    ordinal: u32,
    limits: PolicyLimits,
    location: CapabilityLocation,
) -> Option<CapabilityLocation> {
    (ordinal < limits.max_locations_retained()).then_some(location)
}

fn completeness(retained: u32, total: u32) -> CollectionCompleteness {
    if retained == total {
        CollectionCompleteness::Complete
    } else {
        debug_assert!(retained < total);
        CollectionCompleteness::Truncated
    }
}

fn context_for(
    scene: &Scene,
    graphics: &GraphicsScene,
    context: SceneCapabilityContext,
) -> Option<(CapabilityScope, CapabilityLocation)> {
    let page = scene.binding().page_index();
    let mut location = CapabilityLocation::page(scene);
    match context {
        SceneCapabilityContext::Scene => Some((CapabilityScope::Page { page }, location)),
        SceneCapabilityContext::Command(command) => {
            let record = graphics.commands().get(usize::try_from(command).ok()?)?;
            let object = record.source().object();
            location.object_number = Some(object.number());
            location.object_generation = Some(object.generation());
            location.command_index = Some(command);
            Some((CapabilityScope::Command { page, command }, location))
        }
        SceneCapabilityContext::Resource(resource) => {
            let index = usize::try_from(resource.value()).ok()?;
            let entry = graphics.resources().get(index)?;
            if entry.id() != resource {
                return None;
            }
            location.resource_id = Some(resource.value());
            match entry.resource() {
                GraphicsResource::Image(image) => {
                    let object = image.source().object();
                    location.object_number = Some(object.number());
                    location.object_generation = Some(object.generation());
                }
                GraphicsResource::GlyphOutline(glyph) => {
                    let object = glyph.source().object();
                    location.object_number = Some(object.number());
                    location.object_generation = Some(object.generation());
                }
                GraphicsResource::Path(_) => {}
            }
            Some((
                CapabilityScope::Resource {
                    page,
                    resource: resource.value(),
                },
                location,
            ))
        }
    }
}

pub(crate) fn subject_for_scene(
    scene: &Scene,
    document_revision: u64,
    work: &mut CancellationWork<'_>,
) -> Result<CapabilitySubject, PolicyError> {
    if document_revision == 0 {
        return Err(PolicyError::invalid_document_revision());
    }
    work.check()?;
    let canonical = canonical_scene_bytes(scene, work)?;
    work.check()?;
    let mut hasher = CanonicalHasher::new(b"scene/canonical-json/v1");
    hasher.u64(u64::try_from(canonical.len()).map_err(|_| PolicyError::numeric_overflow())?);
    for chunk in canonical.chunks(4 * 1024) {
        work.step()?;
        hasher.bytes(chunk);
    }
    let scene_hash = SceneHash::new(hasher.finish()?);
    work.check()?;
    let binding = scene.binding();
    let page_object = binding.page_object();
    Ok(CapabilitySubject {
        source: binding.source(),
        document_revision,
        revision_startxref: binding.revision_startxref(),
        page_index: binding.page_index(),
        page_object_number: page_object.number(),
        page_object_generation: page_object.generation(),
        scene_schema_major: scene.version().major(),
        scene_schema_minor: scene.version().minor(),
        scene_hash,
    })
}

fn canonical_scene_bytes(
    scene: &Scene,
    work: &mut CancellationWork<'_>,
) -> Result<Vec<u8>, PolicyError> {
    struct Observer<'work, 'cancellation> {
        work: &'work mut CancellationWork<'cancellation>,
        error: Option<PolicyError>,
    }

    impl SceneCanonicalObserver for Observer<'_, '_> {
        fn observe(&mut self, _next_fragment: &[u8]) -> bool {
            match self.work.step() {
                Ok(()) => true,
                Err(error) => {
                    self.error = Some(error);
                    false
                }
            }
        }
    }

    let mut observer = Observer { work, error: None };
    match scene.canonical_json_bytes_observed(&mut observer) {
        Ok(canonical) => Ok(canonical),
        Err(error) if error.code() == SceneErrorCode::CanonicalizationInterrupted => Err(observer
            .error
            .unwrap_or_else(PolicyError::scene_canonicalization)),
        Err(_) => Err(PolicyError::scene_canonicalization()),
    }
}

fn hash_decision(
    decision: &CapabilityDecision,
    work: &mut CancellationWork<'_>,
) -> Result<[u8; 32], PolicyError> {
    work.check()?;
    let preimage = crate::protocol_projection::capability_decision_hash_preimage(decision, work)?;
    crate::canonical_hash::hash_preimage_observed(&preimage, || work.step())
}

fn dependencies_are_canonical(
    requirement_id: u32,
    dependencies: impl IntoIterator<Item = u32>,
) -> bool {
    let mut previous = None;
    for dependency in dependencies {
        if dependency >= requirement_id || previous.is_some_and(|prior| prior >= dependency) {
            return false;
        }
        previous = Some(dependency);
    }
    true
}

pub(crate) fn capability_code(capability: GraphicsCapability) -> u32 {
    match capability {
        GraphicsCapability::PathFill => 1,
        GraphicsCapability::PathStroke => 2,
        GraphicsCapability::Clip => 3,
        GraphicsCapability::DeviceColor => 4,
        GraphicsCapability::ConstantAlpha => 5,
        GraphicsCapability::Blend => 6,
        GraphicsCapability::SoftMask => 7,
        GraphicsCapability::Image => 8,
        GraphicsCapability::Glyph => 9,
        GraphicsCapability::IsolatedGroup => 10,
    }
}

fn ensure_limit(kind: PolicyLimitKind, maximum: u32, actual: u32) -> Result<(), PolicyError> {
    if actual > maximum {
        return Err(PolicyError::resource(
            kind,
            u64::from(maximum),
            0,
            u64::from(actual),
        ));
    }
    Ok(())
}

pub(crate) struct CancellationWork<'a> {
    cancellation: &'a dyn PolicyCancellation,
    interval: u32,
    since_check: u32,
}

impl<'a> CancellationWork<'a> {
    pub(crate) fn new(
        cancellation: &'a dyn PolicyCancellation,
        interval: u32,
    ) -> Result<Self, PolicyError> {
        let value = Self {
            cancellation,
            interval,
            since_check: 0,
        };
        value.check()?;
        Ok(value)
    }

    pub(crate) fn step(&mut self) -> Result<(), PolicyError> {
        self.since_check = self
            .since_check
            .checked_add(1)
            .ok_or_else(PolicyError::numeric_overflow)?;
        if self.since_check == self.interval {
            self.check()?;
            self.since_check = 0;
        }
        Ok(())
    }

    pub(crate) fn check(&self) -> Result<(), PolicyError> {
        if self.cancellation.is_cancelled() {
            Err(PolicyError::cancelled())
        } else {
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use pdf_rs_bytes::{SourceIdentity, SourceRevision, SourceStableId};
    use pdf_rs_scene::{
        CapabilityContext, CapabilityStatus as SceneCapabilityStatus, GraphicsCapability,
        GraphicsSceneBuilder, GraphicsSceneLimits, PageGeometry, PageRotation, SceneBinding,
        SceneRect, SceneScalar,
    };
    use pdf_rs_syntax::ObjectRef;

    use super::{
        CapabilityEvaluator, CapabilityRejectionCode, CapabilityStatus, dependencies_are_canonical,
    };

    #[test]
    fn malformed_dependency_shapes_are_rejected_without_graph_traversal() {
        assert!(dependencies_are_canonical(3, [0, 1, 2]));
        assert!(!dependencies_are_canonical(3, [0, 1, 1]));
        assert!(!dependencies_are_canonical(3, [1, 0]));
        assert!(!dependencies_are_canonical(3, [0, 3]));
        assert!(!dependencies_are_canonical(3, [4]));
    }

    #[test]
    fn evaluator_rejects_and_hashes_a_malformed_dependency_fixture() {
        let source = SourceIdentity::new(SourceStableId::new([7; 32]), SourceRevision::new(11));
        let binding = SceneBinding::new(source, 19, 3, ObjectRef::new(41, 0).unwrap());
        let media = SceneRect::new([
            SceneScalar::ZERO,
            SceneScalar::ZERO,
            SceneScalar::from_scaled(612_000_000_000),
            SceneScalar::from_scaled(792_000_000_000),
        ])
        .unwrap();
        let mut builder = GraphicsSceneBuilder::new_v2(
            binding,
            PageGeometry::new(media, media, PageRotation::Degrees0),
            GraphicsSceneLimits::default(),
        );
        let first = builder
            .add_requirement(
                GraphicsCapability::PathFill,
                0,
                CapabilityContext::Scene,
                Vec::new(),
                SceneCapabilityStatus::Supported,
            )
            .unwrap();
        builder
            .add_requirement(
                GraphicsCapability::PathStroke,
                0,
                CapabilityContext::Scene,
                vec![first],
                SceneCapabilityStatus::Supported,
            )
            .unwrap();
        builder
            .add_requirement(
                GraphicsCapability::Clip,
                0,
                CapabilityContext::Scene,
                vec![first],
                SceneCapabilityStatus::Supported,
            )
            .unwrap();
        let scene = builder.finish().unwrap();

        for (requirement_index, dependencies) in [
            (1, &[1][..]),
            (1, &[2][..]),
            (1, &[0, 0][..]),
            (2, &[1, 0][..]),
        ] {
            let first = CapabilityEvaluator::default()
                .evaluate_with_dependency_override(&scene, 23, requirement_index, dependencies)
                .unwrap();
            let replay = CapabilityEvaluator::default()
                .evaluate_with_dependency_override(&scene, 23, requirement_index, dependencies)
                .unwrap();
            assert_eq!(first.status(), CapabilityStatus::Rejected);
            assert_eq!(
                first.rejection_code(),
                Some(CapabilityRejectionCode::InvalidDependencyGraph)
            );
            assert_eq!(first.evaluated_requirements(), 3);
            assert_eq!(
                first.evaluated_dependencies(),
                u32::try_from(dependencies.len() + 1).unwrap()
            );
            assert!(!first.hash().is_zero());
            assert_eq!(first.hash(), replay.hash());
            assert!(first.protocol_projection().unwrap().wire_invariants_valid());
        }
    }
}
