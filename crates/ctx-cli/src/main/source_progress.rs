#[allow(unused_imports)]
use super::*;

#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct SourceProgressSnapshot {
    pub(crate) completed_bytes: u64,
    pub(crate) total_bytes: u64,
}
