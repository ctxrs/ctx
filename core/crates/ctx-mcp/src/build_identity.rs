use anyhow::{Context, Result};
use serde_json::Value;
use std::path::{Path, PathBuf};

const ARTIFACT_IDENTITY_FILENAME: &str = "artifact_identity.json";
const BUILD_IDENTITY_PATH_ENV: &str = "CTX_BUILD_IDENTITY_PATH";
const BUNDLE_DIR_ENV: &str = "CTX_BUNDLE_DIR";

fn compile_time_version() -> String {
    option_env!("CTX_RELEASE_EFFECTIVE_VERSION")
        .unwrap_or(env!("CARGO_PKG_VERSION"))
        .to_string()
}

fn configured_identity_path() -> Result<Option<PathBuf>> {
    if let Ok(raw) = std::env::var(BUILD_IDENTITY_PATH_ENV) {
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            anyhow::bail!("{BUILD_IDENTITY_PATH_ENV} must not be empty");
        }
        return Ok(Some(PathBuf::from(trimmed)));
    }
    let Ok(raw) = std::env::var(BUNDLE_DIR_ENV) else {
        return Ok(None);
    };
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        anyhow::bail!("{BUNDLE_DIR_ENV} must not be empty when set");
    }
    Ok(Some(PathBuf::from(trimmed).join(ARTIFACT_IDENTITY_FILENAME)))
}

pub(crate) fn parse_build_version(path: &Path) -> Result<String> {
    let raw = std::fs::read_to_string(path)
        .with_context(|| format!("reading MCP build identity {}", path.display()))?;
    let parsed: Value = serde_json::from_str(&raw)
        .with_context(|| format!("parsing MCP build identity {}", path.display()))?;
    let schema_version = parsed
        .get("schemaVersion")
        .and_then(Value::as_u64)
        .ok_or_else(|| anyhow::anyhow!("build identity missing schemaVersion in {}", path.display()))?;
    if schema_version != 1 {
        anyhow::bail!(
            "unsupported MCP build identity schema {} in {}",
            schema_version,
            path.display()
        );
    }
    let exact_version = parsed
        .get("exactVersion")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| anyhow::anyhow!("build identity missing exactVersion in {}", path.display()))?;
    Ok(exact_version.to_string())
}

pub(crate) fn current_build_version() -> Result<String> {
    let Some(identity_path) = configured_identity_path()? else {
        return Ok(compile_time_version());
    };
    parse_build_version(&identity_path)
}

#[cfg(test)]
mod tests {
    use super::parse_build_version;
    use std::fs;
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_identity_path(name: &str) -> PathBuf {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock before unix epoch")
            .as_nanos();
        std::env::temp_dir().join(format!("ctx-mcp-build-identity-{name}-{unique}.json"))
    }

    #[test]
    fn parse_build_version_accepts_valid_manifest() {
        let path = temp_identity_path("valid");
        fs::write(
            &path,
            r#"{
  "schemaVersion": 1,
  "exactVersion": "0.59.0-canary.deadbeefcafe"
}
"#,
        )
        .expect("write identity");
        let version = parse_build_version(&path).expect("parse version");
        assert_eq!(version, "0.59.0-canary.deadbeefcafe");
        let _ = fs::remove_file(path);
    }

    #[test]
    fn parse_build_version_rejects_missing_exact_version() {
        let path = temp_identity_path("missing-version");
        fs::write(
            &path,
            r#"{
  "schemaVersion": 1,
  "exactVersion": ""
}
"#,
        )
        .expect("write identity");
        let error = parse_build_version(&path).expect_err("missing exactVersion should fail");
        assert!(error.to_string().contains("missing exactVersion"));
        let _ = fs::remove_file(path);
    }
}
