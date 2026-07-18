use core::mem::size_of;
use std::{num::NonZeroU32, sync::Arc};

use pdf_rs_policy::{
    CapabilityEvaluationJob, CapabilityEvaluator, PolicyJobLimits, PolicyJobPoll,
    PolicyLimitConfig, PolicyLimits, PolicyPollBudget, RenderPlan,
};
use pdf_rs_scene::{GraphicsCommand, Scene};

use crate::fast::kernels::{PageMap, vector_bytes};
use crate::fast::limits::checked_total;
use crate::fast::render::{
    Work, bins_retained_bytes, command_belongs, identity, logical_vector_bytes, map_policy_error,
    numeric, reserve, validate_config, validate_subject_base, validate_tile_identity,
};
use crate::fast::{
    FastRasterCancellation, FastRasterError, FastRasterErrorCode, FastRasterLimitKind,
    FastRasterLimits, FastRasterStats, FastTile, FastTileBins, FastTileSet,
};

const HARD_MAX_POLL_WORK_UNITS: u32 = 4_096;
const HARD_MAX_ATOMIC_TILE_FUEL: u64 = 16 * 1024 * 1024;

/// Validated nonzero amount of resumable Fast raster work admitted by one poll.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FastRasterPollBudget(NonZeroU32);

impl FastRasterPollBudget {
    /// Validates one actor-turn work budget.
    pub fn new(work_units: NonZeroU32) -> Result<Self, FastRasterError> {
        if work_units.get() > HARD_MAX_POLL_WORK_UNITS {
            return Err(FastRasterError::resource(
                FastRasterLimitKind::Fuel,
                u64::from(HARD_MAX_POLL_WORK_UNITS),
                u64::from(work_units.get()),
            ));
        }
        Ok(Self(work_units))
    }

    /// Returns the maximum state-machine steps admitted by one poll.
    pub const fn work_units(self) -> NonZeroU32 {
        self.0
    }
}

/// Unvalidated limits specific to the resumable Fast raster job.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FastRasterJobLimitConfig {
    /// Maximum kernel fuel inside one atomic tile render.
    pub max_atomic_tile_fuel: u64,
}

impl Default for FastRasterJobLimitConfig {
    fn default() -> Self {
        Self {
            max_atomic_tile_fuel: HARD_MAX_ATOMIC_TILE_FUEL,
        }
    }
}

/// Validated limits specific to the resumable Fast raster job.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FastRasterJobLimits {
    max_atomic_tile_fuel: u64,
}

impl FastRasterJobLimits {
    /// Validates the nonzero irreducible-region ceiling.
    pub fn validate(config: FastRasterJobLimitConfig) -> Result<Self, FastRasterError> {
        if config.max_atomic_tile_fuel == 0
            || config.max_atomic_tile_fuel > HARD_MAX_ATOMIC_TILE_FUEL
        {
            return Err(FastRasterError::resource(
                FastRasterLimitKind::AtomicTileFuel,
                HARD_MAX_ATOMIC_TILE_FUEL,
                config.max_atomic_tile_fuel,
            ));
        }
        Ok(Self {
            max_atomic_tile_fuel: config.max_atomic_tile_fuel,
        })
    }

    /// Returns the maximum work inside one atomic tile render.
    pub const fn max_atomic_tile_fuel(self) -> u64 {
        self.max_atomic_tile_fuel
    }
}

impl Default for FastRasterJobLimits {
    fn default() -> Self {
        Self::validate(FastRasterJobLimitConfig::default())
            .expect("built-in resumable raster limits satisfy fixed hard ceilings")
    }
}

/// Observable state returned by one bounded owned-raster poll.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FastRasterJobPoll {
    /// More private work remains.
    Pending,
    /// The job reached a replayable terminal result.
    Ready,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Phase {
    Policy,
    ValidateTiles,
    PreflightPixels,
    AllocateCounts,
    InitializeCounts,
    CountBins,
    AllocateBins,
    AllocateBin,
    FillBins,
    PrepareTiles,
    ValidateOrder,
    RenderTile,
    Finalize,
}

/// Owned resumable Fast CPU raster job.
///
/// Scene, plan, bins, partially completed tile bytes, fuel, and allocation
/// accounting remain private across polls. Binning advances one command/tile
/// pair at a time. Rendering advances at most one complete tile per poll,
/// independently hard-capped by [`FastRasterJobLimits::max_atomic_tile_fuel`].
pub struct FastRasterOwnedJob {
    scene: Arc<Scene>,
    plan: Arc<RenderPlan>,
    limits: FastRasterLimits,
    job_limits: FastRasterJobLimits,
    policy_job: Option<CapabilityEvaluationJob>,
    phase: Phase,
    terminal: Option<Result<FastTileSet, FastRasterError>>,
    result_taken: bool,
    map: PageMap,
    tile_cursor: usize,
    command_cursor: usize,
    pair_tile_cursor: usize,
    product_pixels: u64,
    counts: Vec<usize>,
    bins: Vec<Vec<u32>>,
    entries: u64,
    retained_bin_bytes: u64,
    tiles: Vec<FastTile>,
    fuel: u64,
    cancellation_checks: u64,
    peak_intermediate: u64,
    max_atomic_tile_fuel_observed: u64,
}

impl FastRasterOwnedJob {
    /// Creates an owned job after constant-time configuration admission.
    pub fn new(
        scene: Arc<Scene>,
        plan: Arc<RenderPlan>,
        limits: FastRasterLimits,
        policy_job_limits: PolicyJobLimits,
        job_limits: FastRasterJobLimits,
    ) -> Result<Self, FastRasterError> {
        validate_config(&plan, limits)?;
        let decision = plan.decision();
        let policy_limits = PolicyLimits::validate(PolicyLimitConfig {
            max_requirements: decision.evaluated_requirements().max(1),
            max_dependencies: decision.evaluated_dependencies().max(1),
            max_parameters: decision.evaluated_parameters().max(1),
            cancellation_interval: plan.config().input().cancellation_interval,
            ..PolicyLimitConfig::default()
        })
        .map_err(map_policy_error)?;
        let policy_job = CapabilityEvaluator::new(decision.profile(), policy_limits)
            .start_job(
                Arc::clone(&scene),
                decision.subject().document_revision(),
                policy_job_limits,
            )
            .map_err(map_policy_error)?;
        let map = PageMap::new(&scene, &plan)?;
        Ok(Self {
            scene,
            plan,
            limits,
            job_limits,
            policy_job: Some(policy_job),
            phase: Phase::Policy,
            terminal: None,
            result_taken: false,
            map,
            tile_cursor: 0,
            command_cursor: 0,
            pair_tile_cursor: 0,
            product_pixels: 0,
            counts: Vec::new(),
            bins: Vec::new(),
            entries: 0,
            retained_bin_bytes: 0,
            tiles: Vec::new(),
            fuel: 0,
            cancellation_checks: 0,
            peak_intermediate: 0,
            max_atomic_tile_fuel_observed: 0,
        })
    }

    /// Returns the number of complete private tiles retained so far.
    pub fn completed_tiles(&self) -> usize {
        self.terminal
            .as_ref()
            .and_then(|result| result.as_ref().ok())
            .map_or(self.tiles.len(), |tiles| tiles.tiles().len())
    }

    /// Returns the greatest kernel fuel consumed by one completed atomic tile.
    pub const fn max_atomic_tile_fuel_observed(&self) -> u64 {
        self.max_atomic_tile_fuel_observed
    }

    /// Borrows the replayable terminal result.
    pub fn result(&self) -> Option<Result<&FastTileSet, FastRasterError>> {
        self.terminal
            .as_ref()
            .map(|result| result.as_ref().map_err(|error| *error))
    }

    /// Moves the terminal result without cloning tile pixels.
    pub fn take_result(&mut self) -> Option<Result<FastTileSet, FastRasterError>> {
        let result = self.terminal.take();
        if result.is_some() {
            self.result_taken = true;
        }
        result
    }

    /// Advances at most the explicit state-machine work budget.
    pub fn poll(
        &mut self,
        budget: FastRasterPollBudget,
        cancellation: &dyn FastRasterCancellation,
    ) -> FastRasterJobPoll {
        if self.terminal.is_some() || self.result_taken {
            return FastRasterJobPoll::Ready;
        }
        for _ in 0..budget.work_units().get() {
            if cancellation.is_cancelled() {
                self.fail(FastRasterError::for_code(FastRasterErrorCode::Cancelled));
                return FastRasterJobPoll::Ready;
            }
            let rendered_tiles = self.tiles.len();
            match self.step(cancellation) {
                Ok(true) => return FastRasterJobPoll::Ready,
                Ok(false) => {}
                Err(error) => {
                    self.fail(error);
                    return FastRasterJobPoll::Ready;
                }
            }
            if self.tiles.len() != rendered_tiles {
                return FastRasterJobPoll::Pending;
            }
        }
        FastRasterJobPoll::Pending
    }

    fn step(&mut self, cancellation: &dyn FastRasterCancellation) -> Result<bool, FastRasterError> {
        match self.phase {
            Phase::Policy => self.step_policy(cancellation),
            Phase::ValidateTiles => self.step_validate_tiles(),
            Phase::PreflightPixels => self.step_preflight_pixels(),
            Phase::AllocateCounts => self.step_allocate_counts(cancellation),
            Phase::InitializeCounts => self.step_initialize_counts(cancellation),
            Phase::CountBins => self.step_count_bins(cancellation),
            Phase::AllocateBins => self.step_allocate_bins(),
            Phase::AllocateBin => self.step_allocate_bin(cancellation),
            Phase::FillBins => self.step_fill_bins(cancellation),
            Phase::PrepareTiles => self.step_prepare_tiles(cancellation),
            Phase::ValidateOrder => self.step_validate_order(cancellation),
            Phase::RenderTile => self.step_render_tile(cancellation),
            Phase::Finalize => self.step_finalize(cancellation),
        }
    }

    fn step_policy(
        &mut self,
        cancellation: &dyn FastRasterCancellation,
    ) -> Result<bool, FastRasterError> {
        struct Adapter<'a>(&'a dyn FastRasterCancellation);
        impl pdf_rs_policy::PolicyCancellation for Adapter<'_> {
            fn is_cancelled(&self) -> bool {
                self.0.is_cancelled()
            }
        }
        let budget = PolicyPollBudget::new(NonZeroU32::new(1).ok_or_else(numeric)?)
            .map_err(map_policy_error)?;
        let job = self.policy_job.as_mut().ok_or_else(identity)?;
        if job.poll(budget, &Adapter(cancellation)) == PolicyJobPoll::Pending {
            return Ok(false);
        }
        let evaluated = job
            .take_result()
            .ok_or_else(identity)?
            .map_err(map_policy_error)?;
        validate_subject_base(&self.scene, &self.plan, &evaluated)?;
        self.policy_job = None;
        self.phase = Phase::ValidateTiles;
        Ok(false)
    }

    fn step_validate_tiles(&mut self) -> Result<bool, FastRasterError> {
        if self.tile_cursor == self.plan.tiles().len() {
            self.tile_cursor = 0;
            self.phase = Phase::PreflightPixels;
            return Ok(false);
        }
        validate_tile_identity(&self.scene, &self.plan, self.tile_cursor)?;
        self.tile_cursor = self.tile_cursor.checked_add(1).ok_or_else(numeric)?;
        Ok(false)
    }

    fn step_preflight_pixels(&mut self) -> Result<bool, FastRasterError> {
        let Some(tile) = self.plan.tiles().get(self.tile_cursor) else {
            self.tile_cursor = 0;
            self.phase = Phase::AllocateCounts;
            return Ok(false);
        };
        let rect = tile.content_key().tile();
        let pixels = u64::from(rect.width())
            .checked_mul(u64::from(rect.height()))
            .ok_or_else(numeric)?;
        self.product_pixels = checked_total(
            FastRasterLimitKind::Pixels,
            self.product_pixels,
            pixels,
            self.limits.max_pixels(),
        )?;
        self.tile_cursor = self.tile_cursor.checked_add(1).ok_or_else(numeric)?;
        Ok(false)
    }

    fn step_allocate_counts(
        &mut self,
        cancellation: &dyn FastRasterCancellation,
    ) -> Result<bool, FastRasterError> {
        let command_count =
            u64::try_from(self.graphics()?.commands().len()).map_err(|_| numeric())?;
        if command_count > self.limits.max_commands() {
            return Err(FastRasterError::resource(
                FastRasterLimitKind::Commands,
                self.limits.max_commands(),
                command_count,
            ));
        }
        self.probe(cancellation)?;
        let tile_count = self.plan.tiles().len();
        let logical = logical_vector_bytes::<usize>(tile_count)?;
        self.admit_intermediate(logical)?;
        reserve(&mut self.counts, tile_count)?;
        self.admit_intermediate(vector_bytes(&self.counts)?)?;
        self.phase = Phase::InitializeCounts;
        Ok(false)
    }

    fn step_initialize_counts(
        &mut self,
        cancellation: &dyn FastRasterCancellation,
    ) -> Result<bool, FastRasterError> {
        if self.counts.len() == self.plan.tiles().len() {
            self.phase = Phase::CountBins;
            return Ok(false);
        }
        self.charge_fuel(cancellation)?;
        self.counts.push(0);
        Ok(false)
    }

    fn step_count_bins(
        &mut self,
        cancellation: &dyn FastRasterCancellation,
    ) -> Result<bool, FastRasterError> {
        let command_count = self.graphics()?.commands().len();
        let tile_count = self.plan.tiles().len();
        if self.command_cursor == command_count {
            self.command_cursor = 0;
            self.pair_tile_cursor = 0;
            self.phase = Phase::AllocateBins;
            return Ok(false);
        }
        let invalid_group = {
            let record = self
                .graphics()?
                .commands()
                .get(self.command_cursor)
                .ok_or_else(identity)?;
            matches!(
                record.command(),
                GraphicsCommand::BeginIsolatedGroup { .. } | GraphicsCommand::EndIsolatedGroup
            )
        };
        if self.pair_tile_cursor == 0 && invalid_group {
            return Err(FastRasterError::for_code(
                FastRasterErrorCode::InvalidRenderConfig,
            ));
        }
        let tile = self
            .plan
            .tiles()
            .get(self.pair_tile_cursor)
            .ok_or_else(identity)?
            .content_key()
            .tile();
        self.charge_fuel(cancellation)?;
        let belongs = {
            let record = self
                .graphics()?
                .commands()
                .get(self.command_cursor)
                .ok_or_else(identity)?;
            command_belongs(
                record.command(),
                record.bounds(),
                tile,
                &self.plan,
                self.map,
            )?
        };
        if belongs {
            let count = self
                .counts
                .get_mut(self.pair_tile_cursor)
                .ok_or_else(identity)?;
            *count = count.checked_add(1).ok_or_else(numeric)?;
            self.entries = checked_total(
                FastRasterLimitKind::BinEntries,
                self.entries,
                1,
                self.limits.max_bin_entries(),
            )?;
        }
        self.advance_pair(tile_count)?;
        Ok(false)
    }

    fn step_allocate_bins(&mut self) -> Result<bool, FastRasterError> {
        let tile_count = self.plan.tiles().len();
        let logical = logical_vector_bytes::<Vec<u32>>(tile_count)?
            .checked_add(
                self.entries
                    .checked_mul(u64::try_from(size_of::<u32>()).map_err(|_| numeric())?)
                    .ok_or_else(numeric)?,
            )
            .ok_or_else(numeric)?;
        checked_total(
            FastRasterLimitKind::RetainedBytes,
            0,
            logical,
            self.limits.max_retained_bytes(),
        )?;
        self.admit_intermediate(
            vector_bytes(&self.counts)?
                .checked_add(logical)
                .ok_or_else(numeric)?,
        )?;
        reserve(&mut self.bins, tile_count)?;
        let actual_bin_bytes =
            actual_retained_with_vector(0, &self.bins, self.limits.max_retained_bytes())?;
        self.admit_intermediate(
            vector_bytes(&self.counts)?
                .checked_add(actual_bin_bytes)
                .ok_or_else(numeric)?,
        )?;
        self.retained_bin_bytes = actual_bin_bytes;
        self.phase = Phase::AllocateBin;
        Ok(false)
    }

    fn step_allocate_bin(
        &mut self,
        cancellation: &dyn FastRasterCancellation,
    ) -> Result<bool, FastRasterError> {
        let Some(&count) = self.counts.get(self.bins.len()) else {
            self.command_cursor = 0;
            self.pair_tile_cursor = 0;
            self.phase = Phase::FillBins;
            return Ok(false);
        };
        let mut bin = Vec::new();
        let logical = logical_vector_bytes::<u32>(count)?;
        checked_total(
            FastRasterLimitKind::RetainedBytes,
            self.retained_bin_bytes,
            logical,
            self.limits.max_retained_bytes(),
        )?;
        reserve(&mut bin, count)?;
        self.retained_bin_bytes = checked_total(
            FastRasterLimitKind::RetainedBytes,
            self.retained_bin_bytes,
            vector_bytes(&bin)?,
            self.limits.max_retained_bytes(),
        )?;
        self.admit_intermediate(
            vector_bytes(&self.counts)?
                .checked_add(self.retained_bin_bytes)
                .ok_or_else(numeric)?,
        )?;
        self.charge_fuel(cancellation)?;
        self.bins.push(bin);
        Ok(false)
    }

    fn step_fill_bins(
        &mut self,
        cancellation: &dyn FastRasterCancellation,
    ) -> Result<bool, FastRasterError> {
        let command_count = self.graphics()?.commands().len();
        let tile_count = self.plan.tiles().len();
        if self.command_cursor == command_count {
            if self
                .bins
                .iter()
                .map(Vec::len)
                .ne(self.counts.iter().copied())
            {
                return Err(identity());
            }
            self.retained_bin_bytes = bins_retained_bytes(&self.bins)?;
            self.probe(cancellation)?;
            self.counts = Vec::new();
            self.phase = Phase::PrepareTiles;
            return Ok(false);
        }
        let tile = self
            .plan
            .tiles()
            .get(self.pair_tile_cursor)
            .ok_or_else(identity)?
            .content_key()
            .tile();
        self.charge_fuel(cancellation)?;
        let belongs = {
            let record = self
                .graphics()?
                .commands()
                .get(self.command_cursor)
                .ok_or_else(identity)?;
            command_belongs(
                record.command(),
                record.bounds(),
                tile,
                &self.plan,
                self.map,
            )?
        };
        if belongs {
            let bin = self
                .bins
                .get_mut(self.pair_tile_cursor)
                .ok_or_else(identity)?;
            if bin.len() == bin.capacity() {
                return Err(identity());
            }
            bin.push(u32::try_from(self.command_cursor).map_err(|_| numeric())?);
        }
        self.advance_pair(tile_count)?;
        Ok(false)
    }

    fn step_prepare_tiles(
        &mut self,
        cancellation: &dyn FastRasterCancellation,
    ) -> Result<bool, FastRasterError> {
        let pixel_bytes = self.product_pixels.checked_mul(4).ok_or_else(numeric)?;
        let metadata = logical_vector_bytes::<FastTile>(self.plan.tiles().len())?;
        let logical = self
            .retained_bin_bytes
            .checked_add(pixel_bytes)
            .and_then(|value| value.checked_add(metadata))
            .ok_or_else(numeric)?;
        if logical > self.limits.max_retained_bytes() {
            return Err(FastRasterError::resource(
                FastRasterLimitKind::RetainedBytes,
                self.limits.max_retained_bytes(),
                logical,
            ));
        }
        reserve(&mut self.tiles, self.plan.tiles().len())?;
        actual_retained_with_vector(
            self.retained_bin_bytes
                .checked_add(pixel_bytes)
                .ok_or_else(numeric)?,
            &self.tiles,
            self.limits.max_retained_bytes(),
        )?;
        self.probe(cancellation)?;
        self.tile_cursor = 0;
        self.phase = Phase::ValidateOrder;
        Ok(false)
    }

    fn step_validate_order(
        &mut self,
        cancellation: &dyn FastRasterCancellation,
    ) -> Result<bool, FastRasterError> {
        let steps = self.plan.tiles().len().checked_mul(2).ok_or_else(numeric)?;
        if self.tile_cursor == steps {
            self.tile_cursor = 0;
            self.phase = Phase::RenderTile;
            return Ok(false);
        }
        self.charge_fuel(cancellation)?;
        self.tile_cursor = self.tile_cursor.checked_add(1).ok_or_else(numeric)?;
        Ok(false)
    }

    fn step_render_tile(
        &mut self,
        cancellation: &dyn FastRasterCancellation,
    ) -> Result<bool, FastRasterError> {
        if self.tile_cursor == self.plan.tiles().len() {
            self.phase = Phase::Finalize;
            return Ok(false);
        }
        let bins = std::mem::take(&mut self.bins);
        let job = crate::fast::FastRasterJob::from_prepared(
            &self.scene,
            &self.plan,
            self.limits,
            FastTileBins::new(
                self.plan.hash(),
                bins,
                self.entries,
                self.retained_bin_bytes,
            ),
            self.product_pixels,
            self.fuel,
            self.cancellation_checks,
            self.peak_intermediate,
        )?;
        let interval = u64::from(self.plan.config().input().cancellation_interval);
        let tile_start_fuel = self.fuel;
        let mut work = Work::new_bounded(
            self.limits,
            interval,
            cancellation,
            self.fuel,
            self.cancellation_checks,
            self.job_limits.max_atomic_tile_fuel(),
        )?;
        work.peak_intermediate = self.peak_intermediate;
        let result = job.render_one(self.tile_cursor, &mut work);
        self.fuel = work.fuel;
        let tile_fuel = self.fuel.checked_sub(tile_start_fuel).ok_or_else(numeric)?;
        self.max_atomic_tile_fuel_observed = self.max_atomic_tile_fuel_observed.max(tile_fuel);
        self.cancellation_checks = work.cancellation_checks;
        self.peak_intermediate = self.peak_intermediate.max(work.peak_intermediate);
        self.bins = job.into_bins();
        let tile = result?;
        let attempted = self
            .current_retained_bytes()?
            .checked_add(tile.retained_bytes()?)
            .ok_or_else(numeric)?;
        if attempted > self.limits.max_retained_bytes() {
            return Err(FastRasterError::resource(
                FastRasterLimitKind::RetainedBytes,
                self.limits.max_retained_bytes(),
                attempted,
            ));
        }
        if self.tiles.len() == self.tiles.capacity() {
            return Err(identity());
        }
        self.tiles.push(tile);
        self.tile_cursor = self.tile_cursor.checked_add(1).ok_or_else(numeric)?;
        Ok(false)
    }

    fn step_finalize(
        &mut self,
        cancellation: &dyn FastRasterCancellation,
    ) -> Result<bool, FastRasterError> {
        self.probe(cancellation)?;
        let retained_bytes = self.current_retained_bytes()?;
        let stats = FastRasterStats::new(
            u64::try_from(self.graphics()?.commands().len()).map_err(|_| numeric())?,
            self.entries,
            u64::try_from(self.tiles.len()).map_err(|_| numeric())?,
            self.product_pixels,
            retained_bytes,
            self.peak_intermediate,
            self.fuel,
            self.cancellation_checks,
        );
        let tiles = std::mem::take(&mut self.tiles);
        self.bins = Vec::new();
        self.terminal = Some(Ok(FastTileSet::new(self.plan.hash(), tiles, stats)));
        Ok(true)
    }

    fn graphics(&self) -> Result<&pdf_rs_scene::GraphicsScene, FastRasterError> {
        self.scene.graphics().ok_or_else(identity)
    }

    fn advance_pair(&mut self, tile_count: usize) -> Result<(), FastRasterError> {
        self.pair_tile_cursor = self.pair_tile_cursor.checked_add(1).ok_or_else(numeric)?;
        if self.pair_tile_cursor == tile_count {
            self.pair_tile_cursor = 0;
            self.command_cursor = self.command_cursor.checked_add(1).ok_or_else(numeric)?;
        }
        Ok(())
    }

    fn charge_fuel(
        &mut self,
        cancellation: &dyn FastRasterCancellation,
    ) -> Result<(), FastRasterError> {
        self.fuel = checked_total(
            FastRasterLimitKind::Fuel,
            self.fuel,
            1,
            self.limits.max_fuel(),
        )?;
        let interval = u64::from(self.plan.config().input().cancellation_interval);
        if self.fuel.is_multiple_of(interval) {
            self.probe(cancellation)?;
        }
        Ok(())
    }

    fn probe(&mut self, cancellation: &dyn FastRasterCancellation) -> Result<(), FastRasterError> {
        self.cancellation_checks = self
            .cancellation_checks
            .checked_add(1)
            .ok_or_else(numeric)?;
        if cancellation.is_cancelled() {
            return Err(FastRasterError::for_code(FastRasterErrorCode::Cancelled));
        }
        Ok(())
    }

    fn admit_intermediate(&mut self, bytes: u64) -> Result<(), FastRasterError> {
        if bytes > self.limits.max_intermediate_bytes() {
            return Err(FastRasterError::resource(
                FastRasterLimitKind::IntermediateBytes,
                self.limits.max_intermediate_bytes(),
                bytes,
            ));
        }
        self.peak_intermediate = self.peak_intermediate.max(bytes);
        Ok(())
    }

    fn current_retained_bytes(&self) -> Result<u64, FastRasterError> {
        let tile_bytes = self.tiles.iter().try_fold(0_u64, |total, tile| {
            total
                .checked_add(tile.retained_bytes()?)
                .ok_or_else(numeric)
        })?;
        self.retained_bin_bytes
            .checked_add(vector_bytes(&self.tiles)?)
            .and_then(|value| value.checked_add(tile_bytes))
            .ok_or_else(numeric)
    }

    fn fail(&mut self, error: FastRasterError) {
        self.policy_job = None;
        self.counts = Vec::new();
        self.bins = Vec::new();
        self.tiles = Vec::new();
        self.terminal = Some(Err(error));
    }
}

fn actual_retained_with_vector<T>(
    base: u64,
    values: &Vec<T>,
    limit: u64,
) -> Result<u64, FastRasterError> {
    checked_total(
        FastRasterLimitKind::RetainedBytes,
        base,
        vector_bytes(values)?,
        limit,
    )
}

#[cfg(test)]
mod tests {
    use super::actual_retained_with_vector;
    use crate::fast::FastRasterLimitKind;

    #[test]
    fn actual_vector_capacity_has_exact_and_one_less_retained_boundaries() {
        let values = Vec::<Vec<u32>>::with_capacity(4);
        let base = 7;
        let actual = base
            + u64::try_from(values.capacity()).unwrap()
                * u64::try_from(core::mem::size_of::<Vec<u32>>()).unwrap();

        assert_eq!(
            actual_retained_with_vector(base, &values, actual).unwrap(),
            actual
        );
        let error = actual_retained_with_vector(base, &values, actual - 1).unwrap_err();
        let evidence = error.limit().unwrap();
        assert_eq!(evidence.kind(), FastRasterLimitKind::RetainedBytes);
        assert_eq!(evidence.limit(), actual - 1);
        assert_eq!(evidence.observed(), actual);
    }
}
