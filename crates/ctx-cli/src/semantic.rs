include!("semantic/preamble.rs");
pub(crate) mod model_retry;
pub(crate) mod query_priority;
pub(crate) mod query_service_contract;
pub(crate) mod readiness;
#[cfg(any(target_os = "macos", test))]
mod model_bundle {
    include!("semantic/model_bundle.rs");
}
#[cfg(any(target_os = "macos", test))]
mod model_acquisition {
    include!("semantic/model_acquisition.rs");
}
#[cfg(any(target_os = "macos", test))]
use model_acquisition::*;
#[cfg(target_os = "macos")]
use model_bundle::*;
include!("semantic/resource_policy.rs");
mod cache_paths;
#[cfg(ctx_semantic_fastembed)]
mod cpu_model_cache;
#[cfg(ctx_semantic_fastembed)]
use cpu_model_cache::{
    maybe_cleanup_semantic_cpu_download_cache_after_cached_acquisition, read_semantic_model_file,
    replace_cpu_model_cache_from_pinned_revision, semantic_cpu_cache_repairable,
    semantic_cpu_cache_snapshot,
};
include!("semantic/embedding_backend.rs");
include!("semantic/vector_store_schema.rs");
include!("semantic/vector_store_state.rs");
include!("semantic/vector_store_search.rs");
include!("semantic/ort_runtime.rs");
include!("semantic/paths_status.rs");
include!("semantic/query_service_transport.rs");
include!("semantic/daemon.rs");
include!("semantic/health_search.rs");
include!("semantic/indexing.rs");
#[cfg(test)]
include!("semantic/tests.rs");
