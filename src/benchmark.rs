/*
* benchmark terminology
* - suite: a collection of registered benchmark cases
* - case: a benchmark with a specific configuration (e.g. prefix sum over 16,384 elements)
* - run: one execution of the selected cases in a suite
* - sample: one timed batch of iterations for a case
* - iteration: one complete execution of the case's operation ([`BenchmarkCase::record_iteration`])
* - pass: a compute or render pass recorded within an iteration
*/

use crate::{GpuHandle, GpuRequirements, ValidationContext, core::MAX_RECORDED_PASSES_PER_SAMPLE};
use anyhow::{Result, anyhow, ensure};
use std::{any::Any, collections::BTreeSet, num::NonZeroUsize, pin::Pin};

#[derive(Default)]
pub struct Suite {
    cases: Vec<Box<dyn ErasedBenchmarkCase>>,
    registered_case_ids: BTreeSet<Box<str>>,
}

impl Suite {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register_case<C: BenchmarkCase>(&mut self, case_id: CaseId, case: C) -> Result<()> {
        let id = case_id.full_id();
        ensure!(
            self.registered_case_ids.insert(id),
            "duplicate case: {:?}",
            case_id
        );

        self.cases.push(Box::new(CaseAdapter { case_id, case }));

        Ok(())
    }

    pub(crate) fn cases(&self) -> impl Iterator<Item = &dyn ErasedBenchmarkCase> {
        self.cases.iter().map(Box::as_ref)
    }
}

#[allow(async_fn_in_trait)]
pub trait BenchmarkCase: 'static {
    /// the resources this benchmark needs to run, created at setup
    type Fixture: 'static;

    fn requirements(&self) -> GpuRequirements {
        GpuRequirements::default()
    }

    /// whether multiple iterations can be batched into one sample
    fn repeatability(&self) -> Repeatability;

    /// create persistant pipelines, buffers, textures, and bind groups here
    /// basically everything your shader needs
    async fn setup(&self, gpu: &GpuHandle<'_>) -> Result<Self::Fixture>;

    /// records untimed reset or input copy commands before a sample
    fn prepare(
        &self,
        _fixture: &mut Self::Fixture,
        _encoder: &mut wgpu::CommandEncoder,
    ) -> Result<()> {
        Ok(())
    }

    /// records the gpu commands for one iteration
    fn record_iteration(
        &self,
        fixture: &Self::Fixture,
        recorder: &mut IterationRecorder<'_>,
    ) -> Result<()>;

    /// reads out the result and checks for correctness
    ///
    /// note! this is purely optional and will require copying data back into the cpu
    async fn validate_iteration(
        &self,
        _fixture: &mut Self::Fixture,
        _context: &mut ValidationContext<'_>,
    ) -> Result<()> {
        Ok(())
    }
}

#[derive(Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct CaseId {
    // what you're benchmarking
    function: Box<str>,
    // what configuration you're benchmarking
    parameter: Box<str>,
}

impl CaseId {
    pub fn try_new(function: impl Into<Box<str>>, parameter: impl Into<Box<str>>) -> Result<Self> {
        let f = function.into();
        let p = parameter.into();

        let valid_label = |s: &str| {
            let invalid = s.is_empty()
                || s.contains('/')
                || s.chars().any(char::is_control)
                || *s.trim() != *s;

            !invalid
        };

        ensure!(valid_label(&f), "invalid function name");
        ensure!(valid_label(&p), "invalid parameter name");

        Ok(Self {
            function: f,
            parameter: p,
        })
    }

    pub fn full_id(&self) -> Box<str> {
        format!("{}/{}", self.function, self.parameter).into_boxed_str()
    }
}

impl std::fmt::Display for CaseId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}/{}", self.function, self.parameter)
    }
}

/// whether one sample may contain multiple iterations of the workload.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Repeatability {
    Repeatable,
    SingleIteration,
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum PassId {
    Compute(Box<str>),
}

#[derive(Debug)]
pub(crate) struct IterationTiming<'a> {
    pub(crate) query_set: &'a wgpu::QuerySet,
    pub(crate) expected_pass_count: NonZeroUsize,
    pub(crate) start_query: Option<u32>,
    pub(crate) end_query: Option<u32>,
}

/// records the GPU passes for one case iteration
pub struct IterationRecorder<'a> {
    encoder: &'a mut wgpu::CommandEncoder,

    // the passes recorded during this iteration
    passes: Vec<PassId>,

    /// whether to add timestamp markers to this iteration's passes
    ///
    /// `timings: None` instructs the recorder to record the passes without timestamp markers
    timings: Option<IterationTiming<'a>>,
}

impl<'a> IterationRecorder<'a> {
    pub(crate) fn new(
        encoder: &'a mut wgpu::CommandEncoder,
        timings: Option<IterationTiming<'a>>,
    ) -> Self {
        Self {
            encoder,
            passes: Vec::new(),
            timings,
        }
    }

    pub fn compute_pass<R>(
        &mut self,
        name: impl Into<Box<str>>,
        record: impl FnOnce(&mut wgpu::ComputePass<'_>) -> Result<R>,
    ) -> Result<R> {
        ensure!(
            self.passes.len() < MAX_RECORDED_PASSES_PER_SAMPLE,
            "one iteration exceeds the sample pass budget"
        );

        let name = name.into();
        let pass_index = self.passes.len();
        let timestamp_writes = self.timings.as_ref().and_then(|t| {
            let start = (pass_index == 0).then_some(t.start_query).flatten();
            let end = (pass_index == t.expected_pass_count.get() - 1)
                .then_some(t.end_query)
                .flatten();

            if start.is_none() && end.is_none() {
                return None;
            }

            Some(wgpu::ComputePassTimestampWrites {
                query_set: t.query_set,
                beginning_of_pass_write_index: start,
                end_of_pass_write_index: end,
            })
        });

        let result = {
            let mut pass = self
                .encoder
                .begin_compute_pass(&wgpu::ComputePassDescriptor {
                    label: Some(&name),
                    timestamp_writes,
                });

            record(&mut pass)
        };

        self.passes.push(PassId::Compute(name));

        result
    }

    pub(crate) fn finish(self) -> Result<Vec<PassId>> {
        if let Some(timings) = self.timings {
            let recorded_pass_count = self.passes.len();
            let expected_pass_count = usize::from(timings.expected_pass_count);
            ensure!(recorded_pass_count == expected_pass_count);
        }

        Ok(self.passes)
    }
}

/// a boxed future for type-erased asynchronous lifecycle methods
type LocalFuture<'a, T> = Pin<Box<dyn Future<Output = T> + 'a>>;

pub(crate) type ErasedFixture = Box<dyn Any>;

pub(crate) trait ErasedBenchmarkCase {
    fn case_id(&self) -> &CaseId;

    fn requirements(&self) -> GpuRequirements;

    fn repeatability(&self) -> Repeatability;

    fn setup<'a>(&'a self, gpu: &'a GpuHandle<'_>) -> LocalFuture<'a, Result<ErasedFixture>>;

    fn prepare(&self, fixture: &mut dyn Any, encoder: &mut wgpu::CommandEncoder) -> Result<()>;

    fn record_iteration(
        &self,
        fixture: &dyn Any,
        recorder: &mut IterationRecorder<'_>,
    ) -> Result<()>;

    fn validate_iteration<'a>(
        &'a self,
        fixture: &'a mut dyn Any,
        context: &'a mut ValidationContext<'_>,
    ) -> LocalFuture<'a, Result<()>>;
}

/// adapts the entire case to the runner's interface
///
/// the suite has to store different case types in one collection, but each case
/// expects its own fixture type
struct CaseAdapter<C: BenchmarkCase> {
    case_id: CaseId,
    case: C,
}

impl<C: BenchmarkCase> CaseAdapter<C> {
    fn fixture<'a>(&self, f: &'a dyn Any) -> Result<&'a C::Fixture> {
        f.downcast_ref::<C::Fixture>().ok_or_else(|| {
            anyhow!(
                "internal fixture type mismatch for case: {:?}",
                self.case_id
            )
        })
    }

    fn fixture_mut<'a>(&self, f: &'a mut dyn Any) -> Result<&'a mut C::Fixture> {
        f.downcast_mut::<C::Fixture>().ok_or_else(|| {
            anyhow!(
                "internal fixture type mismatch for case: {:?}",
                self.case_id
            )
        })
    }
}

impl<C: BenchmarkCase> ErasedBenchmarkCase for CaseAdapter<C> {
    fn case_id(&self) -> &CaseId {
        &self.case_id
    }

    fn requirements(&self) -> GpuRequirements {
        self.case.requirements()
    }

    fn repeatability(&self) -> Repeatability {
        self.case.repeatability()
    }

    fn setup<'a>(&'a self, gpu: &'a GpuHandle<'_>) -> LocalFuture<'a, Result<ErasedFixture>> {
        Box::pin(async move {
            let f = self.case.setup(gpu).await?;

            Ok::<ErasedFixture, anyhow::Error>(Box::new(f))
        })
    }

    fn prepare(&self, fixture: &mut dyn Any, encoder: &mut wgpu::CommandEncoder) -> Result<()> {
        self.case.prepare(self.fixture_mut(fixture)?, encoder)
    }

    fn record_iteration(
        &self,
        fixture: &dyn Any,
        recorder: &mut IterationRecorder<'_>,
    ) -> Result<()> {
        self.case.record_iteration(self.fixture(fixture)?, recorder)
    }

    fn validate_iteration<'a>(
        &'a self,
        fixture: &'a mut dyn Any,
        context: &'a mut ValidationContext<'_>,
    ) -> LocalFuture<'a, Result<()>> {
        Box::pin(async move {
            self.case
                .validate_iteration(self.fixture_mut(fixture)?, context)
                .await
        })
    }
}
