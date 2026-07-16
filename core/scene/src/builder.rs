use std::collections::HashMap;
use std::fmt;
use std::mem;

use pdf_rs_syntax::ObjectRef;

use crate::{
    CommandSource, FeatureReport, PageGeometry, ResourceId, Scene, SceneBinding, SceneCommand,
    SceneCommandKind, SceneError, SceneErrorCode, SceneFeature, SceneLimitKind, SceneLimits,
    SceneName, SceneResource, SceneStats,
};

/// Bounded single-owner builder for one immutable Scene.
pub struct SceneBuilder {
    binding: SceneBinding,
    geometry: PageGeometry,
    limits: SceneLimits,
    commands: Vec<SceneCommand>,
    resources: Vec<SceneResource>,
    resource_ids: HashMap<ObjectRef, ResourceId>,
    provenance: Vec<CommandSource>,
    name_retained_bytes: u64,
    has_marked_content: bool,
    has_properties: bool,
    marked_content_depth: u32,
}

impl SceneBuilder {
    /// Creates an empty Scene builder without allocating command or resource capacity.
    pub fn new(binding: SceneBinding, geometry: PageGeometry, limits: SceneLimits) -> Self {
        Self {
            binding,
            geometry,
            limits,
            commands: Vec::new(),
            resources: Vec::new(),
            resource_ids: HashMap::new(),
            provenance: Vec::new(),
            name_retained_bytes: 0,
            has_marked_content: false,
            has_properties: false,
            marked_content_depth: 0,
        }
    }

    /// Appends one semantic marked-content begin command.
    ///
    /// A properties object is interned at first command use, so stable resource identifiers are
    /// independent of allocator state and unrelated construction history.
    pub fn begin_marked_content(
        &mut self,
        tag: &[u8],
        properties: Option<ObjectRef>,
        source: CommandSource,
    ) -> Result<(), SceneError> {
        let command_index = self.next_command_index()?;
        let next_depth = self.marked_content_depth.checked_add(1).ok_or_else(|| {
            SceneError::for_code(SceneErrorCode::NumericOverflow, Some(command_index))
        })?;
        if next_depth > self.limits.max_marked_content_depth() {
            return Err(SceneError::resource(
                SceneLimitKind::MarkedContentDepth,
                u64::from(self.limits.max_marked_content_depth()),
                u64::from(self.marked_content_depth),
                1,
                Some(command_index),
            ));
        }
        let existing_resource =
            properties.and_then(|object| self.resource_ids.get(&object).copied());
        let (resource_id, pending_resource) = match (properties, existing_resource) {
            (_, Some(id)) => (Some(id), None),
            (Some(object), None) => {
                let resource_index = u32::try_from(self.resources.len()).map_err(|_| {
                    SceneError::for_code(SceneErrorCode::InternalState, Some(command_index))
                })?;
                if resource_index >= self.limits.max_resources() {
                    return Err(SceneError::resource(
                        SceneLimitKind::Resources,
                        u64::from(self.limits.max_resources()),
                        u64::from(resource_index),
                        1,
                        Some(command_index),
                    ));
                }
                let id = ResourceId::new(resource_index);
                (
                    Some(id),
                    Some(SceneResource::marked_content_properties(id, object)),
                )
            }
            (None, None) => (None, None),
        };
        let will_have_properties = self.has_properties || resource_id.is_some();
        self.preflight_append(
            tag.len(),
            pending_resource.is_some(),
            true,
            will_have_properties,
            command_index,
        )?;
        let tag = SceneName::copy_from(tag, self.limits.max_name_bytes(), Some(command_index))?;
        let pending_name_bytes = tag.retained_bytes()?;

        self.reserve_command_pair(command_index)?;
        if pending_resource.is_some() {
            reserve_one_geometric(
                &mut self.resources,
                self.limits.max_resources(),
                command_index,
            )?;
        }
        self.ensure_actual_retained_after_append(
            pending_name_bytes,
            true,
            will_have_properties,
            command_index,
        )?;
        let next_name_retained_bytes = self
            .name_retained_bytes
            .checked_add(pending_name_bytes)
            .ok_or_else(|| {
                SceneError::for_code(SceneErrorCode::InternalState, Some(command_index))
            })?;

        if let Some(resource) = pending_resource {
            self.resource_ids.try_reserve(1).map_err(|_| {
                SceneError::resource(
                    SceneLimitKind::Allocation,
                    u64::from(self.limits.max_resources()),
                    u64::try_from(self.resource_ids.len()).unwrap_or(u64::MAX),
                    1,
                    Some(command_index),
                )
            })?;
            if self
                .resource_ids
                .insert(resource.object(), resource.id())
                .is_some()
            {
                return Err(SceneError::for_code(
                    SceneErrorCode::InternalState,
                    Some(command_index),
                ));
            }
            self.resources.push(resource);
        }
        self.commands.push(SceneCommand::begin(tag, resource_id));
        self.provenance.push(source);
        self.name_retained_bytes = next_name_retained_bytes;
        self.has_marked_content = true;
        self.has_properties = will_have_properties;
        self.marked_content_depth = next_depth;
        Ok(())
    }

    /// Appends one semantic marked-content end command.
    pub fn end_marked_content(&mut self, source: CommandSource) -> Result<(), SceneError> {
        let command_index = self.next_command_index()?;
        if self.marked_content_depth == 0 {
            return Err(SceneError::for_code(
                SceneErrorCode::InvalidCommandSequence,
                Some(command_index),
            ));
        }
        self.preflight_append(
            0,
            false,
            self.has_marked_content,
            self.has_properties,
            command_index,
        )?;
        self.reserve_command_pair(command_index)?;
        self.ensure_actual_retained_after_append(
            0,
            self.has_marked_content,
            self.has_properties,
            command_index,
        )?;
        self.commands.push(SceneCommand::end());
        self.provenance.push(source);
        self.marked_content_depth -= 1;
        Ok(())
    }

    /// Validates all terminal invariants and publishes the immutable Scene atomically.
    pub fn finish(self) -> Result<Scene, SceneError> {
        let Self {
            binding,
            geometry,
            limits,
            commands,
            resources,
            provenance,
            ..
        } = self;
        finish_scene(binding, geometry, commands, resources, provenance, limits)
    }

    fn next_command_index(&self) -> Result<u32, SceneError> {
        let command_index = u32::try_from(self.commands.len())
            .map_err(|_| SceneError::for_code(SceneErrorCode::InternalState, None))?;
        if command_index >= self.limits.max_commands() {
            return Err(SceneError::resource(
                SceneLimitKind::Commands,
                u64::from(self.limits.max_commands()),
                u64::from(command_index),
                1,
                Some(command_index),
            ));
        }
        Ok(command_index)
    }

    fn reserve_command_pair(&mut self, command_index: u32) -> Result<(), SceneError> {
        reserve_one_geometric(
            &mut self.commands,
            self.limits.max_commands(),
            command_index,
        )?;
        reserve_one_geometric(
            &mut self.provenance,
            self.limits.max_commands(),
            command_index,
        )
    }

    fn preflight_append(
        &self,
        pending_name_bytes: usize,
        adds_resource: bool,
        has_marked_content: bool,
        has_properties: bool,
        command_index: u32,
    ) -> Result<(), SceneError> {
        let command_capacity = capacity_after_one(
            self.commands.len(),
            self.commands.capacity(),
            self.limits.max_commands(),
        )?;
        let provenance_capacity = capacity_after_one(
            self.provenance.len(),
            self.provenance.capacity(),
            self.limits.max_commands(),
        )?;
        let resource_capacity = if adds_resource {
            capacity_after_one(
                self.resources.len(),
                self.resources.capacity(),
                self.limits.max_resources(),
            )?
        } else {
            self.resources.capacity()
        };
        let pending_name_bytes = u64::try_from(pending_name_bytes).map_err(|_| {
            SceneError::for_code(SceneErrorCode::InternalState, Some(command_index))
        })?;
        let name_retained_bytes = self
            .name_retained_bytes
            .checked_add(pending_name_bytes)
            .ok_or_else(|| {
                SceneError::for_code(SceneErrorCode::InternalState, Some(command_index))
            })?;
        let prospective = retained_bytes_for_capacities(
            command_capacity,
            resource_capacity,
            provenance_capacity,
            name_retained_bytes,
            has_marked_content,
            has_properties,
        )?;
        self.ensure_retained_budget(prospective, command_index)
    }

    fn ensure_actual_retained_after_append(
        &self,
        pending_name_bytes: u64,
        has_marked_content: bool,
        has_properties: bool,
        command_index: u32,
    ) -> Result<(), SceneError> {
        let name_retained_bytes = self
            .name_retained_bytes
            .checked_add(pending_name_bytes)
            .ok_or_else(|| {
                SceneError::for_code(SceneErrorCode::InternalState, Some(command_index))
            })?;
        let prospective = retained_bytes_for_capacities(
            self.commands.capacity(),
            self.resources.capacity(),
            self.provenance.capacity(),
            name_retained_bytes,
            has_marked_content,
            has_properties,
        )?;
        self.ensure_retained_budget(prospective, command_index)
    }

    fn ensure_retained_budget(
        &self,
        prospective: u64,
        command_index: u32,
    ) -> Result<(), SceneError> {
        if prospective <= self.limits.max_retained_bytes() {
            return Ok(());
        }
        let consumed = retained_bytes_for_capacities(
            self.commands.capacity(),
            self.resources.capacity(),
            self.provenance.capacity(),
            self.name_retained_bytes,
            self.has_marked_content,
            self.has_properties,
        )?;
        Err(SceneError::resource(
            SceneLimitKind::RetainedBytes,
            self.limits.max_retained_bytes(),
            consumed,
            prospective.saturating_sub(consumed),
            Some(command_index),
        ))
    }
}

fn capacity_after_one(len: usize, capacity: usize, max_items: u32) -> Result<usize, SceneError> {
    if len < capacity {
        return Ok(capacity);
    }
    let max_items = usize::try_from(max_items)
        .map_err(|_| SceneError::for_code(SceneErrorCode::InternalState, None))?;
    let remaining = max_items
        .checked_sub(len)
        .ok_or_else(|| SceneError::for_code(SceneErrorCode::InternalState, None))?;
    let growth = if capacity == 0 {
        1
    } else {
        capacity.min(remaining)
    };
    capacity
        .checked_add(growth)
        .ok_or_else(|| SceneError::for_code(SceneErrorCode::InternalState, None))
}

fn reserve_one_geometric<T>(
    values: &mut Vec<T>,
    max_items: u32,
    command_index: u32,
) -> Result<(), SceneError> {
    let target = capacity_after_one(values.len(), values.capacity(), max_items)?;
    if target <= values.capacity() {
        return Ok(());
    }
    let additional = target
        .checked_sub(values.len())
        .ok_or_else(|| SceneError::for_code(SceneErrorCode::InternalState, Some(command_index)))?;
    values.try_reserve_exact(additional).map_err(|_| {
        SceneError::resource(
            SceneLimitKind::Allocation,
            u64::from(max_items),
            u64::try_from(values.len()).unwrap_or(u64::MAX),
            u64::try_from(additional).unwrap_or(u64::MAX),
            Some(command_index),
        )
    })
}

fn retained_bytes_for_capacities(
    command_capacity: usize,
    resource_capacity: usize,
    provenance_capacity: usize,
    name_retained_bytes: u64,
    has_marked_content: bool,
    has_properties: bool,
) -> Result<u64, SceneError> {
    let feature_count = usize::from(has_marked_content) + usize::from(has_properties);
    capacity_bytes::<SceneCommand>(command_capacity)?
        .checked_add(capacity_bytes::<SceneResource>(resource_capacity)?)
        .and_then(|value| {
            value.checked_add(capacity_bytes::<CommandSource>(provenance_capacity).ok()?)
        })
        .and_then(|value| value.checked_add(name_retained_bytes))
        .and_then(|value| value.checked_add(capacity_bytes::<SceneFeature>(feature_count).ok()?))
        .ok_or_else(|| SceneError::for_code(SceneErrorCode::InternalState, None))
}

impl fmt::Debug for SceneBuilder {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SceneBuilder")
            .field("page_index", &self.binding.page_index())
            .field("command_count", &self.commands.len())
            .field("resource_count", &self.resources.len())
            .field("marked_content_depth", &self.marked_content_depth)
            .field("limits", &self.limits)
            .field("content", &"[REDACTED]")
            .finish()
    }
}

fn finish_scene(
    binding: SceneBinding,
    geometry: PageGeometry,
    commands: Vec<SceneCommand>,
    resources: Vec<SceneResource>,
    provenance: Vec<CommandSource>,
    limits: SceneLimits,
) -> Result<Scene, SceneError> {
    if commands.len() != provenance.len() {
        let mismatch = commands.len().min(provenance.len());
        return Err(SceneError::for_code(
            SceneErrorCode::InvalidProvenance,
            u32::try_from(mismatch).ok(),
        ));
    }
    let command_count = u32::try_from(commands.len())
        .map_err(|_| SceneError::for_code(SceneErrorCode::InternalState, None))?;
    if command_count > limits.max_commands() {
        return Err(SceneError::resource(
            SceneLimitKind::Commands,
            u64::from(limits.max_commands()),
            0,
            u64::from(command_count),
            None,
        ));
    }
    let resource_count = u32::try_from(resources.len())
        .map_err(|_| SceneError::for_code(SceneErrorCode::InternalState, None))?;
    if resource_count > limits.max_resources() {
        return Err(SceneError::resource(
            SceneLimitKind::Resources,
            u64::from(limits.max_resources()),
            0,
            u64::from(resource_count),
            None,
        ));
    }
    for (index, resource) in resources.iter().enumerate() {
        if usize::try_from(resource.id().value()).ok() != Some(index) {
            return Err(SceneError::for_code(SceneErrorCode::InternalState, None));
        }
    }

    let mut depth = 0_u32;
    let mut max_depth = 0_u32;
    let mut has_marked_content = false;
    let mut has_properties = false;
    for (index, command) in commands.iter().enumerate() {
        let command_index = u32::try_from(index).ok();
        match command.kind() {
            SceneCommandKind::BeginMarkedContent => {
                has_marked_content = true;
                let Some(tag) = command.tag() else {
                    return Err(SceneError::for_code(
                        SceneErrorCode::InternalState,
                        command_index,
                    ));
                };
                if u64::try_from(tag.bytes().len()).unwrap_or(u64::MAX)
                    > u64::from(limits.max_name_bytes())
                {
                    return Err(SceneError::resource(
                        SceneLimitKind::NameBytes,
                        u64::from(limits.max_name_bytes()),
                        0,
                        u64::try_from(tag.bytes().len()).unwrap_or(u64::MAX),
                        command_index,
                    ));
                }
                if let Some(resource) = command.properties() {
                    has_properties = true;
                    if resource.value() >= resource_count {
                        return Err(SceneError::for_code(
                            SceneErrorCode::InvalidCommandSequence,
                            command_index,
                        ));
                    }
                }
                depth = depth.checked_add(1).ok_or_else(|| {
                    SceneError::for_code(SceneErrorCode::NumericOverflow, command_index)
                })?;
                if depth > limits.max_marked_content_depth() {
                    return Err(SceneError::resource(
                        SceneLimitKind::MarkedContentDepth,
                        u64::from(limits.max_marked_content_depth()),
                        u64::from(depth - 1),
                        1,
                        command_index,
                    ));
                }
                max_depth = max_depth.max(depth);
            }
            SceneCommandKind::EndMarkedContent => {
                if command.tag().is_some() || command.properties().is_some() || depth == 0 {
                    return Err(SceneError::for_code(
                        SceneErrorCode::InvalidCommandSequence,
                        command_index,
                    ));
                }
                depth -= 1;
            }
        }
    }
    if depth != 0 {
        return Err(SceneError::for_code(
            SceneErrorCode::InvalidCommandSequence,
            command_count.checked_sub(1),
        ));
    }

    let mut tags = Vec::new();
    let tag_count = usize::from(has_marked_content) + usize::from(has_properties);
    tags.try_reserve_exact(tag_count).map_err(|_| {
        SceneError::resource(
            SceneLimitKind::Allocation,
            2,
            0,
            u64::try_from(tag_count).unwrap_or(u64::MAX),
            None,
        )
    })?;
    if has_marked_content {
        tags.push(SceneFeature::MarkedContent);
    }
    if has_properties {
        tags.push(SceneFeature::MarkedContentProperties);
    }
    let features = FeatureReport::supported(tags);
    let retained_bytes = retained_scene_bytes(&commands, &resources, &provenance, &features)?;
    if retained_bytes > limits.max_retained_bytes() {
        return Err(SceneError::resource(
            SceneLimitKind::RetainedBytes,
            limits.max_retained_bytes(),
            0,
            retained_bytes,
            None,
        ));
    }
    let stats = SceneStats::new(command_count, resource_count, max_depth, retained_bytes);
    Ok(Scene::new(
        binding, geometry, commands, resources, features, provenance, limits, stats,
    ))
}

fn retained_scene_bytes(
    commands: &Vec<SceneCommand>,
    resources: &Vec<SceneResource>,
    provenance: &Vec<CommandSource>,
    features: &FeatureReport,
) -> Result<u64, SceneError> {
    let mut retained = capacity_bytes::<SceneCommand>(commands.capacity())?
        .checked_add(capacity_bytes::<SceneResource>(resources.capacity())?)
        .and_then(|value| {
            value.checked_add(capacity_bytes::<CommandSource>(provenance.capacity()).ok()?)
        })
        .and_then(|value| value.checked_add(features.retained_bytes().ok()?))
        .ok_or_else(|| SceneError::for_code(SceneErrorCode::InternalState, None))?;
    for command in commands {
        if let Some(tag) = command.tag() {
            retained = retained
                .checked_add(tag.retained_bytes()?)
                .ok_or_else(|| SceneError::for_code(SceneErrorCode::InternalState, None))?;
        }
    }
    Ok(retained)
}

fn capacity_bytes<T>(capacity: usize) -> Result<u64, SceneError> {
    u64::try_from(capacity)
        .ok()
        .and_then(|count| {
            u64::try_from(mem::size_of::<T>())
                .ok()
                .and_then(|width| count.checked_mul(width))
        })
        .ok_or_else(|| SceneError::for_code(SceneErrorCode::InternalState, None))
}

#[cfg(test)]
mod tests {
    use pdf_rs_bytes::{SourceIdentity, SourceRevision, SourceStableId};
    use pdf_rs_syntax::ObjectRef;

    use super::finish_scene;
    use crate::{
        CommandSource, PageGeometry, PageRotation, SceneBinding, SceneCommand, SceneErrorCode,
        SceneLimits, SceneRect, SceneScalar,
    };

    fn binding() -> SceneBinding {
        SceneBinding::new(
            SourceIdentity::new(SourceStableId::new([1; 32]), SourceRevision::new(1)),
            10,
            0,
            ObjectRef::new(3, 0).unwrap(),
        )
    }

    fn geometry() -> PageGeometry {
        let rect = SceneRect::new([
            SceneScalar::ZERO,
            SceneScalar::ZERO,
            SceneScalar::from_decimal("100").unwrap(),
            SceneScalar::from_decimal("100").unwrap(),
        ])
        .unwrap();
        PageGeometry::new(rect, rect, PageRotation::Degrees0)
    }

    #[test]
    fn final_publication_rejects_unpaired_provenance() {
        let command = SceneCommand::end();
        let source = CommandSource::new(ObjectRef::new(4, 0).unwrap(), 0, 0, 1, 0).unwrap();
        let error = finish_scene(
            binding(),
            geometry(),
            vec![command],
            Vec::new(),
            vec![source, source],
            SceneLimits::default(),
        )
        .unwrap_err();
        assert_eq!(error.code(), SceneErrorCode::InvalidProvenance);
    }
}
