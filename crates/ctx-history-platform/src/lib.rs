//! Platform paths and owner-private filesystem primitives for ctx state.

use thiserror::Error;

#[derive(Debug, Error)]
pub enum PlatformError {
    #[error("could not determine a home directory for the default ctx data root")]
    MissingHome,
    #[error("cannot use managed data-root authority {path}: {source}")]
    ManagedRoot {
        path: std::path::PathBuf,
        source: std::io::Error,
    },
}

pub type Result<T> = std::result::Result<T, PlatformError>;

pub mod installation_identity;
pub mod managed_root;
pub mod paths;
pub mod platform_security;
mod process_resources;
pub mod resource_format;

pub use paths::{
    config_path, default_data_root, device_path, history_dir, logs_dir, managed_data_root,
};
pub use process_resources::{open_file_limit_hint, raise_open_file_soft_limit};

#[cfg(test)]
mod tests;
