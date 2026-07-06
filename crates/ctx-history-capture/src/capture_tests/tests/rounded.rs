#[allow(unused_imports)]
use super::*;

pub(crate) fn rounded(value: f64) -> f64 {
    (value * 100.0).round() / 100.0
}
