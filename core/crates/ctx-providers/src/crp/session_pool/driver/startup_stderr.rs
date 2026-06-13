use std::sync::atomic::Ordering;

use anyhow::Result;
use serde_json::json;
use tokio::sync::mpsc;

use ctx_core::models::SessionEventType;

use crate::adapters::{ProviderTurnOutcome, ProviderTurnStatus};
use crate::events::NormalizedEvent;

use super::super::super::auth_required_notice_payload_from_stderr;
use super::super::super::policy::{
    extract_auth_error_from_stderr_line, extract_auth_url_from_stderr_line,
    extract_runtime_fatal_error_from_stderr_line,
};
use super::super::CrpSession;

pub(super) struct StartupStderrOutcome {
    pub(super) outcome: ProviderTurnOutcome,
    pub(super) terminal_events: Vec<NormalizedEvent>,
}

pub(super) async fn handle_startup_stderr_line(
    line: &str,
    session: &CrpSession,
    event_sink: &mpsc::Sender<NormalizedEvent>,
) -> Result<Option<StartupStderrOutcome>> {
    if let Some(auth_url) = extract_auth_url_from_stderr_line(line) {
        session.opening.store(false, Ordering::SeqCst);
        let notice = NormalizedEvent {
            event_type: SessionEventType::Notice,
            payload_json: auth_required_notice_payload_from_stderr(&auth_url),
        };
        let interrupted = NormalizedEvent {
            event_type: SessionEventType::TurnInterrupted,
            payload_json: json!({
                "reason": "auth_required",
            }),
        };
        let _ = event_sink.send(notice).await;
        let emitted = event_sink.send(interrupted.clone()).await.is_ok();
        session.process.shutdown("crp_auth_required_stderr").await;
        return Ok(Some(StartupStderrOutcome {
            outcome: ProviderTurnOutcome {
                status: ProviderTurnStatus::Interrupted,
                message: None,
                reason: Some("auth_required".to_string()),
                details: None,
                kind: None,
                provider_cancelled: None,
                terminal_event_emitted: emitted,
            },
            terminal_events: vec![interrupted],
        }));
    }

    if let Some(message) = extract_auth_error_from_stderr_line(line) {
        session.opening.store(false, Ordering::SeqCst);
        session.process.shutdown("crp_auth_error_stderr").await;
        return Ok(Some(StartupStderrOutcome {
            outcome: ProviderTurnOutcome::failed_with_context(
                message,
                None,
                Some(json!({ "source": "crp_stderr" })),
                Some(json!("auth_error")),
                false,
            ),
            terminal_events: Vec::new(),
        }));
    }

    if let Some(message) = extract_runtime_fatal_error_from_stderr_line(line) {
        session.process.shutdown("crp_runtime_fatal_stderr").await;
        anyhow::bail!("{message}");
    }

    Ok(None)
}
