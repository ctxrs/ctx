use std::time::Duration;

use ctx_history_core::CaptureProvider;
use uuid::Uuid;

use super::{sender::serialize_event, *};

#[test]
fn buckets_cover_boundaries() {
    assert_eq!(count_bucket(0), CountBucket::Zero);
    assert_eq!(count_bucket(1_001), CountBucket::OverOneThousand);
    assert_eq!(bytes_bucket(102_400), BytesBucket::OneHundredKbToOneMb);
    assert_eq!(text_length_bucket(501), TextLengthBucket::OverFiveHundred);
    assert_eq!(
        duration_bucket(Duration::from_secs(30)),
        DurationBucket::AtLeastThirtySeconds
    );
}

#[test]
fn public_surfaces_are_exhaustive_and_stable() {
    assert_eq!(Surface::Cli.as_str(), "cli");
    assert_eq!(Surface::Mcp.as_str(), "mcp");
    assert_eq!(Surface::ProHost.as_str(), "pro_host");
    assert_eq!(Surface::Daemon.as_str(), "daemon");
}

#[test]
fn runtime_observation_has_typed_constructor_seams() {
    let daemon = PublicEventV1::RuntimeObservation(RuntimeObservationV1::daemon(
        DaemonRuntimeObservationV1::Cycle,
        Outcome::Success,
        Duration::from_secs(1),
    ));
    let mcp = PublicEventV1::RuntimeObservation(RuntimeObservationV1::mcp(
        McpRuntimeObservationV1::Stopped,
        Outcome::Failure,
        Duration::from_secs(31),
    ));
    let occurred_at = chrono::DateTime::parse_from_rfc3339("2026-07-22T12:34:00Z")
        .unwrap()
        .with_timezone(&chrono::Utc);
    let daemon = serialize_event(&daemon, occurred_at, None, None);
    let mcp = serialize_event(&mcp, occurred_at, None, None);
    assert_eq!(daemon["event_name"], "runtime_observation");
    assert_eq!(daemon["surface"], "daemon");
    assert_eq!(daemon["operation"], "cycle");
    assert_eq!(mcp["surface"], "mcp");
    assert_eq!(mcp["operation"], "stopped");
}

#[test]
fn event_ids_are_uuid_v4_and_timestamps_are_minute_aligned() {
    let event = PublicEventV1::OperationCompleted(OperationCompletedV1::for_mcp(
        McpOperationV1::Initialize,
        Outcome::Success,
        Duration::ZERO,
    ));
    let occurred_at = chrono::DateTime::parse_from_rfc3339("2026-07-22T12:34:00Z")
        .unwrap()
        .with_timezone(&chrono::Utc);
    let serialized = serialize_event(&event, occurred_at, None, None);
    let event_id = Uuid::parse_str(serialized["event_id"].as_str().unwrap()).unwrap();
    assert_eq!(event_id.get_version_num(), 4);
    assert_eq!(serialized["occurred_at"], "2026-07-22T12:34:00Z");
    assert!(serialized.get("duration_ms").is_none());
}

#[test]
fn known_doc_topics_are_closed() {
    assert_eq!(
        DocTopicId::from_known_id("provider-import-policy")
            .unwrap()
            .as_str(),
        "provider-import-policy"
    );
    assert!(DocTopicId::from_known_id("/private/topic").is_none());
}

#[test]
fn durable_family_serialization_matches_public_goldens() {
    let occurred_at = chrono::DateTime::parse_from_rfc3339("2026-07-22T12:34:00Z")
        .unwrap()
        .with_timezone(&chrono::Utc);
    let events = [
        (
            PublicEventV1::OperationCompleted(OperationCompletedV1 {
                payload: OperationPayloadV1::Cli(ClientOperationV1::Status(StatusTelemetry {
                    initialized: Some(true),
                    indexed_items: Some(count_bucket(42)),
                    indexed_sessions: None,
                    indexed_events: None,
                    indexed_sources: None,
                    inventory_units: None,
                    pending_inventory_units: None,
                    failed_inventory_units: None,
                    stale_inventory_units: None,
                })),
                output: Some(OutputKind::Json),
                outcome: Outcome::Success,
                duration: duration_bucket(Duration::from_millis(10)),
                auto_upgrade: None,
                deprecated_daemon_control: false,
                deprecated_upgrade_control: false,
            }),
            include_str!(
                "../../../../contracts/telemetry-v1/fixtures/operation_completed.valid.json"
            ),
        ),
        (
            PublicEventV1::ProviderRefreshCompleted(ProviderRefreshCompletedV1::foreground(
                Outcome::Success,
                Duration::from_secs(2),
                ForegroundProviderRefreshV1 {
                    provider: CaptureProvider::Codex,
                    trigger: ProviderRefreshTrigger::Search,
                    source_mode: ProviderRefreshSourceMode::Discovered,
                    change: ProviderRefreshChange::Changed,
                    work_remaining: false,
                    counts: ProviderRefreshCountsV1::new(1, 3, 8, 0, 0, 0, 0, 2048),
                },
            )),
            include_str!(
                "../../../../contracts/telemetry-v1/fixtures/provider_refresh_completed.valid.json"
            ),
        ),
        (
            PublicEventV1::RuntimeObservation(RuntimeObservationV1::daemon(
                DaemonRuntimeObservationV1::liveness(DaemonRuntimeSnapshotV1::new(
                    DaemonRunFactsV1::new(
                        DaemonStartModeV1::Auto,
                        DaemonSupervisorV1::CliAutostart,
                        Some(DaemonTriggerV1::Search),
                    ),
                    DaemonCycleStateV1::new(
                        DaemonHistoryFreshnessV1::Current,
                        DaemonBacklogV1::Bucket(CountBucket::Zero),
                        DaemonCoverageV1::Complete,
                        DaemonBackoffV1::None,
                    ),
                )),
                Outcome::Success,
                Duration::from_secs(23 * 60 * 60),
            )),
            include_str!(
                "../../../../contracts/telemetry-v1/fixtures/runtime_observation.valid.json"
            ),
        ),
    ];

    for (event, fixture) in events {
        let mut actual = serialize_event(&event, occurred_at, None, None);
        let expected: serde_json::Value = serde_json::from_str(fixture).unwrap();
        actual["event_id"] = expected["event_id"].clone();
        assert_eq!(actual, expected);
    }

    let install_stage: serde_json::Value = serde_json::from_str(include_str!(
        "../../../../contracts/telemetry-v1/fixtures/install_stage.valid.json"
    ))
    .unwrap();
    assert_eq!(
        install_stage,
        serde_json::json!({
            "event_name": "install_stage",
            "event_version": 1,
            "install_attempt_id": "ia_01JZCTXHOSTED",
            "stage": "installer",
            "status": "completed",
            "platform": "linux",
            "arch": "x64",
            "script_family": "posix",
        })
    );
}
