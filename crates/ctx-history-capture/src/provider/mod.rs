pub(crate) mod adapter;
pub(crate) mod codex;
pub(crate) mod ctx_retrieval;
pub(crate) mod custom_history_jsonl;
pub(crate) mod file_touches;
pub(crate) mod native_ingestion;
pub(crate) mod normalization;
pub(crate) mod providers;
pub mod source_backed;
pub(crate) mod sqlite;
pub(crate) mod tool_input;

const MAX_PROVIDER_PATH_IDENTITY_RAW_BYTES: usize = 7 * 1024;

pub(crate) fn provider_path_identity(path: &std::path::Path) -> crate::Result<String> {
    if path.to_str().is_none() {
        return Err(crate::CaptureError::InvalidProviderTranscriptPath {
            path: path.to_path_buf(),
            reason:
                "provider transcript path is not Unicode and cannot share durable TEXT authority",
        });
    }
    #[cfg(unix)]
    let (platform, raw) = {
        use std::os::unix::ffi::OsStrExt;

        ("unix-bytes", path.as_os_str().as_bytes().to_vec())
    };
    #[cfg(windows)]
    let (platform, raw) = {
        use std::os::windows::ffi::OsStrExt;

        let mut raw = Vec::new();
        for unit in path.as_os_str().encode_wide() {
            raw.extend_from_slice(&unit.to_le_bytes());
        }
        ("windows-wtf16le", raw)
    };
    #[cfg(not(any(unix, windows)))]
    let (platform, raw) = (
        "platform-encoded-bytes",
        path.as_os_str().as_encoded_bytes().to_vec(),
    );

    if raw.len() > MAX_PROVIDER_PATH_IDENTITY_RAW_BYTES {
        return Err(crate::CaptureError::InvalidProviderTranscriptPath {
            path: path.to_path_buf(),
            reason: "provider transcript path exceeds the durable identity limit",
        });
    }

    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut encoded = String::with_capacity(
        "provider-path-v1:"
            .len()
            .saturating_add(platform.len())
            .saturating_add(1)
            .saturating_add(raw.len().saturating_mul(2)),
    );
    encoded.push_str("provider-path-v1:");
    encoded.push_str(platform);
    encoded.push(':');
    for byte in raw {
        encoded.push(char::from(HEX[usize::from(byte >> 4)]));
        encoded.push(char::from(HEX[usize::from(byte & 0x0f)]));
    }
    Ok(encoded)
}

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
