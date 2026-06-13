use super::*;

pub async fn load_preferred_new_session_model_id(
    store: &Store,
    provider_id: &str,
) -> Result<Option<String>> {
    let provider_id = normalize_provider_preference_key(provider_id);
    let Some(provider_id) = provider_id else {
        return Ok(None);
    };
    let prefs = load_preferred_new_session_models(store).await?;
    Ok(prefs.get(&provider_id).cloned())
}

pub async fn load_preferred_new_session_models(store: &Store) -> Result<HashMap<String, String>> {
    let cfg = load_workspace_settings_doc(store).await?;
    let raw = cfg
        .new_session
        .and_then(|new_session| new_session.preferred_model_by_provider)
        .unwrap_or_default();
    let mut normalized = HashMap::new();
    for (provider_id, model_id) in raw {
        let Some(provider_id) = normalize_provider_preference_key(&provider_id) else {
            continue;
        };
        let Some(model_id) = trimmed_nonempty(&model_id) else {
            continue;
        };
        normalized.insert(provider_id, model_id);
    }
    Ok(normalized)
}

pub async fn update_preferred_new_session_model_id(
    store: &Store,
    provider_id: &str,
    model_id: Option<String>,
) -> Result<()> {
    let Some(provider_id) = normalize_provider_preference_key(provider_id) else {
        bail!("provider_id is required");
    };
    mutate_workspace_settings_doc(store, "preferred_new_session_model", move |cfg| {
        let mut prefs = cfg
            .new_session
            .as_ref()
            .and_then(|new_session| new_session.preferred_model_by_provider.clone())
            .unwrap_or_default();
        if let Some(model_id) = model_id.as_deref().and_then(trimmed_nonempty) {
            prefs.insert(provider_id, model_id);
        } else {
            prefs.remove(&provider_id);
        }

        if prefs.is_empty() {
            cfg.new_session = None;
        } else {
            cfg.new_session = Some(WorkspaceNewSessionConfig {
                preferred_model_by_provider: Some(prefs),
            });
        }
        Ok(())
    })
    .await
}
