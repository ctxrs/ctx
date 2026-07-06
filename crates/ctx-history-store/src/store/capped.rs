#[allow(unused_imports)]
use super::*;

pub(crate) fn capped_i64(value: u64) -> i64 {
    value.min(i64::MAX as u64) as i64
}
