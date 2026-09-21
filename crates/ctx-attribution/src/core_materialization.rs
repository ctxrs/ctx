//! Retained Core preparation is owned by the derivation library.

pub use ctx_attribution_derivation::core_materialization::*;

#[cfg(test)]
#[path = "core_materialization/preparation_tests.rs"]
mod preparation_tests;
