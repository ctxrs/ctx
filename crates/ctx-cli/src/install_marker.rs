use std::{
    env, fs,
    io::Read,
    path::{Path, PathBuf},
};

use chrono::{DateTime, TimeDelta, Utc};
use ctx_history_core::utc_now;
use serde_json::Value;

const MAX_MARKER_BYTES: u64 = 16 * 1024;
const INSTALL_ATTRIBUTION_WINDOW_SECS: i64 = 7 * 24 * 60 * 60;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActiveInstallAttribution {
    pub install_attempt_id: String,
    pub installed_at: DateTime<Utc>,
}

pub fn current_exe_install_marker() -> Option<ActiveInstallAttribution> {
    let exe = env::current_exe().ok()?;
    read_install_marker(&install_marker_path(&exe))
}

pub(crate) fn read_install_marker(path: &Path) -> Option<ActiveInstallAttribution> {
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
    parse_install_marker_at(&bytes, utc_now())
}

pub(crate) fn parse_install_marker_at(
    bytes: &[u8],
    now: DateTime<Utc>,
) -> Option<ActiveInstallAttribution> {
    let value: Value = serde_json::from_slice(bytes).ok()?;
    active_install_attribution_from_value(&value, now)
}

pub(crate) fn active_install_attribution_from_value(
    value: &Value,
    now: DateTime<Utc>,
) -> Option<ActiveInstallAttribution> {
    if value.get("schema_version").and_then(Value::as_u64) != Some(1)
        || value.get("manager").and_then(Value::as_str) != Some("ctx-hosted-installer")
    {
        return None;
    }
    let id = value.get("install_attempt_id")?.as_str()?;
    if !crate::upgrade::is_valid_install_attempt_id(id) {
        return None;
    }
    let installed_at = DateTime::parse_from_rfc3339(value.get("installed_at")?.as_str()?)
        .ok()?
        .with_timezone(&Utc);
    let age = now.signed_duration_since(installed_at);
    if age < TimeDelta::zero() || age >= TimeDelta::seconds(INSTALL_ATTRIBUTION_WINDOW_SECS) {
        return None;
    }
    Some(ActiveInstallAttribution {
        install_attempt_id: id.to_owned(),
        installed_at,
    })
}

pub(crate) fn install_marker_path(exe: &Path) -> PathBuf {
    let mut marker = exe.as_os_str().to_owned();
    marker.push(".install.json");
    PathBuf::from(marker)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn time(value: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(value)
            .unwrap()
            .with_timezone(&Utc)
    }

    #[test]
    fn parses_active_canonical_install_attribution() {
        let marker = parse_install_marker_at(
            br#"{
                "schema_version":1,
                "manager":"ctx-hosted-installer",
                "install_attempt_id":"ia_01-HOSTED",
                "installed_at":"2026-07-15T12:00:01Z"
            }"#,
            time("2026-07-22T12:00:00Z"),
        )
        .expect("active marker");

        assert_eq!(marker.install_attempt_id, "ia_01-HOSTED");
        assert_eq!(marker.installed_at, time("2026-07-15T12:00:01Z"));
    }

    #[test]
    fn ignores_expired_future_missing_or_malformed_attribution() {
        let now = time("2026-07-22T12:00:00Z");
        let marker = |installed_at: &str| {
            format!(
                r#"{{
                    "schema_version":1,
                    "manager":"ctx-hosted-installer",
                    "install_attempt_id":"ia_01-HOSTED",
                    "installed_at":"{installed_at}"
                }}"#
            )
        };
        assert!(
            parse_install_marker_at(marker("2026-07-15T12:00:00Z").as_bytes(), now).is_none(),
            "the seven-day boundary is expired"
        );
        assert!(
            parse_install_marker_at(marker("2026-07-22T12:00:01Z").as_bytes(), now).is_none(),
            "future timestamps fail closed"
        );
        for invalid in [
            b"{not-json".as_slice(),
            br#"{"schema_version":1,"manager":"ctx-hosted-installer","install_attempt_id":"ia_01-HOSTED"}"#,
            br#"{"schema_version":1,"manager":"ctx-hosted-installer","install_attempt_id":"ia_01-HOSTED","installed_at":"not-a-time"}"#,
            br#"{"schema_version":1,"install_attempt_id":"ia_01-HOSTED","installed_at":"2026-07-22T11:00:00Z"}"#,
            br#"{"manager":"ctx-hosted-installer","install_attempt_id":"ia_01-HOSTED","installed_at":"2026-07-22T11:00:00Z"}"#,
            br#"{"schema_version":1,"manager":"ctx-hosted-installer","install_attempt_id":"ia_1234567","installed_at":"2026-07-22T11:00:00Z"}"#,
        ] {
            assert!(parse_install_marker_at(invalid, now).is_none());
        }
        assert!(parse_install_marker_at(
            format!(
                r#"{{
                    "schema_version":1,
                    "manager":"ctx-hosted-installer",
                    "install_attempt_id":"ia_{}",
                    "installed_at":"2026-07-22T11:00:00Z"
                }}"#,
                "a".repeat(129)
            )
            .as_bytes(),
            now,
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
