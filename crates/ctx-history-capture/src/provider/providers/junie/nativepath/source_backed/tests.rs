use super::*;

fn write_tree(root: &Path, session_id: &str, records: &[Value]) {
    let session = root.join(session_id);
    std::fs::create_dir_all(&session).unwrap();
    std::fs::write(
        root.join("index.jsonl"),
        format!(
            "{}\n",
            serde_json::json!({
                "sessionId": session_id,
                "createdAt": 1_783_339_200_000_i64,
                "taskName": "Junie source-backed fixture",
                "projectDir": "/workspace/junie",
            })
        ),
    )
    .unwrap();
    let mut events = String::new();
    for record in records {
        events.push_str(&serde_json::to_string(record).unwrap());
        events.push('\n');
    }
    std::fs::write(session.join(RELATIVE_EVENTS_FILE), events).unwrap();
}

fn scan_documents(root: &Path) -> Vec<LexicalDocument> {
    let mut scanner =
        JunieSourceBackedScannerV0::discover(root, DateTime::<Utc>::UNIX_EPOCH).unwrap();
    let mut began = 0;
    let mut certified = 0;
    let mut documents = Vec::new();
    while let Some(page) = scanner.next_page().unwrap() {
        match page {
            JunieSourceBackedEmissionV0::BeginSource(_) => began += 1,
            JunieSourceBackedEmissionV0::Documents(page) => documents.extend(page),
            JunieSourceBackedEmissionV0::CertifiedSource(source) => {
                assert_eq!(source.counts().indexed_documents, documents.len() as u64);
                certified += 1;
            }
        }
    }
    assert_eq!(began, 1);
    assert_eq!(certified, 1);
    documents
}

fn hydrate(
    root: &Path,
    document: &LexicalDocument,
) -> Result<HydratedProviderRecord, HydrationFailure> {
    let resolver = JunieLocatorResolverV0::discover(root).unwrap();
    let request = EventHydrationRequest::new(document.event_id, document.locator.clone()).unwrap();
    resolver.hydrate_event(&request)
}

fn request(document: &LexicalDocument) -> EventHydrationRequest {
    EventHydrationRequest::new(document.event_id, document.locator.clone()).unwrap()
}

#[test]
fn junie_exact_hydration_indexes_full_body_tail_terms_and_is_session_ordered() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let first = format!("first-head-{}-first-tail", "a".repeat(4_096));
    let second = format!("second-head-{}-second-tail", "b".repeat(4_096));
    write_tree(
        temp.path(),
        "ordered-session",
        &[
            serde_json::json!({
                "kind": "UserPromptEvent",
                "prompt": first.clone(),
            }),
            serde_json::json!({
                "kind": "UserPromptEvent",
                "prompt": second.clone(),
            }),
        ],
    );
    let documents = scan_documents(temp.path());
    assert_eq!(documents.len(), 2);
    assert!(documents.iter().all(|document| {
        document
            .locator
            .certified_source_revision_digest()
            .is_some()
    }));
    assert_eq!(documents[0].body, first);
    assert_eq!(documents[1].body, second);
    assert!(documents[0].body.contains("first-tail"));
    assert!(documents[1].body.contains("second-tail"));

    let session_request = SessionHydrationRequest::new(
        documents[0].session_id,
        vec![request(&documents[1]), request(&documents[0])],
    )
    .unwrap();
    let hydrated = JunieLocatorResolverV0::discover_for_hydration(temp.path())
        .unwrap()
        .hydrate_session(&session_request)
        .unwrap()
        .into_iter()
        .map(|record| String::from_utf8(record.provider_bytes).unwrap())
        .collect::<Vec<_>>();
    assert_eq!(hydrated, vec![second, first]);
}

#[test]
fn junie_rewrite_digest_and_truncation_have_distinct_typed_failures() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let original = [
        serde_json::json!({
            "kind": "UserPromptEvent",
            "prompt": "before",
        }),
        serde_json::json!({
            "kind": "UserPromptEvent",
            "prompt": "second",
        }),
    ];
    write_tree(temp.path(), "mutable-session", &original);
    let documents = scan_documents(temp.path());
    let first_request = request(&documents[0]);
    let second_request = request(&documents[1]);

    write_tree(
        temp.path(),
        "mutable-session",
        &[
            serde_json::json!({
                "kind": "UserPromptEvent",
                "prompt": "rewrit",
            }),
            original[1].clone(),
        ],
    );
    let stale = JunieLocatorResolverV0::discover_for_hydration(temp.path())
        .unwrap()
        .hydrate_event(&first_request)
        .unwrap_err();
    assert_eq!(stale.kind, HydrationFailureKind::StaleRecordEvidence);

    write_tree(temp.path(), "mutable-session", &original[..1]);
    let missing = JunieLocatorResolverV0::discover_for_hydration(temp.path())
        .unwrap()
        .hydrate_event(&second_request)
        .unwrap_err();
    assert_eq!(missing.kind, HydrationFailureKind::MissingRecord);
    let batch =
        BatchHydrationRequest::new(vec![first_request.clone(), second_request.clone()]).unwrap();
    let failed_batch = JunieLocatorResolverV0::discover_for_hydration(temp.path())
        .unwrap()
        .hydrate_batch(&batch)
        .unwrap_err();
    assert_eq!(failed_batch.kind, HydrationFailureKind::MissingRecord);
}

#[test]
fn junie_source_deletion_and_unavailable_root_are_not_conflated() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    write_tree(
        temp.path(),
        "deleted-session",
        &[serde_json::json!({
            "kind": "UserPromptEvent",
            "prompt": "present",
        })],
    );
    let documents = scan_documents(temp.path());
    let event_request = request(&documents[0]);

    std::fs::remove_dir_all(temp.path().join("deleted-session")).unwrap();
    let deleted = JunieLocatorResolverV0::discover_for_hydration(temp.path())
        .unwrap()
        .hydrate_event(&event_request)
        .unwrap_err();
    assert_eq!(deleted.kind, HydrationFailureKind::ConfirmedDeleted);

    let unavailable_root = temp.path().join("unavailable-root");
    let unavailable =
        JunieLocatorResolverV0::discover_for_hydration(&unavailable_root).unwrap_err();
    assert_eq!(
        unavailable.kind,
        HydrationFailureKind::TemporarilyUnavailable
    );
}

#[test]
fn junie_digest_matching_malformed_native_record_is_unsupported() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    write_tree(
        temp.path(),
        "malformed-session",
        &[serde_json::json!({
            "kind": "UserPromptEvent",
            "prompt": "present",
        })],
    );
    let document = scan_documents(temp.path()).pop().unwrap();
    let NativeRecordCoordinate::Jsonl {
        physical_ordinal,
        native_session_key,
        native_event_key,
        ..
    } = document.locator.coordinate()
    else {
        panic!("Junie prompt must use a JSONL coordinate");
    };
    let malformed_payload = b"{not-json}";
    let locator = SourceRecordLocator::new(
        document.source.clone(),
        NativeRecordCoordinate::Jsonl {
            byte_offset: 0,
            byte_length: u64::try_from(malformed_payload.len() + 1).unwrap(),
            physical_ordinal: *physical_ordinal,
            native_session_key: native_session_key.clone(),
            native_event_key: native_event_key.clone(),
        },
        LocatorRevisionPolicy::StableRecordEvidence,
        document.locator.certified_source_revision_digest().copied(),
        Sha256::digest(malformed_payload).into(),
    )
    .unwrap();
    std::fs::write(
        temp.path().join("malformed-session/events.jsonl"),
        [malformed_payload.as_slice(), b"\n"].concat(),
    )
    .unwrap();
    let event_request = EventHydrationRequest::new(document.event_id, locator).unwrap();
    let failure = JunieLocatorResolverV0::discover_for_hydration(temp.path())
        .unwrap()
        .hydrate_event(&event_request)
        .unwrap_err();
    assert_eq!(
        failure.kind,
        HydrationFailureKind::UnsupportedParserRevision
    );
}

#[test]
fn junie_source_backed_has_no_preview_complete_or_legacy_store_fallback() {
    let provider_source = [
        include_str!("../source_backed.rs"),
        include_str!("../source_backed/resolver.rs"),
    ]
    .concat();
    let nativepath_source = include_str!("../../nativepath.rs");
    let projection_source = include_str!("../projection.rs");
    let registry_source = include_str!("../../../../source_backed.rs");
    for source in [provider_source.as_str(), projection_source] {
        for forbidden in [
            ["ctx_history_", "store"].concat(),
            ["Store", "::"].concat(),
            ["import_junie_", "nativepath("].concat(),
            ["provider_bytes: ", "document.body"].concat(),
            ["provider_local_preview(&", "row.text"].concat(),
        ] {
            assert!(
                !source.contains(&forbidden),
                "Junie source-backed route contains forbidden architecture token {forbidden:?}"
            );
        }
    }
    assert!(provider_source.contains("exact_junie_lexical_body"));
    assert!(provider_source.contains("fn hydrate_batch("));
    assert!(provider_source.contains("self.hydrate_requests(request.events())"));
    assert!(!nativepath_source.contains("mod output;"));
    assert!(!nativepath_source.contains("mod core;"));
    assert!(!nativepath_source.contains("mod cursor;"));
    assert!(!nativepath_source.contains("mod lifecycle;"));
    assert!(!nativepath_source.contains("mod publication;"));
    let legacy_store_type = ["ctx_history_", "store::Store"].concat();
    assert_eq!(
        nativepath_source.matches(&legacy_store_type).count(),
        0,
        "Junie production code must not retain a Store compatibility shim"
    );
    assert!(!nativepath_source.contains("legacy Store publication"));
    assert!(registry_source.contains("JunieLocatorResolverV0::discover_for_hydration"));
    assert!(registry_source.contains(".with_batch_hydration(move |request|"));
}

#[test]
fn junie_source_backed_ordinary_record_exact_show_fixture() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let prompt = "ordinary exact Junie prompt ☃\nsecond line";
    write_tree(
        temp.path(),
        "ordinary-session",
        &[serde_json::json!({
            "kind": "UserPromptEvent",
            "prompt": prompt,
        })],
    );
    let documents = scan_documents(temp.path());
    assert_eq!(documents.len(), 1);
    assert_eq!(documents[0].parent_session_id, None);
    assert_eq!(documents[0].root_session_id, documents[0].session_id);
    assert_eq!(
        documents[0].provider_session_id.as_deref(),
        Some("ordinary-session")
    );
    assert_eq!(documents[0].branch, None);
    assert!(documents[0]
        .source_path
        .as_deref()
        .is_some_and(|path| path.ends_with("/ordinary-session/events.jsonl")));
    assert_eq!(documents[0].agent_type, AgentType::Primary.as_str());
    assert!(documents[0].is_primary);
    assert_eq!(documents[0].workspace.as_deref(), Some("/workspace/junie"));
    assert_eq!(documents[0].cwd.as_deref(), Some("/workspace/junie"));
    assert!(matches!(
        documents[0].locator.coordinate(),
        NativeRecordCoordinate::Jsonl { .. }
    ));
    let hydrated = hydrate(temp.path(), &documents[0]).unwrap();
    assert_eq!(hydrated.provider_bytes, prompt.as_bytes());
}

#[test]
fn junie_source_backed_record_set_exact_show_fixture() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let records = [
        serde_json::json!({
            "kind": "SessionA2uxEvent",
            "timestampMs": 1_783_339_200_001_i64,
            "event": {"agentEvent": {
                "kind": "ResultBlockUpdatedEvent",
                "stepId": "a",
                "result": "first exact assistant block",
            }},
        }),
        serde_json::json!({
            "kind": "SessionA2uxEvent",
            "timestampMs": 1_783_339_200_002_i64,
            "event": {"agentEvent": {
                "kind": "ResultBlockUpdatedEvent",
                "stepId": "b",
                "result": "second exact assistant block",
            }},
        }),
    ];
    write_tree(temp.path(), "record-set-session", &records);
    let documents = scan_documents(temp.path());
    assert_eq!(documents.len(), 1);
    assert!(matches!(
        documents[0].locator.coordinate(),
        NativeRecordCoordinate::TreeRecord { .. }
    ));
    let hydrated = hydrate(temp.path(), &documents[0]).unwrap();
    assert_eq!(
        hydrated.provider_bytes,
        b"first exact assistant block\n\nsecond exact assistant block"
    );
}

#[test]
fn junie_source_backed_over_limit_turn_stays_indexed_and_typed_fails_show() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let records: Vec<_> = (0..=MAX_RECORD_SET_ENTRIES)
        .map(|index| {
            serde_json::json!({
                "kind": "SessionA2uxEvent",
                "timestampMs": 1_783_339_200_000_i64 + index as i64,
                "event": {"agentEvent": {
                    "kind": "ResultBlockUpdatedEvent",
                    "stepId": format!("{index:03}"),
                    "result": format!("bounded searchable part {index}"),
                }},
            })
        })
        .collect();
    write_tree(temp.path(), "over-limit-session", &records);
    let documents = scan_documents(temp.path());
    assert_eq!(documents.len(), 1);
    assert!(!documents[0].body.is_empty());
    assert!(matches!(
        documents[0].locator.coordinate(),
        NativeRecordCoordinate::ProviderNative { namespace, .. }
            if namespace == UNAVAILABLE_COORDINATE_NAMESPACE
    ));
    let failure = hydrate(temp.path(), &documents[0]).unwrap_err();
    assert_eq!(
        failure.kind,
        HydrationFailureKind::UnsupportedParserRevision
    );
    assert!(failure.detail.contains("at most 64 source records"));
}
