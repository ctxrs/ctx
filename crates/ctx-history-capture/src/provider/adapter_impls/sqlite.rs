use std::path::Path;

use ctx_history_core::CaptureProvider;

use crate::common::io::ensure_regular_provider_transcript_file;
use crate::provider::adapter::{
    AstrBotSqliteAdapter, CrushSqliteAdapter, DeepAgentsSqliteAdapter, FirebenderSqliteAdapter,
    ForgeCodeSqliteAdapter, GooseSessionsSqliteAdapter, HermesSqliteAdapter, KiloSqliteAdapter,
    KiroSqliteAdapter, LingmaSqliteAdapter, OpenCodeSqliteAdapter, ShelleySqliteAdapter,
    ZedThreadsSqliteAdapter,
};
use crate::provider::providers::{
    astrbot::normalize_astrbot_sqlite,
    crush::normalize_crush_sqlite,
    deepagents::normalize_deepagents_sqlite,
    firebender::normalize_firebender_sqlite,
    forgecode::normalize_forgecode_sqlite,
    goose::normalize_goose_sessions_sqlite,
    hermes::normalize_hermes_sqlite,
    kiro::normalize_kiro_sqlite,
    lingma::normalize_lingma_sqlite,
    opencode::{normalize_opencode_sqlite, KILO_SQLITE_DIALECT, OPENCODE_SQLITE_DIALECT},
    shelley::normalize_shelley_sqlite,
    zed::normalize_zed_threads_sqlite,
};
use crate::{
    ProviderAdapterContext, ProviderCaptureAdapter, ProviderNormalizationResult, Result,
    ASTRBOT_SQLITE_SOURCE_FORMAT, CRUSH_SQLITE_SOURCE_FORMAT, DEEPAGENTS_SQLITE_SOURCE_FORMAT,
    FIREBENDER_SQLITE_SOURCE_FORMAT, FORGECODE_SQLITE_SOURCE_FORMAT,
    GOOSE_SESSIONS_SQLITE_SOURCE_FORMAT, HERMES_SQLITE_SOURCE_FORMAT, KILO_SQLITE_SOURCE_FORMAT,
    KIRO_SQLITE_SOURCE_FORMAT, LINGMA_SQLITE_SOURCE_FORMAT, OPENCODE_SQLITE_SOURCE_FORMAT,
    SHELLEY_SQLITE_SOURCE_FORMAT, ZED_THREADS_SQLITE_SOURCE_FORMAT,
};

macro_rules! sqlite_adapter {
    ($adapter:ty, $provider:path, $format:expr, $normalize:expr) => {
        impl ProviderCaptureAdapter for $adapter {
            fn provider(&self) -> CaptureProvider {
                $provider
            }

            fn source_format(&self) -> &str {
                $format
            }

            fn normalize_path(
                &self,
                path: &Path,
                context: &ProviderAdapterContext,
            ) -> Result<ProviderNormalizationResult> {
                $normalize(path, context)
            }
        }
    };
}

sqlite_adapter!(
    FirebenderSqliteAdapter,
    CaptureProvider::Firebender,
    FIREBENDER_SQLITE_SOURCE_FORMAT,
    normalize_firebender_sqlite
);
sqlite_adapter!(
    KiroSqliteAdapter,
    CaptureProvider::KiroCli,
    KIRO_SQLITE_SOURCE_FORMAT,
    normalize_kiro_sqlite
);
sqlite_adapter!(
    CrushSqliteAdapter,
    CaptureProvider::Crush,
    CRUSH_SQLITE_SOURCE_FORMAT,
    normalize_crush_sqlite
);
sqlite_adapter!(
    GooseSessionsSqliteAdapter,
    CaptureProvider::Goose,
    GOOSE_SESSIONS_SQLITE_SOURCE_FORMAT,
    normalize_goose_sessions_sqlite
);
sqlite_adapter!(
    HermesSqliteAdapter,
    CaptureProvider::Hermes,
    HERMES_SQLITE_SOURCE_FORMAT,
    normalize_hermes_sqlite
);
sqlite_adapter!(
    AstrBotSqliteAdapter,
    CaptureProvider::AstrBot,
    ASTRBOT_SQLITE_SOURCE_FORMAT,
    normalize_astrbot_sqlite
);
sqlite_adapter!(
    ShelleySqliteAdapter,
    CaptureProvider::Shelley,
    SHELLEY_SQLITE_SOURCE_FORMAT,
    normalize_shelley_sqlite
);
sqlite_adapter!(
    LingmaSqliteAdapter,
    CaptureProvider::Lingma,
    LINGMA_SQLITE_SOURCE_FORMAT,
    normalize_lingma_sqlite
);
sqlite_adapter!(
    ZedThreadsSqliteAdapter,
    CaptureProvider::Zed,
    ZED_THREADS_SQLITE_SOURCE_FORMAT,
    normalize_zed_threads_sqlite
);
sqlite_adapter!(
    ForgeCodeSqliteAdapter,
    CaptureProvider::ForgeCode,
    FORGECODE_SQLITE_SOURCE_FORMAT,
    normalize_forgecode_sqlite
);
sqlite_adapter!(
    DeepAgentsSqliteAdapter,
    CaptureProvider::DeepAgents,
    DEEPAGENTS_SQLITE_SOURCE_FORMAT,
    normalize_deepagents_sqlite
);

impl ProviderCaptureAdapter for OpenCodeSqliteAdapter {
    fn provider(&self) -> CaptureProvider {
        CaptureProvider::OpenCode
    }

    fn source_format(&self) -> &str {
        OPENCODE_SQLITE_SOURCE_FORMAT
    }

    fn normalize_path(
        &self,
        path: &Path,
        context: &ProviderAdapterContext,
    ) -> Result<ProviderNormalizationResult> {
        ensure_regular_provider_transcript_file(path)?;
        normalize_opencode_sqlite(path, context, &OPENCODE_SQLITE_DIALECT)
    }
}

impl ProviderCaptureAdapter for KiloSqliteAdapter {
    fn provider(&self) -> CaptureProvider {
        CaptureProvider::Kilo
    }

    fn source_format(&self) -> &str {
        KILO_SQLITE_SOURCE_FORMAT
    }

    fn normalize_path(
        &self,
        path: &Path,
        context: &ProviderAdapterContext,
    ) -> Result<ProviderNormalizationResult> {
        ensure_regular_provider_transcript_file(path)?;
        normalize_opencode_sqlite(path, context, &KILO_SQLITE_DIALECT)
    }
}
