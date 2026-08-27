//! Platform paths and owner-private filesystem primitives for ctx state.

use thiserror::Error;

#[derive(Debug, Error)]
pub enum PlatformError {
    #[error("could not determine a home directory for the default ctx data root")]
    MissingHome,
}

pub type Result<T> = std::result::Result<T, PlatformError>;

pub mod paths;
pub mod platform_security;
mod process_resources;

pub use paths::{
    config_path, default_data_root, device_path, history_dir, logs_dir, managed_data_root,
};
pub use process_resources::raise_open_file_soft_limit;

#[cfg(test)]
mod tests;
