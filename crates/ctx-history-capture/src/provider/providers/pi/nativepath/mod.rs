//! Provider-owned Pi source discovery, projection, and exact hydration.

mod checkpoint;
mod reader;
mod rows;
mod source;
mod source_backed;

pub(crate) use source_backed::{
    project_pi_source_backed_root_cold, PiSourceBackedResolver, PiSourceBackedRoot,
};

#[cfg(test)]
mod source_backed_tests;
