use super::*;

const ARTIFACT_IDENTITY_FILENAME: &str = "artifact_identity.json";

#[derive(Clone, Debug, Deserialize)]
pub(crate) struct DesktopBuildIdentity {
    #[serde(rename = "schemaVersion")]
    pub(crate) schema_version: u32,
    #[serde(rename = "exactVersion")]
    pub(crate) exact_version: String,
    #[serde(rename = "buildId")]
    pub(crate) build_id: String,
    #[serde(rename = "compatibilityToken")]
    pub(crate) compatibility_token: String,
    #[serde(default, rename = "channel")]
    pub(crate) _legacy_channel: Option<String>,
    #[serde(default, rename = "provenanceChannel")]
    pub(crate) _provenance_channel: Option<String>,
}

pub(super) fn desktop_dev_instance_id() -> &'static str {
    option_env!("CTX_DEV_INSTANCE_ID").unwrap_or("unknown")
}

fn artifact_identity_path(app: &tauri::AppHandle) -> Result<PathBuf> {
    let bundle_dir =
        desktop_bundle_dir(app).ok_or_else(|| anyhow!("desktop bundles directory not found"))?;
    Ok(bundle_dir.join(ARTIFACT_IDENTITY_FILENAME))
}

pub(crate) fn load_desktop_build_identity(app: &tauri::AppHandle) -> Result<DesktopBuildIdentity> {
    let artifact_path = artifact_identity_path(app)?;
    let raw = std::fs::read_to_string(&artifact_path).with_context(|| {
        format!(
            "reading desktop artifact identity {}",
            artifact_path.display()
        )
    })?;
    let identity: DesktopBuildIdentity = serde_json::from_str(&raw).with_context(|| {
        format!(
            "parsing desktop artifact identity {}",
            artifact_path.display()
        )
    })?;
    if identity.schema_version != 1 {
        anyhow::bail!(
            "unsupported desktop artifact identity schema {} in {}",
            identity.schema_version,
            artifact_path.display()
        );
    }
    if identity.exact_version.trim().is_empty() {
        anyhow::bail!(
            "desktop artifact identity missing exactVersion in {}",
            artifact_path.display()
        );
    }
    if identity.build_id.trim().is_empty() {
        anyhow::bail!(
            "desktop artifact identity missing buildId in {}",
            artifact_path.display()
        );
    }
    if identity.compatibility_token.trim().is_empty() {
        anyhow::bail!(
            "desktop artifact identity missing compatibilityToken in {}",
            artifact_path.display()
        );
    }
    Ok(identity)
}
