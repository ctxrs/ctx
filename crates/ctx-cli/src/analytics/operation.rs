use std::time::Duration;

use crate::cli::{CommandRoot, ShowTarget};
use crate::commands::locate::LocateTarget;

use super::*;

#[derive(Debug)]
pub(crate) enum ClientOperationV1 {
    Setup(SetupTelemetry),
    Status(StatusTelemetry),
    Index(IndexTelemetry),
    Sources(SourcesTelemetry),
    Import(ImportTelemetry),
    Show(ShowTelemetry),
    Locate(LocateTelemetry),
    Search(SearchTelemetry),
    Docs(DocsTelemetry),
    Integration(IntegrationTelemetry),
    Upgrade(UpgradeTelemetry),
    Doctor(DoctorTelemetry),
}

impl ClientOperationV1 {
    pub(crate) fn name(&self) -> &'static str {
        match self {
            Self::Setup(_) => "setup",
            Self::Status(_) => "status",
            Self::Index(_) => "index",
            Self::Sources(_) => "sources",
            Self::Import(_) => "import",
            Self::Show(_) => "show",
            Self::Locate(_) => "locate",
            Self::Search(_) => "search",
            Self::Docs(_) => "docs",
            Self::Integration(_) => "integration",
            Self::Upgrade(_) => "upgrade",
            Self::Doctor(_) => "doctor",
        }
    }
}

#[allow(dead_code)]
#[derive(Debug)]
pub(crate) enum OperationPayloadV1 {
    Cli(ClientOperationV1),
    Mcp(McpOperationV1),
    ProHost(ProHostOperationV1),
    Daemon(DaemonOperationV1),
}

impl OperationPayloadV1 {
    pub(crate) fn surface(&self) -> Surface {
        match self {
            Self::Cli(_) => Surface::Cli,
            Self::Mcp(_) => Surface::Mcp,
            Self::ProHost(_) => Surface::ProHost,
            Self::Daemon(_) => Surface::Daemon,
        }
    }

    pub(crate) fn name(&self) -> &'static str {
        match self {
            Self::Cli(operation) => operation.name(),
            Self::Mcp(operation) => operation.name(),
            Self::ProHost(operation) => operation.name(),
            Self::Daemon(operation) => operation.name(),
        }
    }
}

#[derive(Debug)]
pub(crate) struct OperationCompletedV1 {
    pub(crate) payload: OperationPayloadV1,
    pub(crate) output: Option<OutputKind>,
    pub(crate) outcome: Outcome,
    pub(crate) duration: DurationBucket,
    pub(crate) deprecated_daemon_control: bool,
    pub(crate) deprecated_upgrade_control: bool,
}

#[allow(dead_code)]
impl OperationCompletedV1 {
    pub(crate) fn for_mcp(operation: McpOperationV1, outcome: Outcome, duration: Duration) -> Self {
        Self::for_non_cli(OperationPayloadV1::Mcp(operation), outcome, duration)
    }

    pub(crate) fn for_pro_host(
        operation: ProHostOperationV1,
        outcome: Outcome,
        duration: Duration,
    ) -> Self {
        Self::for_non_cli(OperationPayloadV1::ProHost(operation), outcome, duration)
    }

    pub(crate) fn for_daemon(
        operation: DaemonOperationV1,
        outcome: Outcome,
        duration: Duration,
    ) -> Self {
        Self::for_non_cli(OperationPayloadV1::Daemon(operation), outcome, duration)
    }

    pub(crate) fn for_non_cli(
        payload: OperationPayloadV1,
        outcome: Outcome,
        duration: Duration,
    ) -> Self {
        Self {
            payload,
            output: None,
            outcome,
            duration: duration_bucket(duration),
            deprecated_daemon_control: false,
            deprecated_upgrade_control: false,
        }
    }

    pub(crate) fn for_automatic_upgrade(
        upgrade: UpgradeTelemetry,
        outcome: Outcome,
        duration: Duration,
    ) -> Self {
        Self::for_non_cli(
            OperationPayloadV1::Cli(ClientOperationV1::Upgrade(upgrade)),
            outcome,
            duration,
        )
    }
}

pub(crate) struct ClientOperationDraft {
    output: OutputKind,
    operation: ClientOperationV1,
    deprecated_daemon_control: bool,
    deprecated_upgrade_control: bool,
}

impl ClientOperationDraft {
    pub(crate) fn from_command(command: &CommandRoot, json_output: bool) -> Option<Self> {
        let operation = match command {
            CommandRoot::Setup(args) => ClientOperationV1::Setup(SetupTelemetry {
                catalog_only: args.catalog_only,
                no_daemon: args.no_daemon,
                wait: args.wait,
                progress_mode: ProgressMode::from_arg(args.progress),
                mode: None,
                providers_detected: None,
                cataloged_sessions: None,
                inventory_sources: None,
                inventory_source_files: None,
                pending_sessions: None,
                catalog_source_bytes: None,
                inventory_source_bytes: None,
                has_indexed_content: None,
                store: StoreTelemetry::default(),
                import: ImportTelemetry::for_setup(args.progress, args.no_daemon),
            }),
            CommandRoot::Status(_) => ClientOperationV1::Status(StatusTelemetry::default()),
            CommandRoot::Index(_) => ClientOperationV1::Index(IndexTelemetry::default()),
            CommandRoot::Sources(args) => ClientOperationV1::Sources(SourcesTelemetry {
                all: args.all,
                show_missing: args.show_missing,
                provider_filter: args.provider.map(|provider| provider.capture_provider()),
                providers_detected: None,
                providers_existing: None,
                providers_importable: None,
            }),
            CommandRoot::Import(args) => {
                ClientOperationV1::Import(ImportTelemetry::from_args(args))
            }
            CommandRoot::Show(args) => match &args.target {
                ShowTarget::Session(args) => ClientOperationV1::Show(ShowTelemetry {
                    target_kind: TargetKind::Session,
                    transcript_mode: Some(TranscriptModeKind::from_mode(args.mode)),
                    output_format: RenderFormat::from_output_format(args.format),
                    writes_out_file: args.out.is_some(),
                    provider_lookup: args.provider.is_some() || args.provider_session.is_some(),
                    window: None,
                    events_returned: None,
                }),
                ShowTarget::Event(args) => ClientOperationV1::Show(ShowTelemetry {
                    target_kind: TargetKind::Event,
                    transcript_mode: None,
                    output_format: RenderFormat::from_output_format(args.format),
                    writes_out_file: false,
                    provider_lookup: false,
                    window: Some(count_bucket(
                        args.window.unwrap_or(args.before.max(args.after)) as u64,
                    )),
                    events_returned: None,
                }),
            },
            CommandRoot::Locate(args) => match &args.target {
                LocateTarget::Session(args) => ClientOperationV1::Locate(LocateTelemetry {
                    target_kind: TargetKind::Session,
                    output_format: RenderFormat::from_json_output_format(args.format),
                    provider_lookup: args.provider.is_some() || args.provider_session.is_some(),
                }),
                LocateTarget::Event(args) => ClientOperationV1::Locate(LocateTelemetry {
                    target_kind: TargetKind::Event,
                    output_format: RenderFormat::from_json_output_format(args.format),
                    provider_lookup: false,
                }),
            },
            CommandRoot::Search(args) => ClientOperationV1::Search(SearchTelemetry {
                has_query: args.query.is_some(),
                has_provider_filter: args.provider.is_some(),
                has_workspace_filter: args.workspace.is_some(),
                has_since_filter: args.since.is_some(),
                has_event_type_filter: args.event_type.is_some(),
                has_file_filter: args.file.is_some(),
                has_session_filter: args.session.is_some(),
                event_results: args.events || args.session.is_some(),
                primary_only: args.primary_only,
                include_subagents: args.include_subagents,
                include_current_session: args.include_current_session,
                limit: count_bucket(args.limit as u64),
                provider_filter: args.provider.map(|provider| provider.capture_provider()),
                had_existing_store: None,
                indexed_content_before_known: None,
                had_indexed_content_before: None,
                refresh_duration: None,
                refresh_mode: None,
                refresh_status: None,
                refresh_source_count: None,
                store_created: None,
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
                store: StoreTelemetry::default(),
            }),
            CommandRoot::Docs(_) => ClientOperationV1::Docs(DocsTelemetry::default()),
            CommandRoot::Integrations(_) => {
                ClientOperationV1::Integration(IntegrationTelemetry::default())
            }
            CommandRoot::Upgrade(args) => ClientOperationV1::Upgrade(UpgradeTelemetry {
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
            }),
            CommandRoot::Doctor(_) => ClientOperationV1::Doctor(DoctorTelemetry::default()),
            CommandRoot::Pro(_)
            | CommandRoot::Referral(_)
            | CommandRoot::Blame(_)
            | CommandRoot::Stats(_)
            | CommandRoot::Mcp(_)
            | CommandRoot::Daemon(_) => return None,
        };
        Some(Self {
            output: OutputKind::from_json_output(json_output),
            operation,
            deprecated_daemon_control: false,
            deprecated_upgrade_control: false,
        })
    }

    pub(crate) fn set_deprecated_controls(&mut self, ids: Option<&str>) {
        let ids = ids.unwrap_or_default();
        self.deprecated_daemon_control =
            ids.contains("CTX_DAEMON_OFF") || ids.contains("CTX_DISABLE_DAEMON");
        self.deprecated_upgrade_control =
            ids.contains("CTX_UPGRADE_OFF") || ids.contains("CTX_DISABLE_AUTO_UPGRADE");
    }

    pub(crate) fn setup_mut(&mut self) -> &mut SetupTelemetry {
        match &mut self.operation {
            ClientOperationV1::Setup(value) => value,
            _ => unreachable!("setup telemetry requested for a different operation"),
        }
    }

    pub(crate) fn status_mut(&mut self) -> &mut StatusTelemetry {
        match &mut self.operation {
            ClientOperationV1::Status(value) => value,
            _ => unreachable!("status telemetry requested for a different operation"),
        }
    }

    pub(crate) fn index_mut(&mut self) -> &mut IndexTelemetry {
        match &mut self.operation {
            ClientOperationV1::Index(value) => value,
            _ => unreachable!("index telemetry requested for a different operation"),
        }
    }

    pub(crate) fn sources_mut(&mut self) -> &mut SourcesTelemetry {
        match &mut self.operation {
            ClientOperationV1::Sources(value) => value,
            _ => unreachable!("sources telemetry requested for a different operation"),
        }
    }

    pub(crate) fn import_mut(&mut self) -> &mut ImportTelemetry {
        match &mut self.operation {
            ClientOperationV1::Import(value) => value,
            _ => unreachable!("import telemetry requested for a different operation"),
        }
    }

    pub(crate) fn show_mut(&mut self) -> &mut ShowTelemetry {
        match &mut self.operation {
            ClientOperationV1::Show(value) => value,
            _ => unreachable!("show telemetry requested for a different operation"),
        }
    }

    pub(crate) fn locate_mut(&mut self) -> &mut LocateTelemetry {
        match &mut self.operation {
            ClientOperationV1::Locate(value) => value,
            _ => unreachable!("locate telemetry requested for a different operation"),
        }
    }

    pub(crate) fn search_mut(&mut self) -> &mut SearchTelemetry {
        match &mut self.operation {
            ClientOperationV1::Search(value) => value,
            _ => unreachable!("search telemetry requested for a different operation"),
        }
    }

    pub(crate) fn docs_mut(&mut self) -> &mut DocsTelemetry {
        match &mut self.operation {
            ClientOperationV1::Docs(value) => value,
            _ => unreachable!("docs telemetry requested for a different operation"),
        }
    }

    pub(crate) fn integration_mut(&mut self) -> &mut IntegrationTelemetry {
        match &mut self.operation {
            ClientOperationV1::Integration(value) => value,
            _ => unreachable!("integration telemetry requested for a different operation"),
        }
    }

    pub(crate) fn upgrade_mut(&mut self) -> &mut UpgradeTelemetry {
        match &mut self.operation {
            ClientOperationV1::Upgrade(value) => value,
            _ => unreachable!("upgrade telemetry requested for a different operation"),
        }
    }

    pub(crate) fn doctor_mut(&mut self) -> &mut DoctorTelemetry {
        match &mut self.operation {
            ClientOperationV1::Doctor(value) => value,
            _ => unreachable!("doctor telemetry requested for a different operation"),
        }
    }

    pub(crate) fn finish(self, success: bool, duration: Duration) -> PublicEventV1 {
        PublicEventV1::OperationCompleted(OperationCompletedV1 {
            payload: OperationPayloadV1::Cli(self.operation),
            output: Some(self.output),
            outcome: if success {
                Outcome::Success
            } else {
                Outcome::Failure
            },
            duration: duration_bucket(duration),
            deprecated_daemon_control: self.deprecated_daemon_control,
            deprecated_upgrade_control: self.deprecated_upgrade_control,
        })
    }

    pub(crate) fn should_emit(&self) -> bool {
        !matches!(
            &self.operation,
            ClientOperationV1::Upgrade(value) if value.suppress_event
        )
    }
}
