use super::*;

pub(in crate::api) async fn install_stream_sse(
    State(providers): State<ProviderInstallHandle>,
    Path(install_id): Path<String>,
) -> Result<Sse<impl Stream<Item = Result<SseEvent, axum::Error>>>, StatusCode> {
    let route = providers
        .open_provider_install_event_stream_for_route(&install_id)
        .await
        .map_err(super::status::provider_install_status_only_error)?;
    let initial = futures::stream::iter(route.history.into_iter().map(|ev| {
        let payload = serde_json::to_string(&ev).unwrap_or_else(|_| "{}".into());
        Ok::<_, axum::Error>(SseEvent::default().event("progress").data(payload))
    }));

    let live = futures::stream::unfold(route.receiver, move |mut rx| async move {
        loop {
            match rx.recv().await {
                Ok(ev) => {
                    let payload = serde_json::to_string(&ev).unwrap_or_else(|_| "{}".into());
                    return Some((Ok(SseEvent::default().event("progress").data(payload)), rx));
                }
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                Err(tokio::sync::broadcast::error::RecvError::Closed) => return None,
            }
        }
    });

    let stream = initial.chain(live);

    Ok(Sse::new(stream).keep_alive(KeepAlive::new().interval(Duration::from_secs(15))))
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::response::IntoResponse;
    use chrono::Utc;
    use ctx_provider_install::install_state::{
        InstallEventLevel, InstallProgressEvent, InstallTarget,
    };
    use http_body_util::BodyExt;

    #[tokio::test]
    async fn install_stream_sse_emits_history_and_live_progress_events() {
        let fixture = crate::test_support::TestDaemonFixture::new("http://127.0.0.1:4310").await;
        let daemon = fixture.daemon();
        let (install_id, started_new) = daemon
            .start_install("codex".to_string(), Some(InstallTarget::Container))
            .await;
        assert!(started_new);
        daemon
            .emit_install_event(
                install_id,
                install_event(install_id, "history-probe", "History probe"),
            )
            .await;

        let response = install_stream_sse(
            State(fixture.provider_install()),
            Path(install_id.to_string()),
        )
        .await
        .expect("install stream should open")
        .into_response();
        let mut body = response.into_body();

        let history_chunk = next_sse_chunk_containing(&mut body, "history-probe").await;
        assert!(history_chunk.contains("event: progress"));
        assert!(history_chunk.contains("\"stage\":\"history-probe\""));

        daemon
            .emit_install_event(
                install_id,
                install_event(install_id, "live-probe", "Live probe"),
            )
            .await;
        let live_chunk = next_sse_chunk_containing(&mut body, "live-probe").await;
        assert!(live_chunk.contains("event: progress"));
        assert!(live_chunk.contains("\"stage\":\"live-probe\""));
    }

    async fn next_sse_chunk_containing(body: &mut axum::body::Body, needle: &str) -> String {
        for _ in 0..8 {
            let frame = tokio::time::timeout(Duration::from_secs(2), body.frame())
                .await
                .expect("timed out waiting for install SSE frame")
                .expect("install SSE stream ended")
                .expect("install SSE frame error");
            let Some(data) = frame.data_ref() else {
                continue;
            };
            let chunk = String::from_utf8_lossy(data).into_owned();
            if chunk.contains(needle) {
                return chunk;
            }
        }
        panic!("install SSE stream did not emit frame containing {needle}");
    }

    fn install_event(install_id: uuid::Uuid, stage: &str, message: &str) -> InstallProgressEvent {
        InstallProgressEvent {
            install_id,
            provider_id: "codex".to_string(),
            target: Some(InstallTarget::Container),
            at: Utc::now(),
            stage: stage.to_string(),
            message: message.to_string(),
            level: InstallEventLevel::Info,
            bytes: None,
            total_bytes: None,
            attempt: None,
            error_code: None,
        }
    }
}
