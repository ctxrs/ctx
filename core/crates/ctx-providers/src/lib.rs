pub mod adapters;
pub mod ask_user_question;
pub mod container_exec;
pub mod crp;
pub mod env;
pub mod events;
pub mod fake;

#[cfg(test)]
mod tests {
    use tokio::sync::mpsc;

    use crate::adapters::ProviderAdapter;
    use crate::fake::FakeProviderAdapter;
    use ctx_core::models::SessionEventType;

    #[tokio::test]
    async fn fake_provider_emits_events() {
        let adapter = FakeProviderAdapter::new();
        let (tx, mut rx) = mpsc::channel(16);
        let _handle = adapter
            .run(
                crate::adapters::TurnInput {
                    content: "hi".into(),
                    attachments: vec![],
                    context_blocks: vec![],
                    model_id: Some("fake-model".into()),
                },
                std::env::current_dir().unwrap(),
                Default::default(),
                tx,
                crate::adapters::ProviderRunHooks::default(),
            )
            .await
            .unwrap();

        let mut types = Vec::new();
        while let Some(ev) = rx.recv().await {
            types.push(ev.event_type);
            if types.len() >= 5 {
                break;
            }
        }
        assert!(types.len() >= 4);
    }

    #[tokio::test]
    async fn fake_provider_slow_stream_emits_incremental_assistant_chunks_with_order_anchor() {
        let adapter = FakeProviderAdapter::new();
        let (tx, mut rx) = mpsc::channel(64);
        let _handle = adapter
            .run(
                crate::adapters::TurnInput {
                    content: "slow-diff-test stream-assistant-partials".into(),
                    attachments: vec![],
                    context_blocks: vec![],
                    model_id: Some("fake-model".into()),
                },
                std::env::current_dir().unwrap(),
                Default::default(),
                tx,
                crate::adapters::ProviderRunHooks::default(),
            )
            .await
            .unwrap();

        let mut assistant_chunk_fragments = Vec::new();
        let mut assistant_chunk_message_ids = Vec::new();
        let mut assistant_chunk_order_seqs = Vec::new();
        let mut saw_assistant_complete = false;

        while let Some(event) = rx.recv().await {
            match event.event_type {
                SessionEventType::AssistantChunk => {
                    let payload = event.payload_json;
                    assistant_chunk_fragments.push(
                        payload
                            .get("content_fragment")
                            .and_then(|value| value.as_str())
                            .unwrap_or_default()
                            .to_string(),
                    );
                    assistant_chunk_message_ids.push(
                        payload
                            .get("message_id")
                            .and_then(|value| value.as_str())
                            .unwrap_or_default()
                            .to_string(),
                    );
                    assistant_chunk_order_seqs.push(
                        payload
                            .get("order_seq")
                            .and_then(|value| value.as_i64())
                            .unwrap_or_default(),
                    );
                }
                SessionEventType::AssistantComplete => {
                    saw_assistant_complete = true;
                    break;
                }
                _ => {}
            }
        }

        assert!(saw_assistant_complete);
        assert!(
            assistant_chunk_fragments.len() >= 2,
            "expected multiple assistant chunks, got {assistant_chunk_fragments:?}",
        );
        assert!(assistant_chunk_fragments
            .iter()
            .all(|fragment| !fragment.is_empty()));
        assert!(assistant_chunk_message_ids
            .iter()
            .all(|message_id| !message_id.is_empty()));
        assert!(assistant_chunk_message_ids
            .windows(2)
            .all(|pair| pair[0] == pair[1]));
        assert!(assistant_chunk_order_seqs
            .iter()
            .all(|order_seq| *order_seq > 0));
        assert!(
            assistant_chunk_order_seqs
                .windows(2)
                .all(|pair| pair[0] <= pair[1]),
            "expected monotonic chunk order_seq values, got {assistant_chunk_order_seqs:?}",
        );
    }
}
