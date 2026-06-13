use super::*;
use ctx_daemon::test_support::TestDaemon;

fn cursor(last_event_seq: i64, projection_rev: i64) -> SessionCursor {
    SessionCursor {
        last_sent: SessionReplayCursor {
            last_event_seq,
            projection_rev,
        },
    }
}

#[tokio::test]
async fn merge_replayed_and_live_subscriptions_keeps_live_cursor_authoritative_after_replay() {
    let root = tempfile::tempdir().unwrap();
    let daemon =
        TestDaemon::new_for_test(root.path().to_path_buf(), "http://127.0.0.1:0".to_string())
            .await
            .expect("test daemon should start");
    let state = daemon.workspace_stream_handle_for_test();
    let replayed_session_id = SessionId::new();
    let live_only_session_id = SessionId::new();
    let removed_session_id = SessionId::new();
    let live_subscriptions = HashMap::from([
        (replayed_session_id, cursor(15, 15).last_sent),
        (live_only_session_id, cursor(7, 7).last_sent),
    ]);
    let replayed_subscriptions = HashMap::from([
        (replayed_session_id, cursor(12, 12).last_sent),
        (removed_session_id, cursor(20, 20).last_sent),
    ]);

    let finalization = state.finalize_workspace_stream_subscription_replay(
        &WorkspaceActiveSubscriptionState::default(),
        &live_subscriptions,
        replayed_subscriptions,
        &[],
    );

    assert_eq!(finalization.subscriptions.len(), 2);
    assert_eq!(
        finalization
            .subscriptions
            .get(&replayed_session_id)
            .copied(),
        Some(SessionReplayCursor {
            last_event_seq: 15,
            projection_rev: 15,
        })
    );
    assert_eq!(
        finalization
            .subscriptions
            .get(&live_only_session_id)
            .copied(),
        Some(SessionReplayCursor {
            last_event_seq: 7,
            projection_rev: 7,
        })
    );
    assert!(
        !finalization.subscriptions.contains_key(&removed_session_id),
        "live subscription state must remain authoritative for removed sessions",
    );
}
