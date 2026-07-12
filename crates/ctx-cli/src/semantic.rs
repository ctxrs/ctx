include!("semantic/preamble.rs");
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
