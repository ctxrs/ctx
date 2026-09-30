use super::*;
use crate::analytics::sender::serialize_event;

fn serialized(event: PublicEventV1) -> Value {
    let at = chrono::DateTime::parse_from_rfc3339("2026-07-22T12:34:00Z")
        .unwrap()
        .with_timezone(&chrono::Utc);
    serialize_event(&event, at, None, None)
}

#[test]
fn hosted_terminals_match_shared_wire_fixtures() {
    for (event, fixture) in [
        (
            hosted_operation_completed(
                HostedOperationV1::RemoteConnect,
                OutputKind::Json,
                Ok(()),
                Duration::from_millis(25),
            ),
            include_str!("../../../../../contracts/telemetry-v1/fixtures/hosted_operation_completed.valid.json"),
        ),
        (
            hosted_operation_completed(
                HostedOperationV1::ServerInvite,
                OutputKind::Human,
                Err(HostedFailureV1::Operation(HostedFailureTypeV1::Unauthorized)),
                Duration::from_millis(300),
            ),
            include_str!("../../../../../contracts/telemetry-v1/fixtures/hosted_operation_failure.valid.json"),
        ),
        (
            hosted_operation_completed(
                HostedOperationV1::ArchiveRestore,
                OutputKind::Json,
                Err(HostedFailureV1::Output),
                Duration::from_secs(3600),
            ),
            include_str!("../../../../../contracts/telemetry-v1/fixtures/hosted_output_failure.valid.json"),
        ),
    ] {
        let expected: Value = serde_json::from_str(fixture).unwrap();
        let mut actual = serialized(event);
        assert!(uuid::Uuid::parse_str(actual["event_id"].as_str().unwrap()).is_ok());
        actual["event_id"] = expected["event_id"].clone();
        assert_eq!(actual, expected);
    }
}

#[test]
fn hosted_wire_is_closed_and_success_omits_failure_facts() {
    for (operation, name) in [
        (HostedOperationV1::ArchiveExport, "archive_export"),
        (HostedOperationV1::ArchiveRestore, "archive_restore"),
        (HostedOperationV1::RemoteConnect, "remote_connect"),
        (HostedOperationV1::RemoteShare, "remote_share"),
        (HostedOperationV1::RemoteSync, "remote_sync"),
        (HostedOperationV1::ServerInit, "server_init"),
        (HostedOperationV1::ServerInvite, "server_invite"),
        (HostedOperationV1::ServerGrant, "server_grant"),
        (HostedOperationV1::ServerRevoke, "server_revoke"),
        (HostedOperationV1::ServerWithdraw, "server_withdraw"),
        (HostedOperationV1::ServerBackup, "server_backup"),
        (HostedOperationV1::ServerRestore, "server_restore"),
    ] {
        let event = serialized(hosted_operation_completed(
            operation,
            OutputKind::Human,
            Ok(()),
            Duration::ZERO,
        ));
        assert_eq!(event["operation"], name);
        assert_eq!(event["surface"], "cli");
        assert_eq!(event["outcome"], "success");
        assert_eq!(event["properties"], json!({"output": "human"}));
    }
    for (kind, name) in [
        (HostedFailureTypeV1::InvalidRequest, "invalid_request"),
        (HostedFailureTypeV1::Unauthorized, "unauthorized"),
        (HostedFailureTypeV1::Forbidden, "forbidden"),
        (HostedFailureTypeV1::NotFound, "not_found"),
        (HostedFailureTypeV1::Conflict, "conflict"),
        (HostedFailureTypeV1::Credentials, "credentials"),
        (HostedFailureTypeV1::PolicyDenied, "policy_denied"),
        (HostedFailureTypeV1::Unavailable, "unavailable"),
        (HostedFailureTypeV1::Capacity, "capacity"),
        (HostedFailureTypeV1::Io, "io"),
        (HostedFailureTypeV1::InvalidArchive, "invalid_archive"),
        (HostedFailureTypeV1::Other, "other"),
    ] {
        let event = serialized(hosted_operation_completed(
            HostedOperationV1::RemoteSync,
            OutputKind::Json,
            Err(HostedFailureV1::Operation(kind)),
            Duration::ZERO,
        ));
        assert_eq!(event["outcome"], "failure");
        assert_eq!(
            event["properties"],
            json!({
                "output": "json", "hosted_failure_stage": "operation", "failure_type": name,
            })
        );
    }
    let excluded = crate::operation_descriptor::CliOperation::Hosted;
    assert!(!excluded.emits_client_analytics());
    assert_eq!(excluded.local_usage_operation(), None);
}
