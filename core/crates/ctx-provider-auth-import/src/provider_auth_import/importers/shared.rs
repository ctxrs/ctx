use super::*;

pub(super) fn import_result(
    material: &CandidateMaterial,
    status: &str,
    profile_id: Option<String>,
    message: Option<String>,
) -> ProviderAuthImportResult {
    ProviderAuthImportResult {
        candidate_id: material.candidate.id.clone(),
        provider_id: material.candidate.provider_id.clone(),
        status: status.to_string(),
        profile_id,
        message,
    }
}

pub(super) async fn set_subscription_source_if_supported(
    data_root: &Path,
    provider_id: &str,
) -> Result<()> {
    let should_set = matches!(
        provider_id,
        "codex"
            | "gemini"
            | "kimi"
            | "qwen"
            | "opencode"
            | "mistral"
            | "goose"
            | "amp"
            | "droid"
            | "cline"
            | "openhands"
            | "copilot"
            | "auggie"
            | "pi"
    );
    if should_set {
        let _ = harness_sources::set_provider_source_selection(
            data_root,
            provider_id,
            harness_sources::HarnessSourceKind::Subscription,
            None,
        )
        .await?;
    }
    Ok(())
}

pub(super) async fn import_endpoint_candidate(
    data_root: &Path,
    material: &CandidateMaterial,
    provider_id: &str,
    api_key: String,
    base_url: Option<String>,
    auth_type: Option<String>,
    model_override: Option<String>,
) -> Result<ProviderAuthImportResult> {
    let api_shape = harness_sources::default_shape_for_provider(provider_id)
        .ok_or_else(|| anyhow::anyhow!("provider does not support endpoint auth import"))?;
    let match_state = harness_sources::find_provider_endpoint_import_match(
        data_root,
        provider_id,
        base_url.clone(),
        api_shape,
        auth_type.clone(),
        model_override.clone(),
        &api_key,
    )
    .await?;

    if let Some(found) = match_state.as_ref() {
        if found.kind == harness_sources::HarnessEndpointImportMatchKind::ExactCredentials {
            let _ = harness_sources::set_provider_source_selection(
                data_root,
                provider_id,
                harness_sources::HarnessSourceKind::Endpoint,
                Some(found.endpoint_id.clone()),
            )
            .await?;
            legacy::upsert_imported_profile_metadata(
                data_root,
                material,
                &found.endpoint_id,
                base_url.clone(),
                auth_type.clone(),
            )
            .await?;
            return Ok(import_result(
                material,
                "already_imported",
                Some(found.endpoint_id.clone()),
                Some("Matching endpoint credential already imported.".to_string()),
            ));
        }
    }

    let endpoint = harness_sources::upsert_provider_endpoint(
        data_root,
        provider_id,
        harness_sources::HarnessEndpointUpsert {
            endpoint_id: match_state.as_ref().map(|found| found.endpoint_id.clone()),
            name: material.label.clone().unwrap_or_else(|| {
                format!("{} imported endpoint", material.candidate.provider_label)
            }),
            base_url,
            api_shape: Some(api_shape),
            auth_type,
            model_override,
            api_key: Some(api_key),
            service_account_json: None,
            project_id: None,
            location: None,
        },
    )
    .await?;

    let _ = harness_sources::set_provider_source_selection(
        data_root,
        provider_id,
        harness_sources::HarnessSourceKind::Endpoint,
        Some(endpoint.id.clone()),
    )
    .await?;
    legacy::upsert_imported_profile_metadata(
        data_root,
        material,
        &endpoint.id,
        endpoint.base_url.clone(),
        Some(endpoint.auth_type.clone()),
    )
    .await?;

    let status = match match_state {
        Some(found)
            if found.kind == harness_sources::HarnessEndpointImportMatchKind::SameConfig =>
        {
            "updated"
        }
        _ => "imported",
    };

    Ok(import_result(
        material,
        status,
        Some(endpoint.id),
        Some(if status == "updated" {
            "Endpoint credential updated.".to_string()
        } else {
            "Endpoint credential imported.".to_string()
        }),
    ))
}
