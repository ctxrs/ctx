use std::{
    fs,
    path::{Path, PathBuf},
};

use ctx_history_core::CaptureProvider;

use crate::provider_sources::{DiscoveryContext, DiscoveryReport, ProviderSource};

pub(super) fn tempdir() -> tempfile::TempDir {
    crate::test_support_paths::tempdir().expect("temporary directory should support resolver tests")
}

pub(super) fn resolve_provider(
    context: &DiscoveryContext,
    provider: CaptureProvider,
) -> DiscoveryReport {
    crate::provider_sources::discover_provider_sources_for_provider_with_context(
        &crate::provider_sources::TEST_PROVIDER_PROBES,
        context,
        provider,
    )
}

pub(super) fn provider_source_for_path(provider: CaptureProvider, path: PathBuf) -> ProviderSource {
    crate::provider_sources::provider_source_for_path(
        &crate::provider_sources::TEST_PROVIDER_PROBES,
        provider,
        path,
    )
}

pub(super) fn write_fixture(path: &Path, body: impl AsRef<[u8]>) {
    fs::create_dir_all(path.parent().expect("fixture path should have a parent")).unwrap();
    fs::write(path, body).unwrap();
}

pub(super) fn assert_automatic_role(source: &ProviderSource, components: &[&[u8]]) {
    let expected =
        ctx_history_capture_model::ProviderRouteRole::from_dynamic(components.iter().copied())
            .expect("expected test role should be bounded");
    assert_eq!(
        source.route_provenance.automatic_route_role(),
        Some(&expected),
        "unexpected route role for {}",
        source.path.display()
    );
}

pub(super) fn source_paths(report: &DiscoveryReport) -> Vec<PathBuf> {
    report
        .sources
        .iter()
        .map(|source| source.path.clone())
        .collect()
}
