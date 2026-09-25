# clairo

Sniff out performative `wgpu` code

`clairo` helps you evaluate shader optimizations through repeatable measurements of GPU execution time. It also aims to make `wgpu` benchmarks easy to write and their results easy to understand

Some of the questions `clairo` tries to answer are _"is one algorithm or sequence of passes cheaper than another?"_, _"does a different workgroup size improve performance?"_, or _"does changing memory access make this shader faster?"_. It does this by combining GPU timestamp queries with repeated sampling and statistical summaries to help identify performance improvements and regressions

```rust,ignore
fn prefix_sum(suite: &mut clairo::Suite) -> anyhow::Result<()> {
    for elements in [1_024, 16_384, 65_536] {
        suite
            .bench(format!("prefix_sum|elements={elements}"))
            .requirements(requirements(elements))
            .repeatable()
            .setup(async move |gpu| create_fixture(gpu, elements).await)
            .record(record_iteration)
            .validate(validate_iteration)
            .register()?;
    }

    Ok(())
}

fn mat_mul(suite: &mut clairo::Suite) -> anyhow::Result<()> {
    for size in [64, 128, 256] {
        suite
            .bench(format!("mat_mul|size={size}x{size}"))
            .requirements(mat_mul_requirements(size))
            .repeatable()
            .setup(async move |gpu| create_mat_mul_fixture(gpu, size).await)
            .record(record_mat_mul)
            .validate(validate_mat_mul)
            .register()?;
    }

    Ok(())
}

clairo::benches!(prefix_sum, mat_mul);
```
# Usage

To see an actual example, check out [examples/prefix_sum/main.rs](https://github.com/friendlymatthew/clairo/blob/main/examples/prefix_sum/main.rs)

The macro expands to `main`. For `benches/gpu.rs`, add this to `Cargo.toml` and run
`cargo bench --bench gpu`:

```toml
[[bench]]
name = "gpu"
harness = false
```
