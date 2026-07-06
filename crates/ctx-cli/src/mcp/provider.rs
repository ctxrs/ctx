#[allow(unused_imports)]
use super::*;

pub(crate) fn optional_provider(arguments: &Value, key: &str) -> Result<Option<ProviderArg>> {
    let Some(provider) = optional_string(arguments, key)? else {
        return Ok(None);
    };
    ProviderArg::parse_name(&provider)
        .filter(|provider| cli_supported_provider(provider.capture_provider()))
        .map(Some)
        .ok_or_else(|| anyhow!("provider must be one of {}", provider_names().join(", ")))
}
