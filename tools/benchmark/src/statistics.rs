use std::cmp::Ordering;
use std::error::Error;
use std::fmt;
use std::num::NonZeroUsize;

/// A raw or summarized duration expressed as an integer number of nanoseconds.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
#[repr(transparent)]
pub struct Nanoseconds(u64);

impl Nanoseconds {
    /// Creates a duration. Zero is valid for raw measurements and synthetic tests.
    #[must_use]
    pub const fn new(value: u64) -> Self {
        Self(value)
    }

    /// Returns the integer nanosecond value.
    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }
}

/// Validation error returned for an empty raw sample collection.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct EmptySamples;

impl fmt::Display for EmptySamples {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("benchmark samples must be non-empty")
    }
}

impl Error for EmptySamples {}

/// Validated, non-empty raw nanosecond samples in producer order.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RawNanosecondSamples {
    values: Vec<Nanoseconds>,
}

impl RawNanosecondSamples {
    /// Validates and preserves non-empty raw integer nanosecond samples.
    pub fn new(values: Vec<u64>) -> Result<Self, EmptySamples> {
        if values.is_empty() {
            return Err(EmptySamples);
        }
        Ok(Self {
            values: values.into_iter().map(Nanoseconds::new).collect(),
        })
    }

    /// Returns the raw samples in their original producer order.
    #[must_use]
    pub fn values(&self) -> &[Nanoseconds] {
        &self.values
    }

    /// Returns the number of preserved raw samples.
    #[must_use]
    pub fn len(&self) -> usize {
        self.values.len()
    }

    /// Returns `false`; construction rejects empty collections.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        false
    }

    /// Computes deterministic nearest-rank descriptive statistics.
    pub fn statistics(&self) -> Result<SampleStatistics, StatisticsError> {
        let mut sorted = self.values.clone();
        sorted.sort_unstable();

        let minimum = *sorted.first().ok_or(StatisticsError::InvariantEmpty)?;
        let maximum = *sorted.last().ok_or(StatisticsError::InvariantEmpty)?;
        Ok(SampleStatistics {
            minimum,
            median: nearest_rank(&sorted, 50)?,
            p95: nearest_rank(&sorted, 95)?,
            p99: nearest_rank(&sorted, 99)?,
            maximum,
            sample_count: sorted.len(),
        })
    }

    /// Computes descriptive statistics and reports only whether a configured count minimum is met.
    ///
    /// Meeting the count minimum is not a CI pass, release decision, confidence interval, or claim
    /// of statistical significance.
    pub fn summarize(
        &self,
        minimum: MinimumSampleCount,
    ) -> Result<BenchmarkSummary, StatisticsError> {
        let statistics = self.statistics()?;
        let actual = statistics.sample_count;
        let required = minimum.get();
        let adequacy = if actual < required {
            SampleAdequacy::Insufficient { actual, required }
        } else {
            SampleAdequacy::MeetsConfiguredMinimum { actual, required }
        };
        Ok(BenchmarkSummary {
            statistics,
            adequacy,
        })
    }
}

/// Failure while deriving a rank from an otherwise validated sample collection.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StatisticsError {
    /// Checked rank arithmetic overflowed.
    RankOverflow,
    /// A validated sample collection unexpectedly became empty.
    InvariantEmpty,
    /// A computed rank fell outside the sorted collection.
    RankOutOfBounds,
}

impl fmt::Display for StatisticsError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::RankOverflow => formatter.write_str("nearest-rank arithmetic overflowed"),
            Self::InvariantEmpty => formatter.write_str("validated sample collection is empty"),
            Self::RankOutOfBounds => formatter.write_str("nearest-rank index is out of bounds"),
        }
    }
}

impl Error for StatisticsError {}

/// Deterministic descriptive timing statistics.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SampleStatistics {
    /// Smallest raw sample.
    pub minimum: Nanoseconds,
    /// Nearest-rank 50th percentile.
    pub median: Nanoseconds,
    /// Nearest-rank 95th percentile.
    pub p95: Nanoseconds,
    /// Nearest-rank 99th percentile.
    pub p99: Nanoseconds,
    /// Largest raw sample.
    pub maximum: Nanoseconds,
    /// Number of raw samples contributing to all statistics.
    pub sample_count: usize,
}

/// Non-zero configured minimum raw sample count.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
#[repr(transparent)]
pub struct MinimumSampleCount(NonZeroUsize);

impl MinimumSampleCount {
    /// Creates a non-zero sample-count minimum.
    #[must_use]
    pub const fn new(value: usize) -> Option<Self> {
        match NonZeroUsize::new(value) {
            Some(value) => Some(Self(value)),
            None => None,
        }
    }

    /// Returns the configured count minimum.
    #[must_use]
    pub const fn get(self) -> usize {
        self.0.get()
    }
}

/// Whether raw sample count meets one configured descriptive-reporting minimum.
///
/// Neither variant is a performance pass/fail or statistical-significance result.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SampleAdequacy {
    /// Fewer raw samples were provided than the configured minimum.
    Insufficient {
        /// Actual raw sample count.
        actual: usize,
        /// Configured minimum raw sample count.
        required: usize,
    },
    /// The count minimum was met; distributional and confidence analysis is still required.
    MeetsConfiguredMinimum {
        /// Actual raw sample count.
        actual: usize,
        /// Configured minimum raw sample count.
        required: usize,
    },
}

/// Descriptive statistics plus an explicitly non-verdict sample-count assessment.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct BenchmarkSummary {
    /// Deterministic descriptive statistics.
    pub statistics: SampleStatistics,
    /// Count-only adequacy; this is never a CI or release verdict.
    pub adequacy: SampleAdequacy,
}

/// Error while constructing an exact Native-to-baseline duration ratio.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RatioError {
    /// The baseline duration is zero, so the ratio is undefined.
    ZeroBaseline,
    /// Floating-point presentation unexpectedly produced a non-finite value.
    NonFinite,
}

impl fmt::Display for RatioError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ZeroBaseline => formatter.write_str("baseline duration must be non-zero"),
            Self::NonFinite => formatter.write_str("native/baseline ratio is not finite"),
        }
    }
}

impl Error for RatioError {}

/// Exact Native and baseline durations plus a checked finite display ratio.
///
/// The integer endpoints remain available for precision-safe ordering even when adjacent `u64`
/// values round to the same `f64`. This type is descriptive and carries no acceptance threshold.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct NativeBaselineRatio {
    native: Nanoseconds,
    baseline: Nanoseconds,
    value: f64,
}

impl NativeBaselineRatio {
    /// Computes `native / baseline`, rejecting a zero baseline and non-finite presentation.
    pub fn new(native: Nanoseconds, baseline: Nanoseconds) -> Result<Self, RatioError> {
        if baseline.get() == 0 {
            return Err(RatioError::ZeroBaseline);
        }

        // Converting u64 to f64 can round but cannot overflow. Exact endpoints are retained below.
        #[allow(clippy::cast_precision_loss)]
        let value = native.get() as f64 / baseline.get() as f64;
        if !value.is_finite() {
            return Err(RatioError::NonFinite);
        }
        Ok(Self {
            native,
            baseline,
            value,
        })
    }

    /// Returns the exact Native duration.
    #[must_use]
    pub const fn native(self) -> Nanoseconds {
        self.native
    }

    /// Returns the exact non-zero baseline duration.
    #[must_use]
    pub const fn baseline(self) -> Nanoseconds {
        self.baseline
    }

    /// Returns the checked finite floating-point presentation of the ratio.
    #[must_use]
    pub const fn value(self) -> f64 {
        self.value
    }

    /// Compares exact integer durations without floating-point precision loss.
    #[must_use]
    pub const fn native_ordering(self) -> Ordering {
        if self.native.get() < self.baseline.get() {
            Ordering::Less
        } else if self.native.get() > self.baseline.get() {
            Ordering::Greater
        } else {
            Ordering::Equal
        }
    }
}

fn nearest_rank(sorted: &[Nanoseconds], percentile: usize) -> Result<Nanoseconds, StatisticsError> {
    let sample_count = sorted.len();
    if sample_count == 0 {
        return Err(StatisticsError::InvariantEmpty);
    }

    let whole = (sample_count / 100)
        .checked_mul(percentile)
        .ok_or(StatisticsError::RankOverflow)?;
    let partial_numerator = (sample_count % 100)
        .checked_mul(percentile)
        .and_then(|value| value.checked_add(99))
        .ok_or(StatisticsError::RankOverflow)?;
    let rank = whole
        .checked_add(partial_numerator / 100)
        .ok_or(StatisticsError::RankOverflow)?;
    let index = rank
        .checked_sub(1)
        .ok_or(StatisticsError::RankOutOfBounds)?;
    sorted
        .get(index)
        .copied()
        .ok_or(StatisticsError::RankOutOfBounds)
}

#[cfg(test)]
mod tests {
    use std::cmp::Ordering;

    use super::{
        EmptySamples, MinimumSampleCount, Nanoseconds, NativeBaselineRatio, RatioError,
        RawNanosecondSamples, SampleAdequacy,
    };

    #[test]
    fn nearest_rank_quantiles_match_the_one_based_definition() {
        let samples = RawNanosecondSamples::new((1..=100).collect())
            .unwrap_or_else(|error| panic!("non-empty samples failed: {error}"));
        let statistics = samples
            .statistics()
            .unwrap_or_else(|error| panic!("statistics failed: {error}"));

        assert_eq!(statistics.minimum, Nanoseconds::new(1));
        assert_eq!(statistics.median, Nanoseconds::new(50));
        assert_eq!(statistics.p95, Nanoseconds::new(95));
        assert_eq!(statistics.p99, Nanoseconds::new(99));
        assert_eq!(statistics.maximum, Nanoseconds::new(100));
        assert_eq!(statistics.sample_count, 100);
    }

    #[test]
    fn statistics_are_order_invariant_without_mutating_raw_order() {
        let forward = RawNanosecondSamples::new(vec![10, 20, 30, 40, 50])
            .unwrap_or_else(|error| panic!("forward samples failed: {error}"));
        let reverse = RawNanosecondSamples::new(vec![50, 40, 30, 20, 10])
            .unwrap_or_else(|error| panic!("reverse samples failed: {error}"));
        let reverse_before = reverse.values().to_vec();

        assert_eq!(
            forward
                .statistics()
                .unwrap_or_else(|error| panic!("forward statistics failed: {error}")),
            reverse
                .statistics()
                .unwrap_or_else(|error| panic!("reverse statistics failed: {error}"))
        );
        assert_eq!(reverse.values(), reverse_before);
    }

    #[test]
    fn empty_samples_fail_and_small_sets_remain_descriptive_only() {
        assert_eq!(RawNanosecondSamples::new(Vec::new()), Err(EmptySamples));
        assert_eq!(MinimumSampleCount::new(0), None);

        let samples = RawNanosecondSamples::new(vec![42])
            .unwrap_or_else(|error| panic!("single sample failed: {error}"));
        let minimum =
            MinimumSampleCount::new(30).unwrap_or_else(|| panic!("non-zero minimum was rejected"));
        let summary = samples
            .summarize(minimum)
            .unwrap_or_else(|error| panic!("summary failed: {error}"));
        assert_eq!(
            summary.adequacy,
            SampleAdequacy::Insufficient {
                actual: 1,
                required: 30
            }
        );
        assert_eq!(summary.statistics.sample_count, 1);
    }

    #[test]
    fn zero_baselines_are_rejected_and_zero_native_is_finite() {
        assert_eq!(
            NativeBaselineRatio::new(Nanoseconds::new(1), Nanoseconds::new(0)),
            Err(RatioError::ZeroBaseline)
        );
        let ratio = NativeBaselineRatio::new(Nanoseconds::new(0), Nanoseconds::new(1))
            .unwrap_or_else(|error| panic!("zero Native duration failed: {error}"));
        assert_eq!(ratio.value(), 0.0);
        assert!(ratio.value().is_finite());
    }

    #[test]
    fn ratio_and_statistics_handle_u64_precision_edges() {
        let native = Nanoseconds::new(u64::MAX);
        let baseline = Nanoseconds::new(u64::MAX - 1);
        let ratio = NativeBaselineRatio::new(native, baseline)
            .unwrap_or_else(|error| panic!("large ratio failed: {error}"));

        assert!(ratio.value().is_finite());
        assert_eq!(ratio.native(), native);
        assert_eq!(ratio.baseline(), baseline);
        assert_eq!(ratio.native_ordering(), Ordering::Greater);

        let samples = RawNanosecondSamples::new(vec![u64::MAX, 0, u64::MAX - 1])
            .unwrap_or_else(|error| panic!("large samples failed: {error}"));
        let statistics = samples
            .statistics()
            .unwrap_or_else(|error| panic!("large statistics failed: {error}"));
        assert_eq!(statistics.minimum, Nanoseconds::new(0));
        assert_eq!(statistics.maximum, Nanoseconds::new(u64::MAX));
        assert_eq!(statistics.median, Nanoseconds::new(u64::MAX - 1));
    }
}
