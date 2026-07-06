#[allow(unused_imports)]
use super::*;

pub(crate) fn source_provider_cli_name(provider: CaptureProvider) -> &'static str {
    ProviderArg::parse_name(provider.as_str())
        .map(ProviderArg::cli_name)
        .unwrap_or_else(|| provider.as_str())
}

pub(crate) fn parse_native_provider_arg(
    value: &str,
) -> std::result::Result<NativeProviderArg, String> {
    let provider =
        NativeProviderArg::from_str(value, false).map_err(|_| compact_provider_error(value))?;
    if cli_supported_provider(provider.capture_provider()) {
        Ok(provider)
    } else {
        Err(compact_provider_error(value))
    }
}

pub(crate) fn parse_provider_arg(value: &str) -> std::result::Result<ProviderArg, String> {
    let provider =
        ProviderArg::from_str(value, false).map_err(|_| compact_provider_error(value))?;
    if cli_supported_provider(provider.capture_provider()) {
        Ok(provider)
    } else {
        Err(compact_provider_error(value))
    }
}

pub(crate) fn no_importable_provider_sources_error(
    provider: CaptureProvider,
    sources: &[SourceInfo],
) -> anyhow::Error {
    let mut message = format!("no importable {} history found", provider.as_str());
    if sources.is_empty() {
        message.push_str("; no default paths are registered for this provider");
    } else {
        message.push_str("\nchecked paths:");
        for source in sources {
            message.push_str(&format!(
                "\n  {} ({})",
                source.path.display(),
                source.status.as_str()
            ));
            if let Some(reason) = source.unsupported_reason {
                message.push_str(&format!(" - {reason}"));
            }
        }
    }
    message.push_str("\nuse `ctx sources` to inspect discovery, or pass --path");
    anyhow!(message)
}

pub(crate) fn discovered_sources() -> Vec<SourceInfo> {
    home_dir()
        .as_deref()
        .map(discover_provider_sources)
        .map(filter_cli_supported_sources)
        .unwrap_or_default()
}

pub(crate) fn discovered_sources_for_provider(provider: CaptureProvider) -> Vec<SourceInfo> {
    if !cli_supported_provider(provider) {
        return Vec::new();
    }
    home_dir()
        .as_deref()
        .map(|home| discover_provider_sources_for_provider(home, provider))
        .unwrap_or_default()
}
