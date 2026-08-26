//! Mistral Vibe and Mux provider adapters for ctx agent history.
//!
//! The adapters own provider discovery, parsing, identity, and projection.
//! Capture supplies the concrete lifecycle binding and remains responsible for
//! route registration, index access, and publication.

mod mistral_vibe;
mod mux;

use std::sync::Arc;

use ctx_history_jsonl::JsonlFamilyAdapter;
use ctx_history_provider_runtime::{ProviderJsonlRuntime, ProviderRuntimeBinding};

pub fn mistral_vibe_jsonl_adapter<B>(
) -> Arc<dyn JsonlFamilyAdapter<Runtime = ProviderJsonlRuntime<B>>>
where
    B: ProviderRuntimeBinding,
{
    mistral_vibe::native_path::source_backed::mistral_vibe_jsonl_adapter::<B>()
}

pub fn mistral_vibe_jsonl_adapter_with_source_root_lineage<B>(
    source_root_lineage: Option<[u8; 32]>,
) -> Arc<dyn JsonlFamilyAdapter<Runtime = ProviderJsonlRuntime<B>>>
where
    B: ProviderRuntimeBinding,
{
    mistral_vibe::native_path::source_backed::mistral_vibe_jsonl_adapter_with_source_root_lineage::<B>(
        source_root_lineage,
    )
}

pub fn mux_jsonl_adapter<B>() -> Arc<dyn JsonlFamilyAdapter<Runtime = ProviderJsonlRuntime<B>>>
where
    B: ProviderRuntimeBinding,
{
    mux::mux_jsonl_adapter::<B>()
}

pub fn mux_jsonl_adapter_with_source_root_lineage<B>(
    source_root_lineage: Option<[u8; 32]>,
) -> Arc<dyn JsonlFamilyAdapter<Runtime = ProviderJsonlRuntime<B>>>
where
    B: ProviderRuntimeBinding,
{
    mux::mux_jsonl_adapter_with_source_root_lineage::<B>(source_root_lineage)
}
