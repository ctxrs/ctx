// Keep the provider matrix as one cohesive table: splitting it alphabetically
// would obscure cross-provider policy defaults and make updates harder to audit.
use ctx_history_core::CaptureProvider;

use super::types::{ProviderCatalogSupport, ProviderImportSupport, ProviderSourceSpec};

pub(super) const PROVIDER_SPECS: &[ProviderSourceSpec] = &[
    ProviderSourceSpec {
        provider: CaptureProvider::Codex,
        import_support: ProviderImportSupport::Native,
        catalog_support: ProviderCatalogSupport::Native,
        unsupported_reason: None,
    },
    ProviderSourceSpec {
        provider: CaptureProvider::GrokBuild,
        import_support: ProviderImportSupport::Native,
        catalog_support: ProviderCatalogSupport::None,
        unsupported_reason: None,
    },
    ProviderSourceSpec {
        provider: CaptureProvider::DeepSeekHarness,
        import_support: ProviderImportSupport::Native,
        catalog_support: ProviderCatalogSupport::None,
        unsupported_reason: None,
    },
    ProviderSourceSpec {
        provider: CaptureProvider::Pi,
        import_support: ProviderImportSupport::Native,
        catalog_support: ProviderCatalogSupport::None,
        unsupported_reason: None,
    },
    ProviderSourceSpec {
        provider: CaptureProvider::Claude,
        import_support: ProviderImportSupport::Native,
        catalog_support: ProviderCatalogSupport::None,
        unsupported_reason: None,
    },
    ProviderSourceSpec {
        provider: CaptureProvider::OpenCode,
        import_support: ProviderImportSupport::Native,
        catalog_support: ProviderCatalogSupport::None,
        unsupported_reason: None,
    },
    ProviderSourceSpec {
        provider: CaptureProvider::Kilo,
        import_support: ProviderImportSupport::Native,
        catalog_support: ProviderCatalogSupport::None,
        unsupported_reason: None,
    },
    ProviderSourceSpec {
        provider: CaptureProvider::MiMoCode,
        import_support: ProviderImportSupport::Native,
        catalog_support: ProviderCatalogSupport::None,
        unsupported_reason: None,
    },
    ProviderSourceSpec {
        provider: CaptureProvider::KiroCli,
        import_support: ProviderImportSupport::Native,
        catalog_support: ProviderCatalogSupport::None,
        unsupported_reason: None,
    },
    ProviderSourceSpec {
        provider: CaptureProvider::Crush,
        import_support: ProviderImportSupport::Native,
        catalog_support: ProviderCatalogSupport::None,
        unsupported_reason: None,
    },
    ProviderSourceSpec {
        provider: CaptureProvider::Goose,
        import_support: ProviderImportSupport::Native,
        catalog_support: ProviderCatalogSupport::None,
        unsupported_reason: None,
    },
    ProviderSourceSpec {
        provider: CaptureProvider::Antigravity,
        import_support: ProviderImportSupport::Native,
        catalog_support: ProviderCatalogSupport::None,
        unsupported_reason: None,
    },
    ProviderSourceSpec {
        provider: CaptureProvider::Gemini,
        import_support: ProviderImportSupport::Native,
        catalog_support: ProviderCatalogSupport::None,
        unsupported_reason: None,
    },
    ProviderSourceSpec {
        provider: CaptureProvider::Tabnine,
        import_support: ProviderImportSupport::Native,
        catalog_support: ProviderCatalogSupport::None,
        unsupported_reason: None,
    },
    ProviderSourceSpec {
        provider: CaptureProvider::Cursor,
        import_support: ProviderImportSupport::Native,
        catalog_support: ProviderCatalogSupport::None,
        unsupported_reason: None,
    },
    ProviderSourceSpec {
        provider: CaptureProvider::Zed,
        import_support: ProviderImportSupport::Native,
        catalog_support: ProviderCatalogSupport::None,
        unsupported_reason: None,
    },
    ProviderSourceSpec {
        provider: CaptureProvider::CopilotCli,
        import_support: ProviderImportSupport::Native,
        catalog_support: ProviderCatalogSupport::None,
        unsupported_reason: None,
    },
    ProviderSourceSpec {
        provider: CaptureProvider::FactoryAiDroid,
        import_support: ProviderImportSupport::Native,
        catalog_support: ProviderCatalogSupport::None,
        unsupported_reason: None,
    },
    ProviderSourceSpec {
        provider: CaptureProvider::QwenCode,
        import_support: ProviderImportSupport::Native,
        catalog_support: ProviderCatalogSupport::None,
        unsupported_reason: None,
    },
    ProviderSourceSpec {
        provider: CaptureProvider::KimiCodeCli,
        import_support: ProviderImportSupport::Native,
        catalog_support: ProviderCatalogSupport::None,
        unsupported_reason: None,
    },
    ProviderSourceSpec {
        provider: CaptureProvider::Auggie,
        import_support: ProviderImportSupport::Native,
        catalog_support: ProviderCatalogSupport::None,
        unsupported_reason: None,
    },
    ProviderSourceSpec {
        provider: CaptureProvider::Junie,
        import_support: ProviderImportSupport::Native,
        catalog_support: ProviderCatalogSupport::None,
        unsupported_reason: None,
    },
    ProviderSourceSpec {
        provider: CaptureProvider::Firebender,
        import_support: ProviderImportSupport::Native,
        catalog_support: ProviderCatalogSupport::None,
        unsupported_reason: None,
    },
    ProviderSourceSpec {
        provider: CaptureProvider::ForgeCode,
        import_support: ProviderImportSupport::Native,
        catalog_support: ProviderCatalogSupport::None,
        unsupported_reason: None,
    },
    ProviderSourceSpec {
        provider: CaptureProvider::DeepAgents,
        import_support: ProviderImportSupport::Native,
        catalog_support: ProviderCatalogSupport::None,
        unsupported_reason: None,
    },
    ProviderSourceSpec {
        provider: CaptureProvider::MistralVibe,
        import_support: ProviderImportSupport::Native,
        catalog_support: ProviderCatalogSupport::None,
        unsupported_reason: None,
    },
    ProviderSourceSpec {
        provider: CaptureProvider::Mux,
        import_support: ProviderImportSupport::Native,
        catalog_support: ProviderCatalogSupport::None,
        unsupported_reason: None,
    },
    ProviderSourceSpec {
        provider: CaptureProvider::RovoDev,
        import_support: ProviderImportSupport::Native,
        catalog_support: ProviderCatalogSupport::None,
        unsupported_reason: None,
    },
    ProviderSourceSpec {
        provider: CaptureProvider::OpenClaw,
        import_support: ProviderImportSupport::Native,
        catalog_support: ProviderCatalogSupport::None,
        unsupported_reason: None,
    },
    ProviderSourceSpec {
        provider: CaptureProvider::Hermes,
        import_support: ProviderImportSupport::Native,
        catalog_support: ProviderCatalogSupport::None,
        unsupported_reason: None,
    },
    ProviderSourceSpec {
        provider: CaptureProvider::NanoClaw,
        import_support: ProviderImportSupport::Native,
        catalog_support: ProviderCatalogSupport::None,
        unsupported_reason: None,
    },
    ProviderSourceSpec {
        provider: CaptureProvider::AstrBot,
        import_support: ProviderImportSupport::Native,
        catalog_support: ProviderCatalogSupport::None,
        unsupported_reason: None,
    },
    ProviderSourceSpec {
        provider: CaptureProvider::Shelley,
        import_support: ProviderImportSupport::Native,
        catalog_support: ProviderCatalogSupport::None,
        unsupported_reason: None,
    },
    ProviderSourceSpec {
        provider: CaptureProvider::Continue,
        import_support: ProviderImportSupport::Native,
        catalog_support: ProviderCatalogSupport::None,
        unsupported_reason: None,
    },
    ProviderSourceSpec {
        provider: CaptureProvider::OpenHands,
        import_support: ProviderImportSupport::Native,
        catalog_support: ProviderCatalogSupport::None,
        unsupported_reason: None,
    },
    ProviderSourceSpec {
        provider: CaptureProvider::Cline,
        import_support: ProviderImportSupport::Native,
        catalog_support: ProviderCatalogSupport::None,
        unsupported_reason: None,
    },
    ProviderSourceSpec {
        provider: CaptureProvider::RooCode,
        import_support: ProviderImportSupport::Native,
        catalog_support: ProviderCatalogSupport::None,
        unsupported_reason: None,
    },
    ProviderSourceSpec {
        provider: CaptureProvider::Lingma,
        import_support: ProviderImportSupport::Native,
        catalog_support: ProviderCatalogSupport::None,
        unsupported_reason: None,
    },
    ProviderSourceSpec {
        provider: CaptureProvider::Qoder,
        import_support: ProviderImportSupport::Native,
        catalog_support: ProviderCatalogSupport::None,
        unsupported_reason: None,
    },
    ProviderSourceSpec {
        provider: CaptureProvider::Warp,
        import_support: ProviderImportSupport::Native,
        catalog_support: ProviderCatalogSupport::None,
        unsupported_reason: None,
    },
    ProviderSourceSpec {
        provider: CaptureProvider::CodeBuddy,
        import_support: ProviderImportSupport::Native,
        catalog_support: ProviderCatalogSupport::None,
        unsupported_reason: None,
    },
    ProviderSourceSpec {
        provider: CaptureProvider::Fx,
        import_support: ProviderImportSupport::Native,
        catalog_support: ProviderCatalogSupport::None,
        unsupported_reason: None,
    },
];

pub fn provider_source_specs() -> &'static [ProviderSourceSpec] {
    PROVIDER_SPECS
}

pub fn provider_source_spec(provider: CaptureProvider) -> Option<&'static ProviderSourceSpec> {
    PROVIDER_SPECS.iter().find(|spec| spec.provider == provider)
}
