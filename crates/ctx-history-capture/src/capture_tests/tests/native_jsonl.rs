#[allow(unused_imports)]
use super::*;

pub(crate) fn assert_provider_failures_include_headerless_and_malformed(
    summary: &ProviderImportSummary,
) {
    assert!(summary.failures.iter().any(|failure| failure
        .error
        .contains("no importable native JSONL session header")));
    assert!(summary
        .failures
        .iter()
        .any(|failure| failure.error.contains("malformed JSONL")));
}
