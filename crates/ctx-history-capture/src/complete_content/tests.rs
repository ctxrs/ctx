use super::*;

fn request() -> CompleteMessageRequest {
    CompleteMessageRequest {
        event_id: Uuid::parse_str("018f45d0-0000-7000-8000-000000000010").unwrap(),
        provider: CaptureProvider::Codex,
        source_format: "fixture".to_owned(),
        raw_source_path: PathBuf::from("fixture.jsonl"),
        source_root: None,
        source_identity: Some("source-1".to_owned()),
        source_family: Some(CompleteContentSourceFamily::Fixture),
        source_locator: None,
        source_snapshot: SourceSnapshot::default(),
        provider_session_id: Some("session-1".to_owned()),
        source_record_ordinal: 0,
        source_record_subrecord_index: 0,
        expected_provider_event_hash: "event-1".to_owned(),
        expected_hash_authority: CompleteContentHashAuthority::ProviderSupplied,
        expected_native_record_id: Some("event-1".to_owned()),
        expected_record_digest: None,
        expected_body_digest: None,
        indexed_text: "1234".to_owned(),
        indexed_limit_chars: 4,
    }
}

#[test]
fn verified_message_requires_exact_character_prefix() {
    let request = request();
    let message = CompleteMessage::verified(
        &request,
        "1234tail".to_owned(),
        SourceVerification::VERIFIED,
    )
    .unwrap();
    assert_eq!(message.text, "1234tail");

    let error = CompleteMessage::verified(
        &request,
        "123Xtail".to_owned(),
        SourceVerification::VERIFIED,
    )
    .unwrap_err();
    assert_eq!(
        error.kind,
        CompleteContentErrorKind::ContentVerificationFailed
    );
}

#[test]
fn errors_are_typed_and_path_free() {
    let error =
        CompleteContentError::new(CompleteContentErrorKind::SourceMissing, request().event_id);
    let rendered = error.to_string();
    assert!(rendered.contains("source_missing"));
    assert!(rendered.contains("ctx locate event"));
    assert!(!rendered.contains("fixture.jsonl"));
}

#[test]
fn persisted_locator_is_versioned_bounded_and_round_trips() {
    let record = CompleteContentBodyDigest::from_text("record");
    let body = CompleteContentBodyDigest::from_text("body");
    let locator = PersistedCompleteContentLocatorV1::new(
        CompleteContentSourceFamily::Jsonl,
        "jsonl-range-v1",
        &[0; 16],
        "native-event-1",
        record.clone(),
        body.clone(),
    )
    .unwrap();
    let value = locator.to_metadata_value();
    assert!(serde_json::to_vec(&value).unwrap().len() <= COMPLETE_CONTENT_MAX_LOCATOR_BYTES);
    let decoded = PersistedCompleteContentLocatorV1::from_metadata_value(&value).unwrap();
    assert_eq!(decoded.family(), CompleteContentSourceFamily::Jsonl);
    assert_eq!(decoded.native_record_id(), "native-event-1");
    assert_eq!(decoded.record_sha256(), &record);
    assert_eq!(decoded.body_sha256(), &body);
    assert_eq!(decoded.source_locator().unwrap().value(), &[0; 16]);
}
