use std::num::NonZeroU32;

use crate::{CaseId, CaseMeasurements, Suite, core::run_suite};
use anyhow::Result;

pub(crate) const DEFAULT_SAMPLE_SIZE: NonZeroU32 = NonZeroU32::new(100).unwrap();

#[derive(Debug)]
pub struct BenchmarkRunner {
    pub selection: CaseSelection,
    pub backends: wgpu::Backends,
    pub sample_size: NonZeroU32,
    // pub plan_policy: Calibrate | Replay,
}

impl Default for BenchmarkRunner {
    fn default() -> Self {
        Self {
            selection: CaseSelection::All,
            backends: wgpu::Backends::all(),
            sample_size: DEFAULT_SAMPLE_SIZE,
        }
    }
}

impl BenchmarkRunner {
    pub fn try_run(
        self,
        register: impl FnOnce(&mut Suite) -> Result<()>,
    ) -> Result<Vec<CaseMeasurements>> {
        // parse arguments here i guess

        let mut suite = Suite::new();
        register(&mut suite)?;

        pollster::block_on(run_suite(
            &suite,
            self.selection,
            self.backends,
            self.sample_size,
        ))
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum CaseSelection {
    #[default]
    All,
    Containing(Box<str>),
    Exact(Box<str>),
}

impl CaseSelection {
    pub fn all() -> Self {
        Self::default()
    }

    pub fn containing(pattern: impl Into<Box<str>>) -> Self {
        Self::Containing(pattern.into())
    }

    pub fn exact(full_id: impl Into<Box<str>>) -> Self {
        Self::Exact(full_id.into())
    }

    pub fn matches(&self, case: &CaseId) -> bool {
        match &self {
            Self::All => true,
            Self::Containing(pattern) => case.as_ref().contains(pattern.as_ref()),
            Self::Exact(full_id) => case.as_ref() == full_id.as_ref(),
        }
    }
}
