use super::*;

#[cfg(unix)]
use std::{
    fs,
    os::unix::fs::PermissionsExt as _,
    path::{Path, PathBuf},
};

#[cfg(unix)]
use crate::pro::authorization::AuthorizationProvider;

fn finish_for(index: &VerifiedIndex, revision: &str) -> FinishCoreMaterializationRequest {
    let sources = core_source_states(index.manifest()).unwrap();
    let head = core_generation_head(index, &sources).unwrap();
    let begin = BeginCoreMaterializationRequest {
        head: head.clone(),
        expected_prior_receipt: None,
    };
    FinishCoreMaterializationRequest {
        materialization_id: ctx_pro_host_protocol::core_materialization_id(&begin, revision)
            .unwrap(),
        head,
        expected_prior_receipt: None,
        source_delta_pages: 1,
        changed_sources: 1,
        removed_sources: 0,
        event_delta_pages: 1,
        event_mutations: 1,
    }
}

fn progress_for(
    index: &VerifiedIndex,
    phase: CoreMaterializationFinalizationPhase,
    cursor: char,
    revision: &str,
) -> CoreMaterializationFinalizationProgress {
    let finish = finish_for(index, revision);
    CoreMaterializationFinalizationProgress {
        materialization_id: finish.materialization_id.clone(),
        core_generation_id: index.generation_id().to_owned(),
        finish_request_digest: finish.canonical_digest().unwrap(),
        materializer_revision: revision.to_owned(),
        phase,
        cursor_sha256: cursor.to_string().repeat(64),
    }
}

#[test]
fn finish_target_lease_is_durable_before_the_finalization_exchange() {
    let data_root = tempdir().unwrap();
    let index_root = ctx_history_refresh::source_backed_index_root(data_root.path());
    let source = source("finalization-crash-ordering.jsonl");
    let mut writer = GenerationWriter::open(&index_root, WriterOptions::default())
        .unwrap()
        .into_writer()
        .unwrap();
    add_source(&mut writer, &source, 1, vec!["body".to_owned()]);
    writer.commit(|_| true).unwrap();
    let index = VerifiedIndex::open_pinned(&index_root).unwrap();
    let finish = finish_for(&index, "test-core-materializer-v1");

    let acquired =
        acquire_finish_generation_lease(data_root.path(), &finish, "test-core-materializer-v1")
            .unwrap();

    // `ProtocolCoreMaterializationConsumer::finish` performs this acquisition
    // before calling exchange. A crash or lost response after this point can
    // therefore leave at most a safe extra hold on the exact target.
    let durable = core_finalization_generation_lease(data_root.path())
        .unwrap()
        .unwrap();
    assert_eq!(durable, acquired);
    assert_eq!(durable.generation_id(), index.generation_id());
}

#[test]
fn continuation_completion_rejects_changed_finish_digest_and_revision() {
    let (_temp, index) =
        single_source_index("finalization-terminal-cas.jsonl", vec!["body".to_owned()]);
    let expected = progress_for(
        &index,
        CoreMaterializationFinalizationPhase::ReadyToActivate,
        '9',
        "test-core-materializer-v1",
    );

    for changed_revision in [false, true] {
        let mut consumer = Consumer::new();
        consumer.finalization_progress = Some(expected.clone());
        consumer.finish = Some(finish_for(&index, "test-core-materializer-v1"));
        if changed_revision {
            consumer.revision = "test-core-materializer-v2".to_owned();
        } else {
            consumer.terminal_finish_digest_override = Some("f".repeat(64));
        }
        let status = consumer
            .status(StatusRequest {
                requested_core_generation_id: Some(index.generation_id().to_owned()),
            })
            .unwrap();
        let error = continue_core_finalization(
            &index,
            &status,
            &mut consumer,
            CoreWorkerLaunchSelection::explicit_test(1),
        )
        .unwrap_err();
        assert!(
            error.to_string().contains("continuation CAS"),
            "unexpected terminal CAS error: {error:#}"
        );
    }
}

fn pending(
    progress: CoreMaterializationFinalizationProgress,
    replayed: bool,
) -> CoreMaterializationFinalizationPending {
    CoreMaterializationFinalizationPending { progress, replayed }
}

fn advanced_pending(
    progress: &CoreMaterializationFinalizationProgress,
    cursor: u64,
) -> CoreMaterializationFinalizationPending {
    pending(
        CoreMaterializationFinalizationProgress {
            cursor_sha256: format!("{cursor:064x}"),
            ..progress.clone()
        },
        false,
    )
}

#[test]
fn finish_yields_once_then_one_restart_session_chains_pending_to_completion() {
    let (_temp, index) = single_source_index("finalization.jsonl", vec!["body".to_owned()]);
    let mut consumer = Consumer::new();
    let first = pending(
        progress_for(
            &index,
            CoreMaterializationFinalizationPhase::SealingInputs,
            '1',
            &consumer.revision,
        ),
        false,
    );
    consumer.finish_pending = Some(first.clone());

    let CoreMaterializationSyncProgress::FinalizationPending(observed) =
        sync_core_feed_progress(&index, None, &mut consumer).unwrap()
    else {
        panic!("Finish should yield durable finalization progress");
    };
    assert_eq!(observed, first);
    assert_eq!(consumer.finish_requests.len(), 1);
    assert!(consumer.continue_requests.is_empty());
    let source_exchanges = consumer.source_exchanges;
    let state_exchanges = consumer.state_exchanges;
    let event_exchanges = consumer.event_exchanges;

    let second = pending(
        CoreMaterializationFinalizationProgress {
            phase: CoreMaterializationFinalizationPhase::EmitReplay,
            cursor_sha256: "2".repeat(64),
            ..first.progress.clone()
        },
        true,
    );
    consumer.continue_pending.push_back(second.clone());
    let status = consumer
        .status(StatusRequest {
            requested_core_generation_id: Some(index.generation_id().to_owned()),
        })
        .unwrap();
    status.validate().unwrap();
    let CoreMaterializationSyncProgress::Finished(report) = continue_core_finalization(
        &index,
        &status,
        &mut consumer,
        CoreWorkerLaunchSelection::explicit_test(1),
    )
    .unwrap() else {
        panic!("one continuation session should reach the terminal receipt");
    };
    assert_eq!(report.receipt.core_generation_id, index.generation_id());
    assert_eq!(consumer.continue_requests.len(), 2);
    assert_eq!(
        consumer.continue_requests[0].expected_progress,
        first.progress
    );
    assert_eq!(
        consumer.continue_requests[1].expected_progress,
        second.progress
    );
    assert_eq!(consumer.source_exchanges, source_exchanges);
    assert_eq!(consumer.state_exchanges, state_exchanges);
    assert_eq!(consumer.event_exchanges, event_exchanges);
}

#[test]
fn continuation_chains_only_validated_nonstale_ordered_matching_cursors() {
    let (_temp, index) =
        single_source_index("finalization-conflict.jsonl", vec!["body".to_owned()]);
    let expected = progress_for(
        &index,
        CoreMaterializationFinalizationPhase::EmitFlat,
        '3',
        "test-core-materializer-v1",
    );

    for (case, response) in [
        ("stale", pending(expected.clone(), true)),
        (
            "reordered",
            pending(
                CoreMaterializationFinalizationProgress {
                    phase: CoreMaterializationFinalizationPhase::SealingInputs,
                    cursor_sha256: "4".repeat(64),
                    ..expected.clone()
                },
                false,
            ),
        ),
        (
            "conflicting",
            pending(
                CoreMaterializationFinalizationProgress {
                    materialization_id: "f".repeat(64),
                    phase: CoreMaterializationFinalizationPhase::EmitEventIndex,
                    cursor_sha256: "5".repeat(64),
                    ..expected.clone()
                },
                false,
            ),
        ),
    ] {
        let mut consumer = Consumer::new();
        consumer.finalization_progress = Some(expected.clone());
        consumer.continue_pending.push_back(response);
        let status = consumer
            .status(StatusRequest {
                requested_core_generation_id: Some(index.generation_id().to_owned()),
            })
            .unwrap();
        let error = continue_core_finalization(
            &index,
            &status,
            &mut consumer,
            CoreWorkerLaunchSelection::explicit_test(1),
        )
        .unwrap_err();
        assert!(
            error.to_string().contains("did not advance"),
            "unexpected {case} cursor error: {error:#}"
        );
        assert_eq!(consumer.continue_requests.len(), 1);
    }
}

#[test]
fn production_continuation_burst_is_large_but_still_bounded() {
    assert_eq!(CoreFinalizationBurstLimits::PRODUCTION.max_requests, 256);
    assert_eq!(
        CoreFinalizationBurstLimits::PRODUCTION.max_elapsed,
        Duration::from_secs(30)
    );
    assert!(CoreFinalizationBurstLimits::PRODUCTION.max_elapsed < BATCH_TIMEOUT);
}

#[test]
fn continuation_burst_yields_truthfully_at_the_fixed_request_bound() {
    let (_temp, index) =
        single_source_index("finalization-request-bound.jsonl", vec!["body".to_owned()]);
    let initial = progress_for(
        &index,
        CoreMaterializationFinalizationPhase::EmitReplay,
        '0',
        "test-core-materializer-v1",
    );
    let mut consumer = Consumer::new();
    consumer.finalization_progress = Some(initial.clone());
    consumer.finish = Some(finish_for(&index, "test-core-materializer-v1"));
    for cursor in 1..=5 {
        consumer
            .continue_pending
            .push_back(advanced_pending(&initial, cursor));
    }
    let status = consumer
        .status(StatusRequest {
            requested_core_generation_id: Some(index.generation_id().to_owned()),
        })
        .unwrap();
    let started = Instant::now();
    let CoreMaterializationSyncProgress::FinalizationPending(observed) =
        continue_core_finalization_burst_with(
            &index,
            &status,
            &mut consumer,
            CoreWorkerLaunchSelection::explicit_test(1),
            CoreFinalizationBurstLimits {
                max_requests: 3,
                max_elapsed: Duration::from_secs(60),
            },
            None,
            || started,
            || Ok(0),
            || false,
        )
        .unwrap()
    else {
        panic!("request-bound burst must yield pending progress");
    };
    assert_eq!(observed, advanced_pending(&initial, 3));
    assert_eq!(consumer.finalization_progress, Some(observed.progress));
    assert_eq!(consumer.continue_requests.len(), 3);
    assert_eq!(consumer.continue_pending.len(), 2);
}

#[test]
fn continuation_burst_yields_truthfully_at_the_elapsed_bound() {
    let (_temp, index) =
        single_source_index("finalization-time-bound.jsonl", vec!["body".to_owned()]);
    let initial = progress_for(
        &index,
        CoreMaterializationFinalizationPhase::EmitFlat,
        '0',
        "test-core-materializer-v1",
    );
    let mut consumer = Consumer::new();
    consumer.finalization_progress = Some(initial.clone());
    consumer.finish = Some(finish_for(&index, "test-core-materializer-v1"));
    for cursor in 1..=4 {
        consumer
            .continue_pending
            .push_back(advanced_pending(&initial, cursor));
    }
    let status = consumer
        .status(StatusRequest {
            requested_core_generation_id: Some(index.generation_id().to_owned()),
        })
        .unwrap();
    let started = Instant::now();
    let mut observations = VecDeque::from([
        started,
        started + Duration::from_millis(25),
        started + Duration::from_millis(50),
        started + Duration::from_millis(100),
    ]);
    let CoreMaterializationSyncProgress::FinalizationPending(observed) =
        continue_core_finalization_burst_with(
            &index,
            &status,
            &mut consumer,
            CoreWorkerLaunchSelection::explicit_test(1),
            CoreFinalizationBurstLimits {
                max_requests: 4,
                max_elapsed: Duration::from_millis(100),
            },
            None,
            || observations.pop_front().unwrap(),
            || Ok(0),
            || false,
        )
        .unwrap()
    else {
        panic!("elapsed-bound burst must yield pending progress");
    };
    assert_eq!(observed, advanced_pending(&initial, 2));
    assert_eq!(consumer.finalization_progress, Some(observed.progress));
    assert_eq!(consumer.continue_requests.len(), 2);
    assert_eq!(consumer.continue_pending.len(), 2);
}

#[test]
fn continuation_burst_does_not_start_after_its_elapsed_budget() {
    let (_temp, index) = single_source_index(
        "finalization-time-expired-before-request.jsonl",
        vec!["body".to_owned()],
    );
    let initial = progress_for(
        &index,
        CoreMaterializationFinalizationPhase::EmitFlat,
        '0',
        "test-core-materializer-v1",
    );
    let mut consumer = Consumer::new();
    consumer.finalization_progress = Some(initial.clone());
    consumer.finish = Some(finish_for(&index, "test-core-materializer-v1"));
    consumer
        .continue_pending
        .push_back(advanced_pending(&initial, 1));
    let status = consumer
        .status(StatusRequest {
            requested_core_generation_id: Some(index.generation_id().to_owned()),
        })
        .unwrap();
    let started = Instant::now();
    let mut observations = VecDeque::from([started, started + Duration::from_millis(1)]);
    let CoreMaterializationSyncProgress::FinalizationPending(observed) =
        continue_core_finalization_burst_with(
            &index,
            &status,
            &mut consumer,
            CoreWorkerLaunchSelection::explicit_test(1),
            CoreFinalizationBurstLimits {
                max_requests: 4,
                max_elapsed: Duration::from_millis(1),
            },
            None,
            || observations.pop_front().unwrap(),
            || Ok(0),
            || false,
        )
        .unwrap()
    else {
        panic!("expired elapsed budget must preserve Pending status");
    };
    assert_eq!(observed.progress, initial);
    assert!(observed.replayed);
    assert!(consumer.continue_requests.is_empty());
    assert_eq!(consumer.continue_pending.len(), 1);
}

#[test]
fn continuation_burst_stops_before_another_exchange_when_cancelled() {
    let (_temp, index) =
        single_source_index("finalization-cancelled.jsonl", vec!["body".to_owned()]);
    let initial = progress_for(
        &index,
        CoreMaterializationFinalizationPhase::EmitEventIndex,
        '0',
        "test-core-materializer-v1",
    );
    let first = advanced_pending(&initial, 1);
    let mut consumer = Consumer::new();
    consumer.finalization_progress = Some(initial);
    consumer.finish = Some(finish_for(&index, "test-core-materializer-v1"));
    consumer.continue_pending.push_back(first.clone());
    consumer
        .continue_pending
        .push_back(advanced_pending(&first.progress, 2));
    let status = consumer
        .status(StatusRequest {
            requested_core_generation_id: Some(index.generation_id().to_owned()),
        })
        .unwrap();
    let started = Instant::now();
    let mut cancellation_checks = 0_usize;
    let error = continue_core_finalization_burst_with(
        &index,
        &status,
        &mut consumer,
        CoreWorkerLaunchSelection::explicit_test(1),
        CoreFinalizationBurstLimits::PRODUCTION,
        None,
        || started,
        || Ok(0),
        || {
            cancellation_checks = cancellation_checks.saturating_add(1);
            cancellation_checks > 1
        },
    )
    .unwrap_err();
    assert_eq!(
        error.to_string(),
        "helper_cancelled: Core finalization continuation burst cancelled"
    );
    assert_eq!(consumer.continue_requests.len(), 1);
    assert_eq!(consumer.finalization_progress, Some(first.progress));
    assert_eq!(consumer.continue_pending.len(), 1);
}

#[test]
fn continuation_burst_propagates_exchange_errors_without_losing_the_last_cursor() {
    for message in [
        "helper_cancelled: synthetic cancellation",
        "helper_crashed: synthetic exchange failure",
    ] {
        let (_temp, index) = single_source_index(message, vec!["body".to_owned()]);
        let initial = progress_for(
            &index,
            CoreMaterializationFinalizationPhase::EmitEventIndex,
            '0',
            "test-core-materializer-v1",
        );
        let first = advanced_pending(&initial, 1);
        let mut consumer = Consumer::new();
        consumer.finalization_progress = Some(initial.clone());
        consumer.finish = Some(finish_for(&index, "test-core-materializer-v1"));
        consumer.continue_pending.push_back(first.clone());
        consumer
            .continue_pending
            .push_back(advanced_pending(&initial, 2));
        consumer.continue_error_after = Some((2, message.to_owned()));
        let status = consumer
            .status(StatusRequest {
                requested_core_generation_id: Some(index.generation_id().to_owned()),
            })
            .unwrap();
        let started = Instant::now();
        let error = continue_core_finalization_burst_with(
            &index,
            &status,
            &mut consumer,
            CoreWorkerLaunchSelection::explicit_test(1),
            CoreFinalizationBurstLimits {
                max_requests: 4,
                max_elapsed: Duration::from_secs(60),
            },
            None,
            || started,
            || Ok(0),
            || false,
        )
        .unwrap_err();
        assert_eq!(error.to_string(), message);
        assert_eq!(consumer.continue_requests.len(), 2);
        assert_eq!(consumer.finalization_progress, Some(first.progress));
        assert_eq!(consumer.continue_pending.len(), 1);
    }
}

#[test]
fn continuation_burst_never_starts_after_the_authenticated_final_deadline() {
    let (_temp, index) = single_source_index("finalization-expired.jsonl", vec!["body".to_owned()]);
    let initial = progress_for(
        &index,
        CoreMaterializationFinalizationPhase::EmitReplay,
        '0',
        "test-core-materializer-v1",
    );
    let mut consumer = Consumer::new();
    consumer.finalization_progress = Some(initial.clone());
    consumer.finish = Some(finish_for(&index, "test-core-materializer-v1"));
    consumer
        .continue_pending
        .push_back(advanced_pending(&initial, 1));
    let status = consumer
        .status(StatusRequest {
            requested_core_generation_id: Some(index.generation_id().to_owned()),
        })
        .unwrap();
    let started = Instant::now();
    let error = continue_core_finalization_burst_with(
        &index,
        &status,
        &mut consumer,
        CoreWorkerLaunchSelection::explicit_test(1),
        CoreFinalizationBurstLimits::PRODUCTION,
        Some(100),
        || started,
        || Ok(101),
        || false,
    )
    .unwrap_err();
    assert!(error.to_string().contains("entitlement_expired"));
    assert!(consumer.continue_requests.is_empty());
    assert_eq!(consumer.finalization_progress, Some(initial));
    assert_eq!(consumer.continue_pending.len(), 1);
}

#[test]
fn continuation_burst_stops_when_the_final_deadline_elapses_between_exchanges() {
    let (_temp, index) = single_source_index(
        "finalization-deadline-between-exchanges.jsonl",
        vec!["body".to_owned()],
    );
    let initial = progress_for(
        &index,
        CoreMaterializationFinalizationPhase::EmitReplay,
        '0',
        "test-core-materializer-v1",
    );
    let first = advanced_pending(&initial, 1);
    let mut consumer = Consumer::new();
    consumer.finalization_progress = Some(initial);
    consumer.finish = Some(finish_for(&index, "test-core-materializer-v1"));
    consumer.continue_pending.push_back(first.clone());
    consumer
        .continue_pending
        .push_back(advanced_pending(&first.progress, 2));
    let status = consumer
        .status(StatusRequest {
            requested_core_generation_id: Some(index.generation_id().to_owned()),
        })
        .unwrap();
    let started = Instant::now();
    let mut unix_observations = VecDeque::from([100, 101]);
    let error = continue_core_finalization_burst_with(
        &index,
        &status,
        &mut consumer,
        CoreWorkerLaunchSelection::explicit_test(1),
        CoreFinalizationBurstLimits::PRODUCTION,
        Some(100),
        || started,
        || Ok(unix_observations.pop_front().unwrap()),
        || false,
    )
    .unwrap_err();
    assert!(error.to_string().contains("entitlement_expired"));
    assert_eq!(consumer.continue_requests.len(), 1);
    assert_eq!(consumer.finalization_progress, Some(first.progress));
    assert_eq!(consumer.continue_pending.len(), 1);
}

#[test]
fn lost_continue_response_restarts_from_the_helpers_durable_cursor() {
    let (_temp, index) =
        single_source_index("finalization-lost-response.jsonl", vec!["body".to_owned()]);
    let initial = progress_for(
        &index,
        CoreMaterializationFinalizationPhase::EmitSources,
        '0',
        "test-core-materializer-v1",
    );
    let committed = advanced_pending(&initial, 1);
    let finish = finish_for(&index, "test-core-materializer-v1");
    let mut first_session = Consumer::new();
    first_session.finalization_progress = Some(initial.clone());
    first_session.finish = Some(finish.clone());
    first_session.continue_pending.push_back(committed.clone());
    first_session.continue_response_loss_after = Some(1);
    let status = first_session
        .status(StatusRequest {
            requested_core_generation_id: Some(index.generation_id().to_owned()),
        })
        .unwrap();
    let error = continue_core_finalization(
        &index,
        &status,
        &mut first_session,
        CoreWorkerLaunchSelection::explicit_test(1),
    )
    .unwrap_err();
    assert!(error.to_string().contains("response_lost"));
    assert_eq!(first_session.continue_requests.len(), 1);
    assert_eq!(
        first_session.continue_requests[0].expected_progress,
        initial
    );
    assert_eq!(
        first_session.finalization_progress,
        Some(committed.progress.clone())
    );

    let resumed = advanced_pending(&committed.progress, 2);
    let mut restarted_session = Consumer::new();
    restarted_session.finalization_progress = Some(committed.progress.clone());
    restarted_session.finish = Some(finish);
    restarted_session
        .continue_pending
        .push_back(resumed.clone());
    let status = restarted_session
        .status(StatusRequest {
            requested_core_generation_id: Some(index.generation_id().to_owned()),
        })
        .unwrap();
    let started = Instant::now();
    let CoreMaterializationSyncProgress::FinalizationPending(observed) =
        continue_core_finalization_burst_with(
            &index,
            &status,
            &mut restarted_session,
            CoreWorkerLaunchSelection::explicit_test(1),
            CoreFinalizationBurstLimits {
                max_requests: 1,
                max_elapsed: Duration::from_secs(60),
            },
            None,
            || started,
            || Ok(0),
            || false,
        )
        .unwrap()
    else {
        panic!("resumed session should accept the next durable cursor");
    };
    assert_eq!(observed, resumed);
    assert_eq!(restarted_session.continue_requests.len(), 1);
    assert_eq!(
        restarted_session.continue_requests[0].expected_progress,
        committed.progress
    );
}

#[cfg(unix)]
struct FixtureAuthorizationProvider;

#[cfg(unix)]
impl AuthorizationProvider for FixtureAuthorizationProvider {
    fn authorization_for_challenge(
        &self,
        challenge: &[u8; ctx_pro_host_protocol::AUTHORIZATION_CHALLENGE_BYTES],
    ) -> Result<ctx_pro_host_protocol::AuthorizationRequest> {
        assert_eq!(
            challenge,
            &[0_u8; ctx_pro_host_protocol::AUTHORIZATION_CHALLENGE_BYTES]
        );
        Ok(serde_json::from_value(serde_json::json!({
            "entitlement": {
                "grant": {
                    "schema_version": 1,
                    "issuer": "https://fixture.invalid",
                    "key_id": "fixture-key",
                    "grant_id": "fixture-grant",
                    "subject": "fixture-user",
                    "account_id": "fixture-account",
                    "product": "ctx-pro",
                    "access_kind": "active",
                    "installation_key_thumbprint": "a".repeat(64),
                    "issued_at_unix": 1,
                    "not_before_unix": 1,
                    "refresh_after_unix": i64::MAX,
                    "access_deadline_unix": i64::MAX,
                    "grace_deadline_unix": i64::MAX,
                    "expires_at_unix": i64::MAX,
                    "minimum_helper_protocol": ctx_pro_host_protocol::PROTOCOL_VERSION,
                    "revocation_epoch": 0,
                    "capabilities": ["graph_write"]
                },
                "signature_base64url": ctx_pro_host_protocol::base64url(&[1_u8; 64])
            },
            "installation_public_key_base64url": ctx_pro_host_protocol::base64url(&[2_u8; 32]),
            "challenge_base64url": ctx_pro_host_protocol::base64url(challenge),
            "proof_signature_base64url": ctx_pro_host_protocol::base64url(&[3_u8; 64])
        }))?)
    }
}

#[cfg(unix)]
fn python3_interpreter() -> PathBuf {
    [
        "/usr/bin/python3",
        "/usr/local/bin/python3",
        "/usr/local/bin/python3.14",
        "/usr/local/bin/python3.13",
        "/opt/homebrew/bin/python3",
    ]
    .into_iter()
    .filter_map(|candidate| fs::canonicalize(candidate).ok())
    .find(|candidate| {
        fs::metadata(candidate)
            .is_ok_and(|metadata| metadata.is_file() && metadata.permissions().mode() & 0o111 != 0)
    })
    .expect("authenticated Pro process test requires an executable Python 3")
}

#[cfg(unix)]
fn write_authenticated_finalization_helper(
    path: &Path,
    state_path: &Path,
    log_path: &Path,
    terminal_after: usize,
    receipt: &CoreMaterializationReceipt,
) {
    const HELPER: &str = r#"
import json, os, pathlib, struct, sys

STATE = pathlib.Path(__STATE_PATH__)
LOG = pathlib.Path(__LOG_PATH__)
TERMINAL_AFTER = __TERMINAL_AFTER__
RECEIPT = __RECEIPT__
FINGERPRINT = __PROTOCOL_FINGERPRINT__

def log(message):
    with LOG.open('a') as stream:
        stream.write(message + '\n')
        stream.flush()
        os.fsync(stream.fileno())

def receive():
    header = sys.stdin.buffer.read(12)
    if not header:
        return None
    if len(header) != 12 or header[:6] != b'CTXPRO' or struct.unpack('>H', header[6:8])[0] != 3:
        sys.exit(20)
    size = struct.unpack('>I', header[8:12])[0]
    payload = sys.stdin.buffer.read(size)
    if len(payload) != size:
        sys.exit(21)
    return json.loads(payload)

def send(request, kind, body):
    envelope = {
        'sequence': request['sequence'],
        'request_id': request['request_id'],
        'message': {'kind': kind, 'body': body},
    }
    payload = json.dumps(envelope, separators=(',', ':')).encode()
    sys.stdout.buffer.write(b'CTXPRO' + struct.pack('>H', 3) + struct.pack('>I', len(payload)) + payload)
    sys.stdout.buffer.flush()

def load_state():
    return json.loads(STATE.read_text())

def store_state(value):
    temporary = STATE.with_name(STATE.name + '.tmp')
    with temporary.open('w') as stream:
        json.dump(value, stream, separators=(',', ':'))
        stream.flush()
        os.fsync(stream.fileno())
    os.replace(temporary, STATE)
    directory = os.open(str(STATE.parent), os.O_RDONLY)
    try:
        os.fsync(directory)
    finally:
        os.close(directory)

def status_body(requested_generation):
    state = load_state()
    common = {
        'requested_core_generation_id': requested_generation,
        'repository_coverage': {
            'repository_candidate_events': 0,
            'logical_binding_events': 0,
            'certified_live_root_access_events': 0,
            'file_evidence_events': 0,
            'exact_commit_evidence_events': 0,
            'exact_pull_request_evidence_events': 0,
        },
        'core_preparation_peak_workers': 0,
        'access': {
            'entitlement': 'available',
            'graph_key': 'available',
            'local_repository': 'unavailable',
        },
        'supported_operations': [],
        'available_operations': [],
    }
    if state['kind'] == 'pending':
        common.update({
            'currentness': 'finalizing',
            'core_receipt': None,
            'coverage': 'partial',
            'finalization_progress': state['progress'],
            'storage_evidence': None,
        })
    else:
        common.update({
            'currentness': 'current',
            'core_receipt': RECEIPT,
            'coverage': 'empty' if RECEIPT['event_count'] == 0 else 'abstained',
            'finalization_progress': None,
            'storage_evidence': {
                'graph_manifest_schema': 3,
                'flat_format_version': 2,
                'materializer_checkpoint_version': 5,
                'journal_pack_format_version': 3,
                'legacy_journals_written': 0,
                'journal_pages_written': 1,
                'journal_packs_written': 1,
                'journal_finish_activity': {
                    'worker_limit': 1,
                    'peak_workers': 1,
                    'started_after_preparation': True,
                },
            },
        })
    return common

log('process_start')
hello = receive()
if hello is None or hello['message']['kind'] != 'hello':
    sys.exit(22)
log('hello')
send(hello, 'hello', {
    'protocol_version': 3,
    'protocol_fingerprint': FINGERPRINT,
    'helper_version': 'authenticated-finalization-burst-fixture-v1',
    'capabilities': ['entitlement_authorization', 'status', 'core_materialization'],
    'authorization_challenge_base64url': 'AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA',
})

authorization = receive()
if authorization is None or authorization['message']['kind'] != 'authorize':
    sys.exit(23)
log('authorize')
send(authorization, 'authorized', {
    'state': 'active',
    'refresh_required': False,
    'expires_at_unix': 9223372036854775807,
    'access_deadline_unix': 9223372036854775807,
    'grace_deadline_unix': 9223372036854775807,
    'capabilities': ['graph_write'],
})

while True:
    request = receive()
    if request is None:
        break
    kind = request['message']['kind']
    body = request['message']['body']
    if kind == 'status':
        log('status')
        send(request, 'status', status_body(body.get('requested_core_generation_id')))
    elif kind == 'continue_core_materialization':
        state = load_state()
        if state['kind'] != 'pending' or body['expected_progress'] != state['progress']:
            sys.exit(24)
        count = state['continue_count'] + 1
        log('continue:' + str(count))
        if count >= TERMINAL_AFTER:
            terminal_progress = state['progress']
            store_state({'kind': 'current', 'continue_count': count})
            log('durable_terminal:' + str(count))
            send(request, 'core_materialization_finished', {
                'materialization_id': terminal_progress['materialization_id'],
                'finish_request_digest': terminal_progress['finish_request_digest'],
                'receipt': RECEIPT,
                'replayed': False,
            })
        else:
            progress = dict(state['progress'])
            progress['cursor_sha256'] = format(count, '064x')
            store_state({'kind': 'pending', 'continue_count': count, 'progress': progress})
            log('durable_pending:' + str(count) + ':' + progress['cursor_sha256'])
            send(request, 'core_materialization_finalization_pending', {
                'progress': progress,
                'replayed': False,
            })
    else:
        sys.exit(25)
"#;
    let helper = HELPER
        .replace(
            "__STATE_PATH__",
            &serde_json::to_string(&state_path.to_string_lossy()).unwrap(),
        )
        .replace(
            "__LOG_PATH__",
            &serde_json::to_string(&log_path.to_string_lossy()).unwrap(),
        )
        .replace("__TERMINAL_AFTER__", &terminal_after.to_string())
        .replace("__RECEIPT__", &serde_json::to_string(receipt).unwrap())
        .replace(
            "__PROTOCOL_FINGERPRINT__",
            &serde_json::to_string(ctx_pro_host_protocol::PROTOCOL_FINGERPRINT).unwrap(),
        );
    let interpreter = python3_interpreter();
    fs::write(path, format!("#!{}\n{helper}", interpreter.display())).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
}

#[cfg(unix)]
fn process_backed_index(data_root: &Path, name: &str) -> VerifiedIndex {
    let index_root = ctx_history_refresh::source_backed_index_root(data_root);
    let source = source(name);
    let mut writer = GenerationWriter::open(&index_root, WriterOptions::default())
        .unwrap()
        .into_writer()
        .unwrap();
    add_source(&mut writer, &source, 1, vec!["body".to_owned()]);
    writer.commit(|_| true).unwrap();
    VerifiedIndex::open_pinned(index_root).unwrap()
}

#[cfg(unix)]
fn initialize_process_data_root(data_root: &Path) {
    fs::create_dir_all(data_root).unwrap();
    fs::set_permissions(data_root, fs::Permissions::from_mode(0o700)).unwrap();
    crate::identity::installation_id(data_root).unwrap();
}

#[cfg(unix)]
fn run_authenticated_process_burst(
    data_root: &Path,
    helper_path: &Path,
    index: &VerifiedIndex,
) -> CoreMaterializationSyncProgress {
    let required = BTreeSet::from([Capability::Status, Capability::CoreMaterialization]);
    let client = ProClient::connect_to_path_with_authorization_mode(
        data_root,
        helper_path,
        None,
        &required,
        Some(&FixtureAuthorizationProvider),
        false,
    )
    .unwrap();
    assert_eq!(
        client.authorization_state,
        Some(ctx_pro_host_protocol::EntitlementAccessState::Active)
    );
    let mut consumer = ProtocolCoreMaterializationConsumer {
        client,
        data_root: data_root.to_path_buf(),
        core_generation_id: index.generation_id().to_owned(),
        materializer_revision: None,
    };
    let status = consumer
        .status(StatusRequest {
            requested_core_generation_id: Some(index.generation_id().to_owned()),
        })
        .unwrap();
    status.validate().unwrap();
    continue_core_finalization(
        index,
        &status,
        &mut consumer,
        CoreWorkerLaunchSelection::explicit_test(1),
    )
    .unwrap()
}

#[cfg(unix)]
fn process_log_count(log_path: &Path, prefix: &str) -> usize {
    fs::read_to_string(log_path)
        .unwrap()
        .lines()
        .filter(|line| line.starts_with(prefix))
        .count()
}

#[cfg(unix)]
#[test]
fn one_authenticated_client_lifetime_services_a_bounded_cursor_burst() {
    let temp = tempdir().unwrap();
    let data_root = temp.path().join("data");
    initialize_process_data_root(&data_root);
    let index = process_backed_index(&data_root, "process-burst.jsonl");
    let initial = progress_for(
        &index,
        CoreMaterializationFinalizationPhase::SealingInputs,
        '0',
        "process-materializer-v1",
    );
    let receipt = receipt_for(&index, "process-materializer-v1");
    let state_path = temp.path().join("helper-state.json");
    let log_path = temp.path().join("helper-log.txt");
    let helper_path = temp.path().join("ctx-pro-finalization-burst");
    fs::write(
        &state_path,
        serde_json::to_vec(&serde_json::json!({
            "kind": "pending",
            "continue_count": 0,
            "progress": initial,
        }))
        .unwrap(),
    )
    .unwrap();
    let terminal_after = CORE_FINALIZATION_BURST_MAX_REQUESTS + 2;
    write_authenticated_finalization_helper(
        &helper_path,
        &state_path,
        &log_path,
        terminal_after,
        &receipt,
    );

    let CoreMaterializationSyncProgress::FinalizationPending(first) =
        run_authenticated_process_burst(&data_root, &helper_path, &index)
    else {
        panic!("the first authenticated process must yield at the request cap");
    };
    let durable: serde_json::Value =
        serde_json::from_slice(&fs::read(&state_path).unwrap()).unwrap();
    assert_eq!(durable["kind"], "pending");
    assert_eq!(
        durable["continue_count"],
        CORE_FINALIZATION_BURST_MAX_REQUESTS
    );
    assert_eq!(
        durable["progress"],
        serde_json::to_value(&first.progress).unwrap()
    );
    assert_eq!(process_log_count(&log_path, "process_start"), 1);
    assert_eq!(process_log_count(&log_path, "hello"), 1);
    assert_eq!(process_log_count(&log_path, "authorize"), 1);
    assert_eq!(
        process_log_count(&log_path, "continue:"),
        CORE_FINALIZATION_BURST_MAX_REQUESTS
    );
    assert_eq!(
        process_log_count(&log_path, "durable_pending:"),
        CORE_FINALIZATION_BURST_MAX_REQUESTS
    );
    assert_eq!(process_log_count(&log_path, "durable_terminal:"), 0);

    let CoreMaterializationSyncProgress::Finished(finished) =
        run_authenticated_process_burst(&data_root, &helper_path, &index)
    else {
        panic!("the second authenticated process must finish from the persisted cursor");
    };
    assert_eq!(finished.receipt, receipt);
    assert_eq!(process_log_count(&log_path, "process_start"), 2);
    assert_eq!(process_log_count(&log_path, "hello"), 2);
    assert_eq!(process_log_count(&log_path, "authorize"), 2);
    assert_eq!(process_log_count(&log_path, "continue:"), terminal_after);
    assert_eq!(
        process_log_count(&log_path, "durable_pending:"),
        terminal_after - 1
    );
    assert_eq!(process_log_count(&log_path, "durable_terminal:"), 1);
}

#[cfg(unix)]
#[test]
#[ignore = "focused authenticated helper lifetime/exchange-count microbenchmark"]
fn authenticated_helper_lifetime_count_microbenchmark() {
    const PRIOR_BURST_MAX_REQUESTS: usize = 16;
    const BEFORE_HELPER_LIFETIMES: usize = 7_461;
    const BEFORE_WALL: &str = "53:38.58";
    const BEFORE_USER_CPU_SECONDS: u64 = 2_690;
    const BEFORE_SYSTEM_CPU_SECONDS: u64 = 744;
    const SEPARATE_SEGMENT_CAP_FAILURE: usize = 4_097;
    const MEASURED_EXCHANGES: usize = 65;
    let terminal_after = MEASURED_EXCHANGES;
    let temp = tempdir().unwrap();
    let data_root = temp.path().join("data");
    initialize_process_data_root(&data_root);
    let index = process_backed_index(&data_root, "process-count-benchmark.jsonl");
    let initial = progress_for(
        &index,
        CoreMaterializationFinalizationPhase::EmitReplay,
        '0',
        "process-materializer-v1",
    );
    let receipt = receipt_for(&index, "process-materializer-v1");
    let state_path = temp.path().join("helper-state.json");
    let log_path = temp.path().join("helper-log.txt");
    let helper_path = temp.path().join("ctx-pro-finalization-benchmark");
    fs::write(
        &state_path,
        serde_json::to_vec(&serde_json::json!({
            "kind": "pending",
            "continue_count": 0,
            "progress": initial,
        }))
        .unwrap(),
    )
    .unwrap();
    write_authenticated_finalization_helper(
        &helper_path,
        &state_path,
        &log_path,
        terminal_after,
        &receipt,
    );

    let mut measured_lifetimes = 0_usize;
    loop {
        measured_lifetimes = measured_lifetimes.saturating_add(1);
        if matches!(
            run_authenticated_process_burst(&data_root, &helper_path, &index),
            CoreMaterializationSyncProgress::Finished(_)
        ) {
            break;
        }
    }
    let measured_hellos = process_log_count(&log_path, "hello");
    let measured_authorizations = process_log_count(&log_path, "authorize");
    let measured_exchanges = process_log_count(&log_path, "continue:");
    assert_eq!(measured_exchanges, terminal_after);
    assert_eq!(measured_hellos, measured_lifetimes);
    assert_eq!(measured_authorizations, measured_lifetimes);
    assert_eq!(
        measured_lifetimes,
        terminal_after.div_ceil(CORE_FINALIZATION_BURST_MAX_REQUESTS)
    );
    let prior_measured_lifetimes = terminal_after.div_ceil(PRIOR_BURST_MAX_REQUESTS);
    assert_eq!(prior_measured_lifetimes, 5);
    assert_eq!(measured_lifetimes, 1);
    let measured_reduction_percent =
        100.0 * (1.0 - measured_lifetimes as f64 / prior_measured_lifetimes as f64);
    assert!(measured_reduction_percent >= 80.0);

    let projected_prior_burst_lifetimes =
        BEFORE_HELPER_LIFETIMES.div_ceil(PRIOR_BURST_MAX_REQUESTS);
    let projected_burst_lifetimes =
        BEFORE_HELPER_LIFETIMES.div_ceil(CORE_FINALIZATION_BURST_MAX_REQUESTS);
    let projected_reduction_from_prior_percent =
        100.0 * (1.0 - projected_burst_lifetimes as f64 / projected_prior_burst_lifetimes as f64);
    let projected_reduction_from_original_percent =
        100.0 * (1.0 - projected_burst_lifetimes as f64 / BEFORE_HELPER_LIFETIMES as f64);
    eprintln!(
        "authenticated_finalization_burst before_lifetimes={BEFORE_HELPER_LIFETIMES} before_wall={BEFORE_WALL} before_user_cpu_seconds={BEFORE_USER_CPU_SECONDS} before_system_cpu_seconds={BEFORE_SYSTEM_CPU_SECONDS} separate_segment_cap_failure={SEPARATE_SEGMENT_CAP_FAILURE} prior_burst_max_requests={PRIOR_BURST_MAX_REQUESTS} production_burst_max_requests={CORE_FINALIZATION_BURST_MAX_REQUESTS} production_burst_max_elapsed_seconds={} measured_exchanges={measured_exchanges} prior_measured_connect_hello_lifetimes={prior_measured_lifetimes} measured_connect_hello_lifetimes={measured_lifetimes} measured_reduction_from_prior_percent={measured_reduction_percent:.2} projected_prior_burst_lifetimes={projected_prior_burst_lifetimes} projected_burst_lifetimes={projected_burst_lifetimes} projected_reduction_from_prior_percent={projected_reduction_from_prior_percent:.2} projected_reduction_from_original_percent={projected_reduction_from_original_percent:.2}",
        CORE_FINALIZATION_BURST_MAX_ELAPSED.as_secs(),
    );
    assert_eq!(projected_prior_burst_lifetimes, 467);
    assert_eq!(projected_burst_lifetimes, 30);
    assert!(projected_reduction_from_prior_percent > 90.0);
    assert!(projected_reduction_from_original_percent > 99.0);
}

#[test]
fn protocol_dispatch_maps_both_typed_finalization_responses() {
    let pending = pending(
        CoreMaterializationFinalizationProgress {
            materialization_id: "a".repeat(64),
            core_generation_id: "b".repeat(64),
            finish_request_digest: "d".repeat(64),
            materializer_revision: "materializer-v1".to_owned(),
            phase: CoreMaterializationFinalizationPhase::ReadyToActivate,
            cursor_sha256: "c".repeat(64),
        },
        true,
    );
    assert!(matches!(
        map_core_finalization_response(HelperMessage::CoreMaterializationFinalizationPending(
            pending
        ))
        .unwrap(),
        CoreMaterializationFinalizationStep::Pending(_)
    ));

    let finished = CoreMaterializationFinished {
        materialization_id: "a".repeat(64),
        finish_request_digest: "d".repeat(64),
        receipt: CoreMaterializationReceipt {
            core_generation_id: "b".repeat(64),
            core_record_contract_fingerprint: "c".repeat(64),
            source_snapshot_sha256: "d".repeat(64),
            materializer_revision: "materializer-v1".to_owned(),
            source_count: 0,
            event_count: 0,
        },
        replayed: false,
    };
    assert!(matches!(
        map_core_finalization_response(HelperMessage::CoreMaterializationFinished(finished))
            .unwrap(),
        CoreMaterializationFinalizationStep::Finished(_)
    ));
    assert!(
        map_core_finalization_response(HelperMessage::Status(status::result(
            StatusRequest {
                requested_core_generation_id: None,
            },
            CoreProjectionCurrentness::NotMaterialized,
            None,
            0,
            JournalFinishActivity::default(),
        )))
        .unwrap_err()
        .to_string()
        .contains("non-Core-finalization")
    );
}
