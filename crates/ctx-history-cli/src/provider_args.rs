use ctx_history_core::CaptureProvider;
pub use ctx_history_core::{
    native_provider_cli_specs, parse_capture_provider_name, provider_cli_spec, provider_cli_specs,
    ProviderCliSpec,
};

use crate::HistoryProvider;

pub fn cli_supported_provider(provider: CaptureProvider) -> bool {
    provider_cli_spec(provider).is_some()
}

pub fn provider_is_importable(provider: CaptureProvider) -> bool {
    ctx_history_capture::provider_source_specs()
        .iter()
        .find(|spec| spec.provider == provider)
        .is_some_and(|spec| spec.import_support.is_importable())
}

pub fn provider_cli_name(provider: CaptureProvider) -> &'static str {
    provider_cli_spec(provider).map_or_else(|| provider.as_str(), |spec| spec.cli_name)
}

pub fn parse_provider_name(value: &str) -> Option<HistoryProvider> {
    parse_capture_provider_name(value).map(HistoryProvider::from)
}

pub fn parse_native_provider_name(value: &str) -> Option<CaptureProvider> {
    parse_capture_provider_name(value).filter(|provider| *provider != CaptureProvider::Custom)
}

pub fn parse_provider(value: &str) -> std::result::Result<HistoryProvider, String> {
    parse_provider_name(value).ok_or_else(|| compact_provider_error(value))
}

pub fn parse_native_provider(value: &str) -> std::result::Result<CaptureProvider, String> {
    parse_native_provider_name(value).ok_or_else(|| compact_provider_error(value))
}

pub fn mcp_provider_names() -> Vec<&'static str> {
    let mut names = Vec::new();
    for spec in provider_cli_specs() {
        names.push(spec.cli_name);
        if spec.provider.as_str() != spec.cli_name {
            names.push(spec.provider.as_str());
        }
    }
    names.sort_unstable();
    names.dedup();
    names
}

pub fn compact_provider_error(value: &str) -> String {
    format!(
        "unknown provider {value:?}; examples: codex, claude, cursor, pi, copilot-cli, opencode; run `ctx sources --all` to inspect every supported provider location"
    )
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderArg(pub HistoryProvider);

impl ProviderArg {
    pub fn capture_provider(self) -> ctx_history_core::CaptureProvider {
        self.0.capture_provider()
    }
}

#[cfg(test)]
#[path = "provider_args/tests.rs"]
mod tests;
