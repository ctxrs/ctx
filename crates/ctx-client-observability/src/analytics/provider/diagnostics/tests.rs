use super::super::*;
use crate::analytics::{sender::serialize_event, PublicEventV1};

fn event() -> ProviderRefreshCompletedV1 {
    ProviderRefreshCompletedV1::bucketed(
        Surface::Daemon,
        Outcome::Failure,
        DurationBucket::Unknown,
        ForegroundProviderRefreshV1 {
            provider: None,
            trigger: ProviderRefreshTrigger::Daemon,
            change: ProviderRefreshChange::NoOp,
            refresh_result: ProviderRefreshResult::Failure,
            core_result: ProviderCoreResult::Failure,
            failure_scope: ProviderRefreshFailureScope::System,
            failure_type: ProviderRefreshFailureType::System,
            failure_code: ProviderRefreshFailureCode::AllProviderTerminalCoverageUnavailable,
            retryable: true,
            work_remaining: true,
            counts: None,
        },
    )
    .with_failure_diagnostic(Some((
        ProviderRefreshFailureStage::Execution,
        ProviderRefreshFailureKind::Provider,
    )))
}

#[test]
fn failure_reason_wire_matrix_is_closed_and_contextual() {
    for reason in [
        ProviderRefreshFailureReason::IoNotFound,
        ProviderRefreshFailureReason::IoPermissionDenied,
        ProviderRefreshFailureReason::IoStorageFull,
        ProviderRefreshFailureReason::IoReadOnlyFilesystem,
        ProviderRefreshFailureReason::IoOutOfMemory,
        ProviderRefreshFailureReason::IoTimedOut,
        ProviderRefreshFailureReason::RouteOutputLimit,
        ProviderRefreshFailureReason::RouteScratchLimit,
        ProviderRefreshFailureReason::IndexMemoryLimit,
        ProviderRefreshFailureReason::IndexScratchLimit,
        ProviderRefreshFailureReason::IndexWriterInvariant,
    ] {
        for kind in [
            ProviderRefreshFailureKind::Io,
            ProviderRefreshFailureKind::Index,
            ProviderRefreshFailureKind::Provider,
            ProviderRefreshFailureKind::Unknown,
        ] {
            for invalid in ["none", "success", "partial", "cli", "mcp", "legacy", "pair"] {
                let mut event = event().with_failure_reason(Some(reason));
                event.failure_diagnostic = Some((ProviderRefreshFailureStage::Execution, kind));
                match invalid {
                    "success" => event.outcome = Outcome::Success,
                    "partial" => {
                        event.foreground.as_mut().unwrap().refresh_result =
                            ProviderRefreshResult::Partial
                    }
                    "cli" => event.surface = Surface::Cli,
                    "mcp" => event.surface = Surface::Mcp,
                    "legacy" => event.foreground = None,
                    "pair" => event.failure_diagnostic = None,
                    _ => {}
                }
                let wire = properties(event);
                assert_eq!(
                    wire.get("refresh_failure_reason")
                        .and_then(serde_json::Value::as_str),
                    (invalid == "none" && reason.permits(kind)).then_some(reason.as_str())
                );
            }
        }
    }
    for text in ["future", "/private/token", "permission denied", ""] {
        assert!(ProviderRefreshFailureReason::parse(text).is_none());
    }
}

#[test]
fn failed_refresh_reason_matches_the_public_fixture() {
    let mut diagnosed =
        event().with_failure_reason(Some(ProviderRefreshFailureReason::IndexWriterInvariant));
    diagnosed.foreground.as_mut().unwrap().failure_code =
        ProviderRefreshFailureCode::SourceRefreshFailed;
    diagnosed.failure_diagnostic = Some((
        ProviderRefreshFailureStage::Execution,
        ProviderRefreshFailureKind::Index,
    ));
    let fixture: serde_json::Value = serde_json::from_str(include_str!(
        "../../../../../../contracts/telemetry-v1/fixtures/provider_refresh_reason.valid.json"
    ))
    .unwrap();
    assert_eq!(properties(diagnosed), fixture["properties"]);
}

fn properties(event: ProviderRefreshCompletedV1) -> serde_json::Value {
    let at = chrono::DateTime::parse_from_rfc3339("2026-07-22T12:34:00Z")
        .unwrap()
        .with_timezone(&chrono::Utc);
    serialize_event(
        &PublicEventV1::ProviderRefreshCompleted(event),
        at,
        None,
        None,
    )["properties"]
        .clone()
}

#[test]
fn coverage_reason_serializes_only_in_its_typed_failure_context() {
    for (reason, wire) in [
        (
            ProviderRefreshCoverageReason::CatalogUnavailable,
            "catalog_unavailable",
        ),
        (ProviderRefreshCoverageReason::UnsafeRoot, "unsafe_root"),
        (
            ProviderRefreshCoverageReason::MissingTerminalAuthority,
            "missing_terminal_authority",
        ),
        (ProviderRefreshCoverageReason::RouteFailed, "route_failed"),
        (
            ProviderRefreshCoverageReason::InvalidRouteIdentity,
            "invalid_route_identity",
        ),
        (
            ProviderRefreshCoverageReason::MissingEmptyAuthority,
            "missing_empty_authority",
        ),
    ] {
        for invalid in [
            "none", "legacy", "code", "kind", "pair", "cli", "mcp", "success",
        ] {
            let mut event = event().with_coverage_reason(Some(reason));
            match invalid {
                "legacy" => event.foreground = None,
                "code" => {
                    event.foreground.as_mut().unwrap().failure_code =
                        ProviderRefreshFailureCode::SourceRefreshFailed
                }
                "kind" => {
                    event.failure_diagnostic = Some((
                        ProviderRefreshFailureStage::Execution,
                        ProviderRefreshFailureKind::Io,
                    ))
                }
                "pair" => event.failure_diagnostic = None,
                "cli" => event.surface = Surface::Cli,
                "mcp" => event.surface = Surface::Mcp,
                "success" => event.outcome = Outcome::Success,
                _ => {}
            }
            let props = properties(event);
            if invalid == "none" {
                assert_eq!(props["refresh_coverage_reason"], wire);
            } else {
                assert!(props.get("refresh_coverage_reason").is_none(), "{invalid}");
            }
        }
    }
    assert!(properties(event()).get("refresh_coverage_reason").is_none());
}

#[test]
fn source_class_serializes_only_for_successful_daemon_source_partial() {
    for (class, wire) in [
        (
            ProviderRefreshSourceFailureClass::Unavailable,
            "unavailable",
        ),
        (
            ProviderRefreshSourceFailureClass::SourceChanged,
            "source_changed",
        ),
        (ProviderRefreshSourceFailureClass::Unreadable, "unreadable"),
        (
            ProviderRefreshSourceFailureClass::Incompatible,
            "incompatible",
        ),
        (ProviderRefreshSourceFailureClass::Mixed, "mixed"),
    ] {
        for context in [
            "source", "mixed", "legacy", "record", "complete", "failure", "cli", "mcp",
        ] {
            let mut event = event().with_source_failure_class(Some(class));
            event.outcome = Outcome::Success;
            let facts = event.foreground.as_mut().unwrap();
            facts.refresh_result = ProviderRefreshResult::Partial;
            facts.core_result = ProviderCoreResult::NoOp;
            facts.failure_code = ProviderRefreshFailureCode::None;
            facts.failure_type = ProviderRefreshFailureType::Unknown;
            facts.failure_scope = ProviderRefreshFailureScope::Source;
            match context {
                "mixed" => facts.failure_scope = ProviderRefreshFailureScope::Mixed,
                "record" => facts.failure_scope = ProviderRefreshFailureScope::Record,
                "complete" => facts.refresh_result = ProviderRefreshResult::Complete,
                "failure" => event.outcome = Outcome::Failure,
                "cli" => event.surface = Surface::Cli,
                "mcp" => event.surface = Surface::Mcp,
                "legacy" => event.foreground = None,
                _ => {}
            }
            let props = properties(event);
            if matches!(context, "source" | "mixed") {
                assert_eq!(props["refresh_source_failure_class"], wire);
            } else {
                assert!(
                    props.get("refresh_source_failure_class").is_none(),
                    "{context}"
                );
            }
            assert!(props.get("refresh_coverage_reason").is_none());
        }
    }
    assert!(properties(event())
        .get("refresh_source_failure_class")
        .is_none());
    assert!(ProviderRefreshSourceFailureClass::parse("/private/history token=secret").is_none());
}
