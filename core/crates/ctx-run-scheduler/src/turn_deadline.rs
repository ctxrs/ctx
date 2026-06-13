use std::collections::HashMap;
use std::time::Duration;

use ctx_core::provider_policy::{CTX_CRP_LAUNCH_POLICY_ENV, CTX_CRP_LAUNCH_POLICY_FULL};
use ctx_settings_model::ProviderControlMode;

pub fn apply_crp_launch_policy_env_for_control_mode(
    provider_env: &mut HashMap<String, String>,
    control_mode: &ProviderControlMode,
) {
    provider_env.remove(CTX_CRP_LAUNCH_POLICY_ENV);
    match control_mode {
        ProviderControlMode::Full => {
            provider_env.insert(
                CTX_CRP_LAUNCH_POLICY_ENV.to_string(),
                CTX_CRP_LAUNCH_POLICY_FULL.to_string(),
            );
        }
        ProviderControlMode::HarnessNative | ProviderControlMode::CtxEnforced => {}
    }
}

const DEFAULT_TURN_START_DEADLINE: Duration = Duration::from_secs(60);
const CONTAINER_TURN_START_DEADLINE: Duration = Duration::from_secs(135);

pub fn turn_start_deadline(provider_env: &HashMap<String, String>) -> Duration {
    turn_start_deadline_with_override(
        provider_env,
        std::env::var("CTX_TURN_START_DEADLINE_MS").ok().as_deref(),
    )
}

fn turn_start_deadline_with_override(
    provider_env: &HashMap<String, String>,
    configured: Option<&str>,
) -> Duration {
    configured
        .and_then(|value| value.trim().parse::<u64>().ok())
        .filter(|value| *value > 0)
        .map(Duration::from_millis)
        .unwrap_or_else(|| {
            if provider_env.contains_key("CTX_HARNESS_CONTAINER_ID") {
                CONTAINER_TURN_START_DEADLINE
            } else {
                DEFAULT_TURN_START_DEADLINE
            }
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn turn_start_deadline_uses_longer_container_budget() {
        let env = HashMap::from([(
            "CTX_HARNESS_CONTAINER_ID".to_string(),
            "ctx-harness-test".to_string(),
        )]);

        assert_eq!(
            turn_start_deadline_with_override(&env, None),
            CONTAINER_TURN_START_DEADLINE
        );
    }

    #[test]
    fn turn_start_deadline_env_override_still_wins() {
        let env = HashMap::from([(
            "CTX_HARNESS_CONTAINER_ID".to_string(),
            "ctx-harness-test".to_string(),
        )]);

        assert_eq!(
            turn_start_deadline_with_override(&env, Some("2500")),
            Duration::from_millis(2500)
        );
    }
}
