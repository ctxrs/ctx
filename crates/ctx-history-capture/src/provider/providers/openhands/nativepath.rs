mod source_backed;

#[cfg(test)]
mod source_backed_tests;

#[allow(
    unused_imports,
    reason = "the shared registration owner wires these provider callbacks in the integration commit"
)]
pub(crate) use source_backed::{
    openhands_owns_source, openhands_route_error, OpenHandsEventFileAdapterV2,
    OpenHandsEventFileSourcePlan, OpenHandsSourceBackedErrorV2, OpenHandsSourceBackedResultV2,
};
