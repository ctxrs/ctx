#[allow(unused_imports)]
use super::*;

pub(crate) fn assert_source_status(
    home: &Path,
    provider: CaptureProvider,
    expected: ProviderSourceStatus,
) {
    let source = discover_provider_sources(home)
        .into_iter()
        .find(|source| source.provider == provider)
        .unwrap();
    assert_eq!(source.status, expected, "{provider:?}");
}
