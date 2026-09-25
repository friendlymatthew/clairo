use anyhow::{Result, anyhow, ensure};
use clairo::{GpuHandle, GpuRequirements, IterationRecorder, Suite, ValidationContext};
use std::{mem::size_of, num::NonZeroU64};
use wgpu::util::DeviceExt;

const COMPUTE_WORKGROUP_SIZE: u32 = 64;

/*
 * here's an example that benchmarks prefix sum at three different input sizes
 *
 * wgpu involves a fair bit of ceremony, and clairo handles timing and sampling
 * for you. but you'll still create the buffers, textures, pipelines, and bind groups
 * your shader needs in setup
 *
 * for each benchmark case:
 * 1. give it a unique labee
 * 2. choose repeatable() if iterations can run back to back without resetting,
 *    or single_iteration() for one iteration per sample.
 * 3. define setup() to return your fixture: the resources and state you'll use
 *    this can be your own struct or tuple, clairo infers its type!
 * 4. define record() to encode one iteration's gpu commands
 * 5. call register() to add the case to the suite
 *
 * requirements is optional: use it for gpu features or limits beyond the defaults.
 * you can also use prepare to reset state before each sample, and validate to
 * check the output outside the timed work
 */
pub fn register_prefix_sum(suite: &mut Suite) -> Result<()> {
    for elements in [1_024u32, 16_384, 65_536] {
        suite
            .bench(format!("prefix_sum|elements={elements}"))
            .requirements(
                GpuRequirements::default().with_required_limits(wgpu::Limits {
                    max_compute_workgroups_per_dimension: elements.div_ceil(COMPUTE_WORKGROUP_SIZE),
                    ..Default::default()
                }),
            )
            .repeatable()
            .setup(async move |gpu| create_fixture(gpu, elements).await)
            .record(record_iteration)
            .validate(validate_iteration)
            .register()?;
    }

    Ok(())
}

// pretty similar to Criterion!
clairo::benches!(register_prefix_sum);

struct PrefixSumFixture {
    pipeline: wgpu::ComputePipeline,
    _input: wgpu::Buffer,
    ping: wgpu::Buffer,
    pong: wgpu::Buffer,
    bind_groups: Vec<wgpu::BindGroup>,
    offsets: Vec<u32>,
    output_size: NonZeroU64,
    expected: Vec<u32>,
    workgroup_count: u32,
}

impl PrefixSumFixture {
    fn output(&self) -> &wgpu::Buffer {
        if self.offsets.len().is_multiple_of(2) {
            &self.pong
        } else {
            &self.ping
        }
    }
}

async fn create_fixture(gpu: &GpuHandle<'_>, element_count: u32) -> Result<PrefixSumFixture> {
    let output_size = u32_buffer_size(element_count)?;

    let input_values = (0..element_count)
        .map(|index| index % 13 + 1)
        .collect::<Vec<_>>();

    let expected = inclusive_prefix_sum(&input_values);

    let offsets = prefix_sum_offsets(element_count)?;

    let shader = gpu
        .device()
        .create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("clairo.example.prefix_sum.v1"),
            source: wgpu::ShaderSource::Wgsl(include_str!("prefix_sum.wgsl").into()),
        });

    let pipeline = create_compute_pipeline(
        gpu.device(),
        &shader,
        "prefix_sum",
        "clairo example prefix-sum pipeline",
    );

    let input = gpu
        .device()
        .create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("clairo example prefix-sum input"),
            contents: bytemuck::cast_slice(&input_values),
            usage: wgpu::BufferUsages::STORAGE,
        });

    let storage_usage = wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC;

    let ping = gpu.device().create_buffer(&wgpu::BufferDescriptor {
        label: Some("clairo example prefix-sum ping"),
        size: output_size.get(),
        usage: storage_usage,
        mapped_at_creation: false,
    });

    let pong = gpu.device().create_buffer(&wgpu::BufferDescriptor {
        label: Some("clairo example prefix-sum pong"),
        size: output_size.get(),
        usage: storage_usage,
        mapped_at_creation: false,
    });

    let mut bind_groups = Vec::with_capacity(offsets.len());

    for (stage, offset) in offsets.iter().copied().enumerate() {
        let parameters = [offset, element_count, 0, 0];
        let parameter_buffer = gpu
            .device()
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("clairo example prefix-sum parameters"),
                contents: bytemuck::cast_slice(&parameters),
                usage: wgpu::BufferUsages::UNIFORM,
            });

        let (source, destination) = if stage == 0 {
            (&input, &ping)
        } else if stage.is_multiple_of(2) {
            (&pong, &ping)
        } else {
            (&ping, &pong)
        };

        let bind_group = gpu.device().create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("clairo example prefix-sum bind group"),
            layout: &pipeline.get_bind_group_layout(0),
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: source.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: destination.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: parameter_buffer.as_entire_binding(),
                },
            ],
        });

        bind_groups.push(bind_group);
    }

    Ok(PrefixSumFixture {
        pipeline,
        _input: input,
        ping,
        pong,
        bind_groups,
        offsets,
        output_size,
        expected,
        workgroup_count: element_count.div_ceil(COMPUTE_WORKGROUP_SIZE),
    })
}

fn record_iteration(
    fixture: &PrefixSumFixture,
    recorder: &mut IterationRecorder<'_>,
) -> Result<()> {
    for (offset, bind_group) in fixture.offsets.iter().zip(&fixture.bind_groups) {
        recorder.compute_pass(format!("prefix_sum_offset_{offset}"), |pass| {
            pass.set_pipeline(&fixture.pipeline);
            pass.set_bind_group(0, bind_group, &[]);
            pass.dispatch_workgroups(fixture.workgroup_count, 1, 1);

            Ok(())
        })?;
    }

    Ok(())
}

async fn validate_iteration(
    fixture: &mut PrefixSumFixture,
    context: &mut ValidationContext<'_>,
) -> Result<()> {
    let bytes = context
        .read_buffer(fixture.output(), 0, fixture.output_size)
        .await?;

    validate_u32_output(&bytes, &fixture.expected)
}

fn prefix_sum_offsets(element_count: u32) -> Result<Vec<u32>> {
    ensure!(
        element_count >= 2,
        "prefix-sum element count must be at least two"
    );

    let mut offsets = Vec::new();
    let mut offset = 1_u32;
    while offset < element_count {
        offsets.push(offset);

        let Some(next_offset) = offset.checked_mul(2) else {
            break;
        };
        offset = next_offset;
    }

    Ok(offsets)
}

fn inclusive_prefix_sum(input: &[u32]) -> Vec<u32> {
    let mut running_total = 0_u32;

    input
        .iter()
        .map(|value| {
            running_total = running_total.wrapping_add(*value);

            running_total
        })
        .collect::<Vec<_>>()
}

fn u32_buffer_size(element_count: u32) -> Result<NonZeroU64> {
    let element_size = u64::try_from(size_of::<u32>())
        .map_err(|_| anyhow!("u32 size does not fit a GPU buffer address"))?;
    let byte_size = u64::from(element_count)
        .checked_mul(element_size)
        .and_then(NonZeroU64::new)
        .ok_or_else(|| anyhow!("compute element count must be nonzero and fit u64"))?;

    Ok(byte_size)
}

fn create_compute_pipeline(
    device: &wgpu::Device,
    shader: &wgpu::ShaderModule,
    entry_point: &str,
    label: &str,
) -> wgpu::ComputePipeline {
    device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
        label: Some(label),
        layout: None,
        module: shader,
        entry_point: Some(entry_point),
        compilation_options: Default::default(),
        cache: None,
    })
}

fn validate_u32_output(bytes: &[u8], expected: &[u32]) -> Result<()> {
    let expected_byte_count = expected
        .len()
        .checked_mul(size_of::<u32>())
        .ok_or_else(|| anyhow!("cpu reference byte count overflowed"))?;

    ensure!(
        bytes.len() == expected_byte_count,
        "compute readback contained {} bytes, expected {expected_byte_count}",
        bytes.len()
    );

    for (index, (actual, expected)) in bytes.chunks_exact(4).zip(expected).enumerate() {
        let actual = <[u8; 4]>::try_from(actual)?;
        let actual = u32::from_le_bytes(actual);

        ensure!(
            actual == *expected,
            "compute element {index} was {actual}, expected {expected}"
        );
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::num::NonZeroU32;

    use clairo::{BenchmarkCase, BenchmarkRunner, CaseSummary, Repeatability};

    struct PrefixSum(u32);

    impl BenchmarkCase for PrefixSum {
        type Fixture = PrefixSumFixture;

        fn requirements(&self) -> GpuRequirements {
            requirements(self.0)
        }

        fn repeatability(&self) -> Repeatability {
            Repeatability::Repeatable
        }

        async fn setup(&self, gpu: &GpuHandle<'_>) -> Result<Self::Fixture> {
            create_fixture(gpu, self.0).await
        }

        fn record_iteration(
            &self,
            fixture: &Self::Fixture,
            recorder: &mut IterationRecorder<'_>,
        ) -> Result<()> {
            record_iteration(fixture, recorder)
        }

        async fn validate_iteration(
            &self,
            fixture: &mut Self::Fixture,
            context: &mut ValidationContext<'_>,
        ) -> Result<()> {
            validate_iteration(fixture, context).await
        }
    }

    #[test]
    fn cpu_validation_distinguishes_correct_wrong_and_truncated_outputs() {
        let expected = [7_u32, 11_u32, u32::MAX];
        let bytes = expected
            .iter()
            .flat_map(|value| value.to_le_bytes())
            .collect::<Vec<_>>();
        assert!(validate_u32_output(&bytes, &expected).is_ok());

        let mut wrong = bytes.clone();
        wrong[4] ^= 1;
        assert!(validate_u32_output(&wrong, &expected).is_err());
        assert!(validate_u32_output(&bytes[..8], &expected).is_err());
    }

    #[test]
    fn prefix_sum_registers_configurations_without_a_gpu() {
        let mut suite = Suite::new();
        register_prefix_sum(&mut suite).expect("prefix sum must register without a gpu");

        assert!(register_prefix_sum(&mut suite).is_err());
    }

    #[test]
    fn prefix_sum_supports_trait_registration() {
        let mut suite = Suite::new();
        let case = PrefixSum(16_384);
        assert_eq!(case.repeatability(), Repeatability::Repeatable);

        suite
            .register_case("prefix_sum|elements=16384", case)
            .expect("a trait case must register without a gpu");
    }

    #[test]
    fn prefix_sum_offsets_and_cpu_reference_are_inclusive() {
        assert_eq!(
            prefix_sum_offsets(9).expect("valid element count"),
            [1, 2, 4, 8]
        );
        assert_eq!(inclusive_prefix_sum(&[3, 1, 4, 1, 5]), [3, 4, 8, 9, 14]);
        assert!(prefix_sum_offsets(1).is_err());
    }

    #[test]
    #[ignore = "requires a native timestamp-query gpu"]
    fn prefix_sum_executes_and_validates_on_a_gpu() {
        let runner = BenchmarkRunner {
            sample_size: NonZeroU32::MIN,
            ..Default::default()
        };
        let results = runner
            .try_run(|suite| {
                for elements in [3, 65, 16_384] {
                    register_case(suite, elements)?;
                }

                Ok(())
            })
            .expect("a timestamp-query gpu is required for the ignored smoke test");

        assert_eq!(results.len(), 3);
        for measurement in &results {
            let summary = CaseSummary::try_from(measurement).expect("valid measurement summary");
            assert_eq!(summary.sample_count, 1);
            assert!(summary.median_ns_per_iteration > 0.0);
        }
    }
}
