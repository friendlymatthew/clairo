pub fn mean_f64(values: &[f64]) -> f64 {
    assert!(!values.is_empty(), "cannot average empty values");

    values.iter().sum::<f64>() / usize_to_f64(values.len())
}

pub fn quantile_sorted(sorted: &[f64], probability: f64) -> f64 {
    assert!(
        !sorted.is_empty(),
        "cannot compute a quantile of empty values"
    );
    assert!(
        (0.0..=1.0).contains(&probability),
        "probability must be between zero and one"
    );

    let position = probability * usize_to_f64(sorted.len() - 1);
    let mut lower = 0;
    let mut upper = sorted.len() - 1;

    // find the neighboring indices without casting a floating point value to an index.
    while upper - lower > 1 {
        let middle = lower + (upper - lower) / 2;
        if usize_to_f64(middle) <= position {
            lower = middle;
        } else {
            upper = middle;
        }
    }

    let fraction = position - usize_to_f64(lower);

    sorted[lower] * (1.0 - fraction) + sorted[upper] * fraction
}

pub fn sample_standard_deviation(values: &[f64], mean: f64) -> Option<f64> {
    if values.len() < 2 {
        return None;
    }

    let squared_deviations = values
        .iter()
        .map(|value| {
            let difference = value - mean;

            difference * difference
        })
        .sum::<f64>();

    let variance = squared_deviations / usize_to_f64(values.len() - 1);

    Some(variance.sqrt())
}

fn usize_to_f64(value: usize) -> f64 {
    let value = u64::try_from(value).expect("sample count must fit in u64");
    let high = u32::try_from(value >> 32).expect("upper half must fit in u32");
    let low = u32::try_from(value & u64::from(u32::MAX)).expect("lower half must fit in u32");

    f64::from(high) * 4_294_967_296.0 + f64::from(low)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn assert_close(actual: f64, expected: f64) {
        assert!(
            (actual - expected).abs() < 1.0e-12,
            "expected {expected}, got {actual}"
        );
    }

    #[test]
    fn known_mean_and_sample_standard_deviation() {
        let values = [2.0, 4.0, 4.0, 4.0, 5.0, 5.0, 7.0, 9.0];
        let mean = mean_f64(&values);

        assert_close(mean, 5.0);
        assert_close(
            sample_standard_deviation(&values, mean).unwrap(),
            2.138_089_935_299_395,
        );
    }

    #[test]
    fn quantiles_interpolate_and_include_endpoints() {
        let values = [0.0, 10.0, 20.0, 30.0];

        for (probability, expected) in [
            (0.0, 0.0),
            (0.25, 7.5),
            (0.5, 15.0),
            (0.75, 22.5),
            (1.0, 30.0),
        ] {
            assert_close(quantile_sorted(&values, probability), expected);
        }
        assert_close(quantile_sorted(&[1.0, 3.0, 9.0], 0.5), 3.0);
        assert_close(quantile_sorted(&[7.0], 0.25), 7.0);
    }

    #[test]
    fn standard_deviation_needs_two_samples() {
        assert_eq!(sample_standard_deviation(&[], 0.0), None);
        assert_eq!(sample_standard_deviation(&[7.0], 7.0), None);
        assert_eq!(sample_standard_deviation(&[7.0, 7.0], 7.0), Some(0.0));
        assert_close(mean_f64(&[7.0]), 7.0);
    }

    #[test]
    #[should_panic(expected = "cannot average empty values")]
    fn mean_rejects_empty_values() {
        mean_f64(&[]);
    }

    #[test]
    #[should_panic(expected = "cannot compute a quantile of empty values")]
    fn quantile_rejects_empty_values() {
        quantile_sorted(&[], 0.5);
    }

    #[test]
    #[should_panic(expected = "probability must be between zero and one")]
    fn quantile_rejects_nan_probability() {
        quantile_sorted(&[1.0], f64::NAN);
    }
}
