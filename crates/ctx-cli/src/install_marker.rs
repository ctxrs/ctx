use std::{
    env, fs,
    io::Read,
    path::{Path, PathBuf},
};

use serde_json::Value;

const MAX_MARKER_BYTES: u64 = 16 * 1024;
const MAX_INSTALL_ATTEMPT_ID_CHARS: usize = 128;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstallMarker {
    pub install_attempt_id: String,
}

pub fn current_exe_install_marker() -> Option<InstallMarker> {
    let exe = env::current_exe().ok()?;
    read_install_marker(&install_marker_path(&exe))
}

fn read_install_marker(path: &Path) -> Option<InstallMarker> {
    let metadata = fs::metadata(path).ok()?;
    if !metadata.is_file() || metadata.len() > MAX_MARKER_BYTES {
        return None;
    }
    let file = fs::File::open(path).ok()?;
    let mut reader = file.take(MAX_MARKER_BYTES + 1);
    let mut bytes = Vec::new();
    if reader.read_to_end(&mut bytes).is_err() || bytes.len() as u64 > MAX_MARKER_BYTES {
        return None;
    }
    parse_install_marker(&bytes)
}

fn parse_install_marker(bytes: &[u8]) -> Option<InstallMarker> {
    let value: Value = serde_json::from_slice(bytes).ok()?;
    let id = value.get("install_attempt_id")?.as_str()?.trim();
    if is_valid_install_attempt_id(id) {
        Some(InstallMarker {
            install_attempt_id: id.to_owned(),
        })
    } else {
        None
    }
}

fn is_valid_install_attempt_id(value: &str) -> bool {
    !value.is_empty()
        && value.chars().count() <= MAX_INSTALL_ATTEMPT_ID_CHARS
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
}

fn install_marker_path(exe: &Path) -> PathBuf {
    let mut marker = exe.as_os_str().to_owned();
    marker.push(".install.json");
    PathBuf::from(marker)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_bounded_install_attempt_id() {
        let marker = parse_install_marker(br#"{"install_attempt_id":"attempt_01-HOSTED"}"#)
            .expect("valid marker");

        assert_eq!(marker.install_attempt_id, "attempt_01-HOSTED");
    }

    #[test]
    fn ignores_malformed_or_unbounded_install_attempt_id() {
        assert!(parse_install_marker(b"{not-json").is_none());
        assert!(parse_install_marker(br#"{"install_attempt_id":""}"#).is_none());
        assert!(parse_install_marker(br#"{"install_attempt_id":"contains space"}"#).is_none());
        assert!(parse_install_marker(
            format!(
                r#"{{"install_attempt_id":"{}"}}"#,
                "a".repeat(MAX_INSTALL_ATTEMPT_ID_CHARS + 1)
            )
            .as_bytes()
        )
        .is_none());
    }

    #[test]
    fn appends_marker_suffix_to_full_exe_path() {
        assert_eq!(
            install_marker_path(Path::new("/tmp/ctx.exe")),
            PathBuf::from("/tmp/ctx.exe.install.json")
        );
    }

    #[test]
    fn ignores_missing_or_oversized_marker_file() {
        let temp = tempfile::tempdir().unwrap();
        assert!(read_install_marker(&temp.path().join("missing.install.json")).is_none());

        let path = temp.path().join("ctx.install.json");
        fs::write(&path, vec![b'a'; MAX_MARKER_BYTES as usize + 1]).unwrap();
        assert!(read_install_marker(&path).is_none());
    }
}
