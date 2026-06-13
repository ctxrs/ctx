use super::*;

#[test]
fn contract_dependency_target_validation_rejects_host_dependencies() {
    let contract = provider_install_contract::ProviderInstallContract {
        resolved_target_key: "linux-x86_64",
        dependencies: vec![provider_install_contract::ProviderInstallDependency {
            provider_id: "claude-cli".to_string(),
            role: provider_install_contract::ProviderInstallDependencyRoleKind::Readiness,
            target: InstallTarget::Host,
            satisfied: false,
        }],
    };

    let err = validate_contract_dependency_targets(&contract, |target| {
        if matches!(target, InstallTarget::Host) {
            anyhow::bail!("host provider installs are disabled by daemon policy");
        }
        Ok(())
    })
    .expect_err("host dependency should be rejected");

    assert_eq!(err.code.as_deref(), Some("install_target_disabled"));
    assert!(err.message.contains("claude-cli"));
    assert!(err.message.contains("target 'host'"));
}

#[test]
fn contract_dependency_target_validation_allows_container_dependencies() {
    let contract = provider_install_contract::ProviderInstallContract {
        resolved_target_key: "linux-x86_64",
        dependencies: vec![provider_install_contract::ProviderInstallDependency {
            provider_id: "runtime-node-container".to_string(),
            role: provider_install_contract::ProviderInstallDependencyRoleKind::Prerequisite,
            target: InstallTarget::Container,
            satisfied: false,
        }],
    };

    validate_contract_dependency_targets(&contract, |_target| Ok(()))
        .expect("container dependency should be allowed");
}
