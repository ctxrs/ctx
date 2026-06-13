use anyhow::{Context, Result};
use serde::Deserialize;
use std::path::{Path, PathBuf};

const ARTIFACT_IDENTITY_FILENAME: &str = "artifact_identity.json";
const BUILD_IDENTITY_PATH_ENV: &str = "CTX_BUILD_IDENTITY_PATH";
const BUNDLE_DIR_ENV: &str = "CTX_BUNDLE_DIR";

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
pub(super) struct HelperBuildIdentity {
    #[serde(rename = "schemaVersion")]
    pub(super) schema_version: u32,
    #[serde(rename = "exactVersion")]
    pub(super) exact_version: String,
    #[serde(rename = "buildId")]
    pub(super) build_id: String,
    #[serde(rename = "compatibilityToken")]
    pub(super) compatibility_token: String,
}

fn compile_time_build_identity() -> HelperBuildIdentity {
    let exact_version = option_env!("CTX_RELEASE_EFFECTIVE_VERSION")
        .unwrap_or(env!("CARGO_PKG_VERSION"))
        .to_string();
    HelperBuildIdentity {
        schema_version: 1,
        exact_version: exact_version.clone(),
        build_id: option_env!("CTX_BUILD_ID")
            .unwrap_or(env!("CARGO_PKG_VERSION"))
            .to_string(),
        compatibility_token: option_env!("CTX_COMPATIBILITY_TOKEN")
            .or(option_env!("CTX_DEV_INSTANCE_ID"))
            .unwrap_or("unknown")
            .to_string(),
    }
}

fn configured_identity_path() -> Result<Option<PathBuf>> {
    if let Some(raw) = std::env::var(BUILD_IDENTITY_PATH_ENV).ok() {
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            anyhow::bail!("{BUILD_IDENTITY_PATH_ENV} must not be empty");
        }
        return Ok(Some(PathBuf::from(trimmed)));
    }
    let Some(raw) = std::env::var(BUNDLE_DIR_ENV).ok() else {
        return Ok(None);
    };
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        anyhow::bail!("{BUNDLE_DIR_ENV} must not be empty when set");
    }
    Ok(Some(
        PathBuf::from(trimmed).join(ARTIFACT_IDENTITY_FILENAME),
    ))
}

pub(super) fn parse_build_identity(path: &Path) -> Result<HelperBuildIdentity> {
    let raw = std::fs::read_to_string(path)
        .with_context(|| format!("reading helper build identity {}", path.display()))?;
    let identity: HelperBuildIdentity = serde_json::from_str(&raw)
        .with_context(|| format!("parsing helper build identity {}", path.display()))?;
    if identity.schema_version != 1 {
        anyhow::bail!(
            "unsupported helper build identity schema {} in {}",
            identity.schema_version,
            path.display()
        );
    }
    if identity.exact_version.trim().is_empty() {
        anyhow::bail!(
            "helper build identity missing exactVersion in {}",
            path.display()
        );
    }
    if identity.build_id.trim().is_empty() {
        anyhow::bail!(
            "helper build identity missing buildId in {}",
            path.display()
        );
    }
    if identity.compatibility_token.trim().is_empty() {
        anyhow::bail!(
            "helper build identity missing compatibilityToken in {}",
            path.display()
        );
    }
    Ok(identity)
}

pub(super) fn current_build_identity() -> Result<HelperBuildIdentity> {
    let Some(identity_path) = configured_identity_path()? else {
        return Ok(compile_time_build_identity());
    };
    parse_build_identity(&identity_path)
}

#[cfg(test)]
mod tests {
    use super::{
        current_build_identity, parse_build_identity, BUILD_IDENTITY_PATH_ENV, BUNDLE_DIR_ENV,
    };
    use std::ffi::OsString;
    use std::fs;
    use std::path::PathBuf;
    use std::sync::{Mutex, OnceLock};
    use std::time::{SystemTime, UNIX_EPOCH};

    fn env_lock() -> &'static Mutex<()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
    }

    fn temp_identity_path(name: &str) -> PathBuf {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock before unix epoch")
            .as_nanos();
        std::env::temp_dir().join(format!("ctx-helper-build-identity-{name}-{unique}.json"))
    }

    fn write_identity(path: &PathBuf) {
        fs::write(
            path,
            r#"{
  "schemaVersion": 1,
  "exactVersion": "0.59.0-canary.deadbeefcafe",
  "buildId": "deadbeefcafe",
  "compatibilityToken": "artifact-deadbeefcafebabefeedface1234567890abcdef"
}
"#,
        )
        .expect("write identity");
    }

    fn restore_var(name: &str, value: Option<OsString>) {
        match value {
            Some(value) => std::env::set_var(name, value),
            None => std::env::remove_var(name),
        }
    }

    #[test]
    fn parse_build_identity_accepts_valid_manifest() {
        let path = temp_identity_path("valid");
        write_identity(&path);
        let identity = parse_build_identity(&path).expect("parse identity");
        assert_eq!(identity.exact_version, "0.59.0-canary.deadbeefcafe");
        assert_eq!(identity.build_id, "deadbeefcafe");
        assert_eq!(
            identity.compatibility_token,
            "artifact-deadbeefcafebabefeedface1234567890abcdef"
        );
        let _ = fs::remove_file(path);
    }

    #[test]
    fn current_build_identity_prefers_explicit_identity_path() {
        let _guard = env_lock().lock().expect("lock env");
        let path = temp_identity_path("explicit-path");
        write_identity(&path);
        let previous_path = std::env::var_os(BUILD_IDENTITY_PATH_ENV);
        let previous_bundle_dir = std::env::var_os(BUNDLE_DIR_ENV);
        std::env::set_var(BUILD_IDENTITY_PATH_ENV, &path);
        std::env::remove_var(BUNDLE_DIR_ENV);

        let identity = current_build_identity().expect("current build identity");
        assert_eq!(identity.exact_version, "0.59.0-canary.deadbeefcafe");
        assert_eq!(identity.build_id, "deadbeefcafe");

        restore_var(BUILD_IDENTITY_PATH_ENV, previous_path);
        restore_var(BUNDLE_DIR_ENV, previous_bundle_dir);
        let _ = fs::remove_file(path);
    }

    #[test]
    fn parse_build_identity_rejects_missing_build_id() {
        let path = temp_identity_path("missing-build-id");
        fs::write(
            &path,
            r#"{
  "schemaVersion": 1,
  "exactVersion": "0.59.0",
  "buildId": "",
  "compatibilityToken": "artifact-localpkg123"
}
"#,
        )
        .expect("write identity");
        let error = parse_build_identity(&path).expect_err("missing buildId should fail");
        assert!(error.to_string().contains("missing buildId"));
        let _ = fs::remove_file(path);
    }
}
