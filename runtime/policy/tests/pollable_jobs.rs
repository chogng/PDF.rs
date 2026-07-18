mod support;

use std::num::NonZeroU32;
use std::sync::Arc;

use pdf_rs_policy::{
    CapabilityEvaluator, CapabilityProfile, PolicyCancellation, PolicyErrorCode,
    PolicyJobLimitConfig, PolicyJobLimits, PolicyJobPoll, PolicyLimitKind, PolicyLimits,
    PolicyPollBudget, RenderPlanJob, RendererEpoch,
};

use support::{Never, evaluate, fast_config, request, scene};

fn one() -> PolicyPollBudget {
    PolicyPollBudget::new(NonZeroU32::new(1).unwrap()).unwrap()
}

fn many() -> PolicyPollBudget {
    PolicyPollBudget::new(NonZeroU32::new(4_096).unwrap()).unwrap()
}

struct Cancelled;

impl PolicyCancellation for Cancelled {
    fn is_cancelled(&self) -> bool {
        true
    }
}

#[test]
fn default_scene_limits_admit_a_small_owned_capability_job() {
    let scene = Arc::new(scene(&[]));
    let mut job = CapabilityEvaluator::default()
        .start_job(Arc::clone(&scene), 23, PolicyJobLimits::default())
        .unwrap();
    let mut pending = 0;
    while job.poll(one(), &Never) == PolicyJobPoll::Pending {
        pending += 1;
    }
    assert!(pending > 1);
    assert!(job.result().unwrap().is_ok());
    assert!(job.stats().atomic_canonical_bytes() > 0);
}

#[test]
fn one_unit_and_large_budget_capability_results_are_identical() {
    let scene = Arc::new(scene(&[]));
    let evaluator = CapabilityEvaluator::new(
        CapabilityProfile::m3_reference_v1(),
        PolicyLimits::default(),
    );
    let mut incremental = evaluator
        .start_job(Arc::clone(&scene), 23, PolicyJobLimits::default())
        .unwrap();
    while incremental.poll(one(), &Never) == PolicyJobPoll::Pending {}
    let incremental = incremental.take_result().unwrap().unwrap();

    let mut batched = evaluator
        .start_job(scene, 23, PolicyJobLimits::default())
        .unwrap();
    while batched.poll(many(), &Never) == PolicyJobPoll::Pending {}
    let batched = batched.take_result().unwrap().unwrap();
    assert_eq!(incremental, batched);
    assert_eq!(
        incremental.protocol_projection().unwrap(),
        batched.protocol_projection().unwrap()
    );
}

#[test]
fn cancellation_at_every_pending_capability_boundary_is_terminal_and_cleans_buffers() {
    let scene = Arc::new(scene(&[]));
    let mut baseline = CapabilityEvaluator::default()
        .start_job(Arc::clone(&scene), 23, PolicyJobLimits::default())
        .unwrap();
    while baseline.poll(one(), &Never) == PolicyJobPoll::Pending {}
    let work = baseline.stats().work_units();
    assert!(work > 4);

    for boundary in 0..work.saturating_sub(1) {
        let mut job = CapabilityEvaluator::default()
            .start_job(Arc::clone(&scene), 23, PolicyJobLimits::default())
            .unwrap();
        for _ in 0..boundary {
            if job.poll(one(), &Never) == PolicyJobPoll::Ready {
                break;
            }
        }
        if job.result().is_some() {
            continue;
        }
        assert_eq!(job.poll(one(), &Cancelled), PolicyJobPoll::Ready);
        assert_eq!(
            job.result().unwrap().unwrap_err().code(),
            PolicyErrorCode::Cancelled
        );
        assert_eq!(job.stats().retained_bytes(), 0);
        assert_eq!(job.poll(one(), &Never), PolicyJobPoll::Ready);
    }
}

#[test]
fn atomic_and_retained_one_less_limits_fail_without_partial_publication() {
    let scene = Arc::new(scene(&[]));
    let mut baseline = CapabilityEvaluator::default()
        .start_job(Arc::clone(&scene), 23, PolicyJobLimits::default())
        .unwrap();
    while baseline.poll(many(), &Never) == PolicyJobPoll::Pending {}
    let stats = baseline.stats();

    let atomic_one_less = PolicyJobLimits::validate(PolicyJobLimitConfig {
        max_atomic_canonical_bytes: stats.atomic_canonical_bytes() - 1,
        max_retained_bytes: PolicyJobLimitConfig::default().max_retained_bytes,
    })
    .unwrap();
    match CapabilityEvaluator::default().start_job(Arc::clone(&scene), 23, atomic_one_less) {
        Ok(mut job) => {
            while job.poll(many(), &Never) == PolicyJobPoll::Pending {}
            let error = job.result().unwrap().unwrap_err();
            assert_eq!(error.code(), PolicyErrorCode::ResourceLimit);
            assert_eq!(
                error.limit().unwrap().kind(),
                PolicyLimitKind::AtomicCanonicalBytes
            );
            assert_eq!(job.stats().retained_bytes(), 0);
        }
        Err(error) => {
            assert_eq!(error.code(), PolicyErrorCode::ResourceLimit);
            assert_eq!(
                error.limit().unwrap().kind(),
                PolicyLimitKind::AtomicCanonicalBytes
            );
        }
    }

    let atomic = PolicyJobLimitConfig::default().max_atomic_canonical_bytes;
    let exact_canonical_capacity = atomic * 2;
    PolicyJobLimits::validate(PolicyJobLimitConfig {
        max_atomic_canonical_bytes: atomic,
        max_retained_bytes: exact_canonical_capacity,
    })
    .unwrap();
    let retained_one_less = PolicyJobLimits::validate(PolicyJobLimitConfig {
        max_atomic_canonical_bytes: atomic,
        max_retained_bytes: exact_canonical_capacity - 1,
    })
    .unwrap_err();
    assert_eq!(retained_one_less.code(), PolicyErrorCode::InvalidLimits);
}

#[test]
fn render_plan_job_is_incremental_equivalent_and_terminal_replay_is_work_free() {
    let scene = Arc::new(scene(&[]));
    let decision = evaluate(&scene, 23);
    let args = || {
        (
            fast_config(),
            request(41, 1024, 1024),
            RendererEpoch::new(7).unwrap(),
            PolicyLimits::default(),
        )
    };
    let (config, request, epoch, limits) = args();
    let mut incremental = RenderPlanJob::new(
        Arc::clone(&scene),
        decision.clone(),
        config,
        request,
        epoch,
        limits,
        PolicyJobLimits::default(),
    )
    .unwrap();
    let mut pending = 0;
    while incremental.poll(one(), &Never) == PolicyJobPoll::Pending {
        pending += 1;
    }
    assert!(pending > 4);
    let ready_work = incremental.stats().work_units();
    assert_eq!(incremental.poll(one(), &Never), PolicyJobPoll::Ready);
    assert_eq!(incremental.stats().work_units(), ready_work);
    let incremental = incremental.take_result().unwrap().unwrap();

    let (config, request, epoch, limits) = args();
    let mut batched = RenderPlanJob::new(
        scene,
        decision,
        config,
        request,
        epoch,
        limits,
        PolicyJobLimits::default(),
    )
    .unwrap();
    while batched.poll(many(), &Never) == PolicyJobPoll::Pending {}
    let batched = batched.take_result().unwrap().unwrap();
    assert_eq!(incremental, batched);
}

#[test]
fn cancellation_between_render_plan_tiles_cleans_all_job_owned_capacity() {
    let scene = Arc::new(scene(&[]));
    let decision = evaluate(&scene, 23);
    let mut job = RenderPlanJob::new(
        scene,
        decision,
        fast_config(),
        request(41, 1024, 1024),
        RendererEpoch::new(7).unwrap(),
        PolicyLimits::default(),
        PolicyJobLimits::default(),
    )
    .unwrap();
    for _ in 0..8 {
        assert_eq!(job.poll(one(), &Never), PolicyJobPoll::Pending);
    }
    assert_eq!(job.poll(one(), &Cancelled), PolicyJobPoll::Ready);
    assert_eq!(
        job.result().unwrap().unwrap_err().code(),
        PolicyErrorCode::Cancelled
    );
    assert_eq!(job.stats().retained_bytes(), 0);
}

#[test]
fn oversized_poll_budget_is_rejected() {
    let error = PolicyPollBudget::new(NonZeroU32::new(4_097).unwrap()).unwrap_err();
    assert_eq!(error.code(), PolicyErrorCode::ResourceLimit);
    assert_eq!(
        error.limit().unwrap().kind(),
        PolicyLimitKind::PollWorkUnits
    );
}
