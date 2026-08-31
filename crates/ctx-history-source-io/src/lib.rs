//! Provider-neutral, policy-free source filesystem access for history capture.
//!
//! This crate owns bounded ordinary-file and tree reads, retained source
//! authority handles, and event-file inventories.
//! Provider discovery and parsing policy remain in their owning crates.

#![cfg_attr(feature = "test-support", allow(dead_code, unused_imports))]

mod bounded_tree;
mod error;
mod event_files;
mod io;
mod mapped_io;
mod ordinary_file;
mod path_identity;

pub use bounded_tree::*;
pub use error::ProviderJsonlInventoryLimit as SourceIoJsonlInventoryLimit;
pub use error::{
    is_provider_source_io_operation, is_provider_source_unavailable_io,
    ProviderJsonlInventoryLimit, Result, SourceIoError, PROVIDER_SOURCE_IO_OPERATION_PREFIX,
};
pub use event_files::*;
pub use io::*;
pub use mapped_io::*;
pub use ordinary_file::*;
pub use path_identity::*;

pub const MAX_PROVIDER_JSONL_LINE_BYTES: usize = 16 * 1024 * 1024;

/// Returns whether a provider-controlled identifier is safe as one portable
/// path segment. This is source-I/O policy because every source reader shares
/// the same traversal and Windows-device-name boundary.
pub fn provider_safe_path_segment(value: &str) -> bool {
    use std::path::{Component, Path};

    if value.is_empty()
        || value != value.trim()
        || matches!(value, "." | "..")
        || value.ends_with('.')
        || value.contains(['/', '\\', ':'])
        || value.chars().any(char::is_control)
        || provider_windows_reserved_segment(value)
    {
        return false;
    }
    let mut components = Path::new(value).components();
    matches!(components.next(), Some(Component::Normal(_))) && components.next().is_none()
}

fn provider_windows_reserved_segment(value: &str) -> bool {
    let base = value
        .split('.')
        .next()
        .unwrap_or(value)
        .to_ascii_uppercase();
    matches!(
        base.as_str(),
        "CON" | "PRN" | "AUX" | "NUL" | "CONIN$" | "CONOUT$"
    ) || ["COM", "LPT"].iter().any(|prefix| {
        base.strip_prefix(prefix).is_some_and(|suffix| {
            matches!(
                suffix,
                "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9" | "¹" | "²" | "³"
            )
        })
    })
}

#[cfg(test)]
mod safe_path_tests {
    use super::provider_safe_path_segment;

    #[test]
    fn provider_path_segments_reject_cross_platform_traversal() {
        for value in [
            "",
            "   ",
            ".",
            "..",
            ". ",
            ".. ",
            "trailing.",
            " leading",
            "trailing ",
            "../outside",
            "..\\outside",
            "/outside",
            "C:\\outside",
            "C:outside",
            "nested/file",
            "nested\\file",
            "line\nfeed",
            "CON",
            "nul.json",
            "COM1.txt",
            "COM¹.txt",
            "lpt9",
            "LPT³.log",
        ] {
            assert!(!provider_safe_path_segment(value), "accepted {value:?}");
        }
        for value in ["message-1", "session_2", "01JZ9.example"] {
            assert!(provider_safe_path_segment(value), "rejected {value:?}");
        }
    }
}

#[cfg(any(test, feature = "test-support"))]
pub mod test_support_paths;
