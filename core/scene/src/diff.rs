use std::fmt;
use std::sync::Arc;

use crate::canonical::CanonicalWriter;
use crate::{
    CapabilityRequirement, GraphicsCommand, GraphicsResourceEntry, Scene, SceneCommand, SceneError,
    SceneErrorCode, SceneLimitKind, SceneVersion,
};

const HARD_MAX_DIFFERENCES: u32 = 16_000_000;
const HARD_MAX_DIFF_RETAINED_BYTES: u64 = 1024 * 1024 * 1024;
const HARD_MAX_DIFF_COMPARE_WORK: u64 = 1_000_000_000_000;
const HARD_MAX_DIFF_CANONICAL_BYTES: u64 = 1024 * 1024 * 1024;
const NO_INDEX: u32 = u32::MAX;

/// Unvalidated semantic Scene-diff limits.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SceneDiffLimitConfig {
    /// Maximum fixed-size difference records retained by one comparison.
    pub max_differences: u32,
    /// Maximum allocator-reported difference-record capacity in bytes.
    pub max_retained_bytes: u64,
    /// Maximum deterministic semantic comparison work.
    pub max_compare_work: u64,
    /// Maximum canonical Scene-diff JSON bytes.
    pub max_canonical_bytes: u64,
}

impl Default for SceneDiffLimitConfig {
    fn default() -> Self {
        Self {
            max_differences: 1_000_000,
            max_retained_bytes: 128 * 1024 * 1024,
            max_compare_work: 4_000_000_000,
            max_canonical_bytes: 256 * 1024 * 1024,
        }
    }
}

/// Validated semantic Scene-diff limits.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SceneDiffLimits {
    max_differences: u32,
    max_retained_bytes: u64,
    max_compare_work: u64,
    max_canonical_bytes: u64,
}

impl SceneDiffLimits {
    /// Validates every nonzero limit against fixed implementation hard ceilings.
    pub fn validate(config: SceneDiffLimitConfig) -> Result<Self, SceneError> {
        if config.max_differences == 0
            || config.max_differences > HARD_MAX_DIFFERENCES
            || config.max_retained_bytes == 0
            || config.max_retained_bytes > HARD_MAX_DIFF_RETAINED_BYTES
            || config.max_compare_work == 0
            || config.max_compare_work > HARD_MAX_DIFF_COMPARE_WORK
            || config.max_canonical_bytes == 0
            || config.max_canonical_bytes > HARD_MAX_DIFF_CANONICAL_BYTES
        {
            return Err(SceneError::for_code(SceneErrorCode::InvalidLimits, None));
        }
        Ok(Self {
            max_differences: config.max_differences,
            max_retained_bytes: config.max_retained_bytes,
            max_compare_work: config.max_compare_work,
            max_canonical_bytes: config.max_canonical_bytes,
        })
    }

    /// Returns the maximum retained difference count.
    pub const fn max_differences(self) -> u32 {
        self.max_differences
    }

    /// Returns the maximum retained difference-record capacity.
    pub const fn max_retained_bytes(self) -> u64 {
        self.max_retained_bytes
    }

    /// Returns the maximum deterministic semantic comparison work.
    pub const fn max_compare_work(self) -> u64 {
        self.max_compare_work
    }

    /// Returns the maximum canonical Scene-diff JSON size.
    pub const fn max_canonical_bytes(self) -> u64 {
        self.max_canonical_bytes
    }
}

impl Default for SceneDiffLimits {
    fn default() -> Self {
        Self::validate(SceneDiffLimitConfig::default())
            .expect("built-in Scene diff limits satisfy hard ceilings")
    }
}

/// Stable top-level semantic section containing one Scene difference.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
#[repr(u8)]
pub enum SceneDiffSection {
    /// Scene schema major or minor version.
    Schema,
    /// Page index, exact Page object, or revision anchor.
    Binding,
    /// MediaBox, CropBox, or canonical rotation.
    Geometry,
    /// Capability decision or ordered feature tags.
    Features,
    /// Stable resource table in identifier order.
    Resources,
    /// Semantic commands in execution order.
    Commands,
    /// Command provenance paired by command index.
    CommandProvenance,
    /// Scene-v2 semantic graphics commands.
    GraphicsCommands,
    /// Scene-v2 decoded command provenance.
    GraphicsCommandProvenance,
    /// Scene-v2 conservative command bounds.
    GraphicsBounds,
    /// Scene-v2 first-use graphics resources.
    GraphicsResources,
    /// Scene-v2 capability requirements and dependencies.
    GraphicsCapabilities,
}

/// Stable field within a semantic Scene-diff section.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
#[repr(u8)]
pub enum SceneDiffField {
    /// Incompatible schema generation.
    Major,
    /// Compatible schema revision.
    Minor,
    /// Zero-based logical page index.
    PageIndex,
    /// Exact indirect Page object identity.
    PageObject,
    /// Revision `startxref` anchor.
    RevisionStartxref,
    /// Inherited MediaBox.
    MediaBox,
    /// Inherited CropBox.
    CropBox,
    /// Canonical clockwise page rotation.
    Rotation,
    /// Page-level capability decision.
    Decision,
    /// One position in an ordered semantic section.
    Entry,
}

/// Relationship between the expected and actual value at one semantic location.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
#[repr(u8)]
pub enum SceneDiffKind {
    /// The actual Scene contains an entry absent from the expected Scene.
    Added,
    /// The expected Scene contains an entry absent from the actual Scene.
    Removed,
    /// Both Scenes contain the location but its semantic value differs.
    Changed,
}

/// One fixed-size, content-redacted semantic Scene difference.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
#[repr(C)]
pub struct SceneDifference {
    section: SceneDiffSection,
    field: SceneDiffField,
    kind: SceneDiffKind,
    index: u32,
}

impl SceneDifference {
    const fn scalar(section: SceneDiffSection, field: SceneDiffField, kind: SceneDiffKind) -> Self {
        Self {
            section,
            field,
            kind,
            index: NO_INDEX,
        }
    }

    const fn entry(section: SceneDiffSection, kind: SceneDiffKind, index: u32) -> Self {
        Self {
            section,
            field: SceneDiffField::Entry,
            kind,
            index,
        }
    }

    /// Returns the top-level semantic section.
    pub const fn section(self) -> SceneDiffSection {
        self.section
    }

    /// Returns the stable field within the section.
    pub const fn field(self) -> SceneDiffField {
        self.field
    }

    /// Returns whether the actual value was added, removed, or changed.
    pub const fn kind(self) -> SceneDiffKind {
        self.kind
    }

    /// Returns the ordered-entry index, or `None` for scalar fields.
    pub const fn index(self) -> Option<u32> {
        if self.index == NO_INDEX {
            None
        } else {
            Some(self.index)
        }
    }
}

/// Deterministic semantic Scene-diff accounting.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct SceneDiffStats {
    differences: u32,
    added: u32,
    removed: u32,
    changed: u32,
    retained_bytes: u64,
    compare_work: u64,
}

impl SceneDiffStats {
    const fn new(
        differences: u32,
        added: u32,
        removed: u32,
        changed: u32,
        retained_bytes: u64,
        compare_work: u64,
    ) -> Self {
        Self {
            differences,
            added,
            removed,
            changed,
            retained_bytes,
            compare_work,
        }
    }

    /// Returns the total semantic difference count.
    pub const fn differences(self) -> u32 {
        self.differences
    }

    /// Returns the count of entries added by the actual Scene.
    pub const fn added(self) -> u32 {
        self.added
    }

    /// Returns the count of entries removed from the actual Scene.
    pub const fn removed(self) -> u32 {
        self.removed
    }

    /// Returns the count of scalar or entry values changed in place.
    pub const fn changed(self) -> u32 {
        self.changed
    }

    /// Returns allocator-reported retained difference-record capacity.
    pub const fn retained_bytes(self) -> u64 {
        self.retained_bytes
    }

    /// Returns deterministic work charged before semantic value comparisons.
    pub const fn compare_work(self) -> u64 {
        self.compare_work
    }
}

/// Immutable, bounded, content-redacted semantic comparison of two Scenes.
#[derive(Clone, Eq, PartialEq)]
pub struct SceneDiff {
    differences: Arc<Vec<SceneDifference>>,
    limits: SceneDiffLimits,
    stats: SceneDiffStats,
    schema_major: u16,
}

impl SceneDiff {
    /// Returns `true` when the compared Scenes have identical canonical semantics.
    pub fn is_exact(&self) -> bool {
        self.differences.is_empty()
    }

    /// Returns fixed-size differences in stable semantic order.
    pub fn differences(&self) -> &[SceneDifference] {
        &self.differences
    }

    /// Returns the complete validated diff limit profile.
    pub const fn limits(&self) -> SceneDiffLimits {
        self.limits
    }

    /// Returns deterministic difference counts and retained-capacity accounting.
    pub const fn stats(&self) -> SceneDiffStats {
        self.stats
    }

    /// Serializes this comparison into compact deterministic schema-1 JSON bytes.
    ///
    /// Records contain only stable section, field, relationship, and index metadata. Source
    /// identity, PDF name bytes, object values, and document content are never emitted.
    pub fn canonical_json_bytes(&self) -> Result<Vec<u8>, SceneError> {
        let mut writer = CanonicalWriter::new(
            self.limits.max_canonical_bytes(),
            SceneLimitKind::DiffCanonicalBytes,
        );
        writer.push(b"{\"differences\":[")?;
        for (index, difference) in self.differences.iter().copied().enumerate() {
            writer.separator(index)?;
            write_difference(&mut writer, difference)?;
        }
        writer.push(b"],\"schema\":{\"major\":")?;
        writer.push_u16(self.schema_major)?;
        writer.push(b",\"minor\":0,\"name\":\"scene-semantic-diff\"}")?;
        writer.push(b",\"summary\":{\"added\":")?;
        writer.push_u32(self.stats.added())?;
        writer.push(b",\"changed\":")?;
        writer.push_u32(self.stats.changed())?;
        writer.push(b",\"removed\":")?;
        writer.push_u32(self.stats.removed())?;
        writer.push(b",\"total\":")?;
        writer.push_u32(self.stats.differences())?;
        writer.push(b"}}")?;
        Ok(writer.finish())
    }
}

impl fmt::Debug for SceneDiff {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SceneDiff")
            .field("stats", &self.stats)
            .field("limits", &self.limits)
            .field("content", &"[REDACTED]")
            .finish()
    }
}

/// Compares expected and actual Scenes under bounded canonical semantic rules.
///
/// Runtime [`pdf_rs_bytes::SourceIdentity`] is deliberately ignored. Differences are emitted in
/// schema, binding, geometry, feature-report, resource, command, then command-provenance order.
/// Ordered sections are compared positionally; shared positions are `Changed` and a trailing
/// length imbalance is represented by ascending `Added` or `Removed` records.
pub fn compare_scenes(
    expected: &Scene,
    actual: &Scene,
    limits: SceneDiffLimits,
) -> Result<SceneDiff, SceneError> {
    let mut collector = DifferenceCollector::new(limits);
    visit_differences(expected, actual, &mut collector)?;
    collector.finish(
        if expected.graphics().is_some() || actual.graphics().is_some() {
            2
        } else {
            1
        },
    )
}

trait DifferenceSink {
    fn charge_compare(&mut self, attempted: u64) -> Result<(), SceneError>;
    fn record(&mut self, difference: SceneDifference) -> Result<(), SceneError>;
}

struct DifferenceCollector {
    limits: SceneDiffLimits,
    values: Vec<SceneDifference>,
    differences: u32,
    added: u32,
    removed: u32,
    changed: u32,
    compare_work: u64,
}

impl DifferenceCollector {
    const fn new(limits: SceneDiffLimits) -> Self {
        Self {
            limits,
            values: Vec::new(),
            differences: 0,
            added: 0,
            removed: 0,
            changed: 0,
            compare_work: 0,
        }
    }

    fn finish(self, schema_major: u16) -> Result<SceneDiff, SceneError> {
        let retained_bytes = retained_bytes_for(
            u32::try_from(self.values.capacity())
                .map_err(|_| SceneError::for_code(SceneErrorCode::InternalState, None))?,
        )?;
        let stats = SceneDiffStats::new(
            self.differences,
            self.added,
            self.removed,
            self.changed,
            retained_bytes,
            self.compare_work,
        );
        Ok(SceneDiff {
            differences: Arc::new(self.values),
            limits: self.limits,
            stats,
            schema_major,
        })
    }
}

impl DifferenceSink for DifferenceCollector {
    fn charge_compare(&mut self, attempted: u64) -> Result<(), SceneError> {
        let remaining = self
            .limits
            .max_compare_work()
            .checked_sub(self.compare_work)
            .ok_or_else(|| SceneError::for_code(SceneErrorCode::InternalState, None))?;
        if attempted > remaining {
            return Err(SceneError::resource(
                SceneLimitKind::DiffCompareWork,
                self.limits.max_compare_work(),
                self.compare_work,
                attempted,
                None,
            ));
        }
        self.compare_work = self
            .compare_work
            .checked_add(attempted)
            .ok_or_else(|| SceneError::for_code(SceneErrorCode::InternalState, None))?;
        Ok(())
    }

    fn record(&mut self, difference: SceneDifference) -> Result<(), SceneError> {
        if self.differences == self.limits.max_differences() {
            return Err(SceneError::resource(
                SceneLimitKind::Differences,
                u64::from(self.limits.max_differences()),
                u64::from(self.differences),
                1,
                None,
            ));
        }

        let next_count = self
            .differences
            .checked_add(1)
            .ok_or_else(|| SceneError::for_code(SceneErrorCode::InternalState, None))?;
        let minimum_retained = retained_bytes_for(next_count)?;
        if minimum_retained > self.limits.max_retained_bytes() {
            return Err(SceneError::resource(
                SceneLimitKind::DiffRetainedBytes,
                self.limits.max_retained_bytes(),
                retained_bytes_for(self.differences)?,
                retained_bytes_for(1)?,
                None,
            ));
        }
        if self.values.len() == self.values.capacity() {
            let retained_before = retained_bytes_for(
                u32::try_from(self.values.capacity())
                    .map_err(|_| SceneError::for_code(SceneErrorCode::InternalState, None))?,
            )?;
            self.values.try_reserve(1).map_err(|_| {
                SceneError::resource(
                    SceneLimitKind::Allocation,
                    self.limits.max_retained_bytes(),
                    retained_before,
                    retained_bytes_for(1).unwrap_or(u64::MAX),
                    None,
                )
            })?;
            let retained_after = retained_bytes_for(
                u32::try_from(self.values.capacity())
                    .map_err(|_| SceneError::for_code(SceneErrorCode::InternalState, None))?,
            )?;
            if retained_after > self.limits.max_retained_bytes() {
                return Err(SceneError::resource(
                    SceneLimitKind::DiffRetainedBytes,
                    self.limits.max_retained_bytes(),
                    retained_before,
                    retained_after.saturating_sub(retained_before),
                    None,
                ));
            }
        }
        self.values.push(difference);
        self.differences = next_count;
        match difference.kind() {
            SceneDiffKind::Added => self.added = checked_increment(self.added)?,
            SceneDiffKind::Removed => self.removed = checked_increment(self.removed)?,
            SceneDiffKind::Changed => self.changed = checked_increment(self.changed)?,
        }
        Ok(())
    }
}

fn checked_increment(value: u32) -> Result<u32, SceneError> {
    value
        .checked_add(1)
        .ok_or_else(|| SceneError::for_code(SceneErrorCode::InternalState, None))
}

fn retained_bytes_for(capacity: u32) -> Result<u64, SceneError> {
    let width = u64::try_from(std::mem::size_of::<SceneDifference>())
        .map_err(|_| SceneError::for_code(SceneErrorCode::InternalState, None))?;
    u64::from(capacity)
        .checked_mul(width)
        .ok_or_else(|| SceneError::for_code(SceneErrorCode::InternalState, None))
}

fn visit_differences(
    expected: &Scene,
    actual: &Scene,
    sink: &mut impl DifferenceSink,
) -> Result<(), SceneError> {
    compare_version(expected.version(), actual.version(), sink)?;

    let expected_binding = expected.binding();
    let actual_binding = actual.binding();
    compare_scalar(
        expected_binding.page_index() != actual_binding.page_index(),
        SceneDiffSection::Binding,
        SceneDiffField::PageIndex,
        sink,
    )?;
    compare_scalar(
        expected_binding.page_object() != actual_binding.page_object(),
        SceneDiffSection::Binding,
        SceneDiffField::PageObject,
        sink,
    )?;
    compare_scalar(
        expected_binding.revision_startxref() != actual_binding.revision_startxref(),
        SceneDiffSection::Binding,
        SceneDiffField::RevisionStartxref,
        sink,
    )?;

    let expected_geometry = expected.geometry();
    let actual_geometry = actual.geometry();
    compare_scalar(
        expected_geometry.media_box() != actual_geometry.media_box(),
        SceneDiffSection::Geometry,
        SceneDiffField::MediaBox,
        sink,
    )?;
    compare_scalar(
        expected_geometry.crop_box() != actual_geometry.crop_box(),
        SceneDiffSection::Geometry,
        SceneDiffField::CropBox,
        sink,
    )?;
    compare_scalar(
        expected_geometry.rotation() != actual_geometry.rotation(),
        SceneDiffSection::Geometry,
        SceneDiffField::Rotation,
        sink,
    )?;

    compare_scalar(
        expected.features().decision() != actual.features().decision(),
        SceneDiffSection::Features,
        SceneDiffField::Decision,
        sink,
    )?;
    compare_entries_with(
        SceneDiffSection::Features,
        expected.features().tags(),
        actual.features().tags(),
        sink,
        |_, _| Ok(1),
        |expected, actual| expected != actual,
    )?;
    compare_entries_with(
        SceneDiffSection::Resources,
        expected.resources(),
        actual.resources(),
        sink,
        |_, _| Ok(1),
        |expected, actual| expected != actual,
    )?;
    compare_entries_with(
        SceneDiffSection::Commands,
        expected.commands(),
        actual.commands(),
        sink,
        scene_command_compare_work,
        scene_commands_differ,
    )?;
    compare_entries_with(
        SceneDiffSection::CommandProvenance,
        expected.provenance(),
        actual.provenance(),
        sink,
        |_, _| Ok(1),
        |expected, actual| expected != actual,
    )?;
    compare_graphics(expected, actual, sink)
}

fn compare_graphics(
    expected: &Scene,
    actual: &Scene,
    sink: &mut impl DifferenceSink,
) -> Result<(), SceneError> {
    let expected_commands = expected
        .graphics()
        .map_or(&[][..], |graphics| graphics.commands());
    let actual_commands = actual
        .graphics()
        .map_or(&[][..], |graphics| graphics.commands());
    compare_entries_with(
        SceneDiffSection::GraphicsCommands,
        expected_commands,
        actual_commands,
        sink,
        |expected, actual| graphics_command_compare_work(expected.command(), actual.command()),
        |expected, actual| expected.command() != actual.command(),
    )?;
    compare_entries_with(
        SceneDiffSection::GraphicsCommandProvenance,
        expected_commands,
        actual_commands,
        sink,
        |_, _| Ok(1),
        |expected, actual| expected.source() != actual.source(),
    )?;
    compare_entries_with(
        SceneDiffSection::GraphicsBounds,
        expected_commands,
        actual_commands,
        sink,
        |_, _| Ok(1),
        |expected, actual| expected.bounds() != actual.bounds(),
    )?;

    let expected_resources = expected
        .graphics()
        .map_or(&[][..], |graphics| graphics.resources());
    let actual_resources = actual
        .graphics()
        .map_or(&[][..], |graphics| graphics.resources());
    compare_entries_with(
        SceneDiffSection::GraphicsResources,
        expected_resources,
        actual_resources,
        sink,
        graphics_resource_compare_work,
        |expected, actual| expected != actual,
    )?;

    let expected_requirements = expected
        .graphics()
        .map_or(&[][..], |graphics| graphics.requirements());
    let actual_requirements = actual
        .graphics()
        .map_or(&[][..], |graphics| graphics.requirements());
    compare_entries_with(
        SceneDiffSection::GraphicsCapabilities,
        expected_requirements,
        actual_requirements,
        sink,
        capability_compare_work,
        |expected, actual| expected != actual,
    )
}

fn compare_version(
    expected: SceneVersion,
    actual: SceneVersion,
    sink: &mut impl DifferenceSink,
) -> Result<(), SceneError> {
    compare_schema_components(
        expected.major(),
        expected.minor(),
        actual.major(),
        actual.minor(),
        sink,
    )
}

fn compare_schema_components(
    expected_major: u16,
    expected_minor: u16,
    actual_major: u16,
    actual_minor: u16,
    sink: &mut impl DifferenceSink,
) -> Result<(), SceneError> {
    compare_scalar(
        expected_major != actual_major,
        SceneDiffSection::Schema,
        SceneDiffField::Major,
        sink,
    )?;
    compare_scalar(
        expected_minor != actual_minor,
        SceneDiffSection::Schema,
        SceneDiffField::Minor,
        sink,
    )
}

fn compare_scalar(
    differs: bool,
    section: SceneDiffSection,
    field: SceneDiffField,
    sink: &mut impl DifferenceSink,
) -> Result<(), SceneError> {
    sink.charge_compare(1)?;
    if differs {
        sink.record(SceneDifference::scalar(
            section,
            field,
            SceneDiffKind::Changed,
        ))?;
    }
    Ok(())
}

fn compare_entries_with<T>(
    section: SceneDiffSection,
    expected: &[T],
    actual: &[T],
    sink: &mut impl DifferenceSink,
    compare_work: impl Fn(&T, &T) -> Result<u64, SceneError>,
    differs: impl Fn(&T, &T) -> bool,
) -> Result<(), SceneError> {
    let shared = expected.len().min(actual.len());
    for index in 0..shared {
        sink.charge_compare(compare_work(&expected[index], &actual[index])?)?;
        if differs(&expected[index], &actual[index]) {
            sink.record(SceneDifference::entry(
                section,
                SceneDiffKind::Changed,
                difference_index(index)?,
            ))?;
        }
    }
    for index in shared..expected.len() {
        sink.charge_compare(1)?;
        sink.record(SceneDifference::entry(
            section,
            SceneDiffKind::Removed,
            difference_index(index)?,
        ))?;
    }
    for index in shared..actual.len() {
        sink.charge_compare(1)?;
        sink.record(SceneDifference::entry(
            section,
            SceneDiffKind::Added,
            difference_index(index)?,
        ))?;
    }
    Ok(())
}

fn scene_command_compare_work(
    expected: &SceneCommand,
    actual: &SceneCommand,
) -> Result<u64, SceneError> {
    let payload = match (expected.tag(), actual.tag()) {
        (Some(expected), Some(actual)) => expected.bytes().len().max(actual.bytes().len()),
        _ => 0,
    };
    compare_work_with_payload(payload)
}

fn scene_commands_differ(expected: &SceneCommand, actual: &SceneCommand) -> bool {
    expected.kind() != actual.kind()
        || expected.properties() != actual.properties()
        || match (expected.tag(), actual.tag()) {
            (Some(expected), Some(actual)) => expected.bytes() != actual.bytes(),
            (None, None) => false,
            (Some(_), None) | (None, Some(_)) => true,
        }
}

fn graphics_command_compare_work(
    expected: &GraphicsCommand,
    actual: &GraphicsCommand,
) -> Result<u64, SceneError> {
    let payload = match (expected, actual) {
        (
            GraphicsCommand::Stroke {
                style: expected, ..
            },
            GraphicsCommand::Stroke { style: actual, .. },
        )
        | (
            GraphicsCommand::FillStroke {
                style: expected, ..
            },
            GraphicsCommand::FillStroke { style: actual, .. },
        ) => expected
            .dash()
            .array()
            .len()
            .max(actual.dash().array().len()),
        (GraphicsCommand::DrawGlyphRun(expected), GraphicsCommand::DrawGlyphRun(actual)) => {
            expected.glyphs().len().max(actual.glyphs().len())
        }
        _ => 0,
    };
    compare_work_with_payload(payload)
}

fn graphics_resource_compare_work(
    expected: &GraphicsResourceEntry,
    actual: &GraphicsResourceEntry,
) -> Result<u64, SceneError> {
    expected.resource().comparison_work(actual.resource())
}

fn capability_compare_work(
    expected: &CapabilityRequirement,
    actual: &CapabilityRequirement,
) -> Result<u64, SceneError> {
    compare_work_with_payload(
        expected
            .dependencies()
            .len()
            .max(actual.dependencies().len()),
    )
}

fn compare_work_with_payload(payload: usize) -> Result<u64, SceneError> {
    u64::try_from(payload)
        .ok()
        .and_then(|payload| payload.checked_add(1))
        .ok_or_else(|| SceneError::for_code(SceneErrorCode::NumericOverflow, None))
}

fn difference_index(index: usize) -> Result<u32, SceneError> {
    u32::try_from(index).map_err(|_| SceneError::for_code(SceneErrorCode::InternalState, None))
}

fn write_difference(
    writer: &mut CanonicalWriter,
    difference: SceneDifference,
) -> Result<(), SceneError> {
    writer.push(b"{\"field\":")?;
    writer.push(match difference.field() {
        SceneDiffField::Major => b"\"major\"",
        SceneDiffField::Minor => b"\"minor\"",
        SceneDiffField::PageIndex => b"\"page-index\"",
        SceneDiffField::PageObject => b"\"page-object\"",
        SceneDiffField::RevisionStartxref => b"\"revision-startxref\"",
        SceneDiffField::MediaBox => b"\"media-box\"",
        SceneDiffField::CropBox => b"\"crop-box\"",
        SceneDiffField::Rotation => b"\"rotation\"",
        SceneDiffField::Decision => b"\"decision\"",
        SceneDiffField::Entry => b"\"entry\"",
    })?;
    writer.push(b",\"index\":")?;
    if let Some(index) = difference.index() {
        writer.push_u32(index)?;
    } else {
        writer.push(b"null")?;
    }
    writer.push(b",\"kind\":")?;
    writer.push(match difference.kind() {
        SceneDiffKind::Added => b"\"added\"",
        SceneDiffKind::Removed => b"\"removed\"",
        SceneDiffKind::Changed => b"\"changed\"",
    })?;
    writer.push(b",\"section\":")?;
    writer.push(match difference.section() {
        SceneDiffSection::Schema => b"\"schema\"",
        SceneDiffSection::Binding => b"\"binding\"",
        SceneDiffSection::Geometry => b"\"geometry\"",
        SceneDiffSection::Features => b"\"features\"",
        SceneDiffSection::Resources => b"\"resources\"",
        SceneDiffSection::Commands => b"\"commands\"",
        SceneDiffSection::CommandProvenance => b"\"command-provenance\"",
        SceneDiffSection::GraphicsCommands => b"\"graphics-commands\"",
        SceneDiffSection::GraphicsCommandProvenance => b"\"graphics-command-provenance\"",
        SceneDiffSection::GraphicsBounds => b"\"graphics-bounds\"",
        SceneDiffSection::GraphicsResources => b"\"graphics-resources\"",
        SceneDiffSection::GraphicsCapabilities => b"\"graphics-capabilities\"",
    })?;
    writer.push(b"}")
}

#[cfg(test)]
mod tests {
    use super::{
        DifferenceCollector, SceneDiffField, SceneDiffKind, SceneDiffLimits, SceneDiffSection,
        compare_schema_components,
    };

    #[test]
    fn schema_major_and_minor_have_stable_first_positions() {
        let mut collector = DifferenceCollector::new(SceneDiffLimits::default());
        compare_schema_components(1, 0, 2, 3, &mut collector).unwrap();
        assert_eq!(collector.values.len(), 2);
        assert_eq!(collector.compare_work, 2);
        assert_eq!(collector.values[0].section(), SceneDiffSection::Schema);
        assert_eq!(collector.values[0].field(), SceneDiffField::Major);
        assert_eq!(collector.values[0].kind(), SceneDiffKind::Changed);
        assert_eq!(collector.values[1].field(), SceneDiffField::Minor);
    }
}
