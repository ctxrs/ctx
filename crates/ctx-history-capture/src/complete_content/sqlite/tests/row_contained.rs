use super::*;

#[test]
fn kiro_source_backed_row_hydrates_each_typed_event_and_rejects_rewrite() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let path = temp.path().join("kiro.db");
    let user_body = long_body("Kiro source-backed user");
    let assistant_body = long_body("Kiro source-backed assistant");
    let value = json!({
        "history": [
            {
                "user": {
                    "timestamp": "2026-07-21T12:00:00Z",
                    "content": {"Prompt": {"prompt": user_body}},
                }
            },
            {
                "assistant": {
                    "timestamp": "2026-07-21T12:00:01Z",
                    "Response": {"content": assistant_body},
                }
            }
        ]
    })
    .to_string();
    let conn = Connection::open(&path).unwrap();
    conn.execute_batch(
        "create table conversations_v2 (
             key text not null,
             conversation_id text not null,
             value text not null,
             created_at integer,
             updated_at integer
         );",
    )
    .unwrap();
    conn.execute(
        "insert into conversations_v2 values (
             '/workspace', 'kiro-session', ?1, 1783653514000, 1783653514001
         )",
        [value],
    )
    .unwrap();
    drop(conn);

    let scan =
        kiro::native_path::scan_kiro_source_backed_v0(&path, KIRO_SQLITE_SOURCE_FORMAT).unwrap();
    assert_eq!(scan.certificate.counts().indexed_documents, 2);
    assert_eq!(
        scan.documents
            .iter()
            .map(|document| document.body.chars().count())
            .collect::<Vec<_>>(),
        vec![PROVIDER_MAX_TEXT_CHARS, PROVIDER_MAX_TEXT_CHARS]
    );
    assert!(user_body.starts_with(&scan.documents[0].body));
    assert!(assistant_body.starts_with(&scan.documents[1].body));
    assert!(scan.documents.iter().all(|document| matches!(
        document.locator.coordinate(),
        NativeRecordCoordinate::ProviderSqlite {
            logical_relation,
            primary_key: TypedKey::Composite(parts),
            row_version: None,
        } if logical_relation == "conversations_v2" && parts.len() == 2
    )));

    let resolver =
        kiro::native_path::KiroLocatorResolverV0::discover(&path, KIRO_SQLITE_SOURCE_FORMAT)
            .unwrap();
    for (document, expected) in scan.documents.iter().zip([&user_body, &assistant_body]) {
        let hydrated = resolver.hydrate(&document.locator).unwrap();
        assert_eq!(hydrated.decoded_display_text, *expected);
        assert_eq!(hydrated.provider_bytes, expected.as_bytes());
    }

    Connection::open(&path)
        .unwrap()
        .execute(
            "update conversations_v2 set value = ?1 where key = '/workspace'",
            [json!({
                "history": [{
                    "user": {
                        "content": {"Prompt": {"prompt": "rewritten Kiro body"}}
                    }
                }]
            })
            .to_string()],
        )
        .unwrap();
    assert!(matches!(
        resolver.hydrate(&scan.documents[0].locator),
        Err(kiro::native_path::KiroSourceBackedErrorV0::ConversationRowDigestMismatch)
    ));
}
