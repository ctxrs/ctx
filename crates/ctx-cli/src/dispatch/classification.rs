//! Classify history and independent commands without initializing either engine.

use super::*;

pub(super) fn command_json_output(command: &CommandRoot) -> bool {
    match command {
        CommandRoot::Unified(_) => false,
        CommandRoot::Blame(args) => args.json_output(),
        CommandRoot::Setup(args) => args.format.is_json(),
        CommandRoot::Semantic(args) => args.json_output(),
        CommandRoot::Status(args) => args.format.is_json(),
        CommandRoot::Stats(args) => args.format.is_json(),
        CommandRoot::Index(args) => args.json_output(),
        CommandRoot::Sources(args) => args.format.is_json(),
        CommandRoot::Import(args) => args.format.is_json(),
        CommandRoot::Show(args) => show_json_output(args),
        CommandRoot::List(_) => true,
        CommandRoot::Locate(args) => match &args.target {
            crate::LocateTarget::Session(args) => args.format.is_json(),
            crate::LocateTarget::Event(args) => args.format.is_json(),
        },
        CommandRoot::Search(args) => args.format.is_json(),
        CommandRoot::Docs(args) => args.json_output(),
        CommandRoot::Integrations(args) => args.json_output(),
        CommandRoot::Mcp(_) => false,
        CommandRoot::Daemon(args) => match &args.command {
            DaemonCommand::Run(args) => args.format.is_json(),
            DaemonCommand::Status(args) | DaemonCommand::Enable(args) => args.format.is_json(),
            DaemonCommand::Disable(args) => args.format.is_json(),
        },
        CommandRoot::Upgrade(args) => args.json_output(),
        CommandRoot::Doctor(args) => args.format.is_json(),
    }
}

pub(super) fn show_json_output(args: &ShowArgs) -> bool {
    match &args.target {
        ShowTarget::Session(args) => args.format == OutputFormat::Json,
        ShowTarget::Event(args) => args.format == OutputFormat::Json,
    }
}

pub(super) fn command_machine_readable_output(command: &CommandRoot, json_output: bool) -> bool {
    if json_output {
        return true;
    }
    match command {
        CommandRoot::Setup(args) => args.progress == crate::progress::ProgressArg::Json,
        CommandRoot::Import(args) => args.progress == crate::progress::ProgressArg::Json,
        CommandRoot::Show(args) => {
            matches!(
                &args.target,
                ShowTarget::Session(args)
                    if matches!(args.format, OutputFormat::Jsonl | OutputFormat::Markdown)
            ) || matches!(
                &args.target,
                ShowTarget::Event(args)
                    if matches!(args.format, OutputFormat::Jsonl | OutputFormat::Markdown)
            )
        }
        CommandRoot::List(_) => true,
        CommandRoot::Mcp(_) => true,
        _ => false,
    }
}

pub(crate) fn command_deprecation_warning_eligible(command: &CommandRoot) -> bool {
    if command_machine_readable_output(command, command_json_output(command)) {
        return false;
    }
    !matches!(command, CommandRoot::Mcp(_) | CommandRoot::Daemon(_))
}

pub(super) fn command_daemon_autostart_trigger(
    command: &CommandRoot,
) -> Option<DaemonTriggerCommandArg> {
    if command_machine_readable_output(command, command_json_output(command)) {
        return None;
    }
    match command {
        CommandRoot::Import(args) if import_should_autostart_daemon(args) => {
            Some(DaemonTriggerCommandArg::Import)
        }
        _ => None,
    }
}

pub(super) fn command_can_report_malformed_config(command: &CommandRoot) -> bool {
    matches!(
        command,
        CommandRoot::Daemon(crate::DaemonArgs {
            command: DaemonCommand::Status(_),
        })
    ) || matches!(command, CommandRoot::Mcp(_))
}

pub(crate) fn command_operation_descriptor(command: &CommandRoot) -> OperationDescriptor {
    let operation = match command {
        CommandRoot::Unified(command) => {
            if command.is_graph() {
                CliOperation::Graph
            } else {
                CliOperation::Output
            }
        }
        CommandRoot::Blame(args) => CliOperation::Blame(crate::analytics::BlameTerminalFacts::new(
            crate::commands::blame::target_kind(&args.target().expect("validated Blame target")),
        )),
        CommandRoot::Setup(args) => CliOperation::Setup(SetupTelemetry {
            no_daemon: args.no_daemon,
            wait: args.wait,
            progress_mode: crate::observability_product::progress_mode(args.progress),
            mode: None,
            providers_detected: None,
            cataloged_sessions: None,
            inventory_sources: None,
            inventory_source_files: None,
            pending_sessions: None,
            catalog_source_bytes: None,
            inventory_source_bytes: None,
            has_indexed_content: None,
            import: crate::observability_product::setup_import_telemetry(
                args.progress,
                args.no_daemon,
            ),
        }),
        CommandRoot::Semantic(args) => match &args.command {
            ctx_cli_presentation::commands::SemanticCommand::Enable(_) => {
                CliOperation::SemanticEnable
            }
            ctx_cli_presentation::commands::SemanticCommand::Status(_) => {
                CliOperation::SemanticStatus
            }
            ctx_cli_presentation::commands::SemanticCommand::Disable(_) => {
                CliOperation::SemanticDisable
            }
        },
        CommandRoot::Status(_) => CliOperation::Status(StatusTelemetry::default()),
        CommandRoot::Stats(_) => CliOperation::Stats,
        CommandRoot::Index(_) => CliOperation::Index(IndexTelemetry::default()),
        CommandRoot::Sources(args) => CliOperation::Sources(SourcesTelemetry {
            all: args.all,
            provider_filter: args.provider.map(|provider| provider.capture_provider()),
            providers_detected: None,
            providers_existing: None,
            providers_importable: None,
        }),
        CommandRoot::Import(args) => {
            CliOperation::Import(crate::observability_product::import_telemetry(args))
        }
        CommandRoot::Show(args) => match &args.target {
            ShowTarget::Session(args) => CliOperation::ShowSession(ShowTelemetry {
                target_kind: TargetKind::Session,
                transcript_mode: Some(crate::observability_product::transcript_mode(args.mode)),
                output_format: crate::observability_product::render_format(args.format),
                writes_out_file: args.out.is_some(),
                provider_lookup: args.provider.is_some() || args.provider_session.is_some(),
                window: None,
                events_returned: None,
            }),
            ShowTarget::Event(args) => CliOperation::ShowEvent(ShowTelemetry {
                target_kind: TargetKind::Event,
                transcript_mode: None,
                output_format: crate::observability_product::render_format(args.format),
                writes_out_file: false,
                provider_lookup: false,
                window: Some(count_bucket(
                    args.window.unwrap_or(args.before.max(args.after)) as u64,
                )),
                events_returned: None,
            }),
        },
        CommandRoot::List(args) => match &args.target {
            crate::commands::list::ListTarget::Events(args) => {
                CliOperation::ShowEvent(ShowTelemetry {
                    target_kind: TargetKind::Events,
                    transcript_mode: None,
                    output_format: match args.format {
                        crate::commands::list::EventQueryFormat::Json => RenderFormat::Json,
                        crate::commands::list::EventQueryFormat::Jsonl => RenderFormat::Jsonl,
                    },
                    writes_out_file: false,
                    provider_lookup: !args.provider.is_empty(),
                    window: None,
                    events_returned: None,
                })
            }
        },
        CommandRoot::Locate(args) => match &args.target {
            crate::LocateTarget::Session(args) => CliOperation::Locate(LocateTelemetry {
                target_kind: TargetKind::Session,
                output_format: crate::observability_product::json_render_format(args.format),
                provider_lookup: args.provider.is_some() || args.provider_session.is_some(),
            }),
            crate::LocateTarget::Event(args) => CliOperation::Locate(LocateTelemetry {
                target_kind: TargetKind::Event,
                output_format: crate::observability_product::json_render_format(args.format),
                provider_lookup: false,
            }),
        },
        CommandRoot::Search(args) => CliOperation::Search(SearchTelemetry {
            has_query: args.query.is_some(),
            has_provider_filter: args.provider.is_some(),
            has_workspace_filter: args.workspace.is_some(),
            has_since_filter: args.since.is_some(),
            has_event_type_filter: args.event_type.is_some(),
            has_file_filter: args.file.is_some(),
            has_session_filter: args.session.is_some(),
            event_results: args.events || args.session.is_some(),
            primary_only: args.primary_only,
            include_current_session: args.include_current_session,
            limit: count_bucket(args.limit as u64),
            provider_filter: args.provider.map(|provider| provider.capture_provider()),
            refresh_duration: None,
            refresh_mode: None,
            refresh_status: None,
            refresh_source_count: None,
            has_indexed_content_after: None,
            query_length: None,
            query_term_count: None,
            query_duration: None,
            backend_requested: None,
            backend_effective: None,
            result_count: None,
            citation_count: None,
            zero_result: None,
            render_duration: None,
            output_duration: None,
            output_served: None,
            health: None,
        }),
        CommandRoot::Docs(_) => CliOperation::Docs(DocsTelemetry::default()),
        CommandRoot::Integrations(_) => CliOperation::Integrations(IntegrationTelemetry::default()),
        CommandRoot::Mcp(_) => CliOperation::McpServe,
        CommandRoot::Daemon(args) => match &args.command {
            DaemonCommand::Run(_) => CliOperation::DaemonRun,
            DaemonCommand::Status(_) => CliOperation::DaemonStatus,
            DaemonCommand::Enable(_) => CliOperation::DaemonEnable,
            DaemonCommand::Disable(_) => CliOperation::DaemonDisable,
        },
        CommandRoot::Upgrade(args) => CliOperation::Upgrade {
            telemetry: UpgradeTelemetry {
                mode: UpgradeMode::Manual,
                operation: match args.operation() {
                    "check" => UpgradeOperation::Check,
                    "status" => UpgradeOperation::Status,
                    "enable" => UpgradeOperation::Enable,
                    "disable" => UpgradeOperation::Disable,
                    _ => UpgradeOperation::Apply,
                },
                dry_run: args.dry_run,
                suppress_event: false,
                status: None,
                applied: None,
                scheduled: None,
                update_available: None,
                update_was_available: None,
                upgrade_attempt_id: None,
                managed_install: None,
                self_upgrade_allowed: None,
                auto_upgrade_allowed: None,
                warning_count: None,
                channel: None,
                failure_kind: None,
            },
            record_local_usage: !args.replacement_helper && args.hosted_transaction.is_none(),
        },
        CommandRoot::Doctor(_) => CliOperation::Doctor(DoctorTelemetry::default()),
    };
    OperationDescriptor::Cli(operation)
}

#[cfg(test)]
pub(super) fn command_local_usage_draft(command: &CommandRoot) -> local_usage::CliUsage {
    let descriptor = command_operation_descriptor(command);
    local_usage::CliUsage::from_descriptor(&descriptor)
}

pub(super) fn command_is_status_report(command: &CommandRoot) -> bool {
    matches!(command, CommandRoot::Status(_))
}

pub(super) fn import_should_autostart_daemon(args: &ImportArgs) -> bool {
    !args.no_daemon
        && args.input_format.is_none()
        && args.history_source.is_none()
        && args.history_source_manifest.is_empty()
}

pub(super) fn quiet_output(flag: bool) -> bool {
    flag || env_truthy("CTX_QUIET")
}

pub(super) fn env_truthy(key: &str) -> bool {
    env::var_os(key).is_some_and(|value| {
        let value = value.to_string_lossy();
        !matches!(
            value.trim().to_ascii_lowercase().as_str(),
            "" | "0" | "false" | "no" | "off"
        )
    })
}
