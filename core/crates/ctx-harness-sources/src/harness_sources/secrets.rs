use super::*;

#[derive(Debug, Clone, Serialize, Deserialize)]
struct EndpointSecretEnvelope {
    version: u32,
    #[serde(default)]
    api_key: Option<String>,
    #[serde(default)]
    service_account_json: Option<String>,
    #[serde(default)]
    project_id: Option<String>,
    #[serde(default)]
    location: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct EndpointSecretMaterial {
    pub api_key: Option<String>,
    pub service_account_json: Option<String>,
    pub project_id: Option<String>,
    pub location: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct GeminiVertexSecretMaterial {
    pub service_account_json: String,
    pub project_id: String,
    pub location: String,
}

pub(super) fn ensure_safe_secret_ref(secret_ref: &str) -> Result<()> {
    if secret_ref.trim().is_empty() {
        anyhow::bail!("secret_ref is required");
    }

    let mut components = Path::new(secret_ref).components();
    match (components.next(), components.next()) {
        (Some(std::path::Component::Normal(_)), None) => Ok(()),
        _ => anyhow::bail!("secret_ref must be a single path segment"),
    }
}

pub(super) fn endpoint_secret_path(data_root: &Path, secret_ref: &str) -> Result<PathBuf> {
    ensure_safe_secret_ref(secret_ref)?;
    Ok(endpoint_secret_dir(data_root).join(secret_ref))
}

pub(super) async fn write_endpoint_secret(
    data_root: &Path,
    secret_ref: &str,
    secret: &EndpointSecretMaterial,
) -> Result<()> {
    let has_api_key = secret
        .api_key
        .as_deref()
        .map(str::trim)
        .is_some_and(|value| !value.is_empty());
    let has_service_account_json = secret
        .service_account_json
        .as_deref()
        .map(str::trim)
        .is_some_and(|value| !value.is_empty());
    if !has_api_key && !has_service_account_json {
        anyhow::bail!("endpoint secret must include api_key or service_account_json");
    }
    let dir = endpoint_secret_dir(data_root);
    ctx_fs::permissions::ensure_private_dir(&dir).await?;
    let path = endpoint_secret_path(data_root, secret_ref)?;
    let payload = serde_json::to_vec_pretty(&EndpointSecretEnvelope {
        version: SECRET_VERSION,
        api_key: secret.api_key.clone(),
        service_account_json: secret.service_account_json.clone(),
        project_id: secret.project_id.clone(),
        location: secret.location.clone(),
    })?;
    ctx_fs::permissions::write_private_file_atomic(&path, &payload).await?;
    Ok(())
}

pub(super) async fn read_endpoint_secret(
    data_root: &Path,
    secret_ref: &str,
) -> Result<EndpointSecretMaterial> {
    let path = endpoint_secret_path(data_root, secret_ref)?;
    let raw = tokio::fs::read_to_string(&path)
        .await
        .with_context(|| format!("reading endpoint secret {}", path.display()))?;
    let parsed: EndpointSecretEnvelope = serde_json::from_str(&raw)
        .with_context(|| format!("parsing endpoint secret {}", path.display()))?;
    let secret = EndpointSecretMaterial {
        api_key: parsed
            .api_key
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(ToOwned::to_owned),
        service_account_json: parsed
            .service_account_json
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(ToOwned::to_owned),
        project_id: parsed
            .project_id
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(ToOwned::to_owned),
        location: parsed
            .location
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(ToOwned::to_owned),
    };
    let has_api_key = secret
        .api_key
        .as_deref()
        .is_some_and(|value| !value.is_empty());
    let has_service_account_json = secret
        .service_account_json
        .as_deref()
        .is_some_and(|value| !value.is_empty());
    if !has_api_key && !has_service_account_json {
        anyhow::bail!("endpoint secret has no api_key or service_account_json");
    }
    Ok(secret)
}

fn endpoint_secret_dir(data_root: &Path) -> PathBuf {
    data_root.join("secrets").join("harness_endpoints")
}

fn normalized_optional_secret_text(raw: Option<&str>) -> Option<String> {
    raw.map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
}

fn normalize_vertex_location(raw: Option<&str>) -> String {
    normalized_optional_secret_text(raw).unwrap_or_else(|| "global".to_string())
}

fn derive_vertex_project_id(
    service_account_json: &str,
    explicit_project_id: Option<&str>,
) -> Result<String> {
    if let Some(project_id) = normalized_optional_secret_text(explicit_project_id) {
        return Ok(project_id);
    }
    let parsed: serde_json::Value = serde_json::from_str(service_account_json)
        .context("service_account_json must be valid JSON")?;
    let project_id = parsed
        .get("project_id")
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| {
            anyhow::anyhow!(
                "service_account_json must include project_id or provide project_id explicitly"
            )
        })?;
    Ok(project_id.to_string())
}

pub(super) fn resolve_endpoint_secret_material(
    provider_id: &str,
    auth_type: &str,
    existing_secret: Option<&EndpointSecretMaterial>,
    input: &HarnessEndpointUpsert,
) -> Result<EndpointSecretMaterial> {
    if provider_id == PROVIDER_GEMINI && auth_type == GEMINI_AUTH_TYPE_VERTEX_AI {
        let service_account_json =
            normalized_optional_secret_text(input.service_account_json.as_deref())
                .or_else(|| {
                    existing_secret
                        .and_then(|secret| secret.service_account_json.as_deref())
                        .map(ToOwned::to_owned)
                })
                .ok_or_else(|| {
                    anyhow::anyhow!("service_account_json is required for Gemini Vertex AI")
                })?;
        let project_id =
            derive_vertex_project_id(&service_account_json, input.project_id.as_deref())?;
        let location = normalized_optional_secret_text(input.location.as_deref())
            .or_else(|| {
                existing_secret
                    .and_then(|secret| secret.location.as_deref())
                    .map(ToOwned::to_owned)
            })
            .unwrap_or_else(|| "global".to_string());
        return Ok(EndpointSecretMaterial {
            api_key: None,
            service_account_json: Some(service_account_json),
            project_id: Some(project_id),
            location: Some(location),
        });
    }

    let api_key = normalized_optional_secret_text(input.api_key.as_deref())
        .or_else(|| existing_secret.and_then(|secret| secret.api_key.clone()))
        .ok_or_else(|| anyhow::anyhow!("api_key is required"))?;
    Ok(EndpointSecretMaterial {
        api_key: Some(api_key),
        service_account_json: None,
        project_id: None,
        location: None,
    })
}

pub(super) fn endpoint_secret_api_key(secret: &EndpointSecretMaterial) -> Result<String> {
    secret
        .api_key
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
        .ok_or_else(|| anyhow::anyhow!("endpoint secret has no api_key"))
}

pub(super) fn endpoint_secret_gemini_vertex(
    secret: &EndpointSecretMaterial,
) -> Result<GeminiVertexSecretMaterial> {
    let service_account_json = secret
        .service_account_json
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| anyhow::anyhow!("endpoint secret has no service_account_json"))?
        .to_string();
    let project_id = derive_vertex_project_id(&service_account_json, secret.project_id.as_deref())?;
    let location = normalize_vertex_location(secret.location.as_deref());
    Ok(GeminiVertexSecretMaterial {
        service_account_json,
        project_id,
        location,
    })
}
