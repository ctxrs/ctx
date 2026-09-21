use super::*;
use crate::analytics::{
    sender::serialize_event, ClientOperationDraft, DocTopicId, DocsOperation, DocsTelemetry,
    McpErrorClassV1, McpErrorLayerV1, McpResponseBoundV1, McpResultMetadataV1,
    OperationCompletedV1, Outcome, PublicEventV1,
};
use crate::operation_descriptor::{
    CliOperation, LocalUsageOperation, McpOperation, ObservedMcpProductOperation,
    OperationDescriptor,
};

fn completed(state: BlameResultState, target_kind: BlameTargetKind) -> BlameTerminalFacts {
    BlameTerminalFacts {
        target_kind,
        request_kind: Some(BlameRequestKind::FirstRequest),
        query_duration: Some(Duration::from_millis(350)),
        result: Some(BlameResultFacts {
            state,
            evaluated: 27,
            freshness: BlameFreshness::Current,
            has_more: false,
        }),
        failure: None,
        output_served: Some(true),
    }
}

#[test]
fn blame_terminals_match_public_worker_fixtures() {
    let fixtures: Value = serde_json::from_str(include_str!(
        "../../../../../contracts/telemetry-v1/fixtures/blame_operation_completed.valid.json"
    ))
    .unwrap();
    let at = chrono::DateTime::parse_from_rfc3339("2026-09-20T12:34:00Z")
        .unwrap()
        .with_timezone(&chrono::Utc);
    let mut cases = vec![
        (
            "proven",
            completed(BlameResultState::Proven, BlameTargetKind::File),
            true,
        ),
        (
            "possible",
            completed(BlameResultState::Possible, BlameTargetKind::Commit),
            true,
        ),
        (
            "conflicting",
            completed(BlameResultState::Conflicting, BlameTargetKind::PullRequest),
            true,
        ),
        (
            "none",
            completed(BlameResultState::None, BlameTargetKind::File),
            true,
        ),
    ];
    let mut continuation = completed(BlameResultState::Proven, BlameTargetKind::File);
    continuation.request_kind = Some(BlameRequestKind::Continuation);
    continuation.result.as_mut().unwrap().freshness = BlameFreshness::StaleCommitted;
    continuation.result.as_mut().unwrap().has_more = true;
    cases.push(("continuation", continuation, true));
    let mut setup_failure = BlameTerminalFacts::new(BlameTargetKind::Commit);
    setup_failure.failure = Some(BlameFailure {
        class: BlameFailureClass::InvalidRequest,
        phase: BlameFailurePhase::Setup,
    });
    setup_failure.output_served = Some(false);
    cases.push(("setup_failure", setup_failure, false));
    let mut output_failure = completed(BlameResultState::Proven, BlameTargetKind::File);
    output_failure.failure = Some(BlameFailure {
        class: BlameFailureClass::Output,
        phase: BlameFailurePhase::Output,
    });
    output_failure.output_served = Some(false);
    cases.push(("output_failure", output_failure, false));

    for (name, facts, success) in cases {
        for surface in ["cli", "mcp"] {
            let event = if surface == "cli" {
                let mut draft = ClientOperationDraft::from_descriptor(
                    OperationDescriptor::Cli(CliOperation::Blame(BlameTerminalFacts::new(
                        facts.target_kind,
                    ))),
                    true,
                )
                .unwrap();
                *draft.blame_mut() = facts;
                draft.finish(success, Duration::from_secs(2))
            } else {
                let mut operation = McpOperation::tool_call(ObservedMcpProductOperation::Blame)
                    .with_result(McpResultMetadataV1 {
                        blame: Some(facts),
                        response_bound: Some(McpResponseBoundV1::WithinLimit),
                        ..Default::default()
                    });
                if !success {
                    operation = if name == "output_failure" {
                        operation
                            .with_error(McpErrorLayerV1::Response, McpErrorClassV1::ResponseFlush)
                    } else {
                        operation.with_error(McpErrorLayerV1::Tool, McpErrorClassV1::ToolFailure)
                    };
                }
                PublicEventV1::OperationCompleted(OperationCompletedV1::for_mcp(
                    operation,
                    if success {
                        Outcome::Success
                    } else {
                        Outcome::Failure
                    },
                    Duration::from_secs(2),
                ))
            };
            let encoded = serialize_event(&event, at, None, None);
            let key = format!("{surface}_{name}");
            let mut expected = fixtures[&key].clone();
            expected["event_id"] = encoded["event_id"].clone();
            assert_eq!(encoded, expected, "{key}");
        }
    }
}

#[test]
fn unobserved_query_and_output_are_absent_and_local_usage_is_reused() {
    let facts = BlameTerminalFacts::new(BlameTargetKind::PullRequest);
    let mut properties = Map::new();
    facts.insert_properties(&mut properties);
    assert_eq!(
        properties,
        json!({"blame_target_kind": "pull_request"})
            .as_object()
            .unwrap()
            .clone()
    );
    assert_eq!(
        CliOperation::Blame(facts).local_usage_operation(),
        Some(LocalUsageOperation::Blame)
    );
    assert_eq!(
        ObservedMcpProductOperation::Blame.local_usage_operation(),
        LocalUsageOperation::Blame
    );
}

#[test]
fn blame_docs_topic_is_closed_and_matches_the_worker_fixture() {
    assert_eq!(DocTopicId::from_known_id("blame"), Some(DocTopicId::Blame));
    assert!(DocTopicId::from_known_id("blame/private-path").is_none());
    let event = ClientOperationDraft::from_descriptor(
        OperationDescriptor::Cli(CliOperation::Docs(DocsTelemetry {
            operation: Some(DocsOperation::Show),
            topic: Some(DocTopicId::Blame),
            ..Default::default()
        })),
        false,
    )
    .unwrap()
    .finish(true, Duration::ZERO);
    let at = chrono::DateTime::parse_from_rfc3339("2026-09-20T12:34:00Z")
        .unwrap()
        .with_timezone(&chrono::Utc);
    let encoded = serialize_event(&event, at, None, None);
    let mut expected: Value = serde_json::from_str(include_str!(
        "../../../../../contracts/telemetry-v1/fixtures/blame_docs_operation_completed.valid.json"
    ))
    .unwrap();
    expected["event_id"] = encoded["event_id"].clone();
    assert_eq!(encoded, expected);
}
