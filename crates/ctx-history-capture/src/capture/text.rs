#[allow(unused_imports)]
use super::*;

pub(crate) fn text_id_index(seed: &str, offset: u64) -> u64 {
    offset.saturating_add(fnv1a64(seed.as_bytes()) & 0x0fff_ffff)
}
