mod activity;
#[cfg(test)]
mod exact_json_tests;
mod fallback_identity;
mod family;
mod model;
mod occurrence;
mod pending_exchange;
mod resumable_sha256;
mod source_identity;
mod terminal_authority;

pub use activity::*;
pub use ctx_history_capture_model::{
    exact_bounded_string_alias, exact_json_value, raw_object_keys_are_unique, ExactJsonStringAlias,
};
pub use fallback_identity::*;
pub use family::*;
pub use model::*;
pub use occurrence::*;
pub use pending_exchange::*;
pub use resumable_sha256::*;
pub use source_identity::*;
pub use terminal_authority::*;

#[cfg(test)]
mod test_support_paths {
    pub fn tempdir() -> std::io::Result<tempfile::TempDir> {
        let root = std::fs::canonicalize(std::env::temp_dir())?;
        tempfile::Builder::new()
            .prefix("ctx-jsonl-")
            .tempdir_in(root)
    }
}
