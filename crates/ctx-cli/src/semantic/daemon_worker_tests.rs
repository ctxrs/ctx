use ctx_history_core::{
    derive_event_id, derive_session_id, CertifiedSource, CoreRecord, EventIdentityInput,
    NativeItemKey, NativeSessionKey, ScannedSourceCounts, SessionIdentityInput, SourceAnchor,
    SourceKey, SourceObservation, TypedKey,
};
use ctx_history_index::{GenerationWriter, WriterOptions};

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

    fn record(&self, sequence: u64, role: EventRole, body: impl Into<String>) -> CoreRecord {
        self.record_in_session("gemini-session", sequence, role, body)
    }

    fn tool_record(&self, sequence: u64) -> CoreRecord {
        let mut record = self.record(sequence, EventRole::Tool, "tool payload");
        record.event_type = EventType::ToolOutput.as_str().to_owned();
        record.validate_contract().unwrap();
        record
    }

    fn record_in_session(
        &self,
        native_session_id: &str,
        sequence: u64,
        role: EventRole,
        body: impl Into<String>,
    ) -> CoreRecord {
        let native_session_key = TypedKey::utf8(native_session_id).unwrap();
        let session_key = NativeSessionKey::native_id("session", native_session_key).unwrap();
        let session_id = derive_session_id(SessionIdentityInput {
            source: &self.source,
            logical_session_kind: "thread",
            native_session_key: &session_key,
        })
        .unwrap();
        let item = NativeItemKey::native_id("message", TypedKey::U64(sequence)).unwrap();
        let event_id = derive_event_id(EventIdentityInput {
            source: &self.source,
            session_id,
            logical_item_kind: "message",
            native_item_key: &item,
            subrecord_selector: None,
        })
        .unwrap();
        let mut record = CoreRecord::new_selected(
            event_id,
            session_id,
            session_id,
            self.source.clone(),
            sequence,
            EventType::Message.as_str(),
            AgentType::Primary.as_str(),
            true,
            "semantic-daemon-test-v1",
            body,
        )
        .unwrap();
        record.provider_session_id = Some(native_session_id.to_owned());
        record.native_event_id = Some(TypedKey::U64(sequence));
        record.occurred_at_unix_ms = Some(sequence as i64);
        record.role = Some(role.as_str().to_owned());
        record.workspace = Some("/workspace".to_owned());
        record.cwd = Some("/workspace".to_owned());
        record.validate_contract().unwrap();
        record
    }

    fn index(&self, records: Vec<CoreRecord>) -> VerifiedIndex {
        let count = records.len() as u64;
        let mut writer =
            GenerationWriter::open(self.temp.path().join("index"), WriterOptions::default())
                .unwrap();
        writer.begin_source(self.source.clone()).unwrap();
        for record in records {
            writer.add_core_record(record).unwrap();
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
        fixture.record(1, EventRole::User, "exact Gemini question"),
        fixture.record(2, EventRole::Assistant, "early answer"),
        fixture.record(3, EventRole::Assistant, "final exact Gemini answer"),
        fixture.record(4, EventRole::User, "next question"),
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
    let mut builder = CoreSemanticDocumentBuilder::new(&index);

    let document = builder.build_document(&anchor).unwrap().unwrap();

    assert_eq!(document.event_id, anchor.event_id.as_uuid());
    assert_eq!(document.provider, Some(CaptureProvider::Gemini));
    assert_eq!(document.occurred_at_ms, 3);
    assert_eq!(
        document.text,
        "user:\nexact Gemini question\n\nassistant:\nfinal exact Gemini answer"
    );
}

#[test]
fn core_builder_preserves_semantic_tail_beyond_sixteen_kib() {
    const TAIL: &str = "semantic-tail-token-7f0d";
    let fixture = CoreFixture::new();
    let body = format!("{} {TAIL}", "prefix ".repeat(2_500));
    assert!(body.len() > 16 * 1024);
    let index = fixture.index(vec![fixture.record(1, EventRole::User, body.clone())]);
    let page = index.core_semantic_event_page(None, 1).unwrap();
    let record = page.items.first().unwrap();
    let mut builder = CoreSemanticDocumentBuilder::new(&index);

    let document = builder.build_document(record).unwrap().unwrap();

    assert!(record.core_record.content.meaningful_text().ends_with(TAIL));
    assert!(document.text.ends_with(TAIL));
    assert!(document.text.len() > 16 * 1024);
}

#[test]
fn core_builder_pairs_multiple_lite_turns_with_bounded_forward_queries() {
    let fixture = CoreFixture::new();
    let index = fixture.index(vec![
        fixture.record(1, EventRole::User, "first question"),
        fixture.record(2, EventRole::Assistant, "first answer"),
        fixture.record(3, EventRole::User, "second question"),
        fixture.record(4, EventRole::Assistant, "second answer"),
    ]);
    let anchors = index
        .core_events_for_session(fixture.session_id.as_uuid())
        .unwrap()
        .into_iter()
        .filter(|record| record.role.as_deref() == Some(EventRole::User.as_str()))
        .collect::<Vec<_>>();
    let mut builder = CoreSemanticDocumentBuilder::new(&index);

    let first = builder.build_document(&anchors[0]).unwrap().unwrap();
    let second = builder.build_document(&anchors[1]).unwrap().unwrap();

    assert_eq!(
        first.text,
        "user:\nfirst question\n\nassistant:\nfirst answer"
    );
    assert_eq!(
        second.text,
        "user:\nsecond question\n\nassistant:\nsecond answer"
    );
}

#[test]
fn core_builder_streams_multiple_pairing_pages_to_the_final_assistant() {
    let fixture = CoreFixture::new();
    let index = fixture.index(vec![
        fixture.record(1, EventRole::User, "bounded question"),
        fixture.record(2, EventRole::Assistant, "early bounded answer"),
        fixture.record(3, EventRole::Assistant, "late bounded answer"),
    ]);
    let anchor = index
        .core_events_for_session(fixture.session_id.as_uuid())
        .unwrap()
        .into_iter()
        .find(|record| record.event_sequence == 1)
        .unwrap();
    let mut builder = CoreSemanticDocumentBuilder {
        index: &index,
        pairing_page_records: 1,
        pairing_budget: LITE_TURN_PAIRING_BUDGET,
    };

    let document = builder.build_document(&anchor).unwrap().unwrap();

    assert_eq!(
        document.text,
        "user:\nbounded question\n\nassistant:\nlate bounded answer"
    );
    assert_eq!(document.occurred_at_ms, 3);
}

#[test]
fn core_builder_pairs_many_sessions_without_retaining_a_session_cache() {
    let fixture = CoreFixture::new();
    let mut records = Vec::new();
    for session in 0..12_u64 {
        let native_session_id = format!("gemini-session-{session}");
        records.push(fixture.record_in_session(
            &native_session_id,
            session * 2 + 1,
            EventRole::User,
            format!("question {session}"),
        ));
        records.push(fixture.record_in_session(
            &native_session_id,
            session * 2 + 2,
            EventRole::Assistant,
            format!("answer {session}"),
        ));
    }
    let index = fixture.index(records);
    let anchors = index.core_semantic_event_page(None, 64).unwrap().items;
    assert_eq!(anchors.len(), 12);
    let mut builder = CoreSemanticDocumentBuilder::new(&index);

    for anchor in &anchors {
        let session = (anchor.event_sequence - 1) / 2;
        let document = builder.build_document(anchor).unwrap().unwrap();
        assert_eq!(
            document.text,
            format!("user:\nquestion {session}\n\nassistant:\nanswer {session}")
        );
    }
}

#[test]
fn core_builder_returns_user_only_when_pairing_byte_budget_is_exhausted() {
    let fixture = CoreFixture::new();
    let index = fixture.index(vec![
        fixture.record(1, EventRole::User, "byte bounded question"),
        fixture.record(2, EventRole::Assistant, "first answer"),
        fixture.record(3, EventRole::Assistant, "second answer"),
    ]);
    let anchor = index
        .core_semantic_event_page(None, 1)
        .unwrap()
        .items
        .remove(0);
    let mut builder = CoreSemanticDocumentBuilder {
        index: &index,
        pairing_page_records: MAX_LITE_TURN_PAIRING_PAGE_RECORDS,
        pairing_budget: CoreEventPageBudget::new(
            ctx_history_core::MAX_ENCODED_CORE_RECORD_BYTES,
            1,
        ),
    };

    let document = builder.build_document(&anchor).unwrap().unwrap();

    assert_eq!(document.text, "user:\nbyte bounded question");
    assert_eq!(document.occurred_at_ms, 1);
}

#[test]
fn core_builder_preserves_assistant_after_more_than_sixty_four_tool_events() {
    const TOOL_EVENTS: u64 = 96;

    let fixture = CoreFixture::new();
    let mut records = Vec::with_capacity(TOOL_EVENTS as usize + 3);
    records.push(fixture.record(1, EventRole::User, "tool-heavy question"));
    for sequence in 2..=TOOL_EVENTS + 1 {
        records.push(fixture.tool_record(sequence));
    }
    records.push(fixture.record(
        TOOL_EVENTS + 2,
        EventRole::Assistant,
        "answer beyond the old window",
    ));
    records.push(fixture.record(TOOL_EVENTS + 3, EventRole::User, "next question"));
    let index = fixture.index(records);
    let anchor = index
        .core_events_for_session(fixture.session_id.as_uuid())
        .unwrap()
        .into_iter()
        .find(|record| record.event_sequence == 1)
        .unwrap();
    let mut builder = CoreSemanticDocumentBuilder::new(&index);

    let document = builder.build_document(&anchor).unwrap().unwrap();

    assert_eq!(
        document.text,
        "user:\ntool-heavy question\n\nassistant:\nanswer beyond the old window"
    );
    assert_eq!(document.occurred_at_ms, (TOOL_EVENTS + 2) as i64);
}
