//! Provider-owned Pi source discovery and complete Core projection.

mod source_backed;

pub(crate) use source_backed::{
    pi_source_backed_adapter, pi_source_backed_adapter_with_source_root_lineage, PiSourceBackedRoot,
};
