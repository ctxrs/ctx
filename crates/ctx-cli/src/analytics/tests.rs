use std::time::Duration;

use ctx_history_core::CaptureProvider;
use uuid::Uuid;

use super::{sender::serialize_event, *};

#[test]
fn buckets_cover_boundaries() {
    for (value, expected) in [
        (0, CountBucket::Zero),
        (1, CountBucket::One),
        (2, CountBucket::TwoToFive),
        (6, CountBucket::SixToTwenty),
        (21, CountBucket::TwentyOneToOneHundred),
        (101, CountBucket::OneHundredOneToOneThousand),
        (1_001, CountBucket::OneThousandOneToTenThousand),
        (10_001, CountBucket::TenThousandOneToOneHundredThousand),
        (100_001, CountBucket::OneHundredThousandOneToOneMillion),
        (1_000_001, CountBucket::OverOneMillion),
    ] {
        assert_eq!(count_bucket(value), expected);
    }
    for (value, expected) in [
        (0, BytesBucket::Zero),
        (1, BytesBucket::UnderOneHundredKb),
        (102_400, BytesBucket::OneHundredKbToOneMb),
        (1_048_576, BytesBucket::OneToTenMb),
        (10_485_760, BytesBucket::TenToOneHundredMb),
        (104_857_600, BytesBucket::OneHundredMbToOneGb),
        (1_073_741_824, BytesBucket::OneToTwoGb),
        (2_147_483_648, BytesBucket::TwoToFiveGb),
        (5_368_709_120, BytesBucket::FiveToTenGb),
        (10_737_418_240, BytesBucket::TenToTwentyFiveGb),
        (26_843_545_600, BytesBucket::TwentyFiveToFiftyGb),
        (53_687_091_200, BytesBucket::FiftyToOneHundredGb),
        (75_161_927_680, BytesBucket::FiftyToOneHundredGb),
        (107_374_182_400, BytesBucket::OverOneHundredGb),
    ] {
        assert_eq!(bytes_bucket(value), expected);
    }
    assert_eq!(text_length_bucket(501), TextLengthBucket::OverFiveHundred);
    for (millis, expected) in [
        (0, DurationBucket::UnderOneHundredMs),
        (100, DurationBucket::UnderOneSecond),
        (1_000, DurationBucket::UnderFiveSeconds),
        (5_000, DurationBucket::UnderThirtySeconds),
        (30_000, DurationBucket::UnderTwoMinutes),
        (120_000, DurationBucket::UnderTenMinutes),
        (600_000, DurationBucket::UnderOneHour),
        (3_600_000, DurationBucket::AtLeastOneHour),
    ] {
        assert_eq!(duration_bucket(Duration::from_millis(millis)), expected);
    }
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
fn automatic_upgrade_event_uses_an_ephemeral_delivery_id() {
    let event = PublicEventV1::OperationCompleted(OperationCompletedV1 {
        payload: OperationPayloadV1::Cli(ClientOperationV1::Upgrade(UpgradeTelemetry {
            mode: UpgradeMode::Auto,
            operation: UpgradeOperation::Apply,
            dry_run: false,
            suppress_event: false,
            status: Some(UpgradeStatus::Applied),
            applied: Some(true),
            scheduled: Some(false),
            update_available: Some(false),
            update_was_available: Some(true),
            upgrade_attempt_id: Some("ua_replacement".to_owned()),
            managed_install: Some(true),
            self_upgrade_allowed: Some(true),
            auto_upgrade_allowed: Some(true),
            warning_count: Some(CountBucket::Zero),
            channel: Some(UpgradeChannel::Stable),
            failure_kind: None,
        })),
        output: Some(OutputKind::Human),
        outcome: Outcome::Success,
        duration: duration_bucket(Duration::ZERO),
        deprecated_daemon_control: false,
        deprecated_upgrade_control: false,
    });
    let occurred_at = chrono::DateTime::parse_from_rfc3339("2026-07-22T12:34:00Z")
        .unwrap()
        .with_timezone(&chrono::Utc);

    let serialized = serialize_event(&event, occurred_at, None, None);

    assert_eq!(
        Uuid::parse_str(serialized["event_id"].as_str().unwrap())
            .unwrap()
            .get_version_num(),
        4
    );
    assert_eq!(serialized["properties"]["upgrade_mode"], "auto");
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
                    provider: Some(CaptureProvider::Codex),
                    trigger: ProviderRefreshTrigger::Search,
                    source_mode: Some(ProviderRefreshSourceMode::Discovered),
                    change: ProviderRefreshChange::Changed,
                    content_evidence: ProviderRefreshContentEvidence::Accepted,
                    work_kind: Some(ProviderRefreshWorkKind::Append),
                    refresh_result: ProviderRefreshResult::Complete,
                    core_result: ProviderCoreResult::Complete,
                    canonical_pro_result: ProviderProResult::NoOp,
                    output_pro_result: ProviderProResult::Complete,
                    failure_scope: ProviderRefreshFailureScope::None,
                    failure_type: ProviderRefreshFailureType::None,
                    work_remaining: false,
                    retired_records: Some(count_bucket(0)),
                    counts: Some(ProviderRefreshCountsV1::new(1, 12, 3, 8, 0, 0, 0, 0, 2048)),
                    performance: Some(ProviderRefreshPerformanceV1::new(
                        Duration::from_millis(800),
                        Some(512 * 1024 * 1024),
                    )),
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

#[test]
fn selected_telemetry_contract_inventory_hashes_match_the_running_public_source() {
    use sha2::{Digest, Sha256};

    let provenance: serde_json::Value = serde_json::from_str(include_str!(
        "../../../../contracts/telemetry-v1/source-provenance.json"
    ))
    .unwrap();
    assert_eq!(provenance["repository"], "ctxrs/ctx");
    assert_eq!(
        provenance["base_commit"],
        "985f06e8e14cbdb6cb19106ec5ad658a305f7eaa"
    );
    assert_eq!(provenance["provenance_kind"], "content_addressed_candidate");
    assert_eq!(
        provenance["scope"],
        "selected_typed_telemetry_contract_inventory"
    );
    assert!(
        provenance.get("state").is_none(),
        "content provenance must not claim a transient worktree state"
    );
    let files = provenance["files"].as_object().unwrap();
    let sources = [
        (
            "crates/ctx-cli/src/analytics/operation.rs",
            include_bytes!("operation.rs").as_slice(),
        ),
        (
            "crates/ctx-cli/src/analytics/daemon.rs",
            include_bytes!("daemon.rs").as_slice(),
        ),
        (
            "crates/ctx-cli/src/analytics/mcp.rs",
            include_bytes!("mcp.rs").as_slice(),
        ),
        (
            "crates/ctx-cli/src/analytics/runtime.rs",
            include_bytes!("runtime.rs").as_slice(),
        ),
        (
            "crates/ctx-cli/src/analytics/pro.rs",
            include_bytes!("pro.rs").as_slice(),
        ),
        (
            "crates/ctx-cli/src/analytics/buckets.rs",
            include_bytes!("buckets.rs").as_slice(),
        ),
        (
            "crates/ctx-cli/src/analytics/provider.rs",
            include_bytes!("provider.rs").as_slice(),
        ),
        (
            "crates/ctx-cli/src/analytics/product.rs",
            include_bytes!("product.rs").as_slice(),
        ),
        (
            "crates/ctx-cli/src/analytics/sender.rs",
            include_bytes!("sender.rs").as_slice(),
        ),
        (
            "crates/ctx-cli/src/upgrade/command.rs",
            include_bytes!("../upgrade/command.rs").as_slice(),
        ),
        (
            "crates/ctx-cli/src/upgrade/command/daemon.rs",
            include_bytes!("../upgrade/command/daemon.rs").as_slice(),
        ),
        (
            "crates/ctx-cli/src/upgrade/state.rs",
            include_bytes!("../upgrade/state.rs").as_slice(),
        ),
    ];
    assert_eq!(files.len(), sources.len());
    for (path, source) in sources {
        let digest = Sha256::digest(source)
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        assert_eq!(files[path], digest, "stale telemetry provenance for {path}");
    }
}
