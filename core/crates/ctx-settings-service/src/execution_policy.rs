use ctx_core::models::ExecutionEnvironment;
use ctx_provider_install::install_state::InstallTarget;

use ctx_settings_model::{ExecutionMode, ExecutionSettings};

pub const CTX_HOST_EXECUTION_POLICY_ENV: &str = "CTX_HOST_EXECUTION_POLICY";
const CTX_EXECUTION_MODE_ENV: &str = "CTX_EXECUTION_MODE";

pub static EXECUTION_POLICY_TEST_ENV_LOCK: tokio::sync::Mutex<()> =
    tokio::sync::Mutex::const_new(());

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum HostExecutionPolicy {
    AllowHost,
    SandboxOnly,
}

#[derive(Debug)]
pub struct ExecutionPolicyDenied {
    message: String,
}

impl ExecutionPolicyDenied {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl std::fmt::Display for ExecutionPolicyDenied {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for ExecutionPolicyDenied {}

pub fn is_execution_policy_denial(error: &anyhow::Error) -> bool {
    error
        .chain()
        .any(|cause| cause.is::<ExecutionPolicyDenied>())
}

impl HostExecutionPolicy {
    pub fn current() -> anyhow::Result<Self> {
        Self::from_env_value(std::env::var(CTX_HOST_EXECUTION_POLICY_ENV).ok().as_deref())
    }

    pub fn from_env_value(raw: Option<&str>) -> anyhow::Result<Self> {
        let Some(raw) = raw.map(str::trim).filter(|value| !value.is_empty()) else {
            return Ok(Self::AllowHost);
        };
        match raw {
            "allow_host" => Ok(Self::AllowHost),
            "sandbox_only" => Ok(Self::SandboxOnly),
            value => anyhow::bail!(
                "unsupported {CTX_HOST_EXECUTION_POLICY_ENV}: {value} (expected allow_host|sandbox_only)"
            ),
        }
    }

    pub fn validate_execution_settings(self, settings: &ExecutionSettings) -> anyhow::Result<()> {
        if matches!(self, Self::SandboxOnly) && matches!(settings.mode, ExecutionMode::Host) {
            return Err(
                ExecutionPolicyDenied::new("host execution is disabled by daemon policy").into(),
            );
        }
        Ok(())
    }

    pub fn validate_process_execution_mode_override(self) -> anyhow::Result<()> {
        let Ok(raw_mode) = std::env::var(CTX_EXECUTION_MODE_ENV) else {
            return Ok(());
        };
        if matches!(self, Self::SandboxOnly) && raw_mode.trim().eq_ignore_ascii_case("host") {
            return Err(ExecutionPolicyDenied::new(format!(
                "{CTX_EXECUTION_MODE_ENV}=host is disabled by daemon host execution policy"
            ))
            .into());
        }
        Ok(())
    }

    pub fn normalize_loaded_execution_settings(self, settings: &mut ExecutionSettings) {
        if matches!(self, Self::SandboxOnly) && matches!(settings.mode, ExecutionMode::Host) {
            settings.mode = ExecutionMode::Sandbox;
            settings.container = Default::default();
        }
    }

    pub fn validate_execution_environment(
        self,
        execution_environment: ExecutionEnvironment,
    ) -> anyhow::Result<()> {
        if matches!(self, Self::SandboxOnly)
            && matches!(execution_environment, ExecutionEnvironment::Host)
        {
            return Err(
                ExecutionPolicyDenied::new("host execution is disabled by daemon policy").into(),
            );
        }
        Ok(())
    }

    pub fn validate_install_target(self, target: InstallTarget) -> anyhow::Result<()> {
        if matches!(self, Self::SandboxOnly) && matches!(target, InstallTarget::Host) {
            return Err(ExecutionPolicyDenied::new(
                "host provider installs are disabled by daemon policy",
            )
            .into());
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn host_execution_policy_defaults_to_allow_host() {
        assert_eq!(
            HostExecutionPolicy::from_env_value(None).expect("parse policy"),
            HostExecutionPolicy::AllowHost
        );
        assert_eq!(
            HostExecutionPolicy::from_env_value(Some(" ")).expect("parse policy"),
            HostExecutionPolicy::AllowHost
        );
    }

    #[test]
    fn host_execution_policy_parses_sandbox_only() {
        assert_eq!(
            HostExecutionPolicy::from_env_value(Some("sandbox_only")).expect("parse policy"),
            HostExecutionPolicy::SandboxOnly
        );
    }

    #[test]
    fn host_execution_policy_rejects_unknown_values() {
        let err = HostExecutionPolicy::from_env_value(Some("host")).expect_err("invalid policy");
        assert!(format!("{err:#}").contains("unsupported CTX_HOST_EXECUTION_POLICY"));
    }

    #[test]
    fn sandbox_only_policy_rejects_host_install_target() {
        let err = HostExecutionPolicy::SandboxOnly
            .validate_install_target(InstallTarget::Host)
            .expect_err("host install should be rejected");
        assert!(format!("{err:#}").contains("host provider installs are disabled"));
        HostExecutionPolicy::SandboxOnly
            .validate_install_target(InstallTarget::Container)
            .expect("container install remains allowed");
    }
}
