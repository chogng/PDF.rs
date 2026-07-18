use std::mem::size_of;
use std::sync::Arc;

use pdf_rs_bytes::{ByteSource, DataTicket, ResumeCheckpoint, SmallRanges, SourceSnapshot};
use pdf_rs_document::{
    AcquireFormXObjectJob, AcquireImageXObjectJob, AcquiredFormXObject, DocumentCancellation,
    FormXObjectPoll, ImageXObjectColorSpace, ImageXObjectPoll, ImageXObjectUnsupportedKind,
    PageXObjectReference,
};
use pdf_rs_scene::{
    GraphicsResourceSource, ImageColorSpace, ImageResource, Matrix, PageGeometry, SceneBinding,
};
use pdf_rs_syntax::ObjectRef;

use super::{
    ContentFormPoll, ContentFormProfile, ContentImageProfile, InterpretFormJob, InterpretedForm,
};
use crate::{
    ContentImageLimit, ContentImageLimitKind, ContentImageStats, ContentOperatorSource,
    ContentUnsupported, ContentVmError, ContentVmErrorCode, ContentVmFailure,
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
    transform: Matrix,
    form: Option<Arc<InterpretedForm>>,
}

enum PlannedXObject {
    Image(ImageResource),
    Form(Arc<AcquiredFormXObject>),
}

struct PlannedImage {
    key: ImageCacheKey,
    proof: PageXObjectReference,
    source: ContentOperatorSource,
    resource: Option<PlannedXObject>,
}

struct ActiveImage {
    image_index: usize,
    job: AcquireImageXObjectJob,
}

struct ActiveForm {
    image_index: usize,
    job: AcquireFormXObjectJob,
}

struct ActiveFormInterpretation {
    use_index: usize,
    jobs: Vec<InterpretFormJob>,
}

pub(super) enum ImagePlanningPoll {
    Ready,
    Pending {
        ticket: DataTicket,
        missing: SmallRanges,
        checkpoint: ResumeCheckpoint,
    },
    Unsupported(ContentUnsupported),
    Failed(ContentVmFailure),
}

pub(super) enum ResolvedXObject {
    Image {
        proof: PageXObjectReference,
        image: ImageResource,
    },
    Form {
        proof: PageXObjectReference,
        form: Arc<InterpretedForm>,
    },
}

pub(super) struct ImageRuntime {
    profile: ContentImageProfile,
    form_profile: Option<ContentFormProfile>,
    uses: Vec<PlannedImageUse>,
    images: Vec<PlannedImage>,
    expected_uses: usize,
    acquisition_cursor: usize,
    form_use_cursor: usize,
    execution_cursor: usize,
    executed_image_uses: u64,
    external_plan_retained: u64,
    active: Option<ActiveImage>,
    active_form: Option<ActiveForm>,
    active_form_interpretation: Option<ActiveFormInterpretation>,
    plan_complete: bool,
    stats: ContentImageStats,
}

impl ImageRuntime {
    pub(super) fn new(profile: ContentImageProfile) -> Self {
        Self {
            profile,
            form_profile: None,
            uses: Vec::new(),
            images: Vec::new(),
            expected_uses: 0,
            acquisition_cursor: 0,
            form_use_cursor: 0,
            execution_cursor: 0,
            executed_image_uses: 0,
            external_plan_retained: 0,
            active: None,
            active_form: None,
            active_form_interpretation: None,
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
        self.plan_complete
            && self.active.is_none()
            && self.active_form.is_none()
            && self.active_form_interpretation.is_none()
            && self.acquisition_cursor == self.images.len()
            && self.form_use_cursor == self.uses.len()
    }

    pub(super) fn enable_forms(
        &mut self,
        profile: ContentFormProfile,
    ) -> Result<(), ContentVmError> {
        if self.plan_complete
            || !self.uses.is_empty()
            || !self.images.is_empty()
            || self.active.is_some()
            || self.active_form.is_some()
            || self.active_form_interpretation.is_some()
            || self.form_profile.is_some()
        {
            return Err(ContentVmError::new(ContentVmErrorCode::InternalState, None));
        }
        self.form_profile = Some(profile);
        Ok(())
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
            || self.active_form.is_some()
            || self.active_form_interpretation.is_some()
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
        transform: Matrix,
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
            transform,
            form: None,
        });
        self.record_plan_retained(Some(source))
            .map_err(ContentVmFailure::Vm)
    }

    pub(super) fn finish_plan(&mut self) -> Result<(), ContentVmError> {
        if self.plan_complete
            || self.uses.len() != self.expected_uses
            || self.active.is_some()
            || self.active_form.is_some()
            || self.active_form_interpretation.is_some()
        {
            return Err(ContentVmError::new(ContentVmErrorCode::InternalState, None));
        }
        self.preflight_plan_retained(None)?;
        self.record_plan_retained(None)?;
        self.plan_complete = true;
        Ok(())
    }

    pub(super) fn poll_acquisitions(
        &mut self,
        binding: SceneBinding,
        geometry: PageGeometry,
        byte_source: &dyn ByteSource,
        cancellation: &dyn DocumentCancellation,
    ) -> ImagePlanningPoll {
        if !self.plan_complete
            || self.acquisition_cursor > self.images.len()
            || self.form_use_cursor > self.uses.len()
        {
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
                || self
                    .active_form
                    .as_ref()
                    .is_some_and(|active| active.image_index != self.acquisition_cursor)
            {
                return ImagePlanningPoll::Failed(ContentVmFailure::Vm(internal(source)));
            }
            if self.active.is_none() && self.active_form.is_none() {
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
            if self.active.is_some() {
                if let Err(error) = self.admit_acquisition_poll(source) {
                    return ImagePlanningPoll::Failed(ContentVmFailure::Vm(error));
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
                    ImageXObjectPoll::Unsupported(unsupported)
                        if unsupported.kind() == ImageXObjectUnsupportedKind::NonImageXObject
                            && self.form_profile.is_some() =>
                    {
                        self.active.take();
                        let profile = self
                            .form_profile
                            .as_ref()
                            .expect("Form profile was just checked");
                        let proof = self.images[self.acquisition_cursor].proof;
                        let job = match profile
                            .authority
                            .acquire_form_xobject(proof, profile.context)
                        {
                            Ok(job) => job,
                            Err(error) => {
                                return ImagePlanningPoll::Failed(ContentVmFailure::Document(
                                    error,
                                ));
                            }
                        };
                        self.active_form = Some(ActiveForm {
                            image_index: self.acquisition_cursor,
                            job,
                        });
                        continue;
                    }
                    ImageXObjectPoll::Unsupported(unsupported) => {
                        self.active.take();
                        return ImagePlanningPoll::Unsupported(ContentUnsupported::from_image(
                            unsupported,
                            source,
                        ));
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
                            return ImagePlanningPoll::Failed(ContentVmFailure::Vm(internal(
                                source,
                            )));
                        }
                        if let Err(error) = self.install(self.acquisition_cursor, &acquired, source)
                        {
                            return ImagePlanningPoll::Failed(error);
                        }
                        self.acquisition_cursor += 1;
                        continue;
                    }
                }
            }
            if let Err(error) = self.admit_acquisition_poll(source) {
                return ImagePlanningPoll::Failed(ContentVmFailure::Vm(error));
            }
            let outcome = self
                .active_form
                .as_mut()
                .expect("non-Image classification installs one active Form")
                .job
                .poll(byte_source, cancellation);
            if let Err(failure) = runtime_guard(snapshot, byte_source, cancellation, source) {
                return ImagePlanningPoll::Failed(failure);
            }
            match outcome {
                FormXObjectPoll::Pending {
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
                FormXObjectPoll::Unsupported(unsupported) => {
                    self.active_form.take();
                    return ImagePlanningPoll::Unsupported(ContentUnsupported::from_form(
                        unsupported,
                        source,
                    ));
                }
                FormXObjectPoll::Failed(error) => {
                    self.active_form.take();
                    return ImagePlanningPoll::Failed(ContentVmFailure::Document(error));
                }
                FormXObjectPoll::Ready(acquired) => {
                    self.active_form.take();
                    if ImageCacheKey::from_proof(acquired.proof())
                        != self.images[self.acquisition_cursor].key
                    {
                        return ImagePlanningPoll::Failed(ContentVmFailure::Vm(internal(source)));
                    }
                    self.images[self.acquisition_cursor].resource =
                        Some(PlannedXObject::Form(acquired));
                    self.acquisition_cursor += 1;
                }
            }
        }

        while self.form_use_cursor < self.uses.len() {
            let source = self.uses[self.form_use_cursor].source;
            let snapshot = self.uses[self.form_use_cursor].proof.snapshot();
            if let Err(failure) = runtime_guard(snapshot, byte_source, cancellation, source) {
                return ImagePlanningPoll::Failed(failure);
            }
            let image_index = match usize::try_from(self.uses[self.form_use_cursor].image_index) {
                Ok(value) => value,
                Err(_) => return ImagePlanningPoll::Failed(ContentVmFailure::Vm(internal(source))),
            };
            let Some(resource) = self
                .images
                .get(image_index)
                .and_then(|image| image.resource.as_ref())
            else {
                return ImagePlanningPoll::Failed(ContentVmFailure::Vm(internal(source)));
            };
            let PlannedXObject::Form(acquired) = resource else {
                self.form_use_cursor += 1;
                continue;
            };
            if self.uses[self.form_use_cursor].form.is_some() {
                return ImagePlanningPoll::Failed(ContentVmFailure::Vm(internal(source)));
            }
            if self
                .active_form_interpretation
                .as_ref()
                .is_some_and(|active| active.use_index != self.form_use_cursor)
            {
                return ImagePlanningPoll::Failed(ContentVmFailure::Vm(internal(source)));
            }
            if self.active_form_interpretation.is_none() {
                let Some(profile) = self.form_profile.as_ref() else {
                    return ImagePlanningPoll::Failed(ContentVmFailure::Vm(internal(source)));
                };
                let mut job = match InterpretFormJob::new_graphics_v2_with_images_and_fonts(
                    Arc::clone(acquired),
                    binding,
                    geometry,
                    self.uses[self.form_use_cursor].transform,
                    profile.scan_limits,
                    profile.vm_limits,
                    profile.graphics_limits,
                    profile.property_limits,
                    profile.image_profile.clone(),
                    profile.font_profile.clone(),
                    profile.scene_limits,
                ) {
                    Ok(job) => job,
                    Err(error) => {
                        return ImagePlanningPoll::Failed(ContentVmFailure::Scene(error));
                    }
                };
                if let Some(ext_gstate_profile) = profile.ext_gstate_profile.clone() {
                    job = job.with_dynamic_ext_gstates(ext_gstate_profile);
                }
                if let Some(color_space_profile) = profile.color_space_profile.clone() {
                    job = job.with_dynamic_color_spaces(color_space_profile);
                }
                if let Some(child) = profile.child() {
                    job = match job.with_forms(child) {
                        Ok(job) => job,
                        Err(error) => {
                            return ImagePlanningPoll::Failed(ContentVmFailure::Vm(error));
                        }
                    };
                }
                let mut jobs = Vec::new();
                if jobs.try_reserve_exact(1).is_err() {
                    return ImagePlanningPoll::Failed(ContentVmFailure::Vm(
                        ContentVmError::image_resource(
                            ContentImageLimit::new(
                                ContentImageLimitKind::CacheAllocation,
                                self.profile.content_limits().max_cache_retained_bytes(),
                                0,
                                u64::try_from(size_of::<InterpretFormJob>()).unwrap_or(u64::MAX),
                            ),
                            Some(source),
                        ),
                    ));
                }
                jobs.push(job);
                self.active_form_interpretation = Some(ActiveFormInterpretation {
                    use_index: self.form_use_cursor,
                    jobs,
                });
            }
            let outcome = self
                .active_form_interpretation
                .as_mut()
                .expect("Form interpretation was just installed")
                .jobs
                .first_mut()
                .expect("active Form interpretation retains one job")
                .poll(byte_source, cancellation);
            match outcome {
                ContentFormPoll::Pending {
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
                ContentFormPoll::Unsupported(unsupported) => {
                    self.active_form_interpretation.take();
                    return ImagePlanningPoll::Unsupported(unsupported);
                }
                ContentFormPoll::Failed(failure) => {
                    self.active_form_interpretation.take();
                    return ImagePlanningPoll::Failed(failure);
                }
                ContentFormPoll::Ready(form) => {
                    self.active_form_interpretation.take();
                    self.uses[self.form_use_cursor].form = Some(form);
                    self.form_use_cursor += 1;
                }
            }
        }
        ImagePlanningPoll::Ready
    }

    pub(super) fn begin_execution(&mut self) -> Result<(), ContentVmError> {
        if !self.acquisitions_complete()
            || self.execution_cursor != 0
            || self.executed_image_uses != 0
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
    ) -> Result<ResolvedXObject, ContentVmError> {
        let usage = self
            .uses
            .get(self.execution_cursor)
            .ok_or_else(|| internal(source))?;
        if usage.source != source {
            return Err(internal(source));
        }
        let resource = self
            .images
            .get(usize::try_from(usage.image_index).map_err(|_| internal(source))?)
            .and_then(|image| image.resource.as_ref())
            .ok_or_else(|| internal(source))?;
        let proof = usage.proof;
        match resource {
            PlannedXObject::Image(image) => Ok(ResolvedXObject::Image {
                proof,
                image: image.clone(),
            }),
            PlannedXObject::Form(_) => Ok(ResolvedXObject::Form {
                proof,
                form: usage
                    .form
                    .as_ref()
                    .map(Arc::clone)
                    .ok_or_else(|| internal(source))?,
            }),
        }
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
        let usage = &self.uses[self.execution_cursor];
        let resource = self
            .images
            .get(usize::try_from(usage.image_index).map_err(|_| internal(source))?)
            .and_then(|image| image.resource.as_ref())
            .ok_or_else(|| internal(source))?;
        if matches!(resource, PlannedXObject::Image(_)) {
            self.executed_image_uses = self
                .executed_image_uses
                .checked_add(1)
                .ok_or_else(|| internal(source))?;
        }
        self.execution_cursor = self
            .execution_cursor
            .checked_add(1)
            .ok_or_else(|| internal(source))?;
        self.stats.set_image_uses(self.executed_image_uses);
        Ok(())
    }

    pub(super) fn finish_execution(&self) -> Result<(), ContentVmError> {
        if self.execution_cursor != self.uses.len() {
            return Err(ContentVmError::new(ContentVmErrorCode::InternalState, None));
        }
        Ok(())
    }

    pub(super) fn form_use_count(&self) -> usize {
        self.uses
            .iter()
            .filter(|usage| usage.form.is_some())
            .count()
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

    fn admit_acquisition_poll(
        &mut self,
        source: ContentOperatorSource,
    ) -> Result<(), ContentVmError> {
        self.profile.content_limits().preflight(
            ContentImageLimitKind::AcquisitionPolls,
            self.stats.acquisition_polls(),
            1,
            Some(source),
        )?;
        self.stats
            .record_acquisition_poll()
            .ok_or_else(|| internal(source))
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
        let packed_len =
            u64::try_from(acquired.decoded_bytes().len()).map_err(|_| vm_failure(source))?;
        if acquired.stats().decoded_bytes() != packed_len {
            return Err(vm_failure(source));
        }
        let decoded_len = u64::from(acquired.width())
            .checked_mul(u64::from(acquired.height()))
            .and_then(|pixels| pixels.checked_mul(u64::from(acquired.components())))
            .ok_or_else(|| vm_failure(source))?;
        self.profile
            .content_limits()
            .preflight(
                ContentImageLimitKind::DecodedBytes,
                self.stats.decoded_bytes(),
                decoded_len,
                Some(source),
            )
            .map_err(ContentVmFailure::Vm)?;
        let decoded_slots = usize::try_from(decoded_len).map_err(|_| vm_failure(source))?;
        let mut decoded = Vec::new();
        decoded.try_reserve_exact(decoded_slots).map_err(|_| {
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
        normalize_image_samples(acquired, &mut decoded, source)?;
        if decoded.len() != decoded_slots {
            return Err(vm_failure(source));
        }
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
            8,
            acquired.interpolate(),
            decoded,
        )
        .map_err(ContentVmFailure::Scene)?;
        let entry = self
            .images
            .get_mut(image_index)
            .ok_or_else(|| vm_failure(source))?;
        if entry
            .resource
            .replace(PlannedXObject::Image(resource))
            .is_some()
        {
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
    reason = "packed-image normalization preserves the complete VM failure contract"
)]
fn normalize_image_samples(
    acquired: &pdf_rs_document::AcquiredImageXObject,
    output: &mut Vec<u8>,
    source: ContentOperatorSource,
) -> Result<(), ContentVmFailure> {
    let packed = acquired.decoded_bytes();
    let stride = usize::try_from(acquired.stride_bytes()).map_err(|_| vm_failure(source))?;
    let height = usize::try_from(acquired.height()).map_err(|_| vm_failure(source))?;
    let expected_packed = stride
        .checked_mul(height)
        .ok_or_else(|| vm_failure(source))?;
    if packed.len() != expected_packed {
        return Err(vm_failure(source));
    }
    if let (Some(high_value), Some(lookup)) = (
        acquired.indexed_high_value(),
        acquired.indexed_lookup_bytes(),
    ) {
        let components = usize::from(acquired.components());
        let expected_lookup = usize::from(high_value)
            .checked_add(1)
            .and_then(|entries| entries.checked_mul(components))
            .ok_or_else(|| vm_failure(source))?;
        if lookup.len() != expected_lookup || acquired.source_components() != 1 {
            return Err(vm_failure(source));
        }
        let width = usize::try_from(acquired.width()).map_err(|_| vm_failure(source))?;
        let bits = acquired.bits_per_component();
        if !matches!(bits, 1 | 2 | 4 | 8) {
            return Err(vm_failure(source));
        }
        for row in packed.chunks_exact(stride) {
            for sample_index in 0..width {
                let sample = packed_sample(row, sample_index, bits, source)?;
                let palette_index = usize::from(sample.min(u16::from(high_value)));
                let start = palette_index
                    .checked_mul(components)
                    .ok_or_else(|| vm_failure(source))?;
                let end = start
                    .checked_add(components)
                    .ok_or_else(|| vm_failure(source))?;
                output.extend_from_slice(lookup.get(start..end).ok_or_else(|| vm_failure(source))?);
            }
        }
        return Ok(());
    }
    if acquired.indexed_high_value().is_some() || acquired.indexed_lookup_bytes().is_some() {
        return Err(vm_failure(source));
    }
    let samples_per_row = usize::try_from(acquired.width())
        .ok()
        .and_then(|width| width.checked_mul(usize::from(acquired.source_components())))
        .ok_or_else(|| vm_failure(source))?;
    let bits = acquired.bits_per_component();
    match bits {
        8 => output.extend_from_slice(packed),
        16 => {
            for row in packed.chunks_exact(stride) {
                for sample in row.chunks_exact(2).take(samples_per_row) {
                    let value = u32::from(u16::from_be_bytes([sample[0], sample[1]]));
                    let normalized = value
                        .checked_mul(255)
                        .and_then(|scaled| scaled.checked_add(32_767))
                        .map(|scaled| scaled / 65_535)
                        .and_then(|scaled| u8::try_from(scaled).ok())
                        .ok_or_else(|| vm_failure(source))?;
                    output.push(normalized);
                }
            }
        }
        1 | 2 | 4 => {
            let mask = (1_u8 << bits) - 1;
            for row in packed.chunks_exact(stride) {
                for sample_index in 0..samples_per_row {
                    let bit_offset = sample_index
                        .checked_mul(usize::from(bits))
                        .ok_or_else(|| vm_failure(source))?;
                    let byte = *row.get(bit_offset / 8).ok_or_else(|| vm_failure(source))?;
                    let shift = 8_usize
                        .checked_sub(usize::from(bits))
                        .and_then(|value| value.checked_sub(bit_offset % 8))
                        .ok_or_else(|| vm_failure(source))?;
                    let sample = (byte >> shift) & mask;
                    let normalized = u16::from(sample)
                        .checked_mul(255)
                        .map(|value| value / u16::from(mask))
                        .and_then(|value| u8::try_from(value).ok())
                        .ok_or_else(|| vm_failure(source))?;
                    output.push(normalized);
                }
            }
        }
        _ => return Err(vm_failure(source)),
    }
    Ok(())
}

#[allow(
    clippy::result_large_err,
    reason = "packed sample extraction preserves the caller's complete VM failure contract"
)]
fn packed_sample(
    row: &[u8],
    sample_index: usize,
    bits: u8,
    source: ContentOperatorSource,
) -> Result<u16, ContentVmFailure> {
    match bits {
        8 => row
            .get(sample_index)
            .copied()
            .map(u16::from)
            .ok_or_else(|| vm_failure(source)),
        1 | 2 | 4 => {
            let bit_offset = sample_index
                .checked_mul(usize::from(bits))
                .ok_or_else(|| vm_failure(source))?;
            let byte = *row.get(bit_offset / 8).ok_or_else(|| vm_failure(source))?;
            let shift = 8_usize
                .checked_sub(usize::from(bits))
                .and_then(|value| value.checked_sub(bit_offset % 8))
                .ok_or_else(|| vm_failure(source))?;
            Ok(u16::from((byte >> shift) & ((1_u8 << bits) - 1)))
        }
        _ => Err(vm_failure(source)),
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
        ImageXObjectColorSpace::DeviceGray | ImageXObjectColorSpace::IndexedGray => {
            ImageColorSpace::DeviceGray
        }
        ImageXObjectColorSpace::DeviceRgb | ImageXObjectColorSpace::IndexedRgb => {
            ImageColorSpace::DeviceRgb
        }
        ImageXObjectColorSpace::DeviceCmyk | ImageXObjectColorSpace::IndexedCmyk => {
            ImageColorSpace::DeviceCmyk
        }
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
