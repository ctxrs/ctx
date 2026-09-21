use std::time::Duration;

use crate::operation_descriptor::{CliOperation, McpOperation, OperationDescriptor};

use super::*;

#[derive(Debug)]
pub struct OperationCompletedV1 {
    pub descriptor: OperationDescriptor,
    pub output: Option<OutputKind>,
    pub outcome: Outcome,
    pub duration: DurationBucket,
    pub deprecated_daemon_control: bool,
    pub deprecated_upgrade_control: bool,
}

#[allow(dead_code)]
impl OperationCompletedV1 {
    pub fn for_mcp(operation: McpOperation, outcome: Outcome, duration: Duration) -> Self {
        Self::for_non_cli(OperationDescriptor::Mcp(operation), outcome, duration)
    }

    pub fn for_daemon(operation: DaemonOperationV1, outcome: Outcome, duration: Duration) -> Self {
        Self::for_non_cli(OperationDescriptor::Daemon(operation), outcome, duration)
    }

    pub fn for_non_cli(
        descriptor: OperationDescriptor,
        outcome: Outcome,
        duration: Duration,
    ) -> Self {
        Self {
            descriptor,
            output: None,
            outcome,
            duration: duration_bucket(duration),
            deprecated_daemon_control: false,
            deprecated_upgrade_control: false,
        }
    }

    pub fn for_automatic_upgrade(
        upgrade: UpgradeTelemetry,
        outcome: Outcome,
        duration: Duration,
    ) -> Self {
        Self::for_non_cli(
            OperationDescriptor::Cli(CliOperation::Upgrade {
                telemetry: upgrade,
                record_local_usage: false,
            }),
            outcome,
            duration,
        )
    }
}

pub struct ClientOperationDraft {
    output: OutputKind,
    operation: CliOperation,
    deprecated_daemon_control: bool,
    deprecated_upgrade_control: bool,
}

impl ClientOperationDraft {
    pub fn from_descriptor(descriptor: OperationDescriptor, json_output: bool) -> Option<Self> {
        let OperationDescriptor::Cli(operation) = descriptor else {
            return None;
        };
        if !operation.emits_client_analytics() {
            return None;
        }
        Some(Self {
            output: OutputKind::from_json_output(json_output),
            operation,
            deprecated_daemon_control: false,
            deprecated_upgrade_control: false,
        })
    }

    pub fn set_deprecated_controls(&mut self, ids: Option<&str>) {
        let ids = ids.unwrap_or_default();
        self.deprecated_daemon_control =
            ids.contains("CTX_DAEMON_OFF") || ids.contains("CTX_DISABLE_DAEMON");
        self.deprecated_upgrade_control =
            ids.contains("CTX_UPGRADE_OFF") || ids.contains("CTX_DISABLE_AUTO_UPGRADE");
    }

    pub fn setup_mut(&mut self) -> &mut SetupTelemetry {
        match &mut self.operation {
            CliOperation::Setup(value) => value,
            _ => unreachable!("setup telemetry requested for a different operation"),
        }
    }

    pub fn status_mut(&mut self) -> &mut StatusTelemetry {
        match &mut self.operation {
            CliOperation::Status(value) => value,
            _ => unreachable!("status telemetry requested for a different operation"),
        }
    }

    pub fn index_mut(&mut self) -> &mut IndexTelemetry {
        match &mut self.operation {
            CliOperation::Index(value) => value,
            _ => unreachable!("index telemetry requested for a different operation"),
        }
    }

    pub fn sources_mut(&mut self) -> &mut SourcesTelemetry {
        match &mut self.operation {
            CliOperation::Sources(value) => value,
            _ => unreachable!("sources telemetry requested for a different operation"),
        }
    }

    pub fn import_mut(&mut self) -> &mut ImportTelemetry {
        match &mut self.operation {
            CliOperation::Import(value) => value,
            _ => unreachable!("import telemetry requested for a different operation"),
        }
    }

    pub fn show_mut(&mut self) -> &mut ShowTelemetry {
        match &mut self.operation {
            CliOperation::ShowSession(value) | CliOperation::ShowEvent(value) => value,
            _ => unreachable!("show telemetry requested for a different operation"),
        }
    }

    pub fn locate_mut(&mut self) -> &mut LocateTelemetry {
        match &mut self.operation {
            CliOperation::Locate(value) => value,
            _ => unreachable!("locate telemetry requested for a different operation"),
        }
    }

    pub fn search_mut(&mut self) -> &mut SearchTelemetry {
        match &mut self.operation {
            CliOperation::Search(value) => value,
            _ => unreachable!("search telemetry requested for a different operation"),
        }
    }

    pub fn blame_mut(&mut self) -> &mut BlameTerminalFacts {
        match &mut self.operation {
            CliOperation::Blame(value) => value,
            _ => unreachable!("blame telemetry requested for a different operation"),
        }
    }

    pub fn docs_mut(&mut self) -> &mut DocsTelemetry {
        match &mut self.operation {
            CliOperation::Docs(value) => value,
            _ => unreachable!("docs telemetry requested for a different operation"),
        }
    }

    pub fn integration_mut(&mut self) -> &mut IntegrationTelemetry {
        match &mut self.operation {
            CliOperation::Integrations(value) => value,
            _ => unreachable!("integration telemetry requested for a different operation"),
        }
    }

    pub fn upgrade_mut(&mut self) -> &mut UpgradeTelemetry {
        match &mut self.operation {
            CliOperation::Upgrade { telemetry, .. } => telemetry,
            _ => unreachable!("upgrade telemetry requested for a different operation"),
        }
    }

    pub fn doctor_mut(&mut self) -> &mut DoctorTelemetry {
        match &mut self.operation {
            CliOperation::Doctor(value) => value,
            _ => unreachable!("doctor telemetry requested for a different operation"),
        }
    }

    pub fn finish(self, success: bool, duration: Duration) -> PublicEventV1 {
        PublicEventV1::OperationCompleted(OperationCompletedV1 {
            descriptor: OperationDescriptor::Cli(self.operation),
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

    pub fn should_emit(&self) -> bool {
        !matches!(
            &self.operation,
            CliOperation::Upgrade { telemetry, .. } if telemetry.suppress_event
        )
    }
}
