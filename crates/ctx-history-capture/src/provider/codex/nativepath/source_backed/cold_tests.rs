use super::*;
use std::{
    fs,
    sync::{Arc, Barrier},
    time::Duration,
};

#[test]
fn ready_handoff_does_not_wait_for_an_idle_lane() {
    let (sender, receiver) = mpsc::sync_channel::<ColdWorkerEventV0>(0);
    let idle_sender = sender.clone();
    let ready_sender = sender.clone();
    drop(sender);
    let release_idle = Arc::new(Barrier::new(2));
    let ready_attempted = Arc::new(Barrier::new(2));
    let (ready_sent, ready_sent_receiver) = mpsc::channel();

    thread::scope(|scope| {
        let idle_release = Arc::clone(&release_idle);
        scope.spawn(move || {
            idle_release.wait();
            let _ = idle_sender.send(empty_page_event(0, 0));
        });
        let ready_attempt = Arc::clone(&ready_attempted);
        scope.spawn(move || {
            ready_attempt.wait();
            ready_sender.send(empty_page_event(1, 1)).unwrap();
            ready_sent.send(()).unwrap();
        });

        ready_attempted.wait();
        assert!(matches!(
            ready_sent_receiver.try_recv(),
            Err(mpsc::TryRecvError::Empty)
        ));
        let ready = receiver.recv_timeout(Duration::from_secs(1)).unwrap();
        assert!(matches!(
            ready,
            ColdWorkerEventV0::Message {
                lane_index: 1,
                message: ColdLaneMessageV0::Page(_)
            }
        ));
        ready_sent_receiver
            .recv_timeout(Duration::from_secs(1))
            .unwrap();
        release_idle.wait();
        assert!(matches!(
            receiver.recv_timeout(Duration::from_secs(1)).unwrap(),
            ColdWorkerEventV0::Message {
                lane_index: 0,
                message: ColdLaneMessageV0::Page(_)
            }
        ));
    });
}

#[test]
fn worker_error_and_disconnect_remain_typed() {
    let lane_states = vec![ColdLaneStateV0 {
        source_indices: vec![0],
        next_source: 0,
        next_page: 0,
        staged_documents: 0,
        last_event_sequence: None,
        mode: None,
    }];
    let finished_lanes = vec![false];

    let (sender, receiver) = mpsc::sync_channel::<ColdWorkerEventV0>(0);
    thread::scope(|scope| {
        scope.spawn(move || {
            sender
                .send(ColdWorkerEventV0::Failed {
                    lane_index: 0,
                    error: CodexSourceBackedErrorV0::InjectedColdWorkerFailure {
                        native_session_id: "source-test".to_owned(),
                    },
                })
                .unwrap();
        });
        let error =
            receive_cold_worker_event_v0(&receiver, &lane_states, &finished_lanes).unwrap_err();
        assert!(matches!(
            error,
            CodexSourceBackedErrorV0::InjectedColdWorkerFailure { native_session_id }
                if native_session_id == "source-test"
        ));
    });

    let (sender, receiver) = mpsc::sync_channel::<ColdWorkerEventV0>(0);
    drop(sender);
    let error = receive_cold_worker_event_v0(&receiver, &lane_states, &finished_lanes).unwrap_err();
    assert!(matches!(
        error,
        CodexSourceBackedErrorV0::ColdLaneDisconnected { lane: 0 }
    ));

    let (sender, receiver) = mpsc::sync_channel::<ColdWorkerEventV0>(0);
    thread::scope(|scope| {
        scope.spawn(move || {
            sender
                .send(ColdWorkerEventV0::Panicked { lane_index: 0 })
                .unwrap();
        });
        let error =
            receive_cold_worker_event_v0(&receiver, &lane_states, &finished_lanes).unwrap_err();
        assert!(matches!(
            error,
            CodexSourceBackedErrorV0::ColdWorkerPanicked { lane: 0 }
        ));
    });
}

#[test]
fn ready_arrival_preserves_exact_receipt_and_record_order() {
    let temp = tempfile::tempdir().unwrap();
    let sessions = temp.path().join("sessions");
    let single_index = temp.path().join("single-index");
    let parallel_index = temp.path().join("parallel-index");
    fs::create_dir_all(&sessions).unwrap();
    let native_session_ids = [
        "019fa000-0000-7000-8000-000000000071",
        "019fa000-0000-7000-8000-000000000072",
    ];
    for (source_index, native_session_id) in native_session_ids.iter().enumerate() {
        write_test_session(&sessions, native_session_id, source_index);
    }

    let single = ingest_codex_source_backed_inner_v0(
        &sessions,
        &single_index,
        ColdParallelOptionsV0 {
            scanner_workers: Some(1),
            ..ColdParallelOptionsV0::default()
        },
    )
    .unwrap();
    let parallel = ingest_codex_source_backed_inner_v0(
        &sessions,
        &parallel_index,
        ColdParallelOptionsV0 {
            scanner_workers: Some(2),
            scanner_rendezvous: Some(2),
            ..ColdParallelOptionsV0::default()
        },
    )
    .unwrap();

    assert_eq!(single.commit.generation_id, parallel.commit.generation_id);
    assert_eq!(single.commit.opstamp, parallel.commit.opstamp);
    assert_eq!(
        single.commit.indexed_documents,
        parallel.commit.indexed_documents
    );
    assert_eq!(
        single.commit.certified_sources,
        parallel.commit.certified_sources
    );
    assert_eq!(
        single.commit.certified_source_bytes,
        parallel.commit.certified_source_bytes
    );
    assert_eq!(
        single.commit.manifest().sources,
        parallel.commit.manifest().sources
    );

    let single_verified = VerifiedIndex::open(&single_index).unwrap();
    let parallel_verified = VerifiedIndex::open(&parallel_index).unwrap();
    for native_session_id in native_session_ids {
        let source = codex_source_key(native_session_id).unwrap();
        let session = codex_session_identity(&source, native_session_id).unwrap();
        assert_eq!(
            single_verified
                .events_for_session(session.as_uuid())
                .unwrap(),
            parallel_verified
                .events_for_session(session.as_uuid())
                .unwrap()
        );
    }
}

fn write_test_session(sessions: &Path, native_session_id: &str, source_index: usize) {
    let mut contents = serde_json::json!({
        "timestamp": "2026-07-28T12:00:00Z",
        "type": "session_meta",
        "payload": {
            "id": native_session_id,
            "timestamp": "2026-07-28T12:00:00Z",
            "cwd": "/tmp/source-backed-ready-handoff",
            "originator": "codex_cli_rs",
            "cli_version": "0.1.0",
            "source": "cli",
            "model_provider": "openai"
        }
    })
    .to_string();
    contents.push('\n');
    for event_index in 0..65 {
        contents.push_str(
            &serde_json::json!({
                "timestamp": "2026-07-28T12:00:01Z",
                "type": "response_item",
                "payload": {
                    "type": "message",
                    "role": if event_index % 2 == 0 { "user" } else { "assistant" },
                    "content": [{
                        "type": "input_text",
                        "text": format!(
                            "ready handoff source {source_index} event {event_index}"
                        )
                    }]
                }
            })
            .to_string(),
        );
        contents.push('\n');
    }
    fs::write(
        sessions.join(format!("rollout-{native_session_id}.jsonl")),
        contents,
    )
    .unwrap();
}

fn empty_page_event(lane_index: usize, source_index: usize) -> ColdWorkerEventV0 {
    ColdWorkerEventV0::Message {
        lane_index,
        message: ColdLaneMessageV0::Page(ColdPreparedPageV0 {
            source_index,
            page_index: 0,
            records: Vec::new(),
        }),
    }
}
