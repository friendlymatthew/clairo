use crate::{
    CaseSelection, ErasedBenchmarkCase, GpuRequirements, GpuSession, IterationRecorder,
    IterationTiming, PassId, Repeatability, Suite, ValidationContext,
};
use anyhow::{Context, Result, anyhow, bail, ensure};
use futures_channel::oneshot;
use std::{
    any::Any,
    num::{NonZeroU32, NonZeroUsize},
    time::{Duration, Instant},
};

const SUBMISSION_TIMEOUT: Duration = Duration::from_secs(15);
const TARGET_SAMPLE_DURATION: Duration = Duration::from_millis(5);
const PRECONDITIONING_DURATION: Duration = Duration::from_secs(1);
pub const MAX_RECORDED_PASSES_PER_SAMPLE: usize = 2_047;

/// the measured samples for one case, excluding calibration and preconditioning.
#[derive(Debug)]
pub struct CaseMeasurements {
    pub case_id: Box<str>,
    pub iterations_per_sample: NonZeroU32,
    /// average GPU nanoseconds per iteration within each sample.
    pub samples_ns_per_iteration: Vec<f64>,
    pub elapsed: Duration,
}

/// descriptive statistics over the measured sample averages, in nanoseconds per iteration.
#[derive(Debug)]
pub struct CaseSummary {
    pub case_id: Box<str>,
    pub sample_count: usize,
    pub iterations_per_sample: NonZeroU32,
    pub mean_ns_per_iteration: f64,
    pub median_ns_per_iteration: f64,
    pub min_ns_per_iteration: f64,
    pub max_ns_per_iteration: f64,
    /// unavailable with fewer than two samples.
    pub sample_standard_deviation_ns_per_iteration: Option<f64>,
    /// sample standard deviation divided by the mean; a ratio, not a percentage.
    /// unavailable with fewer than two samples or a zero mean.
    pub coefficient_of_variation: Option<f64>,
    pub first_quartile_ns_per_iteration: f64,
    pub third_quartile_ns_per_iteration: f64,
    pub interquartile_range_ns_per_iteration: f64,
    /// total host duration, including setup, calibration, and validation.
    pub elapsed: Duration,
}

impl TryFrom<&CaseMeasurements> for CaseSummary {
    type Error = anyhow::Error;

    fn try_from(measurements: &CaseMeasurements) -> Result<Self> {
        use crate::statistics::{mean_f64, quantile_sorted, sample_standard_deviation};

        let samples = &measurements.samples_ns_per_iteration;
        ensure!(
            !samples.is_empty(),
            "cannot summarize a case without samples"
        );
        ensure!(
            samples
                .iter()
                .all(|sample| sample.is_finite() && *sample > 0.0),
            "sample timings must be finite and positive"
        );

        let mean = mean_f64(samples);
        ensure!(
            mean.is_finite() && mean > 0.0,
            "sample mean is not finite and positive"
        );
        let standard_deviation = sample_standard_deviation(samples, mean);
        ensure!(
            standard_deviation.is_none_or(f64::is_finite),
            "sample standard deviation overflowed"
        );
        let coefficient_of_variation = standard_deviation.map(|deviation| deviation / mean);
        ensure!(
            coefficient_of_variation.is_none_or(f64::is_finite),
            "coefficient of variation overflowed"
        );

        let mut sorted = samples.clone();
        sorted.sort_by(f64::total_cmp);
        let first_quartile = quantile_sorted(&sorted, 0.25);
        let third_quartile = quantile_sorted(&sorted, 0.75);

        Ok(Self {
            case_id: measurements.case_id.clone(),
            sample_count: samples.len(),
            iterations_per_sample: measurements.iterations_per_sample,
            mean_ns_per_iteration: mean,
            median_ns_per_iteration: quantile_sorted(&sorted, 0.5),
            min_ns_per_iteration: sorted[0],
            max_ns_per_iteration: sorted[sorted.len() - 1],
            sample_standard_deviation_ns_per_iteration: standard_deviation,
            coefficient_of_variation,
            first_quartile_ns_per_iteration: first_quartile,
            third_quartile_ns_per_iteration: third_quartile,
            interquartile_range_ns_per_iteration: third_quartile - first_quartile,
            elapsed: measurements.elapsed,
        })
    }
}

pub async fn run_suite(
    suite: &Suite,
    selection: CaseSelection,
    backends: wgpu::Backends,
    sample_size: NonZeroU32,
) -> Result<Vec<CaseMeasurements>> {
    ensure!(
        !cfg!(target_arch = "wasm32"),
        "GPU execution currently requires a native target"
    );

    let cases = suite
        .cases()
        .filter(move |case| selection.matches(case.case_id()))
        .collect::<Vec<_>>();

    if cases.is_empty() {
        return Ok(Vec::new());
    }

    let requirements = cases.iter().fold(
        GpuRequirements::default(),
        |requirements: GpuRequirements, case| {
            let left = requirements;
            let right = case.requirements();

            GpuRequirements::new(
                left.required_features() | right.required_features(),
                left.required_limits()
                    .clone()
                    .or_better_values_from(right.required_limits()),
            )
        },
    );

    let session = GpuSession::try_request(backends, requirements).await?;

    let mut results = Vec::with_capacity(cases.len());
    for case in cases {
        let measurements = execute_case(case, &session, sample_size)
            .await
            .with_context(|| format!("case {} failed", case.case_id()))?;

        results.push(measurements);
    }

    Ok(results)
}

fn fail_on_device_error(session: &GpuSession, stage: &str, index: Option<u32>) -> Result<()> {
    let messages = session.errors().snapshot();
    if messages.is_empty() {
        return Ok(());
    }

    bail!(
        "case failed at stage {}, observation {:?}\nmessage: {:?}",
        stage,
        index,
        messages
    );
}

async fn execute_case(
    case: &dyn ErasedBenchmarkCase,
    session: &GpuSession,
    sample_size: NonZeroU32,
) -> Result<CaseMeasurements> {
    let case_started = Instant::now();
    fail_on_device_error(session, "before_setup", None)?;

    let mut fixture = case.setup(&session.gpu()).await?;
    fail_on_device_error(session, "setup", None)?;

    let passes_per_iteration =
        execute_warmup(case, session, fixture.as_mut(), SUBMISSION_TIMEOUT).await?;
    fail_on_device_error(session, "warmup", None)?;

    let num_passes_per_iteration = NonZeroUsize::new(passes_per_iteration.len())
        .ok_or_else(|| anyhow!("passes must be nonzero"))?;

    let maximum = maximum_iterations_per_sample(num_passes_per_iteration);
    validate_case(case, session, fixture.as_mut(), "warmup_validation").await?;

    let slot = ObservationSlot::new(session.device());
    fail_on_device_error(session, "timestamp_resources", None)?;

    let iterations = match case.repeatability() {
        Repeatability::SingleIteration => NonZeroU32::MIN,
        Repeatability::Repeatable => {
            let pilot_ns = collect_observation(
                case,
                session,
                fixture.as_mut(),
                &passes_per_iteration,
                &slot,
                NonZeroU32::MIN,
            )
            .await
            .context("calibration pilot failed")?;
            let mut iterations = predict_iteration_count(pilot_ns, maximum)?;

            if iterations > NonZeroU32::MIN {
                let candidate_ns = collect_observation(
                    case,
                    session,
                    fixture.as_mut(),
                    &passes_per_iteration,
                    &slot,
                    iterations,
                )
                .await
                .context("calibration candidate failed")?;
                ensure!(
                    candidate_ns > 0.0,
                    "calibration candidate is below timestamp resolution"
                );

                let corrected =
                    predict_iteration_count(candidate_ns / f64::from(iterations.get()), maximum)?;
                if corrected != iterations {
                    iterations = corrected;
                    let corrected_ns = collect_observation(
                        case,
                        session,
                        fixture.as_mut(),
                        &passes_per_iteration,
                        &slot,
                        iterations,
                    )
                    .await
                    .context("corrected calibration candidate failed")?;
                    ensure!(
                        corrected_ns > 0.0,
                        "corrected candidate is below timestamp resolution"
                    );
                }
            }

            validate_case(case, session, fixture.as_mut(), "calibration_validation").await?;
            iterations
        }
    };

    let preconditioning_started = Instant::now();
    loop {
        let duration_ns = collect_observation(
            case,
            session,
            fixture.as_mut(),
            &passes_per_iteration,
            &slot,
            iterations,
        )
        .await
        .context("preconditioning failed")?;
        ensure!(duration_ns > 0.0, "sample is below timestamp resolution");

        if preconditioning_started.elapsed() >= PRECONDITIONING_DURATION {
            break;
        }
    }
    validate_case(
        case,
        session,
        fixture.as_mut(),
        "preconditioning_validation",
    )
    .await?;

    let mut samples_ns_per_iteration = Vec::new();
    for index in 0..sample_size.get() {
        let duration_ns = collect_observation(
            case,
            session,
            fixture.as_mut(),
            &passes_per_iteration,
            &slot,
            iterations,
        )
        .await
        .with_context(|| format!("measurement sample {index} failed"))?;
        ensure!(
            duration_ns > 0.0,
            "measurement sample {index} is below timestamp resolution"
        );
        samples_ns_per_iteration.push(duration_ns / f64::from(iterations.get()));
    }
    validate_case(case, session, fixture.as_mut(), "final_validation").await?;

    Ok(CaseMeasurements {
        case_id: case.case_id().full_id(),
        iterations_per_sample: iterations,
        samples_ns_per_iteration,
        elapsed: case_started.elapsed(),
    })
}

fn maximum_iterations_per_sample(num_passes_per_iteration: NonZeroUsize) -> NonZeroU32 {
    let maximum = (MAX_RECORDED_PASSES_PER_SAMPLE / num_passes_per_iteration.get()).max(1);
    let maximum = u32::try_from(maximum)
        .expect("the maximum recorded pass budget must fit the iteration-count type");

    NonZeroU32::new(maximum).expect("the maximum iteration count must be nonzero")
}

fn predict_iteration_count(duration_ns: f64, maximum: NonZeroU32) -> Result<NonZeroU32> {
    ensure!(
        duration_ns.is_finite() && duration_ns >= 0.0,
        "invalid calibration duration"
    );
    if duration_ns == 0.0 {
        return Ok(maximum);
    }

    let target_ns = TARGET_SAMPLE_DURATION.as_secs_f64() * 1_000_000_000.0;
    let count = (target_ns / duration_ns)
        .round()
        .clamp(1.0, f64::from(maximum.get()));
    let count = format!("{count:.0}").parse::<u32>()?;

    NonZeroU32::new(count).ok_or_else(|| anyhow!("calibration produced zero iterations"))
}

async fn validate_case(
    case: &dyn ErasedBenchmarkCase,
    session: &GpuSession,
    fixture: &mut dyn Any,
    stage: &str,
) -> Result<()> {
    let mut context = ValidationContext::new(session.gpu(), SUBMISSION_TIMEOUT);
    case.validate_iteration(fixture, &mut context)
        .await
        .with_context(|| stage.to_owned())?;
    let submission = session.queue().submit([]);
    await_submission(
        session.device(),
        session.queue(),
        submission,
        SUBMISSION_TIMEOUT,
    )
    .await?;

    fail_on_device_error(session, stage, None)
}

struct ObservationSlot {
    query_set: wgpu::QuerySet,
    resolve_buffer: wgpu::Buffer,
    readback_buffer: wgpu::Buffer,
}

impl ObservationSlot {
    fn new(device: &wgpu::Device) -> Self {
        let query_set = device.create_query_set(&wgpu::QuerySetDescriptor {
            label: Some("clairo sample timestamps"),
            ty: wgpu::QueryType::Timestamp,
            count: 2,
        });
        let resolve_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("clairo timestamp resolve"),
            size: 2 * u64::from(wgpu::QUERY_SIZE),
            usage: wgpu::BufferUsages::QUERY_RESOLVE | wgpu::BufferUsages::COPY_SRC,
            mapped_at_creation: false,
        });
        let readback_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("clairo timestamp readback"),
            size: 2 * u64::from(wgpu::QUERY_SIZE),
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });

        Self {
            query_set,
            resolve_buffer,
            readback_buffer,
        }
    }
}

async fn collect_observation(
    case: &dyn ErasedBenchmarkCase,
    session: &GpuSession,
    fixture: &mut dyn Any,
    expected_passes: &[PassId],
    slot: &ObservationSlot,
    iterations: NonZeroU32,
) -> Result<f64> {
    let deadline = Instant::now()
        .checked_add(SUBMISSION_TIMEOUT)
        .ok_or_else(|| anyhow!("sample deadline overflowed"))?;
    let remaining = || {
        deadline
            .checked_duration_since(Instant::now())
            .filter(|duration| !duration.is_zero())
            .ok_or_else(|| anyhow!("sample deadline expired"))
    };
    let expected_pass_count = NonZeroUsize::new(expected_passes.len())
        .ok_or_else(|| anyhow!("sample must contain passes"))?;

    let mut preparation =
        session
            .device()
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("clairo sample preparation"),
            });
    case.prepare(fixture, &mut preparation)?;
    let timeout = remaining()?;
    let submission = session.queue().submit([preparation.finish()]);
    await_submission(session.device(), session.queue(), submission, timeout).await?;
    fail_on_device_error(session, "sample_preparation", None)?;

    let mut encoder = session
        .device()
        .create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("clairo measured sample"),
        });
    for iteration in 0..iterations.get() {
        remaining()?;
        let timing = IterationTiming {
            query_set: &slot.query_set,
            expected_pass_count,
            start_query: (iteration == 0).then_some(0),
            end_query: (iteration == iterations.get() - 1).then_some(1),
        };
        let mut recorder = IterationRecorder::new(&mut encoder, Some(timing));
        case.record_iteration(fixture, &mut recorder)?;
        ensure!(
            recorder.finish()? == expected_passes,
            "pass sequence changed at iteration {iteration}"
        );
    }
    fail_on_device_error(session, "sample_recording", None)?;
    let timeout = remaining()?;
    let submission = session.queue().submit([encoder.finish()]);
    await_submission(session.device(), session.queue(), submission, timeout).await?;
    fail_on_device_error(session, "sample_execution", None)?;

    let mut encoder = session
        .device()
        .create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("clairo timestamp readback"),
        });
    encoder.resolve_query_set(&slot.query_set, 0..2, &slot.resolve_buffer, 0);
    encoder.copy_buffer_to_buffer(
        &slot.resolve_buffer,
        0,
        &slot.readback_buffer,
        0,
        slot.readback_buffer.size(),
    );
    let timeout = remaining()?;
    let submission = session.queue().submit([encoder.finish()]);
    let bytes = crate::gpu::read_mapped_buffer(
        session.device(),
        &slot.readback_buffer,
        submission,
        timeout,
    )?;
    fail_on_device_error(session, "timestamp_readback", None)?;

    let begin = u64::from_ne_bytes(bytes[..8].try_into()?);
    let end = u64::from_ne_bytes(bytes[8..16].try_into()?);
    ensure!(
        begin != u64::MAX && end != u64::MAX,
        "invalid GPU timestamp sentinel"
    );
    let ticks = end
        .checked_sub(begin)
        .ok_or_else(|| anyhow!("GPU timestamps are reversed"))?;
    let high = u32::try_from(ticks >> 32)?;
    let low = u32::try_from(ticks & u64::from(u32::MAX))?;
    let duration_ns = (f64::from(high) * 4_294_967_296.0 + f64::from(low))
        * f64::from(session.queue().get_timestamp_period());

    Ok(duration_ns)
}

async fn await_submission(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    submission: wgpu::SubmissionIndex,
    timeout: Duration,
) -> Result<()> {
    let (sender, receiver) = oneshot::channel();
    queue.on_submitted_work_done(move || {
        let _ = sender.send(());
    });

    #[cfg(not(target_arch = "wasm32"))]
    device
        .poll(wgpu::PollType::Wait {
            submission_index: Some(submission),
            timeout: Some(timeout),
        })
        .map_err(|error| anyhow!("gpu submission wait time failed: {error}"))?;

    #[cfg(target_arch = "wasm32")]
    let _ = (device, submission, timeout);

    receiver
        .await
        .map_err(|_| anyhow!("GPU completion callback was canceled"))?;

    Ok(())
}

async fn execute_warmup(
    case: &dyn ErasedBenchmarkCase,
    session: &GpuSession,
    fixture: &mut dyn Any,
    timeout: Duration,
) -> Result<Vec<PassId>> {
    let deadline = Instant::now()
        .checked_add(timeout)
        .ok_or_else(|| anyhow!("warmup deadline exceeded"))?;

    let mut prep = session
        .device()
        .create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("clairo untimed warm up preparation"),
        });

    case.prepare(fixture, &mut prep)?;

    let prep_submission = session.queue().submit([prep.finish()]);

    let compute_remaining = || {
        deadline
            .checked_duration_since(Instant::now())
            .ok_or_else(|| anyhow!("gpu operation deadline expired"))
    };

    await_submission(
        session.device(),
        session.queue(),
        prep_submission,
        compute_remaining()?,
    )
    .await?;

    fail_on_device_error(session, "warmup_preparation", None)?;

    let mut encoder = session
        .device()
        .create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("clairo untimed shape discovery warm-up"),
        });

    let mut recorder = IterationRecorder::new(&mut encoder, None);
    case.record_iteration(fixture, &mut recorder)?;

    let workload_shape = recorder.finish()?;

    let warmup_submission = session.queue().submit([encoder.finish()]);
    await_submission(
        session.device(),
        session.queue(),
        warmup_submission,
        compute_remaining()?,
    )
    .await?;

    Ok(workload_shape)
}
