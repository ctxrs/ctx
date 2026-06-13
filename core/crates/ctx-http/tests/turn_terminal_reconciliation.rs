use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::Result;
use async_trait::async_trait;
use serde_json::json;

use ctx_core::ids::{RunId, TurnId};
use ctx_core::models::{SessionEventType, SessionTurnStatus};
use ctx_providers::adapters::{
    ProviderAdapter, ProviderHealth, ProviderStatus, RunHandle, TurnInput,
};
use ctx_providers::events::NormalizedEvent;
use tokio::sync::mpsc;

mod common;

struct StartFailProvider;

#[async_trait]
impl ProviderAdapter for StartFailProvider {
    async fn inspect(&self) -> Result<ProviderStatus> {
        Ok(ProviderStatus {
            provider_id: "fake".into(),
            installed: true,
            detected_path: None,
            version: Some("0.1.0".into()),
            capabilities: None,
            health: ProviderHealth::Ok,
            diagnostics: Vec::new(),
            details: HashMap::new(),
            usability: ctx_providers::adapters::ProviderUsability::default(),
        })
    }

    async fn run(
        &self,
        _input: TurnInput,
        _workdir: PathBuf,
        _env: HashMap<String, String>,
        _event_sink: mpsc::Sender<NormalizedEvent>,
        _hooks: ctx_providers::adapters::ProviderRunHooks,
    ) -> Result<RunHandle> {
        anyhow::bail!("synthetic start failure");
    }

    async fn cancel(&self, _handle: &mut RunHandle) -> Result<()> {
        Ok(())
    }
}

struct TestHarness {
    _repo: tempfile::TempDir,
    fixture: common::FakeDaemonFixture,
    session: ctx_core::models::Session,
}

impl TestHarness {
    fn daemon(&self) -> &ctx_daemon::test_support::TestDaemon {
        &self.fixture.daemon
    }

    fn app_router(&self) -> axum::Router {
        self.fixture.router()
    }
}

async fn setup_state() -> TestHarness {
    setup_state_with_providers(common::fake_providers()).await
}

async fn setup_state_with_providers(
    providers: HashMap<String, Arc<dyn ProviderAdapter>>,
) -> TestHarness {
    let repo = common::init_git_repo(&[("file.txt", "hello\n")]).await;
    let fixture = common::fake_daemon_fixture_with_providers(providers, "http://127.0.0.1:0").await;
    let app = fixture.router();
    let ws = common::create_workspace(&app, repo.path(), "ws").await;
    let (_task, session) =
        common::create_task_with_session(&app, ws.id.0, "t1", "fake", "fake-model").await;
    TestHarness {
        _repo: repo,
        fixture,
        session,
    }
}

#[tokio::test]
async fn reconcile_terminal_state_respects_turn_finished_status() {
    let harness = setup_state().await;
    let run_id = RunId::new();
    let turn_id = TurnId::new();
    harness
        .daemon()
        .seed_running_turn_for_reconciliation_test(harness.session.id, run_id, turn_id)
        .await
        .unwrap();

    let finished = harness
        .daemon()
        .append_turn_finished_event_for_test(
            harness.session.id,
            Some(run_id),
            turn_id,
            SessionTurnStatus::Interrupted,
        )
        .await
        .unwrap();

    harness
        .daemon()
        .reconcile_turn_terminal_state_for_test(
            harness.session.id,
            Some(run_id),
            turn_id,
            "daemon_restart",
        )
        .await
        .unwrap();

    let snapshot = harness
        .daemon()
        .turn_reconciliation_snapshot_for_test(harness.session.id, turn_id)
        .await
        .unwrap();
    assert_eq!(snapshot.turn.status, SessionTurnStatus::Interrupted);
    assert_eq!(snapshot.turn.end_seq, Some(finished.seq));
    assert_eq!(
        snapshot.last_turn_status,
        Some(SessionTurnStatus::Interrupted)
    );
    assert!(!snapshot.is_working);
}

#[tokio::test]
async fn reconcile_terminal_state_emits_interrupt_when_terminal_event_missing() {
    let harness = setup_state().await;
    let run_id = RunId::new();
    let turn_id = TurnId::new();
    harness
        .daemon()
        .seed_running_turn_for_reconciliation_test(harness.session.id, run_id, turn_id)
        .await
        .unwrap();

    harness
        .daemon()
        .reconcile_turn_terminal_state_for_test(
            harness.session.id,
            Some(run_id),
            turn_id,
            "daemon_restart",
        )
        .await
        .unwrap();

    let snapshot = harness
        .daemon()
        .turn_reconciliation_snapshot_for_test(harness.session.id, turn_id)
        .await
        .unwrap();
    assert_eq!(snapshot.turn.status, SessionTurnStatus::Interrupted);

    assert!(snapshot
        .events
        .iter()
        .any(|event| matches!(event.event_type, SessionEventType::TurnInterrupted)));
    let finished = snapshot
        .events
        .iter()
        .find(|event| matches!(event.event_type, SessionEventType::TurnFinished))
        .expect("turn finished event");
    assert_eq!(
        finished
            .payload_json
            .get("status")
            .and_then(|value| value.as_str()),
        Some("interrupted")
    );
}

#[tokio::test]
async fn reconcile_provider_exit_emits_failed_terminal_events_when_missing() {
    let harness = setup_state().await;
    let run_id = RunId::new();
    let turn_id = TurnId::new();
    harness
        .daemon()
        .seed_running_turn_for_reconciliation_test(harness.session.id, run_id, turn_id)
        .await
        .unwrap();

    harness
        .daemon()
        .reconcile_turn_failed_on_provider_exit_for_test(
            harness.session.id,
            Some(run_id),
            turn_id,
            "provider_exit",
        )
        .await
        .unwrap();

    let snapshot = harness
        .daemon()
        .turn_reconciliation_snapshot_for_test(harness.session.id, turn_id)
        .await
        .unwrap();
    assert_eq!(snapshot.turn.status, SessionTurnStatus::Failed);
    assert_eq!(snapshot.last_turn_status, Some(SessionTurnStatus::Failed));
    assert!(!snapshot.is_working);

    let failed_finished = snapshot
        .events
        .iter()
        .find(|event| {
            matches!(event.event_type, SessionEventType::TurnFinished)
                && event
                    .payload_json
                    .get("status")
                    .and_then(|value| value.as_str())
                    == Some("failed")
        })
        .expect("failed turn_finished event");
    assert_eq!(
        failed_finished
            .payload_json
            .get("reason")
            .and_then(|value| value.as_str()),
        Some("provider_exit")
    );
    let finished = snapshot
        .events
        .iter()
        .find(|event| matches!(event.event_type, SessionEventType::TurnFinished))
        .expect("turn finished event");
    assert_eq!(
        finished
            .payload_json
            .get("status")
            .and_then(|value| value.as_str()),
        Some("failed")
    );
}

#[tokio::test]
async fn start_failure_marks_turn_failed_and_finishes() {
    let mut providers = common::fake_providers();
    providers.insert("fake".into(), Arc::new(StartFailProvider));
    let harness = setup_state_with_providers(providers).await;
    let app = harness.app_router();

    let (status, message): (axum::http::StatusCode, ctx_core::models::Message) =
        common::json_request(
            &app,
            axum::http::Method::POST,
            format!("/api/sessions/{}/messages", harness.session.id.0),
            Some(json!({"content":"start failure"})),
        )
        .await;
    assert_eq!(status, axum::http::StatusCode::OK);

    let turn_id = message.turn_id.expect("turn id");
    harness
        .daemon()
        .wait_for_scheduler_runtime_events_for_test(
            harness.session.id,
            std::time::Duration::from_secs(5),
            "start failure turn finish",
            |events| {
                Ok(events.iter().any(|event| {
                    event.turn_id == Some(turn_id)
                        && matches!(event.event_type, SessionEventType::TurnFinished)
                }))
            },
        )
        .await
        .unwrap();

    let snapshot = harness
        .daemon()
        .turn_reconciliation_snapshot_for_test(harness.session.id, turn_id)
        .await
        .unwrap();
    assert_eq!(snapshot.turn.status, SessionTurnStatus::Failed);
    assert!(snapshot.events.iter().any(|event| {
        matches!(event.event_type, SessionEventType::TurnFinished)
            && event
                .payload_json
                .get("status")
                .and_then(|value| value.as_str())
                == Some("failed")
    }));
}
