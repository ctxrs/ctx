//! Clap compatibility shells over the history-owned provider vocabulary.

use clap::ValueEnum;
use ctx_history_core::CaptureProvider;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NativeProviderArg(CaptureProvider);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProviderArg(CaptureProvider);

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum ImportFormatArg {
    #[value(name = "ctx-history-jsonl-v2")]
    CtxHistoryJsonlV2,
}

impl NativeProviderArg {
    pub const fn capture_provider(self) -> CaptureProvider {
        self.0
    }
}

impl ProviderArg {
    pub const fn capture_provider(self) -> CaptureProvider {
        self.0
    }

    pub fn parse_name(value: &str) -> Option<Self> {
        ctx_history_cli::parse_provider_name(value)
            .map(ctx_history_cli::HistoryProvider::capture_provider)
            .map(Self)
    }

    pub fn mcp_names() -> Vec<&'static str> {
        ctx_history_cli::mcp_provider_names()
    }
}

pub fn parse_native_provider_arg(value: &str) -> std::result::Result<NativeProviderArg, String> {
    ctx_history_cli::parse_native_provider(value).map(NativeProviderArg)
}

pub fn parse_provider_arg(value: &str) -> std::result::Result<ProviderArg, String> {
    ctx_history_cli::parse_provider(value)
        .map(ctx_history_cli::HistoryProvider::capture_provider)
        .map(ProviderArg)
}

pub fn cli_supported_provider(provider: CaptureProvider) -> bool {
    ctx_history_cli::cli_supported_provider(provider)
}
