use super::*;

#[test]
fn opencode_family_projects_full_bodies_and_hydrates_typed_sqlite_rows() {
    for registration in opencode::opencode_family_source_backed_registrations() {
        let temp = crate::test_support_paths::tempdir().unwrap();
        let path = temp
            .path()
            .join(format!("{}.sqlite", registration.provider().as_str()));
        let body = long_body(registration.provider().as_str());
        create_opencode_session_message_database(&path, &[&body]);

        let documents = collect_opencode_documents(registration, &path);
        assert_eq!(documents.len(), 1);
        let document = &documents[0];
        assert_eq!(document.body, body);
        assert!(document.body.len() > PROVIDER_MAX_TEXT_CHARS);
        assert_eq!(
            document.source.source_format(),
            registration.source_format()
        );
        assert_eq!(
            document.locator.revision_policy(),
            LocatorRevisionPolicy::ExactSourceRevision
        );
        let NativeRecordCoordinate::ProviderSqlite {
            logical_relation,
            primary_key,
            row_version,
        } = document.locator.coordinate()
        else {
            panic!("expected provider SQLite locator")
        };
        assert_eq!(logical_relation, "session_message");
        assert_eq!(primary_key, &TypedKey::Utf8("message-0".to_owned()));
        assert!(matches!(
            row_version,
            Some(TypedKey::Composite(parts)) if parts.len() == 2
        ));

        let locator_json = serde_json::to_string(&document.locator).unwrap();
        assert!(!locator_json.contains(path.to_string_lossy().as_ref()));
        assert!(!locator_json.contains(&body));

        let hydrated = registration
            .exact_resolver(&path)
            .hydrate_event(&event_request(document))
            .unwrap();
        assert_eq!(hydrated.provider_bytes, body.as_bytes());
    }
}

#[test]
fn lingma_source_backed_prompt_hydrates_and_changed_row_fails_closed() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let path = temp.path().join("local.db");
    let body = long_body("Lingma source-backed prompt");
    create_lingma_database(&path, &body);

    let source =
        lingma::LingmaDatabaseSourceV0::new(&path, TypedKey::utf8("vscode:stable:test").unwrap())
            .unwrap();
    let inventory = lingma::LingmaSourceInventoryV0::new(
        TypedKey::utf8("installed-clients").unwrap(),
        vec![source],
    )
    .unwrap();
    let closing = inventory.clone();
    let scan = lingma::scan_lingma_source_backed_v0(inventory.clone(), || Ok(closing)).unwrap();
    let record = scan.databases()[0]
        .records()
        .iter()
        .find(|record| record.document().role.as_deref() == Some("user"))
        .unwrap();
    let document = record.document();
    assert_eq!(document.body, body);
    assert!(matches!(
        document.locator.coordinate(),
        NativeRecordCoordinate::ProviderSqlite {
            logical_relation,
            primary_key: TypedKey::Composite(_),
            row_version: Some(TypedKey::Bytes(digest)),
        } if logical_relation == "chat_record" && digest.len() == 32
    ));

    let resolver = lingma::LingmaSourceBackedResolverV0::new(&inventory).unwrap();
    assert_eq!(
        resolver.hydrate_record(record).unwrap().provider_bytes,
        body.as_bytes()
    );

    Connection::open(&path)
        .unwrap()
        .execute(
            "update chat_record set chat_prompt = ?1 where request_id = 'lingma-request'",
            ["changed Lingma prompt"],
        )
        .unwrap();
    let failure = resolver.hydrate_record(record).unwrap_err();
    assert_eq!(failure.kind, HydrationFailureKind::StaleSourceEvidence);
}

#[test]
fn astrbot_source_backed_conversation_hydrates_the_original_typed_item() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let home = temp.path().join("home");
    let cwd = temp.path().join("core");
    fs::create_dir_all(&home).unwrap();
    let path = cwd.join("data/data_v4.db");
    let body = long_body("AstrBot source-backed conversation");
    create_astrbot_database(&path, "astrbot-session", &body);

    let context = astrbot_discovery_context(&home, &cwd);
    let inventory =
        astrbot::native_path::source_backed::AstrBotSourceBackedInventoryV0::discover(&context)
            .unwrap();
    let source = inventory.sources().first().unwrap();
    let mut documents = Vec::new();
    astrbot::native_path::source_backed::scan_astrbot_source_backed_v0(source, &mut |document| {
        documents.push(document);
        Ok(())
    })
    .unwrap();
    let document = documents
        .iter()
        .find(|document| document.body == body)
        .unwrap();
    assert!(matches!(
        document.locator.coordinate(),
        NativeRecordCoordinate::ProviderSqlite {
            primary_key: TypedKey::Composite(parts),
            row_version: Some(TypedKey::Bytes(digest)),
            ..
        } if parts == &vec![TypedKey::I64(1), TypedKey::U64(0)] && digest.len() == 32
    ));

    let resolver =
        astrbot::native_path::source_backed::AstrBotSourceBackedResolverV0::from_inventory(
            &inventory,
        )
        .unwrap();
    let hydrated = resolver.hydrate_event(&event_request(document)).unwrap();
    assert_eq!(hydrated.provider_bytes, body.as_bytes());

    Connection::open(&path)
        .unwrap()
        .execute(
            "update conversations set content = ?1 where id = 1",
            [json!([{
                "id": "message-astrbot-session",
                "role": "user",
                "content": "changed AstrBot content",
            }])
            .to_string()],
        )
        .unwrap();
    let failure = resolver
        .hydrate_event(&event_request(document))
        .unwrap_err();
    assert_eq!(failure.kind, HydrationFailureKind::StaleSourceEvidence);
}

#[test]
fn trae_source_backed_nested_message_hydrates_without_parent_value_retention() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let workspace = temp.path().join("workspace");
    let path = workspace.join("state.vscdb");
    let body = long_body("Trae source-backed nested message");
    create_trae_database(&path, &body);

    let mut documents = Vec::new();
    let scan = trae::scan_trae_source_backed_explicit_v0(&path, &mut |page| {
        documents.extend(page.documents);
        Ok(())
    })
    .unwrap();
    assert_eq!(scan.source.counts().indexed_documents, 1);
    let document = documents.first().unwrap();
    assert_eq!(document.body, body);
    assert!(matches!(
        document.locator.coordinate(),
        NativeRecordCoordinate::ProviderNative {
            namespace,
            coordinate: TypedKey::Composite(parts),
        } if namespace == "trae.itemtable-json-message-v1" && parts.len() == 6
    ));
    let locator_json = serde_json::to_string(&document.locator).unwrap();
    assert!(!locator_json.contains(path.to_string_lossy().as_ref()));
    assert!(!locator_json.contains(&body));

    let hydrated = trae::hydrate_trae_source_backed_locator_v0(&path, &document.locator).unwrap();
    assert_eq!(hydrated.exact_text, body);

    replace_trae_value(&path, "changed Trae body");
    let failure =
        trae::hydrate_trae_source_backed_locator_v0(&path, &document.locator).unwrap_err();
    assert!(matches!(
        failure,
        trae::TraeSourceBackedErrorV0::SourceRevisionMismatch
            | trae::TraeSourceBackedErrorV0::LocatorValueDigestMismatch
    ));
}
