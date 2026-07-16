use std::mem::size_of;

use pdf_rs_bytes::{ByteSource, DataTicket, ResumeCheckpoint, SmallRanges, SourceSnapshot};
use pdf_rs_document::{
    AcquireImageXObjectJob, DocumentCancellation, ImageXObjectColorSpace, ImageXObjectPoll,
    ImageXObjectUnsupported, PageXObjectReference,
};
use pdf_rs_scene::{GraphicsResourceSource, ImageColorSpace, ImageResource};
use pdf_rs_syntax::ObjectRef;

use super::ContentImageProfile;
use crate::{
    ContentImageLimit, ContentImageLimitKind, ContentImageStats, ContentOperatorSource,
    ContentVmError, ContentVmErrorCode, ContentVmFailure,
};

const CACHE_CANCELLATION_INTERVAL: u64 = 256;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct ImageCacheKey {
    snapshot: SourceSnapshot,
    target: ObjectRef,
    revision_startxref: u64,
}

impl ImageCacheKey {
    const fn from_proof(proof: PageXObjectReference) -> Self {
        Self {
            snapshot: proof.snapshot(),
            target: proof.target(),
            revision_startxref: proof.revision_startxref(),
        }
    }
}

struct PlannedImageUse {
    source: ContentOperatorSource,
    proof: PageXObjectReference,
    image_index: u32,
}

struct PlannedImage {
    key: ImageCacheKey,
    proof: PageXObjectReference,
    source: ContentOperatorSource,
    resource: Option<ImageResource>,
}

struct ActiveImage {
    image_index: usize,
    job: AcquireImageXObjectJob,
}

pub(super) enum ImagePlanningPoll {
    Ready,
    Pending {
        ticket: DataTicket,
        missing: SmallRanges,
        checkpoint: ResumeCheckpoint,
    },
    Unsupported {
        unsupported: ImageXObjectUnsupported,
        source: ContentOperatorSource,
    },
    Failed(ContentVmFailure),
}

pub(super) struct ImageRuntime {
    profile: ContentImageProfile,
    uses: Vec<PlannedImageUse>,
    images: Vec<PlannedImage>,
    expected_uses: usize,
    acquisition_cursor: usize,
    execution_cursor: usize,
    external_plan_retained: u64,
    active: Option<ActiveImage>,
    plan_complete: bool,
    stats: ContentImageStats,
}

impl ImageRuntime {
    pub(super) fn new(profile: ContentImageProfile) -> Self {
        Self {
            profile,
            uses: Vec::new(),
            images: Vec::new(),
            expected_uses: 0,
            acquisition_cursor: 0,
            execution_cursor: 0,
            external_plan_retained: 0,
            active: None,
            plan_complete: false,
            stats: ContentImageStats::default(),
        }
    }

    pub(super) const fn stats(&self) -> ContentImageStats {
        self.stats
    }

    pub(super) const fn plan_complete(&self) -> bool {
        self.plan_complete
    }

    pub(super) fn acquisitions_complete(&self) -> bool {
        self.plan_complete && self.active.is_none() && self.acquisition_cursor == self.images.len()
    }

    pub(super) fn record_scan(&mut self, bytes: u64) -> Result<(), ContentVmError> {
        if self.stats.scan_passes() != 0 {
            return Err(ContentVmError::new(ContentVmErrorCode::InternalState, None));
        }
        self.stats
            .record_scan(bytes)
            .ok_or_else(|| ContentVmError::new(ContentVmErrorCode::InternalState, None))
    }

    pub(super) fn admit_planning_operator(
        &mut self,
        source: ContentOperatorSource,
    ) -> Result<(), ContentVmError> {
        self.profile.content_limits().preflight(
            ContentImageLimitKind::PlanningOperators,
            self.stats.planning_operators(),
            1,
            Some(source),
        )?;
        self.stats
            .record_planning_operator()
            .ok_or_else(|| internal(source))
    }

    pub(super) fn begin_plan(
        &mut self,
        expected_uses: usize,
        source: Option<ContentOperatorSource>,
    ) -> Result<(), ContentVmError> {
        if self.plan_complete
            || !self.uses.is_empty()
            || !self.images.is_empty()
            || self.active.is_some()
        {
            return Err(ContentVmError::new(
                ContentVmErrorCode::InternalState,
                source,
            ));
        }
        let uses = u64::try_from(expected_uses)
            .map_err(|_| ContentVmError::new(ContentVmErrorCode::InternalState, source))?;
        let target = uses
            .checked_mul(
                u64::try_from(size_of::<PlannedImageUse>())
                    .map_err(|_| ContentVmError::new(ContentVmErrorCode::InternalState, source))?,
            )
            .ok_or_else(|| ContentVmError::new(ContentVmErrorCode::InternalState, source))?;
        self.profile.content_limits().preflight(
            ContentImageLimitKind::PlanRetainedBytes,
            0,
            target,
            source,
        )?;
        self.uses.try_reserve_exact(expected_uses).map_err(|_| {
            ContentVmError::image_resource(
                ContentImageLimit::new(
                    ContentImageLimitKind::PlanAllocation,
                    self.profile.content_limits().max_plan_retained_bytes(),
                    0,
                    target,
                ),
                source,
            )
        })?;
        self.expected_uses = expected_uses;
        self.preflight_plan_retained(source)?;
        self.record_plan_retained(source)
    }

    pub(super) fn admit_lookup(
        &mut self,
        source: ContentOperatorSource,
    ) -> Result<(), ContentVmError> {
        self.stats.record_lookup().ok_or_else(|| internal(source))
    }

    pub(super) fn admit_planned_use(
        &self,
        consumed: u64,
        source: ContentOperatorSource,
    ) -> Result<(), ContentVmError> {
        self.profile.content_limits().preflight(
            ContentImageLimitKind::ImageUses,
            consumed,
            1,
            Some(source),
        )
    }

    pub(super) fn record_execution_plan_retained(
        &mut self,
        retained: u64,
        source: Option<ContentOperatorSource>,
    ) -> Result<(), ContentVmError> {
        if self.plan_complete || self.external_plan_retained != 0 {
            return Err(ContentVmError::new(
                ContentVmErrorCode::InternalState,
                source,
            ));
        }
        self.profile.content_limits().preflight(
            ContentImageLimitKind::PlanRetainedBytes,
            0,
            retained,
            source,
        )?;
        self.external_plan_retained = retained;
        self.stats.record_plan_retained(retained);
        Ok(())
    }

    #[allow(
        clippy::result_large_err,
        reason = "planning preserves the complete typed VM failure across source guards"
    )]
    pub(super) fn register_proof(
        &mut self,
        proof: PageXObjectReference,
        source: ContentOperatorSource,
        byte_source: &dyn ByteSource,
        cancellation: &dyn DocumentCancellation,
    ) -> Result<(), ContentVmFailure> {
        if self.plan_complete || self.uses.len() >= self.expected_uses {
            return Err(ContentVmFailure::Vm(internal(source)));
        }
        let key = ImageCacheKey::from_proof(proof);
        let mut image_index = None;
        for (index, image) in self.images.iter().enumerate() {
            runtime_guard(key.snapshot, byte_source, cancellation, source)?;
            self.profile
                .content_limits()
                .preflight(
                    ContentImageLimitKind::CacheProbes,
                    self.stats.cache_probes(),
                    1,
                    Some(source),
                )
                .map_err(ContentVmFailure::Vm)?;
            self.stats
                .record_cache_probe()
                .ok_or_else(|| ContentVmFailure::Vm(internal(source)))?;
            if self
                .stats
                .cache_probes()
                .is_multiple_of(CACHE_CANCELLATION_INTERVAL)
            {
                runtime_guard(key.snapshot, byte_source, cancellation, source)?;
            }
            if image.key == key {
                image_index = Some(index);
                self.stats
                    .record_cache_hit()
                    .ok_or_else(|| ContentVmFailure::Vm(internal(source)))?;
                break;
            }
        }
        let index = match image_index {
            Some(index) => index,
            None => self
                .push_unique_image(key, proof, source)
                .map_err(ContentVmFailure::Vm)?,
        };
        let image_index =
            u32::try_from(index).map_err(|_| ContentVmFailure::Vm(internal(source)))?;
        self.uses.push(PlannedImageUse {
            source,
            proof,
            image_index,
        });
        self.record_plan_retained(Some(source))
            .map_err(ContentVmFailure::Vm)
    }

    pub(super) fn finish_plan(&mut self) -> Result<(), ContentVmError> {
        if self.plan_complete || self.uses.len() != self.expected_uses || self.active.is_some() {
            return Err(ContentVmError::new(ContentVmErrorCode::InternalState, None));
        }
        self.preflight_plan_retained(None)?;
        self.record_plan_retained(None)?;
        self.plan_complete = true;
        Ok(())
    }

    pub(super) fn poll_acquisitions(
        &mut self,
        byte_source: &dyn ByteSource,
        cancellation: &dyn DocumentCancellation,
    ) -> ImagePlanningPoll {
        if !self.plan_complete || self.acquisition_cursor > self.images.len() {
            return ImagePlanningPoll::Failed(ContentVmFailure::Vm(ContentVmError::new(
                ContentVmErrorCode::InternalState,
                None,
            )));
        }
        while self.acquisition_cursor < self.images.len() {
            let source = self.images[self.acquisition_cursor].source;
            let snapshot = self.images[self.acquisition_cursor].key.snapshot;
            if let Err(failure) = runtime_guard(snapshot, byte_source, cancellation, source) {
                return ImagePlanningPoll::Failed(failure);
            }
            if self.images[self.acquisition_cursor].resource.is_some() {
                return ImagePlanningPoll::Failed(ContentVmFailure::Vm(internal(source)));
            }
            if self
                .active
                .as_ref()
                .is_some_and(|active| active.image_index != self.acquisition_cursor)
            {
                return ImagePlanningPoll::Failed(ContentVmFailure::Vm(internal(source)));
            }
            if self.active.is_none() {
                let proof = self.images[self.acquisition_cursor].proof;
                let job = match self.profile.authority().acquire_image_xobject(
                    proof,
                    self.profile.context(),
                    self.profile.acquisition_limits(),
                ) {
                    Ok(job) => job,
                    Err(error) => {
                        return ImagePlanningPoll::Failed(ContentVmFailure::Document(error));
                    }
                };
                self.active = Some(ActiveImage {
                    image_index: self.acquisition_cursor,
                    job,
                });
            }
            if let Err(error) = self.profile.content_limits().preflight(
                ContentImageLimitKind::AcquisitionPolls,
                self.stats.acquisition_polls(),
                1,
                Some(source),
            ) {
                return ImagePlanningPoll::Failed(ContentVmFailure::Vm(error));
            }
            if self.stats.record_acquisition_poll().is_none() {
                return ImagePlanningPoll::Failed(ContentVmFailure::Vm(internal(source)));
            }
            let Some(active) = self.active.as_mut() else {
                return ImagePlanningPoll::Failed(ContentVmFailure::Vm(internal(source)));
            };
            let outcome = active.job.poll(byte_source, cancellation);
            if let Err(failure) = runtime_guard(snapshot, byte_source, cancellation, source) {
                return ImagePlanningPoll::Failed(failure);
            }
            match outcome {
                ImageXObjectPoll::Pending {
                    ticket,
                    missing,
                    checkpoint,
                } => {
                    return ImagePlanningPoll::Pending {
                        ticket,
                        missing,
                        checkpoint,
                    };
                }
                ImageXObjectPoll::Unsupported(unsupported) => {
                    self.active.take();
                    return ImagePlanningPoll::Unsupported {
                        unsupported,
                        source,
                    };
                }
                ImageXObjectPoll::Failed(error) => {
                    self.active.take();
                    return ImagePlanningPoll::Failed(ContentVmFailure::Document(error));
                }
                ImageXObjectPoll::Ready(acquired) => {
                    self.active.take();
                    if ImageCacheKey::from_proof(acquired.proof())
                        != self.images[self.acquisition_cursor].key
                    {
                        return ImagePlanningPoll::Failed(ContentVmFailure::Vm(internal(source)));
                    }
                    if let Err(error) = self.install(self.acquisition_cursor, &acquired, source) {
                        return ImagePlanningPoll::Failed(error);
                    }
                    self.acquisition_cursor += 1;
                }
            }
        }
        ImagePlanningPoll::Ready
    }

    pub(super) fn begin_execution(&mut self) -> Result<(), ContentVmError> {
        if !self.acquisitions_complete()
            || self.execution_cursor != 0
            || self.stats.execution_passes() != 0
        {
            return Err(ContentVmError::new(ContentVmErrorCode::InternalState, None));
        }
        self.stats
            .record_execution_pass()
            .ok_or_else(|| ContentVmError::new(ContentVmErrorCode::InternalState, None))
    }

    pub(super) fn resolve_planned(
        &self,
        source: ContentOperatorSource,
    ) -> Result<(PageXObjectReference, ImageResource), ContentVmError> {
        let usage = self
            .uses
            .get(self.execution_cursor)
            .ok_or_else(|| internal(source))?;
        if usage.source != source {
            return Err(internal(source));
        }
        let image = self
            .images
            .get(usize::try_from(usage.image_index).map_err(|_| internal(source))?)
            .and_then(|image| image.resource.as_ref())
            .ok_or_else(|| internal(source))?
            .clone();
        let proof = usage.proof;
        Ok((proof, image))
    }

    pub(super) fn record_executed_use(
        &mut self,
        source: ContentOperatorSource,
    ) -> Result<(), ContentVmError> {
        if self
            .uses
            .get(self.execution_cursor)
            .is_none_or(|usage| usage.source != source)
        {
            return Err(internal(source));
        }
        self.execution_cursor = self
            .execution_cursor
            .checked_add(1)
            .ok_or_else(|| internal(source))?;
        let image_uses = u64::try_from(self.execution_cursor).map_err(|_| internal(source))?;
        self.stats.set_image_uses(image_uses);
        Ok(())
    }

    pub(super) fn finish_execution(&self) -> Result<(), ContentVmError> {
        if self.execution_cursor != self.uses.len() {
            return Err(ContentVmError::new(ContentVmErrorCode::InternalState, None));
        }
        Ok(())
    }

    fn push_unique_image(
        &mut self,
        key: ImageCacheKey,
        proof: PageXObjectReference,
        source: ContentOperatorSource,
    ) -> Result<usize, ContentVmError> {
        let unique_images = u64::try_from(self.images.len()).map_err(|_| internal(source))?;
        self.profile.content_limits().preflight(
            ContentImageLimitKind::UniqueImages,
            unique_images,
            1,
            Some(source),
        )?;
        self.reserve_image_slot(source)?;
        let index = self.images.len();
        self.images.push(PlannedImage {
            key,
            proof,
            source,
            resource: None,
        });
        Ok(index)
    }

    fn reserve_image_slot(&mut self, source: ContentOperatorSource) -> Result<(), ContentVmError> {
        if self.images.len() == self.images.capacity() {
            let current = cache_retained_bytes(&self.images).ok_or_else(|| internal(source))?;
            let desired = self
                .images
                .len()
                .checked_add(1)
                .ok_or_else(|| internal(source))?;
            let target = u64::try_from(desired)
                .ok()
                .and_then(|value| value.checked_mul(u64::try_from(size_of::<PlannedImage>()).ok()?))
                .ok_or_else(|| internal(source))?;
            let attempted = target
                .checked_sub(current)
                .ok_or_else(|| internal(source))?;
            self.profile.content_limits().preflight(
                ContentImageLimitKind::CacheRetainedBytes,
                current,
                attempted,
                Some(source),
            )?;
            self.images.try_reserve_exact(1).map_err(|_| {
                ContentVmError::image_resource(
                    ContentImageLimit::new(
                        ContentImageLimitKind::CacheAllocation,
                        self.profile.content_limits().max_cache_retained_bytes(),
                        current,
                        attempted,
                    ),
                    Some(source),
                )
            })?;
        }
        let actual = cache_retained_bytes(&self.images).ok_or_else(|| internal(source))?;
        self.profile.content_limits().preflight(
            ContentImageLimitKind::CacheRetainedBytes,
            0,
            actual,
            Some(source),
        )?;
        self.stats.record_cache_retained(actual);
        Ok(())
    }

    #[allow(
        clippy::result_large_err,
        reason = "image integration preserves complete copyable Document, Scene, and VM failures"
    )]
    fn install(
        &mut self,
        image_index: usize,
        acquired: &pdf_rs_document::AcquiredImageXObject,
        source: ContentOperatorSource,
    ) -> Result<(), ContentVmFailure> {
        let decoded_len =
            u64::try_from(acquired.decoded_bytes().len()).map_err(|_| vm_failure(source))?;
        if acquired.stats().decoded_bytes() != decoded_len {
            return Err(vm_failure(source));
        }
        self.profile
            .content_limits()
            .preflight(
                ContentImageLimitKind::DecodedBytes,
                self.stats.decoded_bytes(),
                decoded_len,
                Some(source),
            )
            .map_err(ContentVmFailure::Vm)?;
        let mut decoded = Vec::new();
        decoded
            .try_reserve_exact(acquired.decoded_bytes().len())
            .map_err(|_| {
                ContentVmFailure::Vm(ContentVmError::image_resource(
                    ContentImageLimit::new(
                        ContentImageLimitKind::DecodedAllocation,
                        self.profile.content_limits().max_decoded_bytes(),
                        self.stats.decoded_bytes(),
                        decoded_len,
                    ),
                    Some(source),
                ))
            })?;
        decoded.extend_from_slice(acquired.decoded_bytes());
        let resource_source = GraphicsResourceSource::new(
            acquired.reference(),
            acquired.proof().revision_startxref(),
            acquired.decode_context(),
        );
        let resource = ImageResource::new(
            resource_source,
            acquired.width(),
            acquired.height(),
            scene_color_space(acquired.color_space()),
            acquired.bits_per_component(),
            acquired.interpolate(),
            decoded,
        )
        .map_err(ContentVmFailure::Scene)?;
        let entry = self
            .images
            .get_mut(image_index)
            .ok_or_else(|| vm_failure(source))?;
        if entry.resource.replace(resource).is_some() {
            return Err(vm_failure(source));
        }
        let retained = cache_retained_bytes(&self.images).ok_or_else(|| vm_failure(source))?;
        self.stats
            .record_acquisition(decoded_len, retained, acquired.stats())
            .ok_or_else(|| vm_failure(source))
    }

    fn preflight_plan_retained(
        &self,
        source: Option<ContentOperatorSource>,
    ) -> Result<(), ContentVmError> {
        let retained = plan_retained_bytes(&self.uses)
            .ok_or_else(|| ContentVmError::new(ContentVmErrorCode::InternalState, source))?;
        let retained = self
            .external_plan_retained
            .checked_add(retained)
            .ok_or_else(|| ContentVmError::new(ContentVmErrorCode::InternalState, source))?;
        self.profile.content_limits().preflight(
            ContentImageLimitKind::PlanRetainedBytes,
            0,
            retained,
            source,
        )
    }

    fn record_plan_retained(
        &mut self,
        source: Option<ContentOperatorSource>,
    ) -> Result<(), ContentVmError> {
        let retained = plan_retained_bytes(&self.uses)
            .ok_or_else(|| ContentVmError::new(ContentVmErrorCode::InternalState, source))?;
        let retained = self
            .external_plan_retained
            .checked_add(retained)
            .ok_or_else(|| ContentVmError::new(ContentVmErrorCode::InternalState, source))?;
        self.stats.record_plan_retained(retained);
        Ok(())
    }
}

#[allow(
    clippy::result_large_err,
    reason = "the image runtime preserves complete copyable VM failures at guard boundaries"
)]
fn runtime_guard(
    snapshot: SourceSnapshot,
    source: &dyn ByteSource,
    cancellation: &dyn DocumentCancellation,
    operator: ContentOperatorSource,
) -> Result<(), ContentVmFailure> {
    if source.snapshot() != snapshot {
        return Err(ContentVmFailure::Vm(ContentVmError::new(
            ContentVmErrorCode::SourceSnapshotMismatch,
            Some(operator),
        )));
    }
    let cancelled = cancellation.is_cancelled();
    if source.snapshot() != snapshot {
        return Err(ContentVmFailure::Vm(ContentVmError::new(
            ContentVmErrorCode::SourceSnapshotMismatch,
            Some(operator),
        )));
    }
    if cancelled {
        return Err(ContentVmFailure::Vm(ContentVmError::new(
            ContentVmErrorCode::Cancelled,
            Some(operator),
        )));
    }
    Ok(())
}

fn scene_color_space(value: ImageXObjectColorSpace) -> ImageColorSpace {
    match value {
        ImageXObjectColorSpace::DeviceGray => ImageColorSpace::DeviceGray,
        ImageXObjectColorSpace::DeviceRgb => ImageColorSpace::DeviceRgb,
        ImageXObjectColorSpace::DeviceCmyk => ImageColorSpace::DeviceCmyk,
    }
}

fn plan_retained_bytes(uses: &Vec<PlannedImageUse>) -> Option<u64> {
    u64::try_from(uses.capacity())
        .ok()?
        .checked_mul(u64::try_from(size_of::<PlannedImageUse>()).ok()?)
}

fn cache_retained_bytes(images: &Vec<PlannedImage>) -> Option<u64> {
    u64::try_from(images.capacity())
        .ok()?
        .checked_mul(u64::try_from(size_of::<PlannedImage>()).ok()?)
}

fn internal(source: ContentOperatorSource) -> ContentVmError {
    ContentVmError::new(ContentVmErrorCode::InternalState, Some(source))
}

fn vm_failure(source: ContentOperatorSource) -> ContentVmFailure {
    ContentVmFailure::Vm(internal(source))
}
