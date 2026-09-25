use std::{future::Ready, marker::PhantomData};

use anyhow::{Result, anyhow};

use crate::{
    BenchmarkCase, GpuHandle, GpuRequirements, IterationRecorder, Repeatability, Suite,
    ValidationContext,
};

type DefaultPrepare<F> = fn(&mut F, &mut wgpu::CommandEncoder) -> Result<()>;
type DefaultValidate<F> = fn(&mut F, &mut ValidationContext<'_>) -> Ready<Result<()>>;

#[must_use = "call .register() to add the benchmark to the suite"]
pub struct CaseBuilder<
    'suite,
    F = (),
    Setup = (),
    Record = (),
    Prepare = DefaultPrepare<F>,
    Validate = DefaultValidate<F>,
> {
    registration: Registration<'suite>,
    setup: Setup,
    record: Record,
    prepare: Prepare,
    validate: Validate,
    fixture: PhantomData<fn() -> F>,
}

struct Registration<'suite> {
    suite: &'suite mut Suite,
    label: Box<str>,
    requirements: GpuRequirements,
    repeatability: Option<Repeatability>,
}

impl Suite {
    /// starts a benchmark; call `.setup(...)`, `.record(...)`, and `.register()`
    /// to add it to this suite.
    pub fn bench(&mut self, label: impl Into<Box<str>>) -> CaseBuilder<'_> {
        CaseBuilder {
            registration: Registration {
                suite: self,
                label: label.into(),
                requirements: GpuRequirements::default(),
                repeatability: None,
            },
            setup: (),
            record: (),
            prepare: |_, _| Ok(()),
            validate: |_, _| std::future::ready(Ok(())),
            fixture: PhantomData,
        }
    }
}

impl<'suite, F, Setup, Record, Prepare, Validate>
    CaseBuilder<'suite, F, Setup, Record, Prepare, Validate>
{
    /// permits multiple iterations in a timed sample.
    pub fn repeatable(mut self) -> Self {
        self.registration.repeatability = Some(Repeatability::Repeatable);

        self
    }

    /// limits each timed sample to one iteration.
    pub fn single_iteration(mut self) -> Self {
        self.registration.repeatability = Some(Repeatability::SingleIteration);

        self
    }

    /// sets the capabilities required before setup runs.
    pub fn requirements(mut self, requirements: GpuRequirements) -> Self {
        self.registration.requirements = requirements;

        self
    }
}

impl<'suite> CaseBuilder<'suite> {
    /// creates the resources shared by every iteration of this case.
    pub fn setup<F: 'static, Setup>(self, setup: Setup) -> CaseBuilder<'suite, F, Setup>
    where
        Setup: AsyncFn(&GpuHandle<'_>) -> Result<F> + 'static,
    {
        CaseBuilder {
            registration: self.registration,
            setup,
            record: (),
            prepare: |_, _| Ok(()),
            validate: |_, _| std::future::ready(Ok(())),
            fixture: PhantomData,
        }
    }
}

impl<'suite, F: 'static, Setup, Record, Prepare, Validate>
    CaseBuilder<'suite, F, Setup, Record, Prepare, Validate>
where
    Setup: AsyncFn(&GpuHandle<'_>) -> Result<F> + 'static,
{
    /// records the commands for one iteration.
    pub fn record<R>(self, record: R) -> CaseBuilder<'suite, F, Setup, R, Prepare, Validate>
    where
        R: Fn(&F, &mut IterationRecorder<'_>) -> Result<()> + 'static,
    {
        CaseBuilder {
            registration: self.registration,
            setup: self.setup,
            record,
            prepare: self.prepare,
            validate: self.validate,
            fixture: PhantomData,
        }
    }

    /// records untimed reset commands before each sample.
    pub fn prepare<P>(self, prepare: P) -> CaseBuilder<'suite, F, Setup, Record, P, Validate>
    where
        P: Fn(&mut F, &mut wgpu::CommandEncoder) -> Result<()> + 'static,
    {
        CaseBuilder {
            registration: self.registration,
            setup: self.setup,
            record: self.record,
            prepare,
            validate: self.validate,
            fixture: PhantomData,
        }
    }

    /// checks the recorded operation outside the timed samples.
    pub fn validate<V>(self, validate: V) -> CaseBuilder<'suite, F, Setup, Record, Prepare, V>
    where
        V: AsyncFn(&mut F, &mut ValidationContext<'_>) -> Result<()> + 'static,
    {
        CaseBuilder {
            registration: self.registration,
            setup: self.setup,
            record: self.record,
            prepare: self.prepare,
            validate,
            fixture: PhantomData,
        }
    }
}

impl<F: 'static, Setup, Record, Prepare, Validate>
    CaseBuilder<'_, F, Setup, Record, Prepare, Validate>
where
    Setup: AsyncFn(&GpuHandle<'_>) -> Result<F> + 'static,
    Record: Fn(&F, &mut IterationRecorder<'_>) -> Result<()> + 'static,
    Prepare: Fn(&mut F, &mut wgpu::CommandEncoder) -> Result<()> + 'static,
    Validate: AsyncFn(&mut F, &mut ValidationContext<'_>) -> Result<()> + 'static,
{
    /// validates the configuration and adds this case to the suite.
    pub fn register(self) -> Result<()> {
        let Registration {
            suite,
            label,
            requirements,
            repeatability,
        } = self.registration;
        let repeatability = repeatability.ok_or_else(|| {
            anyhow!("case {label}: choose .repeatable() or .single_iteration() before registration")
        })?;

        suite.register_case(
            label,
            ClosureCase {
                requirements,
                repeatability,
                setup: self.setup,
                record: self.record,
                prepare: self.prepare,
                validate: self.validate,
                fixture: PhantomData,
            },
        )
    }
}

struct ClosureCase<F, Setup, Record, Prepare, Validate> {
    requirements: GpuRequirements,
    repeatability: Repeatability,
    setup: Setup,
    record: Record,
    prepare: Prepare,
    validate: Validate,
    fixture: PhantomData<fn() -> F>,
}

impl<F: 'static, Setup, Record, Prepare, Validate> BenchmarkCase
    for ClosureCase<F, Setup, Record, Prepare, Validate>
where
    Setup: AsyncFn(&GpuHandle<'_>) -> Result<F> + 'static,
    Record: Fn(&F, &mut IterationRecorder<'_>) -> Result<()> + 'static,
    Prepare: Fn(&mut F, &mut wgpu::CommandEncoder) -> Result<()> + 'static,
    Validate: AsyncFn(&mut F, &mut ValidationContext<'_>) -> Result<()> + 'static,
{
    type Fixture = F;

    fn requirements(&self) -> GpuRequirements {
        self.requirements.clone()
    }

    fn repeatability(&self) -> Repeatability {
        self.repeatability
    }

    async fn setup(&self, gpu: &GpuHandle<'_>) -> Result<F> {
        (self.setup)(gpu).await
    }

    fn prepare(&self, fixture: &mut F, encoder: &mut wgpu::CommandEncoder) -> Result<()> {
        (self.prepare)(fixture, encoder)
    }

    fn record_iteration(&self, fixture: &F, recorder: &mut IterationRecorder<'_>) -> Result<()> {
        (self.record)(fixture, recorder)
    }

    async fn validate_iteration(
        &self,
        fixture: &mut F,
        context: &mut ValidationContext<'_>,
    ) -> Result<()> {
        (self.validate)(fixture, context).await
    }
}

#[cfg(test)]
mod tests {
    use std::{cell::Cell, rc::Rc};

    use super::*;

    #[test]
    fn registration_infers_fixtures_and_defers_borrowing_callbacks() -> Result<()> {
        struct Fixture {
            label: String,
        }

        let mut suite = Suite::new();
        let calls = Rc::new(Cell::new(0));
        let setup_calls = calls.clone();
        let label = String::from("owned configuration");
        let requirements = GpuRequirements::default()
            .with_required_features(wgpu::Features::TIMESTAMP_QUERY_INSIDE_PASSES);

        suite
            .bench("first")
            .requirements(requirements.clone())
            .single_iteration()
            .setup(async move |gpu| {
                let label = &label;
                std::future::ready(()).await;
                let _ = gpu.device();
                setup_calls.set(setup_calls.get() + 1);

                Ok(Fixture {
                    label: label.clone(),
                })
            })
            .prepare(|fixture, _encoder| {
                fixture.label.clear();

                Ok(())
            })
            .record(|fixture, _recorder| {
                assert!(fixture.label.is_empty());

                Ok(())
            })
            .validate(async |fixture, context| {
                let label = &mut fixture.label;
                std::future::ready(()).await;
                let _ = context.gpu();
                label.push_str("validated");

                Ok(())
            })
            .register()?;

        suite
            .bench("second")
            .setup(async |_| Ok(vec![1_u32, 2, 3]))
            .record(|fixture, _recorder| {
                assert_eq!(fixture.len(), 3);

                Ok(())
            })
            .repeatable()
            .register()?;

        let cases = suite.cases().collect::<Vec<_>>();
        assert_eq!(cases.len(), 2);
        assert_eq!(cases[0].case_id().as_ref(), "first");
        assert_eq!(cases[0].requirements(), requirements);
        assert_eq!(cases[0].repeatability(), Repeatability::SingleIteration);
        assert_eq!(cases[1].requirements(), GpuRequirements::default());
        assert_eq!(cases[1].repeatability(), Repeatability::Repeatable);
        assert_eq!(calls.get(), 0);

        Ok(())
    }

    #[test]
    fn failed_registration_leaves_the_suite_unchanged() -> Result<()> {
        let mut suite = Suite::new();
        let error = suite
            .bench("retry")
            .setup(async |_| Ok(()))
            .record(|_, _| Ok(()))
            .register()
            .unwrap_err();
        assert!(error.to_string().contains("case retry"));
        assert!(error.to_string().contains("repeatable"));
        assert_eq!(suite.cases().count(), 0);

        for label in ["", " leading", "trailing ", "bad/name", "bad\nname"] {
            assert!(register_empty(&mut suite, label).is_err());
        }
        assert_eq!(suite.cases().count(), 0);

        register_empty(&mut suite, "retry")?;
        let error = register_empty(&mut suite, "retry").unwrap_err();
        assert!(error.to_string().contains("duplicate case"));
        assert_eq!(suite.cases().count(), 1);

        Ok(())
    }

    fn register_empty(suite: &mut Suite, label: &str) -> Result<()> {
        suite
            .bench(label)
            .repeatable()
            .setup(async |_| Ok(()))
            .record(|_, _| Ok(()))
            .register()
    }
}
