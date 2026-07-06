#[allow(unused_imports)]
use super::*;

#[derive(Debug)]
pub(crate) struct TimingStats {
    pub(crate) min_ms: f64,
    pub(crate) p50_ms: f64,
    pub(crate) p95_ms: f64,
    pub(crate) max_ms: f64,
}

pub(crate) fn timing_stats(samples: &[f64]) -> TimingStats {
    assert!(!samples.is_empty(), "timing samples must not be empty");
    let mut sorted = samples.to_vec();
    sorted.sort_by(f64::total_cmp);
    TimingStats {
        min_ms: sorted[0],
        p50_ms: percentile(&sorted, 0.50),
        p95_ms: percentile(&sorted, 0.95),
        max_ms: *sorted.last().unwrap(),
    }
}
