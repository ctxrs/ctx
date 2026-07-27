use std::collections::HashMap;

use uuid::Uuid;

use crate::{Result, Store};

use super::{EventEmbeddingDocument, EventSearchHit};

#[derive(Debug, Default)]
pub struct SemanticProjectionSnapshot {
    pub documents: Vec<EventEmbeddingDocument>,
    pub hits: Vec<EventSearchHit>,
}

impl Store {
    /// Materializes semantic hash inputs and hydrated hits from one canonical
    /// Store read transaction. This prevents a candidate validated against one
    /// projection version from being hydrated from a later version.
    pub fn semantic_projection_snapshot_by_id(
        &self,
        chunk_ranges: &HashMap<Uuid, (usize, usize)>,
    ) -> Result<SemanticProjectionSnapshot> {
        self.semantic_projection_snapshot_by_id_inner(chunk_ranges, || {})
    }

    fn semantic_projection_snapshot_by_id_inner<F>(
        &self,
        chunk_ranges: &HashMap<Uuid, (usize, usize)>,
        after_documents: F,
    ) -> Result<SemanticProjectionSnapshot>
    where
        F: FnOnce(),
    {
        let transaction = self.conn.unchecked_transaction()?;
        let event_ids = chunk_ranges.keys().copied().collect::<Vec<_>>();
        let documents = self.event_embedding_documents_by_ids(&event_ids)?;
        after_documents();
        let hits = self.semantic_event_hits_by_id(chunk_ranges)?;
        transaction.commit()?;
        Ok(SemanticProjectionSnapshot { documents, hits })
    }
}

#[cfg(test)]
mod tests {
    use std::{collections::HashMap, sync::mpsc, thread};

    use chrono::{DateTime, Utc};
    use ctx_history_core::{
        Event, EventRole, EventType, Fidelity, SyncMetadata, SyncState, Visibility,
    };
    use serde_json::json;
    use tempfile::tempdir;

    use super::*;

    fn searchable_event(event_id: Uuid, text: &str) -> Event {
        Event {
            id: event_id,
            seq: 1,
            history_record_id: None,
            session_id: None,
            run_id: None,
            event_type: EventType::Message,
            role: Some(EventRole::User),
            occurred_at: DateTime::parse_from_rfc3339("2026-07-23T12:00:00Z")
                .unwrap()
                .with_timezone(&Utc),
            capture_source_id: None,
            payload: json!({ "text": text }),
            payload_blob_id: None,
            dedupe_key: None,
            sync: SyncMetadata {
                visibility: Visibility::LocalOnly,
                fidelity: Fidelity::Imported,
                sync_state: SyncState::LocalOnly,
                sync_version: 0,
                deleted_at: None,
                metadata: json!({}),
            },
        }
    }

    #[test]
    fn semantic_projection_snapshot_is_stable_across_concurrent_mutation() {
        let temp = tempdir().unwrap();
        let path = temp.path().join("ctx.db");
        let event_id = Uuid::new_v4();
        let original = searchable_event(event_id, "canonical snapshot A");
        let setup = Store::open(&path).unwrap();
        setup.upsert_event(&original).unwrap();
        drop(setup);

        let reader = Store::open_read_only(&path).unwrap();
        let writer = Store::open(&path).unwrap();
        let mut mutated = original;
        mutated.payload = json!({ "text": "canonical snapshot B" });
        let (mutate_tx, mutate_rx) = mpsc::sync_channel(0);
        let (committed_tx, committed_rx) = mpsc::sync_channel(0);
        let writer_thread = thread::spawn(move || {
            mutate_rx.recv().unwrap();
            writer.upsert_event(&mutated).unwrap();
            committed_tx.send(()).unwrap();
        });

        let ranges = HashMap::from([(event_id, (0, 1024))]);
        let snapshot = reader
            .semantic_projection_snapshot_by_id_inner(&ranges, || {
                mutate_tx.send(()).unwrap();
                committed_rx.recv().unwrap();
            })
            .unwrap();
        writer_thread.join().unwrap();

        assert_eq!(snapshot.documents.len(), 1);
        assert!(snapshot.documents[0].text.contains("canonical snapshot A"));
        assert!(!snapshot.documents[0].text.contains("canonical snapshot B"));
        assert_eq!(snapshot.hits.len(), 1);
        assert!(snapshot.hits[0].preview.contains("canonical snapshot A"));
        assert!(!snapshot.hits[0].preview.contains("canonical snapshot B"));

        let current = reader.semantic_projection_snapshot_by_id(&ranges).unwrap();
        assert!(current.documents[0].text.contains("canonical snapshot B"));
        assert!(current.hits[0].preview.contains("canonical snapshot B"));
    }
}
