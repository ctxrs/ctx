//! Provider-owned Pi source discovery, projection, and exact hydration.

mod source_backed;

pub(crate) use source_backed::{pi_source_backed_adapter, PiSourceBackedRoot};

#[cfg(test)]
mod source_backed_tests;
