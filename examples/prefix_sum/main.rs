mod workload;

use anyhow::Result;
use clairo::{BenchmarkRunner, CaseId, CaseSummary};

use crate::workload::PrefixSum;

fn main() -> Result<()> {
    let elements = 16_384;
    let measurements = BenchmarkRunner::default().try_run(|suite| {
        suite.register_case(
            CaseId::try_new("prefix_sum", format!("elements={elements}"))?,
            PrefixSum::new(elements),
        )
    })?;

    for measurement in &measurements {
        let summary = CaseSummary::try_from(measurement)?;
        println!("{:?}", summary);
    }

    Ok(())
}
