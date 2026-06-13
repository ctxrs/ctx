use super::*;

mod providers;
mod shared;

pub(crate) use self::providers::import_codex_candidate;
use self::providers::{
    import_amp_candidate, import_gemini_auth_file_candidate, import_gemini_env_candidate,
    import_opencode_candidate, import_qwen_candidate,
};
use self::shared::import_result;

pub async fn list_provider_auth_profiles(
    data_root: &Path,
) -> Result<Vec<ProviderImportedAuthProfile>> {
    CanonicalAuthImporter::new(data_root).list_profiles().await
}

pub async fn import_provider_auth_candidates(
    data_root: &Path,
    candidate_ids: &[String],
) -> Result<Vec<ProviderAuthImportResult>> {
    let scanner = AuthImportScanner::discover()?;
    CanonicalAuthImporter::new(data_root)
        .import_candidates(&scanner, candidate_ids)
        .await
}

impl<'a> CanonicalAuthImporter<'a> {
    pub(super) fn new(data_root: &'a Path) -> Self {
        Self { data_root }
    }

    async fn list_profiles(&self) -> Result<Vec<ProviderImportedAuthProfile>> {
        self.migrate_legacy_imported_profiles_once().await?;
        let registry = legacy::load_imported_registry(self.data_root).await?;
        Ok(registry.profiles)
    }

    async fn import_candidates(
        &self,
        scanner: &AuthImportScanner,
        candidate_ids: &[String],
    ) -> Result<Vec<ProviderAuthImportResult>> {
        if candidate_ids.is_empty() {
            return Ok(Vec::new());
        }
        self.migrate_legacy_imported_profiles_once().await?;

        let materials = scanner.scan();
        let mut by_id: HashMap<String, CandidateMaterial> = HashMap::new();
        for material in materials {
            by_id.insert(material.candidate.id.clone(), material);
        }

        let mut results = Vec::new();
        for candidate_id in candidate_ids {
            let Some(material) = by_id.get(candidate_id) else {
                results.push(ProviderAuthImportResult {
                    candidate_id: candidate_id.clone(),
                    provider_id: "unknown".to_string(),
                    status: "error".to_string(),
                    profile_id: None,
                    message: Some("Candidate no longer available; re-scan and retry.".to_string()),
                });
                continue;
            };

            if !material.importable {
                results.push(ProviderAuthImportResult {
                    candidate_id: material.candidate.id.clone(),
                    provider_id: material.candidate.provider_id.clone(),
                    status: "unsupported".to_string(),
                    profile_id: None,
                    message: material.candidate.unsupported_reason.clone().or_else(|| {
                        Some("Candidate cannot be imported automatically.".to_string())
                    }),
                });
                continue;
            }

            match self.import_candidate_to_canonical(material).await {
                Ok(result) => results.push(result),
                Err(error) => results.push(ProviderAuthImportResult {
                    candidate_id: material.candidate.id.clone(),
                    provider_id: material.candidate.provider_id.clone(),
                    status: "error".to_string(),
                    profile_id: None,
                    message: Some(error.to_string()),
                }),
            }
        }

        Ok(results)
    }

    async fn import_candidate_to_canonical(
        &self,
        material: &CandidateMaterial,
    ) -> Result<ProviderAuthImportResult> {
        if !material.importable || material.secret_bytes.is_none() {
            return Ok(import_result(
                material,
                "unsupported",
                None,
                material
                    .candidate
                    .unsupported_reason
                    .clone()
                    .or_else(|| Some("No importable auth material.".to_string())),
            ));
        }
        match material.candidate.provider_id.as_str() {
            "codex" => import_codex_candidate(self.data_root, material).await,
            "gemini" => {
                if material.candidate.kind == "env_file" {
                    import_gemini_env_candidate(self.data_root, material).await
                } else {
                    import_gemini_auth_file_candidate(self.data_root, material).await
                }
            }
            "qwen" => import_qwen_candidate(self.data_root, material).await,
            "opencode" => import_opencode_candidate(self.data_root, material).await,
            "amp" => import_amp_candidate(self.data_root, material).await,
            _ => Ok(import_result(
                material,
                "unsupported",
                None,
                Some(format!(
                    "Provider '{}' import is not wired into canonical auth storage yet.",
                    material.candidate.provider_id
                )),
            )),
        }
    }

    pub(super) async fn migrate_legacy_imported_profiles_once(&self) -> Result<()> {
        if legacy::legacy_migration_marker_exists(self.data_root).await {
            return Ok(());
        }

        let mut registry = legacy::load_imported_registry(self.data_root).await?;
        if registry.profiles.is_empty() {
            legacy::write_legacy_migration_marker(self.data_root).await?;
            return Ok(());
        }

        let mut remaining_profiles: Vec<ProviderImportedAuthProfile> = Vec::new();
        for profile in registry.profiles.iter().cloned() {
            let Some(secret_bytes) =
                legacy::read_legacy_secret_material_bytes(self.data_root, &profile.id).await?
            else {
                remaining_profiles.push(profile);
                continue;
            };
            let material = CandidateMaterial {
                candidate: ProviderAuthImportCandidate {
                    id: profile.id.clone(),
                    provider_id: profile.provider_id.clone(),
                    provider_label: profile.provider_label.clone(),
                    kind: profile.source_kind.clone(),
                    path: profile.source_path.clone(),
                    signal_strength: "legacy".to_string(),
                    confidence: "legacy".to_string(),
                    parse_status: "parsed".to_string(),
                    unsupported_reason: None,
                    summary: None,
                    account_identity: profile.account_identity.clone(),
                    endpoint: profile.endpoint.clone(),
                    auth_type: profile.auth_type.clone(),
                    fingerprint: Some(profile.secret_fingerprint.clone()),
                    last_modified: None,
                },
                importable: true,
                secret_bytes: Some(secret_bytes),
                label: Some(profile.label.clone()),
            };
            let migrated = match self.import_candidate_to_canonical(&material).await {
                Ok(result) => provider_auth_import_result_mutates_effective_auth(&result),
                Err(_) => false,
            };
            if migrated {
                let _ = tokio::fs::remove_file(legacy::imported_secret_path(
                    self.data_root,
                    &profile.id,
                ))
                .await;
            } else {
                remaining_profiles.push(profile);
            }
        }

        registry.profiles = remaining_profiles;
        legacy::save_imported_registry(self.data_root, &registry).await?;
        if registry.profiles.is_empty() {
            let _ = tokio::fs::remove_dir_all(legacy::imported_secrets_dir(self.data_root)).await;
            legacy::write_legacy_migration_marker(self.data_root).await?;
        }
        Ok(())
    }
}

#[cfg(test)]
pub(super) async fn import_candidate_to_canonical(
    data_root: &Path,
    material: &CandidateMaterial,
) -> Result<ProviderAuthImportResult> {
    CanonicalAuthImporter::new(data_root)
        .import_candidate_to_canonical(material)
        .await
}

#[cfg(test)]
pub(super) async fn migrate_legacy_imported_profiles_once(data_root: &Path) -> Result<()> {
    CanonicalAuthImporter::new(data_root)
        .migrate_legacy_imported_profiles_once()
        .await
}
