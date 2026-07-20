use std::mem::size_of;
use std::sync::Arc;

use pdf_rs_bytes::{ByteSource, DataTicket, ResumeCheckpoint, SmallRanges, SourceSnapshot};
use pdf_rs_document::{
    AcquireFontResourceJob, AcquiredFontResource, DocumentCancellation, FontResourcePoll,
    FontResourceUnsupported, PageFontLookupStats, PageFontReference,
};
use pdf_rs_scene::GraphicsResourceSource;
use pdf_rs_syntax::ObjectRef;

use super::ContentFontProfile;
use crate::{
    ContentFontLimit, ContentFontLimitKind, ContentFontStats, ContentOperatorSource,
    ContentVmError, ContentVmErrorCode, ContentVmFailure,
};

const CACHE_CANCELLATION_INTERVAL: u64 = 256;
pub(super) const FONT_DECODE_CONTEXT: u64 = 0x6d33_666f_6e74_0001;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct FontCacheKey {
    snapshot: SourceSnapshot,
    target: ObjectRef,
    revision_startxref: u64,
}

impl FontCacheKey {
    const fn from_proof(proof: PageFontReference) -> Self {
        Self {
            snapshot: proof.snapshot(),
            target: proof.target(),
            revision_startxref: proof.revision_startxref(),
        }
    }
}

struct PlannedFontUse {
    source: ContentOperatorSource,
    proof: PageFontReference,
    font_index: u32,
}

struct PlannedFont {
    key: FontCacheKey,
    proof: PageFontReference,
    source: ContentOperatorSource,
    resource: Option<Arc<AcquiredFontResource>>,
}

struct ActiveFont {
    font_index: usize,
    job: AcquireFontResourceJob,
}

pub(super) enum FontPlanningPoll {
    Ready,
    Pending {
        ticket: DataTicket,
        missing: SmallRanges,
        checkpoint: ResumeCheckpoint,
    },
    Unsupported {
        unsupported: FontResourceUnsupported,
        source: ContentOperatorSource,
    },
    Failed(ContentVmFailure),
}

pub(super) struct FontRuntime {
    profile: ContentFontProfile,
    uses: Vec<PlannedFontUse>,
    fonts: Vec<PlannedFont>,
    expected_uses: usize,
    acquisition_cursor: usize,
    execution_cursor: usize,
    external_plan_retained: u64,
    active: Option<ActiveFont>,
    plan_complete: bool,
    stats: ContentFontStats,
    lookup_stats: PageFontLookupStats,
}

impl FontRuntime {
    pub(super) fn new(profile: ContentFontProfile) -> Self {
        Self {
            profile,
            uses: Vec::new(),
            fonts: Vec::new(),
            expected_uses: 0,
            acquisition_cursor: 0,
            execution_cursor: 0,
            external_plan_retained: 0,
            active: None,
            plan_complete: false,
            stats: ContentFontStats::default(),
            lookup_stats: PageFontLookupStats::default(),
        }
    }

    pub(super) const fn stats(&self) -> ContentFontStats {
        self.stats
    }

    pub(super) const fn lookup_stats(&self) -> PageFontLookupStats {
        self.lookup_stats
    }

    pub(super) fn set_lookup_stats(&mut self, stats: PageFontLookupStats) {
        self.lookup_stats = stats;
    }

    pub(super) const fn plan_complete(&self) -> bool {
        self.plan_complete
    }

    pub(super) fn acquisitions_complete(&self) -> bool {
        self.plan_complete && self.active.is_none() && self.acquisition_cursor == self.fonts.len()
    }

    pub(super) fn admit_planning_operator(
        &mut self,
        source: ContentOperatorSource,
    ) -> Result<(), ContentVmError> {
        self.profile.content_limits().preflight(
            ContentFontLimitKind::PlanningOperators,
            self.stats.planning_operators(),
            1,
            Some(source),
        )?;
        self.stats
            .record_planning_operator()
            .ok_or_else(|| internal(source))
    }

    pub(super) fn admit_text(
        &mut self,
        bytes: u64,
        adjustments: u64,
        source: ContentOperatorSource,
    ) -> Result<(), ContentVmError> {
        self.profile.content_limits().preflight(
            ContentFontLimitKind::TextBytes,
            self.stats.text_bytes(),
            bytes,
            Some(source),
        )?;
        self.profile.content_limits().preflight(
            ContentFontLimitKind::TextAdjustments,
            self.stats.text_adjustments(),
            adjustments,
            Some(source),
        )?;
        self.stats
            .add_text(bytes, adjustments)
            .ok_or_else(|| internal(source))
    }

    pub(super) fn begin_plan(
        &mut self,
        expected_uses: usize,
        source: Option<ContentOperatorSource>,
    ) -> Result<(), ContentVmError> {
        if self.plan_complete
            || !self.uses.is_empty()
            || !self.fonts.is_empty()
            || self.active.is_some()
        {
            return Err(ContentVmError::new(
                ContentVmErrorCode::InternalState,
                source,
            ));
        }
        let target = u64::try_from(expected_uses)
            .ok()
            .and_then(|count| count.checked_mul(u64::try_from(size_of::<PlannedFontUse>()).ok()?))
            .ok_or_else(|| ContentVmError::new(ContentVmErrorCode::InternalState, source))?;
        self.profile.content_limits().preflight(
            ContentFontLimitKind::PlanRetainedBytes,
            self.external_plan_retained,
            target,
            source,
        )?;
        self.uses.try_reserve_exact(expected_uses).map_err(|_| {
            ContentVmError::font_resource(
                ContentFontLimit::new(
                    ContentFontLimitKind::PlanAllocation,
                    self.profile.content_limits().max_plan_retained_bytes(),
                    self.external_plan_retained,
                    target,
                ),
                source,
            )
        })?;
        self.expected_uses = expected_uses;
        self.record_plan_retained(source)?;
        self.preflight_plan_retained(source)
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
            ContentFontLimitKind::FontUses,
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
        self.stats.observe_plan_retained(retained);
        self.profile.content_limits().preflight(
            ContentFontLimitKind::PlanRetainedBytes,
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
        reason = "font planning preserves the complete typed VM failure across source guards"
    )]
    pub(super) fn register_proof(
        &mut self,
        proof: PageFontReference,
        source: ContentOperatorSource,
        byte_source: &dyn ByteSource,
        cancellation: &dyn DocumentCancellation,
    ) -> Result<(), ContentVmFailure> {
        if self.plan_complete || self.uses.len() >= self.expected_uses {
            return Err(ContentVmFailure::Vm(internal(source)));
        }
        let key = FontCacheKey::from_proof(proof);
        let mut font_index = None;
        for (index, font) in self.fonts.iter().enumerate() {
            runtime_guard(key.snapshot, byte_source, cancellation, source)?;
            self.profile
                .content_limits()
                .preflight(
                    ContentFontLimitKind::CacheProbes,
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
            if font.key == key {
                font_index = Some(index);
                self.stats
                    .record_cache_hit()
                    .ok_or_else(|| ContentVmFailure::Vm(internal(source)))?;
                break;
            }
        }
        let index = match font_index {
            Some(index) => index,
            None => self
                .push_unique_font(key, proof, source)
                .map_err(ContentVmFailure::Vm)?,
        };
        self.uses.push(PlannedFontUse {
            source,
            proof,
            font_index: u32::try_from(index).map_err(|_| ContentVmFailure::Vm(internal(source)))?,
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
    ) -> FontPlanningPoll {
        if !self.plan_complete || self.acquisition_cursor > self.fonts.len() {
            return FontPlanningPoll::Failed(ContentVmFailure::Vm(ContentVmError::new(
                ContentVmErrorCode::InternalState,
                None,
            )));
        }
        while self.acquisition_cursor < self.fonts.len() {
            let source = self.fonts[self.acquisition_cursor].source;
            let snapshot = self.fonts[self.acquisition_cursor].key.snapshot;
            if let Err(failure) = runtime_guard(snapshot, byte_source, cancellation, source) {
                return FontPlanningPoll::Failed(failure);
            }
            if self.fonts[self.acquisition_cursor].resource.is_some()
                || self
                    .active
                    .as_ref()
                    .is_some_and(|active| active.font_index != self.acquisition_cursor)
            {
                return FontPlanningPoll::Failed(ContentVmFailure::Vm(internal(source)));
            }
            if self.active.is_none() {
                let proof = self.fonts[self.acquisition_cursor].proof;
                let job = match self.profile.authority().acquire_font_resource(
                    proof,
                    self.profile.context(),
                    self.profile.acquisition_limits(),
                ) {
                    Ok(job) => job,
                    Err(error) => {
                        return FontPlanningPoll::Failed(ContentVmFailure::Document(error));
                    }
                };
                self.active = Some(ActiveFont {
                    font_index: self.acquisition_cursor,
                    job,
                });
            }
            if let Err(error) = self.profile.content_limits().preflight(
                ContentFontLimitKind::AcquisitionPolls,
                self.stats.acquisition_polls(),
                1,
                Some(source),
            ) {
                return FontPlanningPoll::Failed(ContentVmFailure::Vm(error));
            }
            if self.stats.record_acquisition_poll().is_none() {
                return FontPlanningPoll::Failed(ContentVmFailure::Vm(internal(source)));
            }
            let Some(active) = self.active.as_mut() else {
                return FontPlanningPoll::Failed(ContentVmFailure::Vm(internal(source)));
            };
            let outcome = active.job.poll(byte_source, cancellation);
            if let Err(failure) = runtime_guard(snapshot, byte_source, cancellation, source) {
                return FontPlanningPoll::Failed(failure);
            }
            match outcome {
                FontResourcePoll::Pending {
                    ticket,
                    missing,
                    checkpoint,
                } => {
                    return FontPlanningPoll::Pending {
                        ticket,
                        missing,
                        checkpoint,
                    };
                }
                FontResourcePoll::Unsupported(unsupported) => {
                    self.active.take();
                    return FontPlanningPoll::Unsupported {
                        unsupported,
                        source,
                    };
                }
                FontResourcePoll::Failed(error) => {
                    self.active.take();
                    return FontPlanningPoll::Failed(ContentVmFailure::Document(error));
                }
                FontResourcePoll::Ready(acquired) => {
                    self.active.take();
                    if FontCacheKey::from_proof(acquired.proof())
                        != self.fonts[self.acquisition_cursor].key
                    {
                        return FontPlanningPoll::Failed(ContentVmFailure::Vm(internal(source)));
                    }
                    if let Err(error) = self.install(self.acquisition_cursor, acquired, source) {
                        return FontPlanningPoll::Failed(error);
                    }
                    self.acquisition_cursor += 1;
                }
            }
        }
        FontPlanningPoll::Ready
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
    ) -> Result<(PageFontReference, Arc<AcquiredFontResource>), ContentVmError> {
        let usage = self
            .uses
            .get(self.execution_cursor)
            .ok_or_else(|| internal(source))?;
        if usage.source != source {
            return Err(internal(source));
        }
        let font = self
            .fonts
            .get(usize::try_from(usage.font_index).map_err(|_| internal(source))?)
            .and_then(|font| font.resource.as_ref())
            .ok_or_else(|| internal(source))?;
        Ok((usage.proof, Arc::clone(font)))
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
        self.stats
            .set_font_uses(u64::try_from(self.execution_cursor).map_err(|_| internal(source))?);
        Ok(())
    }

    pub(super) fn finish_execution(&self) -> Result<(), ContentVmError> {
        if self.execution_cursor != self.uses.len() {
            return Err(ContentVmError::new(ContentVmErrorCode::InternalState, None));
        }
        Ok(())
    }

    pub(super) fn preflight_glyphs(
        &self,
        glyphs: u64,
        segments: u64,
        source: ContentOperatorSource,
    ) -> Result<(), ContentVmError> {
        self.profile.content_limits().preflight(
            ContentFontLimitKind::Glyphs,
            self.stats.glyphs(),
            glyphs,
            Some(source),
        )?;
        self.profile.content_limits().preflight(
            ContentFontLimitKind::OutlineSegments,
            self.stats.outline_segments(),
            segments,
            Some(source),
        )
    }

    pub(super) fn record_glyphs(
        &mut self,
        glyphs: u64,
        segments: u64,
        source: ContentOperatorSource,
    ) -> Result<(), ContentVmError> {
        self.stats
            .add_glyphs(glyphs, segments)
            .ok_or_else(|| internal(source))
    }

    pub(super) fn preflight_glyph_retained(
        &self,
        consumed: u64,
        attempted: u64,
        source: ContentOperatorSource,
    ) -> Result<(), ContentVmError> {
        self.profile.content_limits().preflight(
            ContentFontLimitKind::GlyphRetainedBytes,
            consumed,
            attempted,
            Some(source),
        )
    }

    pub(super) fn record_glyph_retained(&mut self, retained: u64) {
        self.stats.record_glyph_retained(retained);
    }

    pub(super) fn glyph_allocation_error(
        &self,
        consumed: u64,
        attempted: u64,
        source: ContentOperatorSource,
    ) -> ContentVmError {
        ContentVmError::font_resource(
            ContentFontLimit::new(
                ContentFontLimitKind::GlyphAllocation,
                self.profile.content_limits().max_glyph_retained_bytes(),
                consumed,
                attempted,
            ),
            Some(source),
        )
    }

    fn push_unique_font(
        &mut self,
        key: FontCacheKey,
        proof: PageFontReference,
        source: ContentOperatorSource,
    ) -> Result<usize, ContentVmError> {
        let unique = u64::try_from(self.fonts.len()).map_err(|_| internal(source))?;
        self.profile.content_limits().preflight(
            ContentFontLimitKind::UniqueFonts,
            unique,
            1,
            Some(source),
        )?;
        self.reserve_font_slot(source)?;
        let index = self.fonts.len();
        self.fonts.push(PlannedFont {
            key,
            proof,
            source,
            resource: None,
        });
        Ok(index)
    }

    fn reserve_font_slot(&mut self, source: ContentOperatorSource) -> Result<(), ContentVmError> {
        if self.fonts.len() == self.fonts.capacity() {
            let current = cache_retained_bytes(&self.fonts).ok_or_else(|| internal(source))?;
            let desired = self
                .fonts
                .len()
                .checked_add(1)
                .ok_or_else(|| internal(source))?;
            let target = u64::try_from(desired)
                .ok()
                .and_then(|count| count.checked_mul(u64::try_from(size_of::<PlannedFont>()).ok()?))
                .ok_or_else(|| internal(source))?;
            let attempted = target
                .checked_sub(current)
                .ok_or_else(|| internal(source))?;
            self.profile.content_limits().preflight(
                ContentFontLimitKind::CacheRetainedBytes,
                current,
                attempted,
                Some(source),
            )?;
            self.fonts.try_reserve_exact(1).map_err(|_| {
                ContentVmError::font_resource(
                    ContentFontLimit::new(
                        ContentFontLimitKind::CacheAllocation,
                        self.profile.content_limits().max_cache_retained_bytes(),
                        current,
                        attempted,
                    ),
                    Some(source),
                )
            })?;
        }
        let actual = cache_retained_bytes(&self.fonts).ok_or_else(|| internal(source))?;
        self.stats.record_cache_retained(actual);
        self.profile.content_limits().preflight(
            ContentFontLimitKind::CacheRetainedBytes,
            0,
            actual,
            Some(source),
        )?;
        Ok(())
    }

    #[allow(
        clippy::result_large_err,
        reason = "cache installation preserves complete lower and VM failures atomically"
    )]
    fn install(
        &mut self,
        font_index: usize,
        acquired: Arc<AcquiredFontResource>,
        source: ContentOperatorSource,
    ) -> Result<(), ContentVmFailure> {
        let stats = acquired.stats();
        self.stats.observe_acquisition_peaks(stats);
        let current_resource_retained = self.stats.resource_retained_bytes();
        self.profile
            .content_limits()
            .preflight(
                ContentFontLimitKind::ResourceRetainedBytes,
                current_resource_retained,
                stats.retained_bytes(),
                Some(source),
            )
            .map_err(ContentVmFailure::Vm)?;
        let entry = self
            .fonts
            .get(font_index)
            .ok_or_else(|| ContentVmFailure::Vm(internal(source)))?;
        if entry.resource.is_some() {
            return Err(ContentVmFailure::Vm(internal(source)));
        }
        let retained = cache_retained_bytes(&self.fonts)
            .ok_or_else(|| ContentVmFailure::Vm(internal(source)))?;
        let mut next_stats = self.stats;
        next_stats
            .record_acquisition(retained, stats)
            .ok_or_else(|| ContentVmFailure::Vm(internal(source)))?;
        self.fonts
            .get_mut(font_index)
            .ok_or_else(|| ContentVmFailure::Vm(internal(source)))?
            .resource = Some(acquired);
        self.stats = next_stats;
        Ok(())
    }

    fn preflight_plan_retained(
        &self,
        source: Option<ContentOperatorSource>,
    ) -> Result<(), ContentVmError> {
        let retained = plan_retained_bytes(&self.uses)
            .and_then(|value| value.checked_add(self.external_plan_retained))
            .ok_or_else(|| ContentVmError::new(ContentVmErrorCode::InternalState, source))?;
        self.profile.content_limits().preflight(
            ContentFontLimitKind::PlanRetainedBytes,
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
            .and_then(|value| value.checked_add(self.external_plan_retained))
            .ok_or_else(|| ContentVmError::new(ContentVmErrorCode::InternalState, source))?;
        self.stats.record_plan_retained(retained);
        Ok(())
    }
}

pub(super) fn resource_source(acquired: &AcquiredFontResource) -> GraphicsResourceSource {
    GraphicsResourceSource::new(
        acquired.reference(),
        acquired.proof().revision_startxref(),
        FONT_DECODE_CONTEXT,
    )
}

#[allow(
    clippy::result_large_err,
    reason = "font runtime preserves complete copyable VM failures at guard boundaries"
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

fn plan_retained_bytes(uses: &Vec<PlannedFontUse>) -> Option<u64> {
    u64::try_from(uses.capacity())
        .ok()?
        .checked_mul(u64::try_from(size_of::<PlannedFontUse>()).ok()?)
}

fn cache_retained_bytes(fonts: &Vec<PlannedFont>) -> Option<u64> {
    u64::try_from(fonts.capacity())
        .ok()?
        .checked_mul(u64::try_from(size_of::<PlannedFont>()).ok()?)
}

fn internal(source: ContentOperatorSource) -> ContentVmError {
    ContentVmError::new(ContentVmErrorCode::InternalState, Some(source))
}
