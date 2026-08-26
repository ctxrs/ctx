//! Compatibility facade for index-backed provider capture composition.

use std::{collections::BTreeMap, path::Path};

pub use ctx_history_capture_composition::source_backed::*;

pub(crate) mod family {
    #[allow(
        dead_code,
        reason = "capture-local provider compatibility aliases retain the pre-split path"
    )]
    pub(crate) type CaptureProviderRuntime =
        ctx_history_capture_composition::CaptureProviderRuntime;
}

use crate::{DiscoveryContext, DiscoveryReport};

pub fn build_automatic_source_backed_registry(
    discovery: &DiscoveryContext,
    data_root: &Path,
) -> SourceBackedAutomaticRegistryBuild {
    ctx_history_capture_composition::build_automatic_source_backed_registry_with_probes(
        &crate::provider_sources::BUILTIN_PROVIDER_PROBES,
        discovery,
        data_root,
    )
}

pub fn build_automatic_source_backed_registry_from_report(
    discovery: &DiscoveryContext,
    data_root: &Path,
    report: DiscoveryReport,
) -> SourceBackedAutomaticRegistryBuild {
    ctx_history_capture_composition::build_automatic_source_backed_registry_from_report_with_probes(
        &crate::provider_sources::BUILTIN_PROVIDER_PROBES,
        discovery,
        data_root,
        report,
    )
}

#[doc(hidden)]
pub fn build_automatic_source_backed_registry_from_report_with_retained_roots(
    discovery: &DiscoveryContext,
    data_root: &Path,
    report: DiscoveryReport,
    retained_provider_roots: &BTreeMap<
        String,
        ctx_history_capture_model::RetainedProviderRootAuthority,
    >,
) -> SourceBackedAutomaticRegistryBuild {
    ctx_history_capture_composition::build_automatic_source_backed_registry_from_report_with_probes_and_retained_roots(
        &crate::provider_sources::BUILTIN_PROVIDER_PROBES,
        discovery,
        data_root,
        report,
        retained_provider_roots,
    )
}
