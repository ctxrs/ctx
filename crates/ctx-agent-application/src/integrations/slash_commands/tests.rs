use std::fs;

use ctx_agent_integrations::slash_commands::{
    PathContext, SlashCommandAgent, SlashCommandInstallStatus,
};

use super::*;
use crate::IntegrationResultFact;

const PRODUCT: ProductIdentity<'static> = ProductIdentity {
    name: "ctx",
    version: "1.0.0-test",
};

#[test]
fn modified_copy_is_preserved_and_has_a_neutral_force_action() {
    let temp = tempfile::tempdir().unwrap();
    let context = PathContext::for_tests(temp.path().to_owned(), temp.path().to_owned());
    let command_path = temp.path().join(".gemini/commands/ctx.toml");
    fs::create_dir_all(command_path.parent().unwrap()).unwrap();
    fs::write(&command_path, "prompt = 'local'\n").unwrap();

    let outcome = install(
        SlashCommandInstallApplicationRequest {
            agents: vec![SlashCommandAgent::GeminiCli],
            all_agents: false,
            project: true,
            force: false,
        },
        &context,
        PRODUCT,
    )
    .unwrap();

    let result = &outcome.receipt.results[0];
    assert_eq!(result.status, SlashCommandInstallStatus::Modified);
    assert_eq!(
        fs::read_to_string(command_path).unwrap(),
        "prompt = 'local'\n"
    );
    assert_eq!(
        outcome.telemetry.result,
        Some(IntegrationResultFact::PartialError)
    );
    assert_eq!(
        force_install_command(PRODUCT, result).as_deref(),
        Some("ctx integrations install slash-command --agent gemini-cli --project --force")
    );
}

#[test]
fn product_version_is_injected_into_installed_metadata() {
    let temp = tempfile::tempdir().unwrap();
    let context = PathContext::for_tests(temp.path().to_owned(), temp.path().to_owned());
    let outcome = install(
        SlashCommandInstallApplicationRequest {
            agents: vec![SlashCommandAgent::OpenCode],
            all_agents: false,
            project: true,
            force: false,
        },
        &context,
        PRODUCT,
    )
    .unwrap();

    assert_eq!(outcome.telemetry.result, Some(IntegrationResultFact::Ok));
    let metadata: serde_json::Value = serde_json::from_slice(
        &fs::read(
            temp.path()
                .join(".opencode/commands/.ctx-slash-commands.json"),
        )
        .unwrap(),
    )
    .unwrap();
    assert_eq!(metadata["ctx_cli_version"], PRODUCT.version);
}

#[test]
fn status_reports_current_then_modified_and_renders_a_singular_recovery_command() {
    let temp = tempfile::tempdir().unwrap();
    let context = PathContext::for_tests(temp.path().to_owned(), temp.path().to_owned());
    install(
        SlashCommandInstallApplicationRequest {
            agents: vec![SlashCommandAgent::OpenCode],
            all_agents: false,
            project: true,
            force: false,
        },
        &context,
        PRODUCT,
    )
    .unwrap();

    let request = SlashCommandStatusApplicationRequest {
        agents: vec![SlashCommandAgent::OpenCode],
        all_agents: false,
        project: true,
    };
    let current = status(request.clone(), &context, PRODUCT);
    assert_eq!(
        current.telemetry.result,
        Some(IntegrationResultFact::AllCurrent)
    );
    assert!(current.recovery_command.is_none());

    fs::write(
        temp.path().join(".opencode/commands/ctx.md"),
        "locally changed",
    )
    .unwrap();
    let modified = status(request, &context, PRODUCT);
    assert_eq!(
        modified.receipt.results[0].status,
        SlashCommandInstallStatus::Modified
    );
    assert!(modified.receipt.results[0].force_required);
    assert_eq!(
        modified.recovery_command.as_deref(),
        Some("ctx integrations install slash-command --agent opencode --project --force")
    );
    assert_eq!(modified.telemetry.modified_targets, Some(1));
}

#[test]
fn remove_translates_success_and_preserved_modifications_into_telemetry() {
    let temp = tempfile::tempdir().unwrap();
    let context = PathContext::for_tests(temp.path().to_owned(), temp.path().to_owned());
    let command_path = temp.path().join(".qwen/commands/ctx.md");
    fs::create_dir_all(command_path.parent().unwrap()).unwrap();
    fs::write(&command_path, "local command").unwrap();

    let preserved = remove(
        SlashCommandRemoveApplicationRequest {
            agents: vec![SlashCommandAgent::QwenCode],
            all_agents: false,
            project: true,
            force: false,
        },
        &context,
    );
    assert_eq!(
        preserved.telemetry.result,
        Some(IntegrationResultFact::PartialError)
    );
    assert!(command_path.is_file());
    assert!(preserved.receipt.results[0].force_required);
    assert_eq!(
        force_remove_command(PRODUCT, &preserved.receipt.results[0]).as_deref(),
        Some("ctx integrations remove slash-command --agent qwen-code --project --force")
    );

    let forced = remove(
        SlashCommandRemoveApplicationRequest {
            agents: vec![SlashCommandAgent::QwenCode],
            all_agents: false,
            project: true,
            force: true,
        },
        &context,
    );
    assert_eq!(forced.telemetry.result, Some(IntegrationResultFact::Ok));
    assert_eq!(forced.telemetry.modified_targets, Some(1));
    assert!(!command_path.exists());
}

#[test]
fn unsafe_nonregular_target_never_offers_force_recovery() {
    let temp = tempfile::tempdir().unwrap();
    let context = PathContext::for_tests(temp.path().to_owned(), temp.path().to_owned());
    let command_path = temp.path().join(".qwen/commands/ctx.md");
    fs::create_dir_all(&command_path).unwrap();
    let status_request = SlashCommandStatusApplicationRequest {
        agents: vec![SlashCommandAgent::QwenCode],
        all_agents: false,
        project: true,
    };

    let status = status(status_request, &context, PRODUCT);
    assert!(!status.receipt.results[0].success);
    assert!(!status.receipt.results[0].force_required);
    assert!(status.recovery_command.is_none());

    let removed = remove(
        SlashCommandRemoveApplicationRequest {
            agents: vec![SlashCommandAgent::QwenCode],
            all_agents: false,
            project: true,
            force: false,
        },
        &context,
    );
    assert!(!removed.receipt.results[0].success);
    assert!(!removed.receipt.results[0].force_required);
    assert!(force_remove_command(PRODUCT, &removed.receipt.results[0]).is_none());
    assert!(command_path.is_dir());
}

#[test]
fn empty_detected_status_is_none_current() {
    let temp = tempfile::tempdir().unwrap();
    let context = PathContext::for_tests(temp.path().to_owned(), temp.path().to_owned());

    let outcome = status(
        SlashCommandStatusApplicationRequest {
            agents: Vec::new(),
            all_agents: false,
            project: true,
        },
        &context,
        PRODUCT,
    );

    assert!(outcome.receipt.results.is_empty());
    assert_eq!(outcome.receipt.selected_agents, 0);
    assert_eq!(
        outcome.telemetry.result,
        Some(IntegrationResultFact::NoneCurrent)
    );
    assert!(outcome.recovery_command.is_none());
}

#[test]
fn informational_status_is_successful_and_needs_no_recovery() {
    let temp = tempfile::tempdir().unwrap();
    let context = PathContext::for_tests(temp.path().to_owned(), temp.path().to_owned());
    let outcome = status(
        SlashCommandStatusApplicationRequest {
            agents: vec![SlashCommandAgent::Codex],
            all_agents: false,
            project: false,
        },
        &context,
        PRODUCT,
    );
    assert_eq!(
        outcome.receipt.results[0].status,
        SlashCommandInstallStatus::SkillOnly
    );
    assert_eq!(
        outcome.telemetry.result,
        Some(IntegrationResultFact::AllCurrent)
    );
    assert!(outcome.recovery_command.is_none());
}
