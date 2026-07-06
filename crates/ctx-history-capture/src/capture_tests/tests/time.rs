#[allow(unused_imports)]
use super::*;

pub(crate) fn elapsed_ms(duration: std::time::Duration) -> f64 {
    duration.as_secs_f64() * 1000.0
}
