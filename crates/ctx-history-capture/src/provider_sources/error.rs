#[allow(unused_imports)]
use super::*;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum BoundedProbe {
    Found,
    NotFound,
    BudgetExhausted,
    IoError,
}
