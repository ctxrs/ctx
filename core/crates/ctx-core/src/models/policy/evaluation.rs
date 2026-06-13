use super::*;
use chrono::{DateTime, Utc};
use std::collections::BTreeSet;

pub fn is_provider_allowed(allowed_providers: Option<&[String]>, provider_id: &str) -> bool {
    match allowed_providers {
        Some(providers) => providers.iter().any(|candidate| candidate == provider_id),
        None => true,
    }
}

pub fn is_provider_model_allowed(
    allowed_providers: Option<&[String]>,
    allowed_models: &BTreeMap<String, Vec<String>>,
    provider_id: &str,
    model_id: &str,
) -> bool {
    if !is_provider_allowed(allowed_providers, provider_id) {
        return false;
    }

    match allowed_models.get(provider_id) {
        Some(models) => models.iter().any(|candidate| candidate == model_id),
        None => true,
    }
}

pub fn execution_environment_satisfies_requirement(
    required_execution_environment: Option<RequiredExecutionEnvironment>,
    execution_environment: ExecutionEnvironment,
) -> bool {
    match required_execution_environment {
        Some(RequiredExecutionEnvironment::Sandbox) => {
            matches!(execution_environment, ExecutionEnvironment::Sandbox)
        }
        None => true,
    }
}

pub fn intersect_network_profiles(
    allowed_network_profiles: &[NetworkProfile],
    overlay_network_profiles: Option<&[NetworkProfile]>,
) -> Vec<NetworkProfile> {
    let allowed: Vec<_> = allowed_network_profiles
        .iter()
        .copied()
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    let Some(overlay) = overlay_network_profiles else {
        return allowed;
    };

    let overlay_set: BTreeSet<_> = overlay.iter().copied().collect();
    allowed
        .into_iter()
        .filter(|profile| overlay_set.contains(profile))
        .collect()
}

pub fn is_network_profile_allowed(
    allowed_network_profiles: &[NetworkProfile],
    requested_network_profile: NetworkProfile,
) -> bool {
    allowed_network_profiles.contains(&requested_network_profile)
}

pub fn is_route_allowed(route_policy: &RoutePolicy, route_type: RouteType) -> bool {
    route_policy.allowed_route_types.contains(&route_type)
}

pub fn is_personal_route_allowed(route_policy: &RoutePolicy, route_type: RouteType) -> bool {
    route_type.is_personal() && is_route_allowed(route_policy, route_type)
}

pub fn is_personal_route_blocked(route_policy: &RoutePolicy, route_type: RouteType) -> bool {
    route_type.is_personal() && !is_route_allowed(route_policy, route_type)
}

pub fn policy_window_state(snapshot: &OrgPolicySnapshot, now: DateTime<Utc>) -> PolicyWindowState {
    if now <= snapshot.expires_at {
        PolicyWindowState::Fresh
    } else if now <= snapshot.grace_expires_at {
        PolicyWindowState::Grace
    } else {
        PolicyWindowState::Expired
    }
}
