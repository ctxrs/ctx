use super::*;

pub async fn load_imported_registry(data_root: &Path) -> Result<ProviderImportedAuthRegistry> {
    let path = imported_registry_path(data_root);
    match tokio::fs::read_to_string(&path).await {
        Ok(contents) => serde_json::from_str(&contents)
            .with_context(|| format!("parsing imported auth registry at {}", path.display())),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            Ok(ProviderImportedAuthRegistry::default())
        }
        Err(err) => Err(err)
            .with_context(|| format!("reading imported auth registry at {}", path.display())),
    }
}

pub async fn save_imported_registry(
    data_root: &Path,
    registry: &ProviderImportedAuthRegistry,
) -> Result<()> {
    let path = imported_registry_path(data_root);
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }
    let payload = serde_json::to_vec_pretty(registry)?;
    tokio::fs::write(path, payload).await?;
    Ok(())
}

pub(super) async fn upsert_imported_profile_metadata(
    data_root: &Path,
    material: &CandidateMaterial,
    profile_id: &str,
    endpoint: Option<String>,
    auth_type: Option<String>,
) -> Result<()> {
    let fingerprint = material
        .candidate
        .fingerprint
        .clone()
        .or_else(|| {
            material
                .secret_bytes
                .as_ref()
                .map(|bytes| catalog::sha256_hex(bytes))
        })
        .ok_or_else(|| anyhow::anyhow!("imported profile metadata requires secret fingerprint"))?;
    let label = material
        .label
        .clone()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| {
            format!(
                "Imported {} profile",
                material.candidate.provider_label.as_str()
            )
        });
    let now = Utc::now();
    let mut registry = load_imported_registry(data_root).await?;

    if let Some(existing) = registry
        .profiles
        .iter_mut()
        .find(|profile| profile.id == profile_id)
    {
        let imported_at = existing.imported_at;
        *existing = ProviderImportedAuthProfile {
            id: profile_id.to_string(),
            provider_id: material.candidate.provider_id.clone(),
            provider_label: material.candidate.provider_label.clone(),
            label,
            account_identity: material.candidate.account_identity.clone(),
            endpoint: endpoint.or_else(|| material.candidate.endpoint.clone()),
            auth_type: auth_type.or_else(|| material.candidate.auth_type.clone()),
            source_path: material.candidate.path.clone(),
            source_kind: material.candidate.kind.clone(),
            secret_fingerprint: fingerprint,
            imported_at,
            updated_at: now,
        };
    } else {
        registry.profiles.push(ProviderImportedAuthProfile {
            id: profile_id.to_string(),
            provider_id: material.candidate.provider_id.clone(),
            provider_label: material.candidate.provider_label.clone(),
            label,
            account_identity: material.candidate.account_identity.clone(),
            endpoint: endpoint.or_else(|| material.candidate.endpoint.clone()),
            auth_type: auth_type.or_else(|| material.candidate.auth_type.clone()),
            source_path: material.candidate.path.clone(),
            source_kind: material.candidate.kind.clone(),
            secret_fingerprint: fingerprint,
            imported_at: now,
            updated_at: now,
        });
    }

    sort_imported_profiles(&mut registry.profiles);
    save_imported_registry(data_root, &registry).await?;
    Ok(())
}

pub(super) async fn legacy_migration_marker_exists(data_root: &Path) -> bool {
    tokio::fs::metadata(legacy_migration_marker_path(data_root))
        .await
        .is_ok()
}

pub(super) async fn write_legacy_migration_marker(data_root: &Path) -> Result<()> {
    let marker_path = legacy_migration_marker_path(data_root);
    if let Some(parent) = marker_path.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }
    let marker = LegacyMigrationMarker {
        version: 1,
        completed_at: Utc::now(),
    };
    tokio::fs::write(marker_path, serde_json::to_vec_pretty(&marker)?).await?;
    Ok(())
}

pub(super) async fn read_legacy_secret_material_bytes(
    data_root: &Path,
    profile_id: &str,
) -> Result<Option<Vec<u8>>> {
    let path = imported_secret_path(data_root, profile_id);
    let payload = match tokio::fs::read_to_string(&path).await {
        Ok(payload) => payload,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(err) => {
            return Err(err).with_context(|| {
                format!(
                    "reading imported auth secret material at {}",
                    path.display()
                )
            });
        }
    };
    let parsed = serde_json::from_str::<StoredSecretMaterial>(&payload).with_context(|| {
        format!(
            "parsing imported auth secret material at {}",
            path.display()
        )
    })?;
    let content = parsed.content_b64.ok_or_else(|| {
        anyhow::anyhow!(
            "imported auth secret material at {} is missing content_b64",
            path.display()
        )
    })?;
    let bytes = base64::Engine::decode(&base64::engine::general_purpose::STANDARD, content)
        .with_context(|| {
            format!(
                "decoding imported auth secret material at {}",
                path.display()
            )
        })?;
    Ok(Some(bytes))
}

pub(super) fn imported_secret_path(data_root: &Path, profile_id: &str) -> PathBuf {
    imported_secrets_dir(data_root).join(format!("{profile_id}.json"))
}

pub(super) fn legacy_migration_marker_path(data_root: &Path) -> PathBuf {
    data_root
        .join("providers")
        .join("auth_import")
        .join("migration_v1.json")
}

pub(super) fn imported_registry_path(data_root: &Path) -> PathBuf {
    data_root
        .join("providers")
        .join("auth_import")
        .join("profiles.json")
}

pub(super) fn imported_secrets_dir(data_root: &Path) -> PathBuf {
    data_root
        .join("providers")
        .join("auth_import")
        .join("secrets")
}

fn sort_imported_profiles(profiles: &mut [ProviderImportedAuthProfile]) {
    profiles.sort_by(|a, b| {
        a.provider_label
            .cmp(&b.provider_label)
            .then_with(|| a.label.cmp(&b.label))
            .then_with(|| a.id.cmp(&b.id))
    });
}
