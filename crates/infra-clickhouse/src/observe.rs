const OPERATION_DURATION_METRIC: &str = "clickhouse_operation_duration_seconds";

const OPERATION_DURATION_BUCKETS: &[f64] = &[
    0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0, 10.0, 30.0,
];

pub const HISTOGRAMS: &[(&str, &[f64])] =
    &[(OPERATION_DURATION_METRIC, OPERATION_DURATION_BUCKETS)];
