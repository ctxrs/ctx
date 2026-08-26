use super::*;
use crate::{fields_from_schema, lexical_schema, IndexError};
use ctx_history_core::{
    derive_event_id, derive_session_id, CoreRecord, EventIdentityInput, NativeItemKey,
    NativeSessionKey, ProviderNativeCopyProof, ProviderNativeEventCopy, SessionIdentityInput,
    SourceAnchor, SourceKey, TypedKey,
};
use tantivy::schema::{Document, TantivyDocument};

fn source(source_format: &str) -> SourceKey {
    SourceKey::derive(
        "codex",
        source_format,
        "session",
        1,
        SourceAnchor::provider_native(
            "session-file",
            TypedKey::utf8("move-backed-document-test").unwrap(),
        )
        .unwrap(),
    )
    .unwrap()
}

fn core_record(source: &SourceKey) -> CoreRecord {
    let session_key =
        NativeSessionKey::native_id("session", TypedKey::utf8("session").unwrap()).unwrap();
    let session_id = derive_session_id(SessionIdentityInput {
        source,
        logical_session_kind: "thread",
        native_session_key: &session_key,
    })
    .unwrap();
    let native_item_key = NativeItemKey::native_id("message", TypedKey::U64(1)).unwrap();
    let event_id = derive_event_id(EventIdentityInput {
        source,
        session_id,
        logical_item_kind: "message",
        native_item_key: &native_item_key,
        subrecord_selector: None,
    })
    .unwrap();
    let mut record = CoreRecord::new_selected(
        event_id,
        session_id,
        source.clone(),
        1,
        "message",
        "index-document-test-v1",
        "body",
    )
    .unwrap();
    record.native_event_id = Some(TypedKey::U64(1));
    record
}

#[test]
fn move_backed_values_match_tantivy_document_field_semantics() {
    let schema = lexical_schema();
    let fields = fields_from_schema(&schema).unwrap();
    let body = "move-backed body".repeat(512);
    let body_pointer = body.as_ptr();
    let source = Arc::<str>::from("shared-source-token");
    let source_pointer = source.as_ptr();
    let bytes = vec![7_u8; 113];
    let bytes_pointer = bytes.as_ptr();

    let mut actual = IndexDocument::with_capacity(7);
    actual.add_text(fields.body_search, body);
    actual.add_shared_text(fields.source_key, Arc::clone(&source));
    actual.add_bytes(fields.core_record, bytes);
    actual.add_u64(fields.event_sequence, 42);
    actual.add_i64(fields.occurred_at_unix_ms, -9);
    actual.add_text(fields.fact_file, "first.rs".to_owned());
    actual.add_text(fields.fact_file, "second.rs".to_owned());

    assert!(actual.fields.iter().any(|(field, value)| {
        *field == fields.body_search
            && matches!(value, IndexValue::Text(value) if value.as_ptr() == body_pointer)
    }));
    assert!(actual.fields.iter().any(|(field, value)| {
        *field == fields.source_key
            && matches!(value, IndexValue::SharedText(value) if value.as_ptr() == source_pointer)
    }));
    assert!(actual.fields.iter().any(|(field, value)| {
        *field == fields.core_record
            && matches!(value, IndexValue::Bytes(value) if value.as_ptr() == bytes_pointer)
    }));

    let mut expected = TantivyDocument::default();
    expected.add_text(fields.body_search, "move-backed body".repeat(512));
    expected.add_text(fields.source_key, source.as_ref());
    expected.add_bytes(fields.core_record, &[7_u8; 113]);
    expected.add_u64(fields.event_sequence, 42);
    expected.add_i64(fields.occurred_at_unix_ms, -9);
    expected.add_text(fields.fact_file, "first.rs");
    expected.add_text(fields.fact_file, "second.rs");

    assert_eq!(
        serde_json::to_value(actual.to_named_doc(&schema)).unwrap(),
        serde_json::to_value(expected.to_named_doc(&schema)).unwrap()
    );
}

#[test]
fn stack_source_token_matches_the_persisted_token_encoding() {
    let digest = [0xa5; 32];
    let token = SourceToken::new(&digest);
    assert_eq!(token.as_str().unwrap(), crate::hex(&digest));
}

#[test]
fn core_content_accounting_preserves_the_index_maximum_for_direct_callers() {
    let source = source("codex_session_jsonl");
    let mut record = core_record(&source);
    record.content.normalized_body = Some("x".repeat(MAX_CORE_CONTENT_BYTES + 1));

    assert!(matches!(
        core_content_bytes(&record.content),
        Err(IndexError::DocumentFieldTooLarge {
            field: "core_content",
            actual,
            maximum: MAX_CORE_CONTENT_BYTES,
        }) if actual == MAX_CORE_CONTENT_BYTES + 1
    ));
}

#[test]
fn provider_native_copy_keeps_exact_fields_and_remains_discoverable() {
    let schema = lexical_schema();
    let fields = fields_from_schema(&schema).unwrap();
    let source = source("codex_session_jsonl");
    let mut record = core_record(&source);
    let ancestor_session_key =
        NativeSessionKey::native_id("session", TypedKey::utf8("ancestor-session").unwrap())
            .unwrap();
    let ancestor_session_id = derive_session_id(SessionIdentityInput {
        source: &source,
        logical_session_kind: "thread",
        native_session_key: &ancestor_session_key,
    })
    .unwrap();
    let ancestor_item_key = NativeItemKey::native_id("message", TypedKey::U64(9)).unwrap();
    let ancestor_event_id = derive_event_id(EventIdentityInput {
        source: &source,
        session_id: ancestor_session_id,
        logical_item_kind: "message",
        native_item_key: &ancestor_item_key,
        subrecord_selector: None,
    })
    .unwrap();
    record.event_copy = Some(ProviderNativeEventCopy {
        ancestor_session_id,
        ancestor_event_id,
        proof: ProviderNativeCopyProof::NativeCopiedFromField,
    });
    let expected_event_id = record.event_id.to_string();
    let expected_session_id = record.session_id.to_string();
    let expected_body = record.content.normalized_body.clone();
    let encoded = record.encode_stored().unwrap();
    let content_bytes = core_content_bytes(&record.content).unwrap();
    let document = IndexDocument::from_core(fields, record, encoded, content_bytes)
        .unwrap()
        .into_tantivy_document();

    assert_eq!(
        document
            .get_first(fields.body_search)
            .and_then(|value| value.as_str()),
        Some("body")
    );
    assert_eq!(
        document
            .get_first(fields.event_id)
            .and_then(|value| value.as_str()),
        Some(expected_event_id.as_str())
    );
    assert_eq!(
        document
            .get_first(fields.session_id)
            .and_then(|value| value.as_str()),
        Some(expected_session_id.as_str())
    );
    assert_eq!(
        document
            .get_first(fields.event_copy_ancestor_event_id)
            .and_then(|value| value.as_str()),
        Some(ancestor_event_id.to_string().as_str())
    );
    assert_eq!(
        document
            .get_first(fields.event_copy_proof)
            .and_then(|value| value.as_str()),
        Some("native_copied_from_field")
    );
    assert_eq!(
        document
            .get_first(fields.discovery_eligible)
            .and_then(|value| value.as_u64()),
        Some(1)
    );
    let stored = document
        .get_first(fields.core_record)
        .and_then(|value| value.as_bytes())
        .map(CoreRecord::decode_stored)
        .unwrap()
        .unwrap();
    assert_eq!(stored.content.normalized_body, expected_body);
}

#[test]
fn session_authority_attachment_is_canonical_and_one_shot() {
    let schema = lexical_schema();
    let fields = fields_from_schema(&schema).unwrap();
    let core_source = source("codex_session_jsonl");
    let record = core_record(&core_source);
    let expected = SessionAuthorityKey::for_core_record(&record)
        .unwrap()
        .into_bytes();
    let encoded = record.encode_stored().unwrap();
    let content_bytes = core_content_bytes(&record.content).unwrap();
    let mut document = IndexDocument::from_core(fields, record, encoded, content_bytes).unwrap();

    document.add_session_authority(fields);
    document.add_session_authority(fields);

    let document = document.into_tantivy_document();
    let authorities = document
        .get_all(fields.session_authority)
        .filter_map(|value| value.as_bytes())
        .collect::<Vec<_>>();
    assert_eq!(authorities, vec![expected.as_slice()]);
}

#[test]
fn session_authority_exact_key_binds_full_session_and_source_identities() {
    let core_source = source("codex_session_jsonl");
    let record = core_record(&core_source);
    let exact = SessionAuthorityKey::exact(record.session_id, core_source.identity()).unwrap();
    assert_eq!(exact.as_bytes().len(), crate::SESSION_AUTHORITY_KEY_LEN);
    assert_eq!(
        SessionAuthorityKey::decode(exact.as_bytes())
            .unwrap()
            .identities()
            .unwrap(),
        (record.session_id, core_source.identity())
    );

    let mut colliding = record.session_id.encode_canonical().unwrap();
    colliding[20] ^= 1;
    let colliding = StableEntityId::decode_canonical(&colliding).unwrap();
    assert_eq!(colliding.as_uuid(), record.session_id.as_uuid());
    assert_ne!(colliding.digest(), record.session_id.digest());
    let colliding_key = SessionAuthorityKey::exact(colliding, core_source.identity()).unwrap();
    assert_ne!(exact, colliding_key);

    let foreign_source = SourceKey::derive(
        "codex",
        "codex_session_jsonl",
        "session",
        1,
        SourceAnchor::provider_native(
            "session-file",
            TypedKey::utf8("foreign-session-source").unwrap(),
        )
        .unwrap(),
    )
    .unwrap();
    assert!(matches!(
        SessionAuthorityKey::exact(record.session_id, foreign_source.identity()),
        Err(IndexError::InvalidStoredDocumentField("session_authority"))
    ));
}

#[test]
fn session_authority_key_round_trips_only_direct_literal_claims() {
    let core_source = source("codex_session_jsonl");
    let mut record = core_record(&core_source);
    let parent = record.session_id;
    let root = record.session_id;
    record.parent_session_id = Some(parent);
    record.root_session_id = Some(root);
    record.session_relationship = Some(ProviderNativeSessionRelationship::Forked);

    let key = SessionAuthorityKey::for_core_record(&record).unwrap();
    let decoded = SessionAuthorityKey::decode(key.as_bytes()).unwrap();
    assert_eq!(
        decoded.identities().unwrap(),
        (record.session_id, core_source.identity())
    );
    assert_eq!(
        decoded.direct_claims().unwrap(),
        SessionAuthorityClaims {
            parent_session_id: Some(parent),
            root_session_id: Some(root),
            relationship: Some(ProviderNativeSessionRelationship::Forked),
        }
    );
}

#[test]
fn session_authority_key_rejects_noncanonical_direct_claim_encodings() {
    let core_source = source("codex_session_jsonl");
    let record = core_record(&core_source);
    let exact = SessionAuthorityKey::exact(record.session_id, core_source.identity()).unwrap();

    let mut absent_claim_with_payload = exact.into_bytes();
    absent_claim_with_payload[SESSION_AUTHORITY_PARENT_OFFSET] = 1;
    assert!(matches!(
        SessionAuthorityKey::decode(&absent_claim_with_payload),
        Err(IndexError::InvalidStoredDocumentField("session_authority"))
    ));

    let mut invalid_relationship = exact.into_bytes();
    invalid_relationship[SESSION_AUTHORITY_RELATIONSHIP_OFFSET] = u8::MAX;
    assert!(matches!(
        SessionAuthorityKey::decode(&invalid_relationship),
        Err(IndexError::InvalidStoredDocumentField("session_authority"))
    ));
}

#[test]
fn source_event_order_key_has_exact_source_order_and_size_layout() {
    let source = source("codex_session_jsonl");
    let record = core_record(&source);
    let core_record_bytes = record.encode_stored().unwrap();
    let content_bytes = core_content_bytes(&record.content).unwrap();
    let index_source = IndexSourceFields::new(&source, &crate::source_token(&source));
    let key = SourceEventOrderKey::for_document(
        &index_source,
        record.event_id.digest(),
        core_record_bytes.len(),
        content_bytes,
    )
    .unwrap()
    .into_bytes();

    assert_eq!(&key[..32], &source.identity().digest());
    assert_eq!(
        &key[32..SOURCE_EVENT_ORDER_SOURCE_PREFIX_LEN],
        &source.exact_descriptor_digest()
    );
    assert_eq!(
        &key[SOURCE_EVENT_ORDER_EVENT_DIGEST_OFFSET..SOURCE_EVENT_ORDER_ENCODED_BYTES_OFFSET],
        &record.event_id.digest()
    );
    assert_eq!(
        u32::from_be_bytes(
            key[SOURCE_EVENT_ORDER_ENCODED_BYTES_OFFSET..SOURCE_EVENT_ORDER_CONTENT_BYTES_OFFSET]
                .try_into()
                .unwrap()
        ) as usize,
        core_record_bytes.len()
    );
    assert_eq!(
        u32::from_be_bytes(
            key[SOURCE_EVENT_ORDER_CONTENT_BYTES_OFFSET..]
                .try_into()
                .unwrap()
        ) as usize,
        content_bytes
    );
}

#[test]
fn session_event_order_key_matches_deterministic_session_coordinates() {
    let source = source("codex_session_jsonl");
    let mut record = core_record(&source);
    record.event_sequence = 42;
    record.occurred_at_unix_ms = Some(-9);
    let key = SessionEventOrderKey::for_core_record(&record).unwrap();

    assert_eq!(
        &key.as_bytes()[..SESSION_EVENT_ORDER_SESSION_PREFIX_LEN],
        &record.session_id.encode_canonical().unwrap()
    );
    assert_eq!(key.event_sequence(), 42);
    assert_eq!(key.occurred_at_unix_ms(), Some(-9));
    assert_eq!(key.event_id(), record.event_id.as_uuid());
    assert!(
        SessionEventOrderKey::session_range_end(record.session_id)
            .unwrap()
            .as_slice()
            > key.as_bytes().as_slice()
    );
}
