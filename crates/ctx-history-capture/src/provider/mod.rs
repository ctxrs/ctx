pub(crate) mod adapter;
pub(crate) mod adapter_impls;
pub mod api;
pub(crate) mod codex;
pub(crate) mod custom_history_jsonl;
pub(crate) mod file_touches;
pub(crate) mod importer;
pub(crate) mod native;
pub(crate) mod providers;
pub(crate) mod sqlite;
pub(crate) mod sqlite_observation;

pub(crate) fn provider_safe_path_segment(value: &str) -> bool {
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
mod tests {
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
