mod source_backed;

#[cfg(test)]
mod source_backed_tests;

#[allow(unused_imports)]
pub(crate) use source_backed::{
    project_openhands_source_backed_v1, OpenHandsHydratedRecordV1, OpenHandsLocatorResolverV1,
    OpenHandsRejectedEventV1, OpenHandsSourceBackedAdapterV1, OpenHandsSourceBackedErrorV1,
    OpenHandsSourceBackedProjectionV1, OpenHandsSourceBackedResultV1,
};
