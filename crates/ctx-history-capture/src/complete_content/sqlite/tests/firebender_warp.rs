use super::*;

#[test]
fn firebender_source_backed_row_recovers_unicode_multiline_body_and_digest() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let path = temp.path().join("chat_history.db");
    let body = long_body("Firebender source-backed body");
    let conn = Connection::open(&path).unwrap();
    conn.execute_batch(
        "create table chat_sessions (
             id text not null,
             name text not null,
             created_at integer not null,
             updated_at integer not null,
             messages_json text not null,
             metadata_json text not null
         );",
    )
    .unwrap();
    conn.execute(
        "insert into chat_sessions values (
             'firebender-session', 'Source-backed fixture', 1783653514000, 1783653514001,
             ?1, '{}'
         )",
        [json!([{
            "id": "firebender-message",
            "role": "user",
            "timestamp": 1783653514000_i64,
            "content": {"type": "text", "text": body},
        }])
        .to_string()],
    )
    .unwrap();
    drop(conn);

    let mut scanner =
        match firebender::native_path::prepare_firebender_source_backed(&path, None).unwrap() {
            firebender::native_path::FirebenderSourceBackedPlan::Replacement(scanner) => scanner,
            firebender::native_path::FirebenderSourceBackedPlan::Exact(_) => {
                panic!("first source-backed scan cannot be exact")
            }
        };
    let mut documents = Vec::new();
    while let Some(page) = scanner.next_page().unwrap() {
        documents.extend(page.into_documents());
    }
    let certificate = scanner.finish().unwrap();
    assert_eq!(certificate.counts().indexed_documents, 1);
    let document = documents.first().unwrap();
    assert_eq!(document.body.chars().count(), PROVIDER_MAX_TEXT_CHARS);
    assert!(body.starts_with(&document.body));
    assert!(matches!(
        document.locator.coordinate(),
        NativeRecordCoordinate::ProviderSqlite {
            logical_relation,
            primary_key: TypedKey::I64(1),
            row_version: Some(TypedKey::Composite(parts)),
        } if logical_relation == "chat_sessions.messages_json"
            && matches!(
                parts.as_slice(),
                [
                    TypedKey::Utf8(session_id),
                    TypedKey::I64(1783653514001),
                    TypedKey::U64(0),
                ] if session_id == "firebender-session"
            )
    ));

    let hydrated =
        firebender::native_path::hydrate_firebender_source_backed_row(&path, &document.locator)
            .unwrap();
    assert_eq!(hydrated.provider_session_id(), "firebender-session");
    assert_eq!(firebender_message_from_hydrated_row(&hydrated), body);
}
