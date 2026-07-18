use std::fmt;
use std::mem::size_of;
use std::num::NonZeroU32;
use std::sync::Arc;

use pdf_rs_bytes::SourceIdentity;
pub use pdf_rs_protocol::CapabilityProfileId;
use pdf_rs_scene::{
    CapabilityContext as SceneCapabilityContext, CapabilityRequirement as SceneRequirement,
    CapabilityStatus as SceneCapabilityStatus, GraphicsCapability, GraphicsResource, GraphicsScene,
    Scene, SceneCanonicalObserver, SceneErrorCode, SceneVersion,
};

use crate::canonical_hash::CanonicalHasher;
use crate::{
    CapabilityDecisionHash, PolicyError, PolicyJobLimits, PolicyJobPoll, PolicyJobStats,
    PolicyLimitKind, PolicyLimits, PolicyPollBudget, SceneHash,
};

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
    pub(crate) fn from_scene_hash(
        scene: &Scene,
        document_revision: u64,
        scene_hash: SceneHash,
    ) -> Self {
        let binding = scene.binding();
        let page_object = binding.page_object();
        Self {
            source: binding.source(),
            document_revision,
            revision_startxref: binding.revision_startxref(),
            page_index: binding.page_index(),
            page_object_number: page_object.number(),
            page_object_generation: page_object.generation(),
            scene_schema_major: scene.version().major(),
            scene_schema_minor: scene.version().minor(),
            scene_hash,
        }
    }

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
        let scene = Arc::new(scene.clone());
        let job_limits = PolicyJobLimits::synchronous_compatibility(
            canonical_scene_upper_bound(&scene),
            u64::MAX,
        );
        let mut job =
            CapabilityEvaluationJob::new_compatibility(self, scene, document_revision, job_limits);
        let budget = PolicyPollBudget::new(
            NonZeroU32::new(4_096).expect("fixed synchronous poll budget is nonzero"),
        )?;
        loop {
            match job.poll(budget, cancellation) {
                PolicyJobPoll::Pending => {}
                PolicyJobPoll::Ready => {
                    return job
                        .take_result()
                        .ok_or_else(PolicyError::identity_mismatch)?;
                }
            }
        }
    }

    /// Creates an owned, bounded, resumable capability-evaluation job.
    ///
    /// A checked conservative bound derived from the Scene's published retained capacity must fit
    /// [`PolicyJobLimits::max_atomic_canonical_bytes`]. This makes the one Scene API operation
    /// that cannot expose a writer cursor an explicitly small and rejectable atomic phase. The
    /// observer independently enforces the same limit against actual bytes before each allocation.
    pub fn start_job(
        self,
        scene: Arc<Scene>,
        document_revision: u64,
        job_limits: PolicyJobLimits,
    ) -> Result<CapabilityEvaluationJob, PolicyError> {
        CapabilityEvaluationJob::new(self, scene, document_revision, job_limits)
    }

    #[cfg(test)]
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

    #[cfg(test)]
    fn evaluate_with_resource_override(
        self,
        scene: &Scene,
        document_revision: u64,
        resource_index: usize,
        resource_id: u32,
    ) -> Result<CapabilityDecision, PolicyError> {
        let scene = Arc::new(scene.clone());
        let mut job = CapabilityEvaluationJob::new_compatibility(
            self,
            Arc::clone(&scene),
            document_revision,
            PolicyJobLimits::synchronous_compatibility(
                canonical_scene_upper_bound(&scene),
                u64::MAX,
            ),
        );
        job.resource_id_override = Some((resource_index, resource_id));
        let budget = PolicyPollBudget::new(NonZeroU32::new(4_096).unwrap())?;
        while job.poll(budget, &NeverCancelled) == PolicyJobPoll::Pending {}
        job.take_result()
            .ok_or_else(PolicyError::identity_mismatch)?
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
enum CapabilityJobPhase {
    Preflight,
    Canonicalize,
    HashCanonical,
    AuditResources,
    AuditCommands,
    AllocateClosure,
    EvaluateRequirements,
    CountMissing,
    AllocateDecision,
    BuildMissing,
    Seal,
}

struct PendingMissing {
    requirement_index: usize,
    dependency_index: usize,
    dependencies: Vec<u32>,
    contributor_ids: Vec<u32>,
    scope: CapabilityScope,
    location: Option<CapabilityLocation>,
}

/// Owned capability evaluation with explicit cursors and terminal replay.
///
/// Each [`Self::poll`] advances at most the supplied nonzero work budget. Job-owned working
/// allocations are admitted before publication and accounted by [`Self::stats`]. Dropping a
/// pending or terminal job releases the owned Scene reference and every unpublished buffer.
pub struct CapabilityEvaluationJob {
    evaluator: CapabilityEvaluator,
    scene: Arc<Scene>,
    document_revision: u64,
    job_limits: PolicyJobLimits,
    stats: PolicyJobStats,
    phase: CapabilityJobPhase,
    terminal: Option<Result<CapabilityDecision, PolicyError>>,
    result_taken: bool,
    preflight_index: usize,
    cardinalities: Option<GraphCardinalities>,
    canonical: Option<Vec<u8>>,
    canonical_offset: usize,
    canonical_hasher: Option<CanonicalHasher>,
    subject: Option<CapabilitySubject>,
    resource_index: usize,
    #[cfg(test)]
    resource_id_override: Option<(usize, u32)>,
    #[cfg(test)]
    requirement_id_override: Option<(usize, u32)>,
    command_index: usize,
    effective: Vec<bool>,
    causes: Vec<MissingCause>,
    requirement_index: usize,
    dependency_index: usize,
    previous_dependency: Option<u32>,
    first_missing_dependency: Option<u32>,
    missing_count_index: usize,
    missing_total: u32,
    missing: Vec<MissingCapabilityRequirement>,
    contributors: Vec<CapabilityContributor>,
    missing_index: usize,
    missing_ordinal: u32,
    first_scope: Option<CapabilityScope>,
    first_location: Option<CapabilityLocation>,
    pending_missing: Option<PendingMissing>,
    pending_decision: Option<CapabilityDecision>,
}

impl CapabilityEvaluationJob {
    fn new(
        evaluator: CapabilityEvaluator,
        scene: Arc<Scene>,
        document_revision: u64,
        job_limits: PolicyJobLimits,
    ) -> Result<Self, PolicyError> {
        let declared = canonical_scene_upper_bound(&scene);
        if declared > job_limits.max_atomic_canonical_bytes() {
            return Err(PolicyError::resource(
                PolicyLimitKind::AtomicCanonicalBytes,
                job_limits.max_atomic_canonical_bytes(),
                0,
                declared,
            ));
        }
        if declared > job_limits.max_retained_bytes() {
            return Err(PolicyError::resource(
                PolicyLimitKind::JobRetainedBytes,
                job_limits.max_retained_bytes(),
                0,
                declared,
            ));
        }
        Ok(Self::new_compatibility(
            evaluator,
            scene,
            document_revision,
            job_limits,
        ))
    }

    fn new_compatibility(
        evaluator: CapabilityEvaluator,
        scene: Arc<Scene>,
        document_revision: u64,
        job_limits: PolicyJobLimits,
    ) -> Self {
        Self {
            evaluator,
            scene,
            document_revision,
            job_limits,
            stats: PolicyJobStats::default(),
            phase: CapabilityJobPhase::Preflight,
            terminal: None,
            result_taken: false,
            preflight_index: 0,
            cardinalities: None,
            canonical: None,
            canonical_offset: 0,
            canonical_hasher: None,
            subject: None,
            resource_index: 0,
            #[cfg(test)]
            resource_id_override: None,
            #[cfg(test)]
            requirement_id_override: None,
            command_index: 0,
            effective: Vec::new(),
            causes: Vec::new(),
            requirement_index: 0,
            dependency_index: 0,
            previous_dependency: None,
            first_missing_dependency: None,
            missing_count_index: 0,
            missing_total: 0,
            missing: Vec::new(),
            contributors: Vec::new(),
            missing_index: 0,
            missing_ordinal: 0,
            first_scope: None,
            first_location: None,
            pending_missing: None,
            pending_decision: None,
        }
    }

    /// Returns deterministic work and owned-capacity accounting through the latest poll.
    pub const fn stats(&self) -> PolicyJobStats {
        self.stats
    }

    /// Returns the current replayable terminal result, when ready.
    pub fn result(&self) -> Option<Result<&CapabilityDecision, PolicyError>> {
        self.terminal
            .as_ref()
            .map(|result| result.as_ref().map_err(|error| *error))
    }

    /// Moves the terminal result out of this job without cloning retained decision vectors.
    pub fn take_result(&mut self) -> Option<Result<CapabilityDecision, PolicyError>> {
        let result = self.terminal.take();
        if result.is_some() {
            self.stats.clear_retained();
            self.result_taken = true;
        }
        result
    }

    /// Advances at most `budget` explicit work units or reaches one terminal result.
    pub fn poll(
        &mut self,
        budget: PolicyPollBudget,
        cancellation: &dyn PolicyCancellation,
    ) -> PolicyJobPoll {
        if self.terminal.is_some() || self.result_taken {
            return PolicyJobPoll::Ready;
        }
        if self.phase == CapabilityJobPhase::Preflight
            && self.preflight_index == 0
            && self.document_revision == 0
        {
            self.fail(PolicyError::invalid_document_revision());
            return PolicyJobPoll::Ready;
        }
        for _ in 0..budget.work_units().get() {
            if cancellation.is_cancelled() {
                self.fail(PolicyError::cancelled());
                return PolicyJobPoll::Ready;
            }
            if let Err(error) = self.stats.charge_work() {
                self.fail(error);
                return PolicyJobPoll::Ready;
            }
            match self.step(cancellation) {
                Ok(true) => return PolicyJobPoll::Ready,
                Ok(false) => {}
                Err(error) => {
                    self.fail(error);
                    return PolicyJobPoll::Ready;
                }
            }
        }
        PolicyJobPoll::Pending
    }

    fn step(&mut self, cancellation: &dyn PolicyCancellation) -> Result<bool, PolicyError> {
        match self.phase {
            CapabilityJobPhase::Preflight => self.step_preflight(),
            CapabilityJobPhase::Canonicalize => self.step_canonicalize(cancellation),
            CapabilityJobPhase::HashCanonical => self.step_hash_canonical(),
            CapabilityJobPhase::AuditResources => self.step_audit_resources(),
            CapabilityJobPhase::AuditCommands => self.step_audit_commands(),
            CapabilityJobPhase::AllocateClosure => self.step_allocate_closure(),
            CapabilityJobPhase::EvaluateRequirements => self.step_evaluate_requirements(),
            CapabilityJobPhase::CountMissing => self.step_count_missing(),
            CapabilityJobPhase::AllocateDecision => self.step_allocate_decision(),
            CapabilityJobPhase::BuildMissing => self.step_build_missing(),
            CapabilityJobPhase::Seal => self.step_seal(cancellation),
        }
    }

    fn step_preflight(&mut self) -> Result<bool, PolicyError> {
        if self.document_revision == 0 {
            return Err(PolicyError::invalid_document_revision());
        }
        let Some(graphics) = self.scene.graphics() else {
            self.cardinalities = Some(GraphCardinalities {
                requirements: 0,
                dependencies: 0,
                parameters: 0,
                commands: u32::try_from(self.scene.commands().len())
                    .map_err(|_| PolicyError::numeric_overflow())?,
                resources: u32::try_from(self.scene.resources().len())
                    .map_err(|_| PolicyError::numeric_overflow())?,
            });
            self.phase = CapabilityJobPhase::Canonicalize;
            return Ok(false);
        };
        if self.cardinalities.is_none() {
            let requirements = u32::try_from(graphics.requirements().len())
                .map_err(|_| PolicyError::numeric_overflow())?;
            ensure_limit(
                PolicyLimitKind::Requirements,
                self.evaluator.limits.max_requirements(),
                requirements,
            )?;
            ensure_limit(
                PolicyLimitKind::Parameters,
                self.evaluator.limits.max_parameters(),
                requirements,
            )?;
            self.cardinalities = Some(GraphCardinalities {
                requirements,
                dependencies: 0,
                parameters: requirements,
                commands: u32::try_from(graphics.commands().len())
                    .map_err(|_| PolicyError::numeric_overflow())?,
                resources: u32::try_from(graphics.resources().len())
                    .map_err(|_| PolicyError::numeric_overflow())?,
            });
        }
        if let Some(requirement) = graphics.requirements().get(self.preflight_index) {
            let additional = u32::try_from(requirement.dependencies().len())
                .map_err(|_| PolicyError::numeric_overflow())?;
            let cardinalities = self
                .cardinalities
                .as_mut()
                .ok_or_else(PolicyError::identity_mismatch)?;
            cardinalities.dependencies = cardinalities
                .dependencies
                .checked_add(additional)
                .ok_or_else(PolicyError::numeric_overflow)?;
            self.preflight_index = self
                .preflight_index
                .checked_add(1)
                .ok_or_else(PolicyError::numeric_overflow)?;
            return Ok(false);
        }
        ensure_limit(
            PolicyLimitKind::Dependencies,
            self.evaluator.limits.max_dependencies(),
            self.cardinalities
                .ok_or_else(PolicyError::identity_mismatch)?
                .dependencies,
        )?;
        self.phase = CapabilityJobPhase::Canonicalize;
        Ok(false)
    }

    fn step_canonicalize(
        &mut self,
        cancellation: &dyn PolicyCancellation,
    ) -> Result<bool, PolicyError> {
        struct Observer<'a> {
            cancellation: &'a dyn PolicyCancellation,
            limit: u64,
            observed: u64,
            error: Option<PolicyError>,
        }

        impl SceneCanonicalObserver for Observer<'_> {
            fn observe(&mut self, next_fragment: &[u8]) -> bool {
                if self.cancellation.is_cancelled() {
                    self.error = Some(PolicyError::cancelled());
                    return false;
                }
                let Ok(additional) = u64::try_from(next_fragment.len()) else {
                    self.error = Some(PolicyError::numeric_overflow());
                    return false;
                };
                let Some(attempted) = self.observed.checked_add(additional) else {
                    self.error = Some(PolicyError::numeric_overflow());
                    return false;
                };
                if attempted > self.limit {
                    self.error = Some(PolicyError::resource(
                        PolicyLimitKind::AtomicCanonicalBytes,
                        self.limit,
                        self.observed,
                        additional,
                    ));
                    return false;
                }
                self.observed = attempted;
                true
            }
        }

        let mut observer = Observer {
            cancellation,
            limit: self.job_limits.max_atomic_canonical_bytes(),
            observed: 0,
            error: None,
        };
        let canonical = match self.scene.canonical_json_bytes_observed(&mut observer) {
            Ok(value) => value,
            Err(error) if error.code() == SceneErrorCode::CanonicalizationInterrupted => {
                return Err(observer
                    .error
                    .unwrap_or_else(PolicyError::scene_canonicalization));
            }
            Err(_) => return Err(PolicyError::scene_canonicalization()),
        };
        let retained = vec_capacity_bytes(&canonical)?;
        self.stats.charge_allocation(retained, self.job_limits)?;
        self.stats.set_atomic_canonical_bytes(
            u64::try_from(canonical.len()).map_err(|_| PolicyError::numeric_overflow())?,
        );
        let mut hasher = CanonicalHasher::new(b"scene/canonical-json/v1");
        hasher.u64(u64::try_from(canonical.len()).map_err(|_| PolicyError::numeric_overflow())?);
        self.canonical = Some(canonical);
        self.canonical_hasher = Some(hasher);
        self.phase = CapabilityJobPhase::HashCanonical;
        Ok(false)
    }

    fn step_hash_canonical(&mut self) -> Result<bool, PolicyError> {
        let canonical = self
            .canonical
            .as_ref()
            .ok_or_else(PolicyError::identity_mismatch)?;
        if let Some(chunk) = canonical
            .get(self.canonical_offset..)
            .and_then(|remaining| remaining.chunks(4 * 1024).next())
        {
            self.canonical_hasher
                .as_mut()
                .ok_or_else(PolicyError::identity_mismatch)?
                .bytes(chunk);
            self.canonical_offset = self
                .canonical_offset
                .checked_add(chunk.len())
                .ok_or_else(PolicyError::numeric_overflow)?;
            return Ok(false);
        }
        let scene_hash = SceneHash::new(
            self.canonical_hasher
                .take()
                .ok_or_else(PolicyError::identity_mismatch)?
                .finish()?,
        );
        self.subject = Some(CapabilitySubject::from_scene_hash(
            &self.scene,
            self.document_revision,
            scene_hash,
        ));
        let released = vec_capacity_bytes(
            self.canonical
                .as_ref()
                .ok_or_else(PolicyError::identity_mismatch)?,
        )?;
        self.canonical = None;
        self.stats.release(released)?;
        if self.scene.version() != SceneVersion::V2_0 || self.scene.graphics().is_none() {
            let cardinalities = self
                .cardinalities
                .ok_or_else(PolicyError::identity_mismatch)?;
            let subject = self.subject.ok_or_else(PolicyError::identity_mismatch)?;
            let page_scope = CapabilityScope::Page {
                page: subject.page_index(),
            };
            self.pending_decision = Some(rejected(
                self.evaluator.profile,
                subject,
                CapabilityRejectionCode::UnsupportedSceneSchema,
                page_scope,
                retained_location(
                    0,
                    self.evaluator.limits,
                    CapabilityLocation::page(&self.scene),
                ),
                cardinalities.requirements,
                cardinalities.dependencies,
                cardinalities.parameters,
                cardinalities.commands,
                cardinalities.resources,
                self.evaluator.limits,
            )?);
            self.phase = CapabilityJobPhase::Seal;
        } else {
            self.phase = CapabilityJobPhase::AuditResources;
        }
        Ok(false)
    }

    fn step_audit_resources(&mut self) -> Result<bool, PolicyError> {
        let graphics = self
            .scene
            .graphics()
            .ok_or_else(PolicyError::identity_mismatch)?;
        if let Some(entry) = graphics.resources().get(self.resource_index) {
            let resource_id = self.audited_resource_id(entry.id().value());
            if resource_id
                != u32::try_from(self.resource_index)
                    .map_err(|_| PolicyError::numeric_overflow())?
            {
                self.reject(CapabilityRejectionCode::NonCanonicalResourceId)?;
                return Ok(false);
            }
            self.resource_index = self
                .resource_index
                .checked_add(1)
                .ok_or_else(PolicyError::numeric_overflow)?;
            return Ok(false);
        }
        self.phase = CapabilityJobPhase::AuditCommands;
        Ok(false)
    }

    fn step_audit_commands(&mut self) -> Result<bool, PolicyError> {
        let command_len = self
            .scene
            .graphics()
            .ok_or_else(PolicyError::identity_mismatch)?
            .commands()
            .len();
        if self.command_index < command_len {
            self.command_index = self
                .command_index
                .checked_add(1)
                .ok_or_else(PolicyError::numeric_overflow)?;
            return Ok(false);
        }
        self.phase = CapabilityJobPhase::AllocateClosure;
        Ok(false)
    }

    fn step_allocate_closure(&mut self) -> Result<bool, PolicyError> {
        let capacity = usize::try_from(
            self.cardinalities
                .ok_or_else(PolicyError::identity_mismatch)?
                .requirements,
        )
        .map_err(|_| PolicyError::numeric_overflow())?;
        reserve_job_vec(
            &mut self.effective,
            capacity,
            self.job_limits,
            &mut self.stats,
        )?;
        reserve_job_vec(&mut self.causes, capacity, self.job_limits, &mut self.stats)?;
        self.phase = CapabilityJobPhase::EvaluateRequirements;
        Ok(false)
    }

    fn step_evaluate_requirements(&mut self) -> Result<bool, PolicyError> {
        let graphics = self
            .scene
            .graphics()
            .ok_or_else(PolicyError::identity_mismatch)?;
        let Some(requirement) = graphics.requirements().get(self.requirement_index) else {
            self.phase = CapabilityJobPhase::CountMissing;
            return Ok(false);
        };
        let canonical_id =
            u32::try_from(self.requirement_index).map_err(|_| PolicyError::numeric_overflow())?;
        let page_scope = CapabilityScope::Page {
            page: self.scene.binding().page_index(),
        };
        let Some((scope, _location)) = context_for(&self.scene, graphics, requirement.context())
        else {
            self.reject_at(
                CapabilityRejectionCode::InvalidContext,
                page_scope,
                CapabilityLocation::page(&self.scene),
            )?;
            return Ok(false);
        };
        if self.audited_requirement_id(requirement.id().value()) != canonical_id {
            self.reject_at(
                CapabilityRejectionCode::NonCanonicalRequirementId,
                page_scope,
                CapabilityLocation::page(&self.scene),
            )?;
            return Ok(false);
        }
        if u32::try_from(requirement.dependencies().len())
            .map_err(|_| PolicyError::numeric_overflow())?
            > self.evaluator.limits.max_dependencies_per_requirement()
        {
            let location = context_for(&self.scene, graphics, requirement.context())
                .ok_or_else(PolicyError::identity_mismatch)?
                .1;
            self.reject_at(
                CapabilityRejectionCode::DependencyFanoutProhibited,
                scope,
                location,
            )?;
            return Ok(false);
        }
        if let Some(dependency) = requirement.dependencies().get(self.dependency_index) {
            let value = dependency.value();
            if value >= canonical_id
                || self
                    .previous_dependency
                    .is_some_and(|previous| previous >= value)
            {
                let location = context_for(&self.scene, graphics, requirement.context())
                    .ok_or_else(PolicyError::identity_mismatch)?
                    .1;
                self.reject_at(
                    CapabilityRejectionCode::InvalidDependencyGraph,
                    scope,
                    location,
                )?;
                return Ok(false);
            }
            let offset = usize::try_from(value).map_err(|_| PolicyError::numeric_overflow())?;
            if !*self
                .effective
                .get(offset)
                .ok_or_else(PolicyError::identity_mismatch)?
                && self.first_missing_dependency.is_none()
            {
                self.first_missing_dependency = Some(value);
            }
            self.previous_dependency = Some(value);
            self.dependency_index = self
                .dependency_index
                .checked_add(1)
                .ok_or_else(PolicyError::numeric_overflow)?;
            return Ok(false);
        }
        let direct = self.evaluator.profile.directly_supports(requirement);
        let supported = direct && self.first_missing_dependency.is_none();
        self.effective.push(supported);
        self.causes.push(if supported {
            MissingCause::Supported
        } else if direct {
            MissingCause::Dependency(
                self.first_missing_dependency
                    .ok_or_else(PolicyError::identity_mismatch)?,
            )
        } else {
            MissingCause::Direct(capability_code(requirement.capability()))
        });
        self.requirement_index = self
            .requirement_index
            .checked_add(1)
            .ok_or_else(PolicyError::numeric_overflow)?;
        self.dependency_index = 0;
        self.previous_dependency = None;
        self.first_missing_dependency = None;
        Ok(false)
    }

    fn step_count_missing(&mut self) -> Result<bool, PolicyError> {
        if let Some(supported) = self.effective.get(self.missing_count_index) {
            if !supported {
                self.missing_total = self
                    .missing_total
                    .checked_add(1)
                    .ok_or_else(PolicyError::numeric_overflow)?;
            }
            self.missing_count_index = self
                .missing_count_index
                .checked_add(1)
                .ok_or_else(PolicyError::numeric_overflow)?;
            return Ok(false);
        }
        self.phase = CapabilityJobPhase::AllocateDecision;
        Ok(false)
    }

    fn step_allocate_decision(&mut self) -> Result<bool, PolicyError> {
        let retained_missing = self
            .missing_total
            .min(self.evaluator.limits.max_missing_retained());
        let retained_contributors = self
            .missing_total
            .min(self.evaluator.limits.max_contributors_retained());
        reserve_job_vec(
            &mut self.missing,
            usize::try_from(retained_missing).map_err(|_| PolicyError::numeric_overflow())?,
            self.job_limits,
            &mut self.stats,
        )?;
        reserve_job_vec(
            &mut self.contributors,
            usize::try_from(retained_contributors).map_err(|_| PolicyError::numeric_overflow())?,
            self.job_limits,
            &mut self.stats,
        )?;
        self.phase = CapabilityJobPhase::BuildMissing;
        Ok(false)
    }

    fn step_build_missing(&mut self) -> Result<bool, PolicyError> {
        if let Some(pending) = self.pending_missing.as_mut() {
            let graphics = self
                .scene
                .graphics()
                .ok_or_else(PolicyError::identity_mismatch)?;
            let requirement = graphics
                .requirements()
                .get(pending.requirement_index)
                .ok_or_else(PolicyError::identity_mismatch)?;
            if let Some(dependency) = requirement.dependencies().get(pending.dependency_index) {
                pending.dependencies.push(dependency.value());
                pending.dependency_index = pending
                    .dependency_index
                    .checked_add(1)
                    .ok_or_else(PolicyError::numeric_overflow)?;
                return Ok(false);
            }
            let pending = self
                .pending_missing
                .take()
                .ok_or_else(PolicyError::identity_mismatch)?;
            self.missing.push(MissingCapabilityRequirement {
                id: requirement.id().value(),
                capability: requirement.capability(),
                parameter: requirement.parameter(),
                dependencies: pending.dependencies,
                scope: pending.scope,
                contributor_ids: pending.contributor_ids,
                location: pending.location,
            });
            self.finish_missing_requirement()?;
            return Ok(false);
        }

        let graphics = self
            .scene
            .graphics()
            .ok_or_else(PolicyError::identity_mismatch)?;
        let Some(requirement) = graphics.requirements().get(self.missing_index) else {
            if self.missing_ordinal != self.missing_total {
                return Err(PolicyError::identity_mismatch());
            }
            self.build_decision()?;
            self.phase = CapabilityJobPhase::Seal;
            return Ok(false);
        };
        if *self
            .effective
            .get(self.missing_index)
            .ok_or_else(PolicyError::identity_mismatch)?
        {
            self.missing_index = self
                .missing_index
                .checked_add(1)
                .ok_or_else(PolicyError::numeric_overflow)?;
            return Ok(false);
        }
        let (scope, available_location) = context_for(&self.scene, graphics, requirement.context())
            .ok_or_else(PolicyError::identity_mismatch)?;
        let location = (self.missing_ordinal < self.evaluator.limits.max_locations_retained())
            .then_some(available_location);
        if self.missing_ordinal == 0 {
            self.first_scope = Some(scope);
            self.first_location = location;
        }
        let cause = *self
            .causes
            .get(self.missing_index)
            .ok_or_else(PolicyError::identity_mismatch)?;
        let (kind, code) = match cause {
            MissingCause::Direct(code) => (CapabilityContributorKind::SceneRequirement, code),
            MissingCause::Dependency(id) => {
                (CapabilityContributorKind::PolicyDependencyClosure, id)
            }
            MissingCause::Supported => return Err(PolicyError::identity_mismatch()),
        };
        if self.missing_ordinal < self.evaluator.limits.max_contributors_retained() {
            self.contributors.push(CapabilityContributor {
                id: self.missing_ordinal,
                kind,
                code,
                location,
            });
        }
        if self.missing_ordinal < self.evaluator.limits.max_missing_retained() {
            let mut dependencies = Vec::new();
            reserve_job_vec(
                &mut dependencies,
                requirement.dependencies().len(),
                self.job_limits,
                &mut self.stats,
            )?;
            let mut contributor_ids = Vec::new();
            if self.missing_ordinal < self.evaluator.limits.max_contributors_retained() {
                reserve_job_vec(&mut contributor_ids, 1, self.job_limits, &mut self.stats)?;
                contributor_ids.push(self.missing_ordinal);
            }
            self.pending_missing = Some(PendingMissing {
                requirement_index: self.missing_index,
                dependency_index: 0,
                dependencies,
                contributor_ids,
                scope,
                location,
            });
        } else {
            self.finish_missing_requirement()?;
        }
        Ok(false)
    }

    fn finish_missing_requirement(&mut self) -> Result<(), PolicyError> {
        self.missing_ordinal = self
            .missing_ordinal
            .checked_add(1)
            .ok_or_else(PolicyError::numeric_overflow)?;
        self.missing_index = self
            .missing_index
            .checked_add(1)
            .ok_or_else(PolicyError::numeric_overflow)?;
        Ok(())
    }

    fn build_decision(&mut self) -> Result<(), PolicyError> {
        let subject = self.subject.ok_or_else(PolicyError::identity_mismatch)?;
        let cardinalities = self
            .cardinalities
            .ok_or_else(PolicyError::identity_mismatch)?;
        let page_scope = CapabilityScope::Page {
            page: subject.page_index(),
        };
        let status = if self.missing_total == 0 {
            CapabilityStatus::Supported
        } else {
            CapabilityStatus::Unsupported
        };
        let missing = std::mem::take(&mut self.missing);
        let contributors = std::mem::take(&mut self.contributors);
        self.pending_decision = Some(CapabilityDecision {
            schema_version: DECISION_SCHEMA_VERSION,
            status,
            profile: self.evaluator.profile,
            subject,
            missing,
            missing_total: self.missing_total,
            missing_completeness: completeness(
                self.missing_total
                    .min(self.evaluator.limits.max_missing_retained()),
                self.missing_total,
            ),
            contributors,
            contributors_total: self.missing_total,
            contributors_completeness: completeness(
                self.missing_total
                    .min(self.evaluator.limits.max_contributors_retained()),
                self.missing_total,
            ),
            locations_total: self.missing_total,
            locations_completeness: completeness(
                u32::from(self.first_location.is_some()),
                self.missing_total,
            ),
            evaluated_requirements: cardinalities.requirements,
            evaluated_dependencies: cardinalities.dependencies,
            evaluated_parameters: cardinalities.parameters,
            evaluated_commands: cardinalities.commands,
            evaluated_resources: cardinalities.resources,
            scope: if status == CapabilityStatus::Supported {
                page_scope
            } else {
                self.first_scope
                    .ok_or_else(PolicyError::identity_mismatch)?
            },
            location: if status == CapabilityStatus::Supported {
                None
            } else {
                self.first_location
            },
            rejection_code: None,
            hash: CapabilityDecisionHash::new([0; 32]),
        });
        let effective = vec_capacity_bytes(&self.effective)?;
        let causes = vec_capacity_bytes(&self.causes)?;
        self.effective = Vec::new();
        self.causes = Vec::new();
        self.stats.release(
            effective
                .checked_add(causes)
                .ok_or_else(PolicyError::numeric_overflow)?,
        )?;
        Ok(())
    }

    fn step_seal(&mut self, cancellation: &dyn PolicyCancellation) -> Result<bool, PolicyError> {
        let mut work =
            CancellationWork::new(cancellation, self.evaluator.limits.cancellation_interval())?;
        let decision = self
            .pending_decision
            .take()
            .ok_or_else(PolicyError::identity_mismatch)?
            .seal(&mut work)?;
        self.terminal = Some(Ok(decision));
        Ok(true)
    }

    fn reject(&mut self, code: CapabilityRejectionCode) -> Result<(), PolicyError> {
        self.reject_at(
            code,
            CapabilityScope::Page {
                page: self.scene.binding().page_index(),
            },
            CapabilityLocation::page(&self.scene),
        )
    }

    fn reject_at(
        &mut self,
        code: CapabilityRejectionCode,
        scope: CapabilityScope,
        location: CapabilityLocation,
    ) -> Result<(), PolicyError> {
        let cardinalities = self
            .cardinalities
            .ok_or_else(PolicyError::identity_mismatch)?;
        self.pending_decision = Some(rejected(
            self.evaluator.profile,
            self.subject.ok_or_else(PolicyError::identity_mismatch)?,
            code,
            scope,
            retained_location(0, self.evaluator.limits, location),
            cardinalities.requirements,
            cardinalities.dependencies,
            cardinalities.parameters,
            cardinalities.commands,
            cardinalities.resources,
            self.evaluator.limits,
        )?);
        let effective = vec_capacity_bytes(&self.effective)?;
        let causes = vec_capacity_bytes(&self.causes)?;
        self.effective = Vec::new();
        self.causes = Vec::new();
        self.stats.release(
            effective
                .checked_add(causes)
                .ok_or_else(PolicyError::numeric_overflow)?,
        )?;
        self.phase = CapabilityJobPhase::Seal;
        Ok(())
    }

    #[cfg(not(test))]
    const fn audited_resource_id(&self, resource_id: u32) -> u32 {
        resource_id
    }

    #[cfg(test)]
    fn audited_resource_id(&self, resource_id: u32) -> u32 {
        self.resource_id_override
            .filter(|(index, _)| *index == self.resource_index)
            .map_or(resource_id, |(_, value)| value)
    }

    #[cfg(not(test))]
    const fn audited_requirement_id(&self, requirement_id: u32) -> u32 {
        requirement_id
    }

    #[cfg(test)]
    fn audited_requirement_id(&self, requirement_id: u32) -> u32 {
        self.requirement_id_override
            .filter(|(index, _)| *index == self.requirement_index)
            .map_or(requirement_id, |(_, value)| value)
    }

    fn fail(&mut self, error: PolicyError) {
        self.canonical = None;
        self.effective = Vec::new();
        self.causes = Vec::new();
        self.missing = Vec::new();
        self.contributors = Vec::new();
        self.pending_missing = None;
        self.pending_decision = None;
        self.stats.clear_retained();
        self.terminal = Some(Err(error));
    }
}

impl fmt::Debug for CapabilityEvaluationJob {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CapabilityEvaluationJob")
            .field("document_revision", &self.document_revision)
            .field("job_limits", &self.job_limits)
            .field("stats", &self.stats)
            .field("phase", &self.phase)
            .field("terminal", &self.terminal.as_ref().map(Result::is_ok))
            .field("scene", &"[REDACTED]")
            .field("working_bytes", &"[REDACTED]")
            .finish()
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

#[cfg(test)]
#[derive(Clone, Copy)]
struct DependencyOverride<'a> {
    requirement_index: usize,
    dependencies: &'a [u32],
}

#[cfg(test)]
#[derive(Clone, Copy)]
enum DependencyValues<'a> {
    Scene(&'a [pdf_rs_scene::CapabilityRequirementId]),
    Override(&'a [u32]),
}

#[cfg(test)]
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

#[cfg(test)]
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

#[cfg(test)]
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
#[cfg(test)]
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

#[allow(dead_code)]
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

#[allow(dead_code)]
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

#[cfg(test)]
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

/// Returns a checked conservative upper bound for this Scene's canonical JSON bytes.
///
/// Published Scene retained-byte accounting includes every variable-length name, image, path,
/// glyph, command, resource, requirement, and dependency allocation. Canonical encoding expands
/// arbitrary bytes to at most two hexadecimal digits and signed integers to at most 20 decimal
/// bytes. The 64x retained-capacity factor dominates those representations, 1 KiB per top-level
/// semantic record dominates fixed field labels and inline scalars, and 64 KiB dominates the
/// document envelope. The serializer's declared maximum remains an independent upper bound.
/// Checked arithmetic overflow conservatively returns that declared maximum.
pub(crate) fn canonical_scene_upper_bound(scene: &Scene) -> u64 {
    const FIXED_CANONICAL_OVERHEAD: u64 = 64 * 1024;
    const BINARY_TO_JSON_EXPANSION: u64 = 64;
    const TOP_LEVEL_ITEM_OVERHEAD: u64 = 1024;
    let (declared, retained, items) = scene.graphics().map_or_else(
        || {
            (
                scene.limits().max_canonical_bytes(),
                scene.stats().retained_bytes(),
                Some(u64::from(scene.stats().commands()) + u64::from(scene.stats().resources())),
            )
        },
        |graphics| {
            (
                graphics.limits().max_canonical_bytes(),
                graphics.stats().retained_bytes(),
                u64::try_from(graphics.commands().len())
                    .ok()
                    .and_then(|count| {
                        count.checked_add(u64::try_from(graphics.resources().len()).ok()?)
                    })
                    .and_then(|count| {
                        count.checked_add(u64::try_from(graphics.requirements().len()).ok()?)
                    }),
            )
        },
    );
    let estimate = retained
        .checked_mul(BINARY_TO_JSON_EXPANSION)
        .and_then(|bytes| {
            items?
                .checked_mul(TOP_LEVEL_ITEM_OVERHEAD)
                .and_then(|overhead| bytes.checked_add(overhead))
        })
        .and_then(|bytes| bytes.checked_add(FIXED_CANONICAL_OVERHEAD));
    estimate.map_or(declared, |value| value.min(declared))
}

fn vec_capacity_bytes<T>(values: &Vec<T>) -> Result<u64, PolicyError> {
    u64::try_from(values.capacity())
        .ok()
        .and_then(|count| {
            u64::try_from(size_of::<T>())
                .ok()
                .and_then(|width| count.checked_mul(width))
        })
        .ok_or_else(PolicyError::numeric_overflow)
}

fn reserve_job_vec<T>(
    values: &mut Vec<T>,
    capacity: usize,
    limits: PolicyJobLimits,
    stats: &mut PolicyJobStats,
) -> Result<(), PolicyError> {
    if capacity == 0 {
        return Ok(());
    }
    let maximum = limits.max_retained_bytes();
    let requested = u64::try_from(capacity)
        .ok()
        .and_then(|count| {
            u64::try_from(size_of::<T>())
                .ok()
                .and_then(|width| count.checked_mul(width))
        })
        .ok_or_else(PolicyError::numeric_overflow)?;
    if requested > maximum.saturating_sub(stats.retained_bytes()) {
        return Err(PolicyError::resource(
            PolicyLimitKind::JobRetainedBytes,
            maximum,
            stats.retained_bytes(),
            requested,
        ));
    }
    values
        .try_reserve_exact(capacity)
        .map_err(|_| PolicyError::allocation())?;
    stats.charge_allocation(vec_capacity_bytes(values)?, limits)
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
    use std::num::NonZeroU32;
    use std::sync::Arc;

    use pdf_rs_bytes::{SourceIdentity, SourceRevision, SourceStableId};
    use pdf_rs_scene::{
        BlendMode, CapabilityContext, CapabilityStatus as SceneCapabilityStatus, CommandSource,
        DeviceColor, FillRule, GlyphOutline, GlyphUse, GraphicsCapability, GraphicsResourceSource,
        GraphicsSceneBuilder, GraphicsSceneLimits, ImageColorSpace, ImageResource, Matrix,
        PageGeometry, PageRotation, Paint, PathResource, PathSegment, SceneBinding, SceneBounds,
        SceneBuilder, SceneLimits, ScenePoint, SceneRect, SceneScalar, SceneUnit,
    };
    use pdf_rs_syntax::ObjectRef;

    use super::{
        CapabilityEvaluationJob, CapabilityEvaluator, CapabilityRejectionCode, CapabilityStatus,
        NeverCancelled, PolicyJobLimits, PolicyJobPoll, PolicyPollBudget,
        canonical_scene_upper_bound, dependencies_are_canonical,
    };

    fn scalar(value: &str) -> SceneScalar {
        SceneScalar::from_decimal(value).unwrap()
    }

    fn point(x: &str, y: &str) -> ScenePoint {
        ScenePoint::new(scalar(x), scalar(y))
    }

    fn test_binding(salt: u8) -> SceneBinding {
        SceneBinding::new(
            SourceIdentity::new(
                SourceStableId::new([salt; 32]),
                SourceRevision::new(u64::MAX),
            ),
            u64::MAX,
            u32::MAX,
            ObjectRef::new(u32::MAX, u16::MAX).unwrap(),
        )
    }

    fn test_geometry() -> PageGeometry {
        let page = SceneRect::new([
            SceneScalar::from_scaled(-4_000_000_000_000_000_000),
            SceneScalar::from_scaled(-4_000_000_000_000_000_000),
            SceneScalar::from_scaled(4_000_000_000_000_000_000),
            SceneScalar::from_scaled(4_000_000_000_000_000_000),
        ])
        .unwrap();
        PageGeometry::new(page, page, PageRotation::Degrees270)
    }

    fn test_source(index: u32) -> CommandSource {
        CommandSource::new(
            ObjectRef::new(u32::MAX - 1, u16::MAX).unwrap(),
            u32::MAX,
            u64::MAX - 16 - u64::from(index),
            8,
            index,
        )
        .unwrap()
    }

    fn test_path() -> PathResource {
        PathResource::new(vec![
            PathSegment::MoveTo(point("-1", "-2")),
            PathSegment::LineTo(point("5", "2")),
            PathSegment::CubicTo {
                control_1: point("7", "8"),
                control_2: point("9", "10"),
                end: point("11", "12"),
            },
            PathSegment::ClosePath,
        ])
        .unwrap()
    }

    fn alternate_path() -> PathResource {
        PathResource::new(vec![
            PathSegment::MoveTo(point("2", "3")),
            PathSegment::LineTo(point("6", "3")),
            PathSegment::LineTo(point("6", "8")),
            PathSegment::ClosePath,
        ])
        .unwrap()
    }

    fn black() -> Paint {
        Paint::new(
            DeviceColor::Gray(SceneUnit::ZERO),
            SceneUnit::ONE,
            BlendMode::Normal,
        )
    }

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

    #[test]
    fn canonical_upper_bound_covers_decimal_names_and_every_variable_graphics_family() {
        let mut legacy =
            SceneBuilder::new(test_binding(1), test_geometry(), SceneLimits::default());
        let long_name = vec![0xff; 32 * 1024];
        legacy
            .begin_marked_content(&long_name, None, test_source(0))
            .unwrap();
        legacy.end_marked_content(test_source(1)).unwrap();
        let legacy = legacy.finish().unwrap();
        assert!(
            u64::try_from(legacy.canonical_json_bytes().unwrap().len()).unwrap()
                <= canonical_scene_upper_bound(&legacy)
        );

        let mut graphics = GraphicsSceneBuilder::new_v2(
            test_binding(2),
            test_geometry(),
            GraphicsSceneLimits::default(),
        );
        graphics
            .append_fill(
                test_path(),
                FillRule::EvenOdd,
                black(),
                Matrix::IDENTITY,
                SceneBounds::Page,
                test_source(0),
            )
            .unwrap();
        let resource_source = GraphicsResourceSource::new(
            ObjectRef::new(u32::MAX - 2, u16::MAX).unwrap(),
            u64::MAX,
            u64::MAX,
        );
        graphics
            .draw_image(
                ImageResource::new(
                    resource_source,
                    64,
                    64,
                    ImageColorSpace::DeviceRgb,
                    8,
                    false,
                    vec![0xab; 64 * 64 * 3],
                )
                .unwrap(),
                Matrix::IDENTITY,
                SceneUnit::ONE,
                BlendMode::Normal,
                SceneBounds::Page,
                test_source(1),
            )
            .unwrap();
        let outline = GlyphOutline::new(resource_source, u32::MAX, u16::MAX, test_path()).unwrap();
        graphics
            .draw_glyph_run(
                vec![
                    GlyphUse::new(outline.clone(), Matrix::IDENTITY, u32::MAX),
                    GlyphUse::new(outline, Matrix::IDENTITY, u32::MAX - 1),
                ],
                black(),
                SceneBounds::Page,
                test_source(2),
            )
            .unwrap();
        let first = graphics
            .add_requirement(
                GraphicsCapability::SoftMask,
                u64::MAX,
                CapabilityContext::Scene,
                Vec::new(),
                SceneCapabilityStatus::Unsupported,
            )
            .unwrap();
        graphics
            .add_requirement(
                GraphicsCapability::IsolatedGroup,
                u64::MAX,
                CapabilityContext::Scene,
                vec![first],
                SceneCapabilityStatus::Unsupported,
            )
            .unwrap();
        let graphics = graphics.finish().unwrap();
        assert!(
            u64::try_from(graphics.canonical_json_bytes().unwrap().len()).unwrap()
                <= canonical_scene_upper_bound(&graphics)
        );
    }

    #[test]
    fn second_resource_id_is_audited_by_sync_and_one_unit_jobs() {
        let mut builder = GraphicsSceneBuilder::new_v2(
            test_binding(3),
            test_geometry(),
            GraphicsSceneLimits::default(),
        );
        for (index, path) in [test_path(), alternate_path()].into_iter().enumerate() {
            builder
                .append_fill(
                    path,
                    FillRule::Nonzero,
                    black(),
                    Matrix::IDENTITY,
                    SceneBounds::Page,
                    test_source(u32::try_from(index).unwrap()),
                )
                .unwrap();
        }
        let scene = builder.finish().unwrap();
        assert_eq!(scene.graphics().unwrap().resources().len(), 2);

        let sync = CapabilityEvaluator::default()
            .evaluate_with_resource_override(&scene, 23, 1, 7)
            .unwrap();
        assert_eq!(sync.status(), CapabilityStatus::Rejected);
        assert_eq!(
            sync.rejection_code(),
            Some(CapabilityRejectionCode::NonCanonicalResourceId)
        );

        let scene = Arc::new(scene);
        let mut incremental = CapabilityEvaluationJob::new_compatibility(
            CapabilityEvaluator::default(),
            Arc::clone(&scene),
            23,
            PolicyJobLimits::synchronous_compatibility(
                canonical_scene_upper_bound(&scene),
                u64::MAX,
            ),
        );
        incremental.resource_id_override = Some((1, 7));
        let one = PolicyPollBudget::new(NonZeroU32::new(1).unwrap()).unwrap();
        let mut pending = 0;
        while incremental.poll(one, &NeverCancelled) == PolicyJobPoll::Pending {
            pending += 1;
        }
        assert!(pending > 1);
        let incremental = incremental.take_result().unwrap().unwrap();
        assert_eq!(incremental, sync);
    }

    #[test]
    fn later_requirement_rejection_releases_closure_buffers_and_stays_terminal_after_take() {
        let mut builder = GraphicsSceneBuilder::new_v2(
            test_binding(4),
            test_geometry(),
            GraphicsSceneLimits::default(),
        );
        builder
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
                Vec::new(),
                SceneCapabilityStatus::Supported,
            )
            .unwrap();
        let scene = Arc::new(builder.finish().unwrap());
        let mut job = CapabilityEvaluationJob::new_compatibility(
            CapabilityEvaluator::default(),
            Arc::clone(&scene),
            23,
            PolicyJobLimits::synchronous_compatibility(
                canonical_scene_upper_bound(&scene),
                u64::MAX,
            ),
        );
        job.requirement_id_override = Some((1, 9));
        let one = PolicyPollBudget::new(NonZeroU32::new(1).unwrap()).unwrap();
        while job.poll(one, &NeverCancelled) == PolicyJobPoll::Pending {}
        assert_eq!(job.stats().retained_bytes(), 0);
        let decision = job.take_result().unwrap().unwrap();
        assert_eq!(
            decision.rejection_code(),
            Some(CapabilityRejectionCode::NonCanonicalRequirementId)
        );
        assert_eq!(job.stats().retained_bytes(), 0);
        assert_eq!(job.poll(one, &NeverCancelled), PolicyJobPoll::Ready);
        assert!(job.result().is_none());
    }
}
