use ctx_history_core::{
    derive_event_id, derive_session_id, CertifiedSource, EventIdentityInput, LocatorRevisionPolicy,
    NativeItemKey, NativeRecordCoordinate, NativeSessionKey, ScannedSourceCounts,
    SessionIdentityInput, SourceAnchor, SourceKey, SourceObservation, SourceRecordLocator,
    TypedKey,
};
use ctx_history_index::{GenerationWriter, LexicalDocument, WriterOptions};

use super::*;

struct CoreFixture {
    temp: tempfile::TempDir,
    source: SourceKey,
    session_id: ctx_history_core::StableEntityId,
}

impl CoreFixture {
    fn new() -> Self {
        let source = SourceKey::derive(
            "gemini",
            "gemini_cli_chat_recording_jsonl",
            "session",
            1,
            SourceAnchor::CatalogLineage([41; 32]),
        )
        .unwrap();
        let session_key =
            NativeSessionKey::native_id("session", TypedKey::utf8("gemini-session").unwrap())
                .unwrap();
        let session_id = derive_session_id(SessionIdentityInput {
            source: &source,
            logical_session_kind: "thread",
            native_session_key: &session_key,
        })
        .unwrap();
        Self {
            temp: tempfile::tempdir().unwrap(),
            source,
            session_id,
        }
    }

    fn document(&self, sequence: u64, role: EventRole, body: impl Into<String>) -> LexicalDocument {
        let item = NativeItemKey::native_id("message", TypedKey::U64(sequence)).unwrap();
        let event_id = derive_event_id(EventIdentityInput {
            source: &self.source,
            session_id: self.session_id,
            logical_item_kind: "message",
            native_item_key: &item,
            subrecord_selector: None,
        })
        .unwrap();
        LexicalDocument {
            event_id,
            session_id: self.session_id,
            parent_session_id: None,
            root_session_id: self.session_id,
            source: self.source.clone(),
            locator: SourceRecordLocator::new(
                self.source.clone(),
                NativeRecordCoordinate::Jsonl {
                    byte_offset: sequence * 100,
                    byte_length: 80,
                    physical_ordinal: sequence,
                    native_session_key: Some(TypedKey::utf8("gemini-session").unwrap()),
                    native_event_key: Some(TypedKey::U64(sequence)),
                },
                LocatorRevisionPolicy::ExactSourceRevision,
                Some([17; 32]),
                [sequence as u8; 32],
            )
            .unwrap(),
            provider_session_id: Some("gemini-session".to_owned()),
            branch: None,
            source_path: Some(
                self.temp
                    .path()
                    .join("provider-source-was-removed.jsonl")
                    .display()
                    .to_string(),
            ),
            agent_type: "primary".to_owned(),
            is_primary: true,
            event_sequence: sequence,
            occurred_at_unix_ms: Some(sequence as i64),
            event_type: "message".to_owned(),
            role: Some(role.as_str().to_owned()),
            body: body.into(),
            workspace: Some("/workspace".to_owned()),
            cwd: Some("/workspace".to_owned()),
            touched_files: Vec::new(),
        }
    }

    fn index(&self, documents: Vec<LexicalDocument>) -> VerifiedIndex {
        let count = documents.len() as u64;
        let mut writer =
            GenerationWriter::open(self.temp.path().join("index"), WriterOptions::default())
                .unwrap();
        writer.begin_source(self.source.clone()).unwrap();
        for document in documents {
            writer.add_document(document).unwrap();
        }
        let observation =
            SourceObservation::new(self.source.clone(), "fixture-v1", vec![1]).unwrap();
        writer
            .certify_source(
                CertifiedSource::certify(
                    observation.clone(),
                    observation,
                    "fixture-parser-v1",
                    [1; 32],
                    ScannedSourceCounts {
                        complete_records: count,
                        retained_records: count,
                        indexed_documents: count,
                        certified_bytes: count * 80,
                        ..ScannedSourceCounts::default()
                    },
                )
                .unwrap(),
            )
            .unwrap();
        writer.commit(|_| true).unwrap();
        VerifiedIndex::open(self.temp.path().join("index")).unwrap()
    }
}

#[test]
fn core_builder_combines_complete_lite_turn_with_provider_source_absent() {
    let fixture = CoreFixture::new();
    let index = fixture.index(vec![
        fixture.document(1, EventRole::User, "exact Gemini question"),
        fixture.document(2, EventRole::Assistant, "early answer"),
        fixture.document(3, EventRole::Assistant, "final exact Gemini answer"),
        fixture.document(4, EventRole::User, "next question"),
    ]);
    assert!(!fixture
        .temp
        .path()
        .join("provider-source-was-removed.jsonl")
        .exists());
    let anchor = index
        .core_events_for_session(fixture.session_id.as_uuid())
        .unwrap()
        .into_iter()
        .find(|record| record.event_sequence == 1)
        .unwrap();
    let mut builder = CoreSemanticDocumentBuilder { index: &index };

    let document = builder.build_document(&anchor).unwrap().unwrap();

    assert_eq!(document.event_id, anchor.event_id.as_uuid());
    assert_eq!(document.provider, Some(CaptureProvider::Gemini));
    assert_eq!(document.occurred_at_ms, 3);
    assert_eq!(
        document.text,
        "user:\nexact Gemini question\n\nassistant:\nfinal exact Gemini answer"
    );
    assert_eq!(document.raw_source_path, None);
}

#[test]
fn core_builder_preserves_semantic_tail_beyond_sixteen_kib() {
    const TAIL: &str = "semantic-tail-token-7f0d";
    let fixture = CoreFixture::new();
    let body = format!("{} {TAIL}", "prefix ".repeat(2_500));
    assert!(body.len() > 16 * 1024);
    let index = fixture.index(vec![fixture.document(1, EventRole::User, body.clone())]);
    let page = index.core_semantic_event_page(None, 1).unwrap();
    let record = page.items.first().unwrap();
    let mut builder = CoreSemanticDocumentBuilder { index: &index };

    let document = builder.build_document(record).unwrap().unwrap();

    assert!(record.core_record.content.meaningful_text().ends_with(TAIL));
    assert!(document.text.ends_with(TAIL));
    assert!(document.text.len() > 16 * 1024);
}
