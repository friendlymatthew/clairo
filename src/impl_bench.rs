/// defines an entry point that registers benchmark functions, runs their cases,
/// and prints each case's summary
///
/// for a `benches/gpu.rs` executable, disable the built-in test harness in `Cargo.toml`:
///
/// ```toml
/// [[bench]]
/// name = "gpu"
/// harness = false
/// ```
#[macro_export]
macro_rules! benches {
    ($($register:path),+ $(,)?) => {
        fn main() -> ::std::result::Result<
            (),
            ::std::boxed::Box<
                dyn ::std::error::Error + ::std::marker::Send + ::std::marker::Sync,
            >,
        > {
            $crate::BenchmarkRunner::default()
                .run(|suite| {
                    $($register(suite)?;)+

                    ::std::result::Result::Ok(())
                })
                .map_err(::std::convert::Into::into)
        }
    };
}
