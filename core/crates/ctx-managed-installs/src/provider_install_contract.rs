use std::fmt;
use std::path::Path;

use crate::{self as installer, AgentServerConfigFile};
use ctx_provider_install::install_state::InstallTarget;
use ctx_provider_matrix as provider_matrix;
use ctx_provider_matrix::{
    ProviderInstallDependencyRole, ProviderInstallDependencyTarget, ProviderMatrix,
    ProviderMatrixEntry,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderInstallViabilityIssue {
    pub code: &'static str,
    pub message: String,
}

impl fmt::Display for ProviderInstallViabilityIssue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for ProviderInstallViabilityIssue {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderInstallDependencyRoleKind {
    Prerequisite,
    Readiness,
}

impl ProviderInstallDependencyRoleKind {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Prerequisite => "prerequisite",
            Self::Readiness => "readiness",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderInstallDependency {
    pub provider_id: String,
    pub role: ProviderInstallDependencyRoleKind,
    pub target: InstallTarget,
    pub satisfied: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderInstallContract {
    pub resolved_target_key: &'static str,
    pub dependencies: Vec<ProviderInstallDependency>,
}

impl ProviderInstallContract {
    pub fn dependencies_for_role(
        &self,
        role: ProviderInstallDependencyRoleKind,
    ) -> Vec<ProviderInstallDependency> {
        self.dependencies
            .iter()
            .filter(|dependency| dependency.role == role)
            .cloned()
            .collect()
    }
}

fn acp_bridge_missing_issue(
    provider_id: &str,
    target: InstallTarget,
) -> ProviderInstallViabilityIssue {
    ProviderInstallViabilityIssue {
        code: "acp_bridge_missing",
        message: format!(
            "ACP bridge runtime is not viable for target '{}': runtime command is not configured for provider 'acp-crp-bridge' required by provider '{}'",
            target.as_str(),
            provider_id
        ),
    }
}

fn dependency_target(
    target: ProviderInstallDependencyTarget,
    provider_target: InstallTarget,
) -> InstallTarget {
    match target {
        ProviderInstallDependencyTarget::SameAsProvider => provider_target,
        ProviderInstallDependencyTarget::Host => InstallTarget::Host,
        ProviderInstallDependencyTarget::Container => InstallTarget::Container,
        ProviderInstallDependencyTarget::LinuxAarch64 => InstallTarget::LinuxAarch64,
        ProviderInstallDependencyTarget::LinuxX8664 => InstallTarget::LinuxX8664,
    }
}

fn dependency_resolution_issue(
    provider_id: &str,
    dependency_id: &str,
    dependency_target: InstallTarget,
    dependency_role: ProviderInstallDependencyRoleKind,
    code: &'static str,
    detail: impl Into<String>,
) -> ProviderInstallViabilityIssue {
    ProviderInstallViabilityIssue {
        code,
        message: format!(
            "Required {} dependency '{}' is not viable for target '{}' required by provider '{}': {}",
            dependency_role.as_str(),
            dependency_id,
            dependency_target.as_str(),
            provider_id,
            detail.into()
        ),
    }
}

#[derive(Clone, Copy)]
struct DependencyResolutionCodes {
    missing: &'static str,
    invalid: &'static str,
}

struct DependencyViabilityResolver<'a> {
    cfg: &'a AgentServerConfigFile,
    matrix: &'a ProviderMatrix,
    current_ctx_version: &'a str,
}

impl<'a> DependencyViabilityResolver<'a> {
    fn resolve_dependency_viability(
        &self,
        provider_id: &str,
        dependency_id: &str,
        dependency_target: InstallTarget,
        dependency_role: ProviderInstallDependencyRoleKind,
        codes: DependencyResolutionCodes,
    ) -> Result<ProviderInstallDependency, ProviderInstallViabilityIssue> {
        match installer::resolve_runtime_provider_command_for_target_repairable_managed(
            self.cfg,
            dependency_id,
            Some(dependency_target),
        ) {
            Ok(Some(_)) => Ok(ProviderInstallDependency {
                provider_id: dependency_id.to_string(),
                role: dependency_role,
                target: dependency_target,
                satisfied: true,
            }),
            Ok(None) => {
                if installer::is_supported_managed_provider_for_target(
                    self.matrix,
                    dependency_id,
                    dependency_target,
                ) && installer::is_compatible_managed_provider_for_target(
                    self.matrix,
                    dependency_id,
                    dependency_target,
                    Some(self.current_ctx_version),
                ) {
                    return Ok(ProviderInstallDependency {
                        provider_id: dependency_id.to_string(),
                        role: dependency_role,
                        target: dependency_target,
                        satisfied: false,
                    });
                }
                Err(dependency_resolution_issue(
                    provider_id,
                    dependency_id,
                    dependency_target,
                    dependency_role,
                    codes.missing,
                    format!(
                        "runtime command is not configured for provider '{dependency_id}' and ctx cannot managed-install it for that target"
                    ),
                ))
            }
            Err(err) => Err(dependency_resolution_issue(
                provider_id,
                dependency_id,
                dependency_target,
                dependency_role,
                codes.invalid,
                err.to_string(),
            )),
        }
    }

    fn resolve_acp_bridge_dependencies(
        &self,
        provider_id: &str,
        target: InstallTarget,
    ) -> Result<Vec<ProviderInstallDependency>, ProviderInstallViabilityIssue> {
        match self.resolve_dependency_viability(
            provider_id,
            "acp-crp-bridge",
            target,
            ProviderInstallDependencyRoleKind::Prerequisite,
            DependencyResolutionCodes {
                missing: "acp_bridge_missing",
                invalid: "acp_bridge_invalid",
            },
        ) {
            Ok(dependency) => {
                if dependency.satisfied {
                    Ok(Vec::new())
                } else {
                    Ok(vec![dependency])
                }
            }
            Err(err) if err.code == "acp_bridge_missing" => {
                Err(acp_bridge_missing_issue(provider_id, target))
            }
            Err(err) => Err(err),
        }
    }

    fn resolve_matrix_provider_dependencies(
        &self,
        entry: &ProviderMatrixEntry,
        provider_target: InstallTarget,
    ) -> Result<Vec<ProviderInstallDependency>, ProviderInstallViabilityIssue> {
        entry
            .provider_dependencies
            .iter()
            .map(|dependency| {
                let dependency_target = dependency_target(dependency.target, provider_target);
                let dependency_role = match dependency.role {
                    ProviderInstallDependencyRole::Prerequisite => {
                        ProviderInstallDependencyRoleKind::Prerequisite
                    }
                    ProviderInstallDependencyRole::Readiness => {
                        ProviderInstallDependencyRoleKind::Readiness
                    }
                };
                self.resolve_dependency_viability(
                    &entry.id,
                    &dependency.id,
                    dependency_target,
                    dependency_role,
                    DependencyResolutionCodes {
                        missing: "dependency_missing",
                        invalid: "dependency_invalid",
                    },
                )
            })
            .collect()
    }
}

pub fn resolve_provider_install_contract(
    _data_root: &Path,
    cfg: &AgentServerConfigFile,
    matrix: &ProviderMatrix,
    provider_id: &str,
    target: InstallTarget,
    current_ctx_version: Option<&str>,
) -> Result<ProviderInstallContract, ProviderInstallViabilityIssue> {
    let current_ctx_version = match current_ctx_version {
        Some(raw) => raw,
        None => {
            return Err(ProviderInstallViabilityIssue {
                code: "ctx_version_unavailable",
                message: "current ctx build version is unavailable for provider install resolution"
                    .to_string(),
            })
        }
    };

    let parsed_current_ctx_version =
        provider_matrix::parse_version_loose(current_ctx_version).ok_or_else(|| {
            ProviderInstallViabilityIssue {
                code: "ctx_version_invalid",
                message: format!(
                    "current ctx build version '{current_ctx_version}' is not valid semver for provider install resolution"
                ),
            }
        })?;

    if !provider_matrix::is_managed_supported_for_context(
        matrix,
        provider_id,
        Some(&parsed_current_ctx_version),
    ) {
        return Err(ProviderInstallViabilityIssue {
            code: "ctx_version_unsupported",
            message: format!(
                "provider '{}' is not compatible with ctx build '{}' for managed install target '{}'",
                provider_id,
                parsed_current_ctx_version,
                target.as_str()
            ),
        });
    }

    let resolved_target_key = installer::resolve_matrix_target_key(target).map_err(|err| {
        ProviderInstallViabilityIssue {
            code: "install_target_invalid",
            message: format!(
                "provider '{}' has no valid managed install target mapping for '{}': {err}",
                provider_id,
                target.as_str()
            ),
        }
    })?;

    let entry = provider_matrix::get_entry(matrix, provider_id).ok_or_else(|| {
        ProviderInstallViabilityIssue {
            code: "install_target_unsupported",
            message: format!(
                "provider '{}' does not support managed install target '{}'",
                provider_id,
                target.as_str()
            ),
        }
    })?;
    let install = entry
        .managed_install
        .as_ref()
        .ok_or_else(|| ProviderInstallViabilityIssue {
            code: "install_target_unsupported",
            message: format!(
                "provider '{}' does not support managed install target '{}'",
                provider_id,
                target.as_str()
            ),
        })?;

    if matches!(
        target,
        InstallTarget::Container | InstallTarget::LinuxAarch64 | InstallTarget::LinuxX8664
    ) && matches!(
        install,
        provider_matrix::ProviderInstall::Npm { targets, .. }
            | provider_matrix::ProviderInstall::Python { targets, .. }
            if !targets.contains_key(resolved_target_key)
    ) {
        return Err(ProviderInstallViabilityIssue {
            code: "container_artifact_missing",
            message: format!(
                "provider '{}' does not have a published managed artifact for target '{}'",
                provider_id,
                target.as_str()
            ),
        });
    }

    if !installer::is_supported_managed_provider_for_target(matrix, provider_id, target) {
        return Err(ProviderInstallViabilityIssue {
            code: "install_target_unsupported",
            message: format!(
                "provider '{}' does not support managed install target '{}'",
                provider_id,
                target.as_str()
            ),
        });
    }

    let resolver = DependencyViabilityResolver {
        cfg,
        matrix,
        current_ctx_version,
    };

    let mut dependencies = resolver.resolve_matrix_provider_dependencies(entry, target)?;
    if crate::is_acp_provider_id(provider_id) {
        dependencies.extend(resolver.resolve_acp_bridge_dependencies(provider_id, target)?);
    }

    Ok(ProviderInstallContract {
        resolved_target_key,
        dependencies,
    })
}

pub fn provider_install_viability_issue(
    data_root: &Path,
    cfg: &AgentServerConfigFile,
    matrix: &ProviderMatrix,
    provider_id: &str,
    target: InstallTarget,
    current_ctx_version: Option<&str>,
) -> Option<ProviderInstallViabilityIssue> {
    resolve_provider_install_contract(
        data_root,
        cfg,
        matrix,
        provider_id,
        target,
        current_ctx_version,
    )
    .err()
}

#[cfg(test)]
mod tests;
