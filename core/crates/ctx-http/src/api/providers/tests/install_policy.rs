use super::*;

#[test]
fn apply_install_target_status_marks_mismatched_managed_target_missing() {
    let mut status = ctx_providers::adapters::ProviderStatus {
        provider_id: "codex".to_string(),
        installed: true,
        detected_path: Some("/tmp/codex".to_string()),
        version: Some("1.0.0".to_string()),
        capabilities: None,
        health: ctx_providers::adapters::ProviderHealth::Ok,
        diagnostics: Vec::new(),
        details: HashMap::from([("managed_target".to_string(), "host".to_string())]),
        usability: ctx_providers::adapters::ProviderUsability::default(),
    };

    installer::apply_install_target_status(&mut status, InstallTarget::Container);

    assert!(!status.installed);
    assert!(matches!(
        status.health,
        ctx_providers::adapters::ProviderHealth::Missing
    ));
    assert_eq!(
        status.details.get("target_mismatch").map(String::as_str),
        Some("true")
    );
}

#[test]
fn apply_install_target_status_keeps_matching_managed_target_healthy() {
    let mut status = ctx_providers::adapters::ProviderStatus {
        provider_id: "codex".to_string(),
        installed: true,
        detected_path: Some("/tmp/codex".to_string()),
        version: Some("1.0.0".to_string()),
        capabilities: None,
        health: ctx_providers::adapters::ProviderHealth::Ok,
        diagnostics: Vec::new(),
        details: HashMap::from([("managed_target".to_string(), "container".to_string())]),
        usability: ctx_providers::adapters::ProviderUsability::default(),
    };

    installer::apply_install_target_status(&mut status, InstallTarget::Container);

    assert!(status.installed);
    assert!(matches!(
        status.health,
        ctx_providers::adapters::ProviderHealth::Ok
    ));
    assert!(!status.details.contains_key("target_mismatch"));
}

#[test]
fn apply_install_target_status_marks_host_detected_status_unverified_for_container() {
    let mut status = ctx_providers::adapters::ProviderStatus {
        provider_id: "codex".to_string(),
        installed: true,
        detected_path: Some("/usr/local/bin/codex".to_string()),
        version: Some("1.0.0".to_string()),
        capabilities: None,
        health: ctx_providers::adapters::ProviderHealth::Ok,
        diagnostics: Vec::new(),
        details: HashMap::new(),
        usability: ctx_providers::adapters::ProviderUsability::default(),
    };

    installer::apply_install_target_status(&mut status, InstallTarget::Container);

    assert!(!status.installed);
    assert!(matches!(
        status.health,
        ctx_providers::adapters::ProviderHealth::Missing
    ));
    assert_eq!(
        status.details.get("target_unverified").map(String::as_str),
        Some("true")
    );
}

#[test]
fn should_skip_install_for_healthy_provider_without_updates() {
    let status = ctx_providers::adapters::ProviderStatus {
        provider_id: "codex".to_string(),
        installed: true,
        detected_path: None,
        version: Some("1.0.0".to_string()),
        capabilities: None,
        health: ctx_providers::adapters::ProviderHealth::Ok,
        diagnostics: Vec::new(),
        details: HashMap::new(),
        usability: ctx_providers::adapters::ProviderUsability::default(),
    };
    assert!(should_skip_install_for_healthy_provider(&status));
}

#[test]
fn should_not_skip_install_for_healthy_provider_with_release_update() {
    let mut details = HashMap::new();
    details.insert("matrix_update_available".to_string(), "true".to_string());
    let status = ctx_providers::adapters::ProviderStatus {
        provider_id: "codex".to_string(),
        installed: true,
        detected_path: None,
        version: Some("1.0.0".to_string()),
        capabilities: None,
        health: ctx_providers::adapters::ProviderHealth::Ok,
        diagnostics: Vec::new(),
        details,
        usability: ctx_providers::adapters::ProviderUsability::default(),
    };
    assert!(!should_skip_install_for_healthy_provider(&status));
}

#[test]
fn should_not_skip_install_for_healthy_provider_with_dependency_update() {
    let mut details = HashMap::new();
    details.insert(
        "managed_dependency_update_available".to_string(),
        "true".to_string(),
    );
    let status = ctx_providers::adapters::ProviderStatus {
        provider_id: "codex".to_string(),
        installed: true,
        detected_path: None,
        version: Some("1.0.0".to_string()),
        capabilities: None,
        health: ctx_providers::adapters::ProviderHealth::Ok,
        diagnostics: Vec::new(),
        details,
        usability: ctx_providers::adapters::ProviderUsability::default(),
    };
    assert!(!should_skip_install_for_healthy_provider(&status));
}

#[test]
fn target_aware_status_does_not_skip_container_install_for_host_only_status() {
    let mut status = ctx_providers::adapters::ProviderStatus {
        provider_id: "codex".to_string(),
        installed: true,
        detected_path: Some("/usr/local/bin/codex".to_string()),
        version: Some("1.0.0".to_string()),
        capabilities: None,
        health: ctx_providers::adapters::ProviderHealth::Ok,
        diagnostics: Vec::new(),
        details: HashMap::new(),
        usability: ctx_providers::adapters::ProviderUsability::default(),
    };

    apply_target_aware_provider_status(
        &mut status,
        &installer::AgentServerConfigFile::default(),
        InstallTarget::Container,
    );

    assert!(!status.installed);
    assert!(matches!(
        status.health,
        ctx_providers::adapters::ProviderHealth::Missing
    ));
    assert!(!should_skip_install_for_healthy_provider(&status));
}
