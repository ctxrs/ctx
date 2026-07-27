use super::*;

#[test]
fn firebender_recovers_unicode_escaped_multiline_bytes_and_retains_only_truncated_locator() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let path = temp.path().join("chat_history.db");
    let body = long_body("Firebender exact body");
    let (values, mut event) = create_firebender_database(&path, &body);
    assert_eq!(event.payload["text_retention"]["truncated"], true);

    let locator =
        NativeLocator::new(FIREBENDER_LOCATOR_KIND, 1_i64.to_be_bytes().to_vec()).unwrap();
    attach_test_sqlite_message_locator(
        &mut event,
        CaptureProvider::Firebender,
        FIREBENDER_SQLITE_SOURCE_FORMAT,
        &locator,
        &values,
        || body.clone(),
    )
    .unwrap();
    let persisted = VerifiedContentLocatorsV1::from_metadata_value(
        &event.metadata[VERIFIED_CONTENT_LOCATORS_METADATA_KEY],
    )
    .unwrap();
    let persisted = persisted.locator(VerifiedContentRole::MessageBody).unwrap();
    assert_eq!(persisted.family(), CompleteContentSourceFamily::Sqlite);
    assert_eq!(persisted.kind(), FIREBENDER_LOCATOR_KIND);
    assert_eq!(persisted.native_record_id(), "native-message-1");
    assert_eq!(
        persisted.record_sha256(),
        &sqlite_logical_record_digest(&values)
    );
    assert_eq!(
        persisted.content_ref(),
        &ContentRef::from_bytes(body.as_bytes()).unwrap()
    );

    let request = firebender_request(&path, &body, &values, &event);
    let messages = SqliteCompleteContentResolver::new()
        .resolve(&[request])
        .unwrap();
    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0].text.as_bytes(), body.as_bytes());
    assert!(messages[0].verification.is_verified());

    let short = "ordinary short message";
    let (_, mut short_event) = create_event_without_database(short);
    attach_test_sqlite_message_locator(
        &mut short_event,
        CaptureProvider::Firebender,
        FIREBENDER_SQLITE_SOURCE_FORMAT,
        &locator,
        &values,
        || short.to_owned(),
    )
    .unwrap();
    assert!(short_event
        .metadata
        .get(VERIFIED_CONTENT_LOCATORS_METADATA_KEY)
        .is_none());
}

#[test]
fn firebender_name_only_tool_message_never_gets_result_evidence() {
    let message = json!({
        "id": "name-only-result",
        "role": "tool",
        "name": "display-only-name",
        "tool_calls": [{"name": "display-only-tool-call"}],
    });
    assert!(firebender::firebender_result_content(&message).is_none());
    let event = firebender_event(SESSION_ID, 0, &message, DateTime::<Utc>::UNIX_EPOCH);
    assert!(event.payload.get("result_content_ref").is_none());
    assert!(event
        .metadata
        .get(VERIFIED_CONTENT_LOCATORS_METADATA_KEY)
        .is_none());
}
