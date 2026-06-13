use super::*;
use std::collections::{BTreeMap, BTreeSet};

pub fn merge_org_policy_with_overlay(
    snapshot: &OrgPolicySnapshot,
    overlay: Option<&WorkspacePolicyOverlay>,
) -> EffectiveWorkspacePolicy {
    debug_assert!(
        overlay.is_none_or(|value| value.org_id == snapshot.org_id),
        "workspace policy overlay org_id must match snapshot org_id"
    );
    let overlay_allowed_providers = overlay.and_then(|value| value.allowed_providers.as_deref());
    let merged_allowed_providers = intersect_optional_string_lists(
        snapshot.allowed_providers.as_deref(),
        overlay_allowed_providers,
    );

    let merged_allowed_models = merge_allowed_models(
        &snapshot.allowed_models,
        overlay.map(|value| &value.allowed_models),
        merged_allowed_providers.as_deref(),
    );

    let merged_required_execution_environment = if snapshot.required_execution_environment.is_some()
        || overlay
            .and_then(|value| value.required_execution_environment)
            .is_some()
    {
        Some(RequiredExecutionEnvironment::Sandbox)
    } else {
        None
    };

    let merged_network_profiles = intersect_network_profiles(
        &snapshot.allowed_network_profiles,
        overlay.and_then(|value| value.allowed_network_profiles.as_deref()),
    );

    let merged_route_types = intersect_optional_copy_lists(
        Some(snapshot.route_policy.allowed_route_types.as_slice()),
        overlay.and_then(|value| value.allowed_route_types.as_deref()),
    )
    .unwrap_or_default();

    EffectiveWorkspacePolicy {
        org_id: snapshot.org_id,
        policy_snapshot_id: snapshot.id,
        policy_version: snapshot.policy_version.clone(),
        workspace_id: overlay.map(|value| value.workspace_id),
        allowed_providers: merged_allowed_providers,
        allowed_models: merged_allowed_models,
        required_execution_environment: merged_required_execution_environment,
        allowed_network_profiles: merged_network_profiles,
        route_policy: RoutePolicy {
            allowed_route_types: merged_route_types,
        },
        archive_policy: snapshot.archive_policy.clone(),
        features: merge_feature_states(&snapshot.features, overlay.map(|value| &value.features)),
    }
}

pub struct OrgPolicyRunRequest<'a> {
    pub provider_id: &'a str,
    pub model_id: &'a str,
    pub execution_environment: ExecutionEnvironment,
    pub network_profile: NetworkProfile,
    pub route_type: Option<RouteType>,
    pub now: DateTime<Utc>,
}

pub fn org_policy_allows_run(
    snapshot: &OrgPolicySnapshot,
    overlay: Option<&WorkspacePolicyOverlay>,
    request: OrgPolicyRunRequest<'_>,
) -> Result<PolicyWindowState, PolicyDenyReason> {
    let window_state = policy_window_state(snapshot, request.now);
    if !window_state.permits_org_run() {
        return Err(PolicyDenyReason::PolicyHardExpired);
    }

    let effective_policy = merge_org_policy_with_overlay(snapshot, overlay);
    if !is_provider_allowed(
        effective_policy.allowed_providers.as_deref(),
        request.provider_id,
    ) {
        return Err(PolicyDenyReason::ProviderNotAllowed);
    }
    if !is_provider_model_allowed(
        effective_policy.allowed_providers.as_deref(),
        &effective_policy.allowed_models,
        request.provider_id,
        request.model_id,
    ) {
        return Err(PolicyDenyReason::ModelNotAllowed);
    }
    if !execution_environment_satisfies_requirement(
        effective_policy.required_execution_environment,
        request.execution_environment,
    ) {
        return Err(PolicyDenyReason::ExecutionEnvironmentNotAllowed);
    }
    if !is_network_profile_allowed(
        &effective_policy.allowed_network_profiles,
        request.network_profile,
    ) {
        return Err(PolicyDenyReason::NetworkProfileNotAllowed);
    }
    if let Some(route_type) = request.route_type {
        if !is_route_allowed(&effective_policy.route_policy, route_type) {
            if route_type.is_personal() {
                return Err(PolicyDenyReason::PersonalRouteNotAllowed);
            }
            return Err(PolicyDenyReason::RouteTypeNotAllowed);
        }
    }

    Ok(window_state)
}

fn merge_allowed_models(
    allowed_models: &BTreeMap<String, Vec<String>>,
    overlay_allowed_models: Option<&BTreeMap<String, Vec<String>>>,
    merged_allowed_providers: Option<&[String]>,
) -> BTreeMap<String, Vec<String>> {
    let mut provider_ids = BTreeSet::new();
    provider_ids.extend(allowed_models.keys().cloned());
    if let Some(overlay_allowed_models) = overlay_allowed_models {
        provider_ids.extend(overlay_allowed_models.keys().cloned());
    }

    let mut out = BTreeMap::new();
    for provider_id in provider_ids {
        if !is_provider_allowed(merged_allowed_providers, &provider_id) {
            continue;
        }

        let merged = match (
            allowed_models.get(&provider_id),
            overlay_allowed_models.and_then(|value| value.get(&provider_id)),
        ) {
            (Some(org_models), Some(overlay_models)) => {
                let overlay_set: BTreeSet<_> = overlay_models.iter().cloned().collect();
                org_models
                    .iter()
                    .filter(|candidate| overlay_set.contains(*candidate))
                    .cloned()
                    .collect::<Vec<_>>()
            }
            (Some(org_models), None) => canonicalize_string_list(org_models),
            (None, Some(overlay_models)) => canonicalize_string_list(overlay_models),
            (None, None) => continue,
        };

        out.insert(provider_id, canonicalize_string_list(&merged));
    }
    out
}

fn merge_feature_states(
    feature_states: &BTreeMap<String, PolicyFeatureState>,
    overlay_feature_states: Option<&BTreeMap<String, PolicyFeatureState>>,
) -> BTreeMap<String, PolicyFeatureState> {
    let mut out = feature_states.clone();
    if let Some(overlay_feature_states) = overlay_feature_states {
        for (feature, state) in overlay_feature_states {
            if matches!(state, PolicyFeatureState::Disabled) {
                out.insert(feature.clone(), PolicyFeatureState::Disabled);
            }
        }
    }
    out
}

fn intersect_optional_string_lists(
    values: Option<&[String]>,
    overlays: Option<&[String]>,
) -> Option<Vec<String>> {
    match (values, overlays) {
        (Some(values), Some(overlays)) => {
            let overlay_set: BTreeSet<_> = overlays.iter().cloned().collect();
            Some(
                values
                    .iter()
                    .filter(|value| overlay_set.contains(*value))
                    .cloned()
                    .collect::<Vec<_>>(),
            )
        }
        (Some(values), None) => Some(canonicalize_string_list(values)),
        (None, Some(overlays)) => Some(canonicalize_string_list(overlays)),
        (None, None) => None,
    }
}

fn intersect_optional_copy_lists<T>(values: Option<&[T]>, overlays: Option<&[T]>) -> Option<Vec<T>>
where
    T: Copy + Ord,
{
    match (values, overlays) {
        (Some(values), Some(overlays)) => {
            let overlay_set: BTreeSet<_> = overlays.iter().copied().collect();
            Some(
                values
                    .iter()
                    .copied()
                    .filter(|value| overlay_set.contains(value))
                    .collect::<Vec<_>>(),
            )
        }
        (Some(values), None) => Some(canonicalize_copy_list(values)),
        (None, Some(overlays)) => Some(canonicalize_copy_list(overlays)),
        (None, None) => None,
    }
}

fn canonicalize_string_list(values: &[String]) -> Vec<String> {
    let unique: BTreeSet<_> = values.iter().cloned().collect();
    unique.into_iter().collect()
}

fn canonicalize_copy_list<T>(values: &[T]) -> Vec<T>
where
    T: Copy + Ord,
{
    let unique: BTreeSet<_> = values.iter().copied().collect();
    unique.into_iter().collect()
}
