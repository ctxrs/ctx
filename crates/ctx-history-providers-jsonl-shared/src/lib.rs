#![allow(
    unused_imports,
    reason = "the provider compatibility surface preserves the moved module imports"
)]

mod error;
pub use error::{CaptureError, ProviderJsonlInventoryLimit, Result};

pub use ctx_history_capture_model::{
    fnv1a64, stable_capture_uuid, CatalogSummary, ProviderImportFailure, ProviderImportSummary,
    ProviderImportWorkResult, ProviderSourceFailureKind,
};

pub const MAX_PROVIDER_JSONL_LINE_BYTES: usize =
    ctx_history_source_io::MAX_PROVIDER_JSONL_LINE_BYTES;
pub const MAX_OPENCLAW_SESSION_INDEX_BYTES: usize = 1024 * 1024;
pub const JUNIE_SESSION_EVENTS_SOURCE_FORMAT: &str = "junie_session_events_jsonl_tree";
pub const OPENCLAW_SOURCE_FORMAT: &str = "openclaw_session_jsonl_tree";
pub const KIMI_CODE_CLI_SOURCE_FORMAT: &str = "kimi_code_cli_wire_jsonl";
pub const PROVIDER_MAX_PREVIEW_CHARS: usize = ctx_history_capture_model::PROVIDER_MAX_PREVIEW_CHARS;

pub struct NormalizedOpenClawEvent {
    pub event_type: ctx_history_core::EventType,
    pub role: Option<ctx_history_core::EventRole>,
    pub occurred_at: chrono::DateTime<chrono::Utc>,
    pub lexical_text: String,
}

pub trait JsonlProviderRuntime:
    ctx_history_jsonl::JsonlFamilyRuntime<Error = CaptureError>
{
}

impl<T> JsonlProviderRuntime for T where
    T: ctx_history_jsonl::JsonlFamilyRuntime<Error = CaptureError>
{
}

pub(crate) mod common {
    pub(crate) mod io {
        pub(crate) use ctx_history_source_io::{
            ProviderJsonlInventory, ProviderJsonlInventoryLimits, ProviderJsonlLineRead,
            PROVIDER_JSONL_INVENTORY_MAX_DEPTH, PROVIDER_JSONL_INVENTORY_MAX_DIRECTORIES,
            PROVIDER_JSONL_INVENTORY_MAX_ELIGIBLE_PATHS,
            PROVIDER_JSONL_INVENTORY_MAX_METADATA_ENTRIES, PROVIDER_JSONL_INVENTORY_MAX_PATH_BYTES,
        };
        ctx_history_source_io::define_mapped_source_io_compat!(crate::CaptureError);
    }
    pub(crate) mod json {
        pub(crate) use ctx_history_capture_model::{
            exact_bounded_string_alias, raw_object_keys_are_unique, ExactJsonStringAlias,
        };
    }
}

pub(crate) mod provider;

pub mod jsonl {
    pub use ctx_history_jsonl::*;
}

pub mod adapters {
    use std::{path::PathBuf, sync::Arc};

    use ctx_history_jsonl::JsonlFamilyAdapter;

    use crate::{provider, JsonlProviderRuntime, Result};

    /// Normalizes one provider-native OpenClaw transcript event without
    /// assigning source, session, or event identity.
    pub fn normalize_openclaw_event(
        event_index: u64,
        row: &serde_json::Value,
        occurred_at: chrono::DateTime<chrono::Utc>,
    ) -> crate::NormalizedOpenClawEvent {
        let fact = provider::providers::openclaw::event_fact(event_index, 0, row, occurred_at);
        crate::NormalizedOpenClawEvent {
            event_type: fact.event_type,
            role: fact.role,
            occurred_at: fact.occurred_at,
            lexical_text: fact.lexical_text,
        }
    }

    pub fn custom_history<R: JsonlProviderRuntime>(
        path: PathBuf,
        catalog_lineage: [u8; 32],
    ) -> Result<Arc<dyn JsonlFamilyAdapter<Runtime = R>>> {
        provider::custom_history_jsonl::custom_history_jsonl_family_adapter::<R>(
            provider::custom_history_jsonl::CustomHistorySourceBackedInput::explicit(
                path,
                catalog_lineage,
            ),
        )
        .map_err(|error| crate::CaptureError::InvalidPayload(error.to_string()))
    }

    pub fn custom_history_with_source_root_lineage<R: JsonlProviderRuntime>(
        path: PathBuf,
        catalog_lineage: [u8; 32],
        source_root_lineage: Option<[u8; 32]>,
    ) -> Result<Arc<dyn JsonlFamilyAdapter<Runtime = R>>> {
        provider::custom_history_jsonl::custom_history_jsonl_family_adapter::<R>(
            provider::custom_history_jsonl::CustomHistorySourceBackedInput::explicit_with_source_root_lineage(
                path,
                catalog_lineage,
                source_root_lineage,
            ),
        )
        .map_err(|error| crate::CaptureError::InvalidPayload(error.to_string()))
    }

    pub fn junie<R: JsonlProviderRuntime>() -> Arc<dyn JsonlFamilyAdapter<Runtime = R>> {
        provider::providers::junie::nativepath::junie_jsonl_adapter::<R>()
    }

    pub fn junie_with_source_root_lineage<R: JsonlProviderRuntime>(
        source_root_lineage: Option<[u8; 32]>,
    ) -> Arc<dyn JsonlFamilyAdapter<Runtime = R>> {
        provider::providers::junie::nativepath::junie_jsonl_adapter_with_source_root_lineage::<R>(
            source_root_lineage,
        )
    }

    pub fn kimi<R: JsonlProviderRuntime>() -> Arc<dyn JsonlFamilyAdapter<Runtime = R>> {
        provider::providers::kimi::native_path::source_backed::KimiSourceBackedCatalog::shared::<R>(
        )
        .into_shared()
    }

    pub fn kimi_with_source_root_lineage<R: JsonlProviderRuntime>(
        source_root_lineage: Option<[u8; 32]>,
    ) -> Arc<dyn JsonlFamilyAdapter<Runtime = R>> {
        provider::providers::kimi::native_path::source_backed::KimiSourceBackedCatalog::shared_with_source_root_lineage::<R>(source_root_lineage)
            .into_shared()
    }

    pub fn openclaw<R: JsonlProviderRuntime>() -> Arc<dyn JsonlFamilyAdapter<Runtime = R>> {
        provider::providers::openclaw::openclaw_source_backed_adapter_v0::<R>()
    }

    pub fn openclaw_with_source_root_lineage<R: JsonlProviderRuntime>(
        source_root_lineage: Option<[u8; 32]>,
    ) -> Arc<dyn JsonlFamilyAdapter<Runtime = R>> {
        provider::providers::openclaw::openclaw_source_backed_adapter_v0_with_source_root_lineage::<R>(
            source_root_lineage,
        )
    }

    pub fn pi<R: JsonlProviderRuntime>(
        path: PathBuf,
        automatic: bool,
    ) -> Result<(PathBuf, Arc<dyn JsonlFamilyAdapter<Runtime = R>>)> {
        let root = if automatic {
            provider::providers::pi::nativepath::PiSourceBackedRoot::winning(path)?
        } else {
            provider::providers::pi::nativepath::PiSourceBackedRoot::explicit(path)
        };
        Ok((
            root.path().to_path_buf(),
            provider::providers::pi::nativepath::pi_source_backed_adapter::<R>(),
        ))
    }

    pub fn pi_with_source_root_lineage<R: JsonlProviderRuntime>(
        path: PathBuf,
        automatic: bool,
        source_root_lineage: Option<[u8; 32]>,
    ) -> Result<(PathBuf, Arc<dyn JsonlFamilyAdapter<Runtime = R>>)> {
        let root = if automatic {
            provider::providers::pi::nativepath::PiSourceBackedRoot::winning(path)?
        } else {
            provider::providers::pi::nativepath::PiSourceBackedRoot::explicit(path)
        };
        Ok((
            root.path().to_path_buf(),
            provider::providers::pi::nativepath::pi_source_backed_adapter_with_source_root_lineage::<
                R,
            >(source_root_lineage),
        ))
    }

    pub fn deepseek_harness<R: JsonlProviderRuntime>(
        source_format: &'static str,
    ) -> Result<Arc<dyn JsonlFamilyAdapter<Runtime = R>>> {
        provider::providers::deepseek_harness::jsonl_adapter::<R>(source_format)
    }

    pub fn deepseek_harness_with_source_root_lineage<R: JsonlProviderRuntime>(
        source_format: &'static str,
        source_root_lineage: Option<[u8; 32]>,
    ) -> Result<Arc<dyn JsonlFamilyAdapter<Runtime = R>>> {
        provider::providers::deepseek_harness::jsonl_adapter_with_source_root_lineage::<R>(
            source_format,
            source_root_lineage,
        )
    }
}

#[cfg(test)]
pub(crate) mod test_support_paths {
    use std::{fs, io};

    pub(crate) fn tempdir() -> io::Result<tempfile::TempDir> {
        let temp_root = fs::canonicalize(std::env::temp_dir())?;
        tempfile::Builder::new()
            .prefix("ctx-history-providers-jsonl-shared-")
            .tempdir_in(temp_root)
    }
}
