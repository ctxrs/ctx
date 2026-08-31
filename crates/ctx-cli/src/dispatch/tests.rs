use std::io::Write;

use clap::Parser as _;

use super::*;
use crate::cli::Cli;
use crate::dispatch::test_support::pipe_ui;
use crate::operation_descriptor::LocalUsageOperation;
use crate::ui::ColorMode;

fn daemon_autostart_trigger(args: &[&str]) -> Option<DaemonTriggerCommandArg> {
    let cli = Cli::try_parse_from(std::iter::once("ctx").chain(args.iter().copied()))
        .unwrap_or_else(|error| panic!("failed to parse {args:?}: {error}"));
    command_daemon_autostart_trigger(&cli.command)
}

#[test]
fn setup_handoff_is_owned_by_setup_and_machine_import_does_not_autostart() {
    for args in [
        &["setup"][..],
        &["setup", "--format", "json"][..],
        &["setup", "--progress", "json"],
        &["import", "--format", "json"],
        &["import", "--progress", "json"],
    ] {
        assert!(daemon_autostart_trigger(args).is_none(), "{args:?}");
    }
}

#[test]
fn human_import_retains_post_command_daemon_autostart() {
    assert!(matches!(
        daemon_autostart_trigger(&["import"]),
        Some(DaemonTriggerCommandArg::Import)
    ));
}

#[test]
fn stats_is_excluded_from_remote_analytics() {
    for args in [
        &["stats"][..],
        &["stats", "--detail"][..],
        &["stats", "--format=json"][..],
    ] {
        let cli = Cli::try_parse_from(std::iter::once("ctx").chain(args.iter().copied()))
            .unwrap_or_else(|error| panic!("failed to parse {args:?}: {error}"));
        assert!(
            ClientOperationDraft::from_descriptor(
                command_operation_descriptor(&cli.command),
                command_json_output(&cli.command),
            )
            .is_none(),
            "{args:?}"
        );
    }
}

#[test]
fn show_commands_are_typed_and_status_and_stats_are_excluded_from_local_usage() {
    for (args, expected) in [
        (&["show", "session", "abc"][..], "show_session"),
        (&["show", "event", "abc"][..], "show_event"),
        (&["list", "events"][..], "show_event"),
    ] {
        let cli = Cli::try_parse_from(std::iter::once("ctx").chain(args.iter().copied()))
            .unwrap_or_else(|error| panic!("failed to parse {args:?}: {error}"));
        let descriptor = command_operation_descriptor(&cli.command);
        let local_operation = match &descriptor {
            OperationDescriptor::Cli(operation) => operation.local_usage_operation(),
            _ => None,
        };
        assert_eq!(
            local_operation.map(LocalUsageOperation::as_str),
            Some(expected)
        );
        assert!(
            local_usage::CliUsage::from_descriptor(&descriptor)
                .completed(true, std::time::Duration::ZERO)
                .is_some(),
            "{args:?}"
        );
    }

    for args in [&["status"][..], &["stats"][..]] {
        let cli = Cli::try_parse_from(std::iter::once("ctx").chain(args.iter().copied()))
            .unwrap_or_else(|error| panic!("failed to parse {args:?}: {error}"));
        let descriptor = command_operation_descriptor(&cli.command);
        let local_operation = match &descriptor {
            OperationDescriptor::Cli(operation) => operation.local_usage_operation(),
            _ => None,
        };
        assert!(local_operation.is_none(), "{args:?}");
        assert!(
            local_usage::CliUsage::from_descriptor(&descriptor)
                .completed(true, std::time::Duration::ZERO)
                .is_none(),
            "{args:?}"
        );
    }
}

#[test]
fn query_authority_error_json_is_scoped_to_machine_search_show_and_locate() {
    for (args, expected) in [
        (&["search", "authority", "--format=json"][..], true),
        (&["show", "event", "bad", "--format=json"][..], true),
        (&["locate", "event", "bad", "--format=json"][..], true),
        (&["search", "authority"][..], false),
        (&["status", "--format=json"][..], false),
    ] {
        let cli = Cli::try_parse_from(std::iter::once("ctx").chain(args.iter().copied()))
            .unwrap_or_else(|error| panic!("failed to parse {args:?}: {error}"));
        let json_output = command_json_output(&cli.command);
        assert_eq!(
            command_uses_query_authority_error_json(&cli.command, json_output),
            expected,
            "{args:?}"
        );
    }
}

fn rendered_generic_error(error: &anyhow::Error, machine: bool, color: ColorMode) -> Vec<u8> {
    let (mut ui, _, stderr) = pipe_ui(color);
    render_generic_command_error(error, machine, &mut ui).unwrap();
    ui.flush().unwrap();
    stderr.bytes()
}

#[test]
fn clap_value_errors_use_the_selected_stderr_stream_with_contextual_usage() {
    let arguments = ["ctx", "sources", "--provider", "unknown"];
    let mut error = Cli::try_parse_from(arguments).unwrap_err();
    let os_arguments = arguments
        .iter()
        .map(std::ffi::OsString::from)
        .collect::<Vec<_>>();
    parse::attach_value_validation_usage(&mut error, &os_arguments);

    let (mut ui, _, stderr) = pipe_ui(ColorMode::Always);
    write_clap_output(&error, &mut ui).unwrap();
    ui.flush().unwrap();

    let rendered = String::from_utf8(stderr.bytes()).unwrap();
    assert!(rendered.contains('\u{1b}'));
    let mut stripped = anstream::StripStream::new(Vec::new());
    stripped.write_all(rendered.as_bytes()).unwrap();
    let plain = String::from_utf8(stripped.into_inner()).unwrap();
    assert!(plain.contains("unknown provider"));
    assert!(plain.contains("Usage: ctx sources [OPTIONS]"));
}

#[test]
fn forced_color_never_decorates_generic_machine_mode_errors() {
    for args in [
        &["show", "session", "bad", "--format", "jsonl"][..],
        &["show", "event", "bad", "--format", "markdown"][..],
        &["setup", "--progress", "json"][..],
        &["import", "--progress", "json"][..],
        &["mcp", "serve"][..],
        &["mcp", "--quiet", "serve"][..],
    ] {
        let cli = Cli::try_parse_from(std::iter::once("ctx").chain(args.iter().copied()))
            .unwrap_or_else(|error| panic!("failed to parse {args:?}: {error}"));
        let json_output = command_json_output(&cli.command);
        assert!(
            command_machine_readable_output(&cli.command, json_output),
            "{args:?}"
        );

        let machine_stderr = rendered_generic_error(
            &anyhow::anyhow!("representative command failure"),
            true,
            ColorMode::Always,
        );

        assert!(!machine_stderr.contains(&0x1b), "{args:?}");
        assert!(String::from_utf8_lossy(&machine_stderr)
            .starts_with("Error: representative command failure"));
    }
}

#[test]
fn forced_color_still_styles_generic_human_mode_errors() {
    let rendered = rendered_generic_error(
        &anyhow::anyhow!("human command failure"),
        false,
        ColorMode::Always,
    );
    assert!(rendered.contains(&0x1b));
}

#[test]
fn generic_human_errors_include_the_actionable_cause_chain() {
    let error = anyhow::anyhow!("No such file or directory")
        .context("approve explicit source path /tmp/missing.jsonl");
    let rendered =
        String::from_utf8(rendered_generic_error(&error, false, ColorMode::Never)).unwrap();
    assert!(rendered.contains("approve explicit source path /tmp/missing.jsonl"));
    assert!(rendered.contains("No such file or directory"));
    assert!(!rendered.contains("Stack backtrace"));
}

struct SemanticCompletionErrorCase {
    name: &'static str,
    error: ctx_daemon_cli::SemanticCompletionError,
    reason: &'static str,
    retryable: bool,
    active_generation_id: Option<&'static str>,
    failure_class: Option<&'static str>,
}

fn semantic_completion_error_cases() -> Vec<SemanticCompletionErrorCase> {
    use ctx_daemon_cli::{SemanticCompletionError, SemanticFailureClass};

    vec![
        SemanticCompletionErrorCase {
            name: "contract",
            error: SemanticCompletionError::Contract {
                generation_id: "core-contract".to_owned(),
                source: anyhow::anyhow!("contract detail"),
            },
            reason: "semantic_completion_contract_invalid",
            retryable: false,
            active_generation_id: None,
            failure_class: None,
        },
        SemanticCompletionErrorCase {
            name: "core_superseded",
            error: SemanticCompletionError::CoreSuperseded {
                generation_id: "core-superseded".to_owned(),
                active_generation_id: "core-active".to_owned(),
                retryable: true,
            },
            reason: "semantic_completion_generation_superseded",
            retryable: true,
            active_generation_id: Some("core-active"),
            failure_class: None,
        },
        SemanticCompletionErrorCase {
            name: "checkpoint",
            error: SemanticCompletionError::Checkpoint {
                generation_id: "core-checkpoint".to_owned(),
                source: anyhow::anyhow!("checkpoint detail"),
            },
            reason: "semantic_completion_interrupted",
            retryable: false,
            active_generation_id: None,
            failure_class: None,
        },
        SemanticCompletionErrorCase {
            name: "preflight",
            error: SemanticCompletionError::Preflight {
                generation_id: "core-preflight".to_owned(),
                retryable: true,
                source: anyhow::anyhow!("preflight detail"),
            },
            reason: "semantic_completion_preflight_failed",
            retryable: true,
            active_generation_id: None,
            failure_class: None,
        },
        SemanticCompletionErrorCase {
            name: "reconciliation",
            error: SemanticCompletionError::Reconciliation {
                generation_id: "core-reconciliation".to_owned(),
                retryable: false,
                source: anyhow::anyhow!("reconciliation detail"),
            },
            reason: "semantic_completion_reconciliation_failed",
            retryable: false,
            active_generation_id: None,
            failure_class: None,
        },
        SemanticCompletionErrorCase {
            name: "daemon_activation",
            error: SemanticCompletionError::DaemonActivationFailed {
                generation_id: "core-activation".to_owned(),
                detail: "activation detail".to_owned(),
                retryable: true,
            },
            reason: "semantic_completion_activation_failed",
            retryable: true,
            active_generation_id: None,
            failure_class: None,
        },
        SemanticCompletionErrorCase {
            name: "daemon_configuration",
            error: SemanticCompletionError::DaemonConfigurationFailed {
                generation_id: "core-configuration".to_owned(),
                detail: "configuration detail".to_owned(),
                retryable: true,
            },
            reason: "semantic_completion_configuration_failed",
            retryable: true,
            active_generation_id: None,
            failure_class: None,
        },
        SemanticCompletionErrorCase {
            name: "daemon_job_with_failure_class",
            error: SemanticCompletionError::DaemonJobFailed {
                generation_id: "core-job-classified".to_owned(),
                detail: "job detail".to_owned(),
                retryable: true,
                failure_class: Some(SemanticFailureClass::ResourcePressure),
            },
            reason: "semantic_completion_job_failed",
            retryable: true,
            active_generation_id: None,
            failure_class: Some("resource_pressure"),
        },
        SemanticCompletionErrorCase {
            name: "daemon_job_without_failure_class",
            error: SemanticCompletionError::DaemonJobFailed {
                generation_id: "core-job-unclassified".to_owned(),
                detail: "unclassified job detail".to_owned(),
                retryable: false,
                failure_class: None,
            },
            reason: "semantic_completion_job_failed",
            retryable: false,
            active_generation_id: None,
            failure_class: None,
        },
        SemanticCompletionErrorCase {
            name: "no_progress",
            error: SemanticCompletionError::NoProgress {
                generation_id: "core-no-progress".to_owned(),
                retryable: true,
            },
            reason: "semantic_completion_no_progress",
            retryable: true,
            active_generation_id: None,
            failure_class: None,
        },
        SemanticCompletionErrorCase {
            name: "observation_outage",
            error: SemanticCompletionError::ObservationOutage {
                generation_id: "core-outage".to_owned(),
                detail: "observation detail".to_owned(),
                retryable: true,
            },
            reason: "semantic_completion_observation_unavailable",
            retryable: true,
            active_generation_id: None,
            failure_class: None,
        },
        SemanticCompletionErrorCase {
            name: "postcondition",
            error: SemanticCompletionError::Postcondition {
                generation_id: "core-postcondition".to_owned(),
                retryable: false,
                source: anyhow::anyhow!("postcondition detail"),
            },
            reason: "semantic_completion_postcondition_failed",
            retryable: false,
            active_generation_id: None,
            failure_class: None,
        },
    ]
}

#[test]
fn semantic_completion_json_dispatch_covers_every_variant() {
    for case in semantic_completion_error_cases() {
        let generation_id = case.error.generation_id().to_owned();
        let detail = case.error.to_string();
        let result: Result<()> = Err(case.error.into());
        let (mut ui, stdout, stderr) = pipe_ui(ColorMode::Always);
        let rendered = render_command_result_error(&result, true, false, true, true, &mut ui)
            .unwrap_or_else(|error| panic!("{} failed to render: {error:#}", case.name))
            .unwrap_or_else(|| panic!("{} did not return a rendered error", case.name));
        assert!(rendered.is::<RenderedJsonError>(), "{}", case.name);
        ui.flush().unwrap();

        assert!(stdout.bytes().is_empty(), "{}", case.name);
        let stderr = stderr.bytes();
        assert!(!stderr.contains(&0x1b), "{}", case.name);
        let document: serde_json::Value = serde_json::from_slice(&stderr)
            .unwrap_or_else(|error| panic!("{} stderr was not JSON: {error}", case.name));
        assert_eq!(document["error"], detail, "{}", case.name);
        assert_eq!(
            document["error_code"], "semantic_completion_failed",
            "{}",
            case.name
        );
        assert_eq!(document["reason"], case.reason, "{}", case.name);
        assert_eq!(document["generation_id"], generation_id, "{}", case.name);
        assert_eq!(document["core_published"], true, "{}", case.name);
        assert_eq!(document["retryable"], case.retryable, "{}", case.name);
        assert_eq!(document["detail"], detail, "{}", case.name);
        assert_eq!(
            document
                .get("active_generation_id")
                .and_then(|value| value.as_str()),
            case.active_generation_id,
            "{}",
            case.name
        );
        assert_eq!(
            document
                .get("failure_class")
                .and_then(|value| value.as_str()),
            case.failure_class,
            "{}",
            case.name
        );
        assert_eq!(
            document.as_object().expect("error document object").len(),
            7 + usize::from(case.active_generation_id.is_some())
                + usize::from(case.failure_class.is_some()),
            "{}",
            case.name
        );
    }
}

#[test]
fn semantic_completion_human_dispatch_remains_generic() {
    let error = ctx_daemon_cli::SemanticCompletionError::NoProgress {
        generation_id: "core-human".to_owned(),
        retryable: true,
    };
    let result: Result<()> = Err(error.into());
    let (mut ui, stdout, stderr) = pipe_ui(ColorMode::Never);
    let rendered = render_command_result_error(&result, false, false, false, false, &mut ui)
        .unwrap()
        .expect("human error must be rendered");
    assert!(rendered.is::<RenderedCliError>());
    ui.flush().unwrap();

    assert!(stdout.bytes().is_empty());
    let rendered = String::from_utf8(stderr.bytes()).unwrap();
    assert!(rendered
        .contains("daemon semantic completion made no progress for Core generation core-human"));
    assert!(!rendered.contains("semantic_completion_failed"));
}
