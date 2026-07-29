use super::*;

// The fixture helper keeps all nine expected contract values explicit at call
// sites; a test-only argument struct would make failures less legible.
#[allow(clippy::too_many_arguments)]
pub(in super::super) fn assert_source_backed_fixture(
    adapter: DirectJsonlSourceAdapter,
    root: &Path,
    expected_native_session_id: &str,
    expected_body: &str,
    expected_record: &[u8],
    expected_parent_provider_session_id: Option<&str>,
    expected_root_provider_session_id: &str,
    expected_agent_type: &str,
    expected_is_primary: bool,
) {
    use ctx_history_core::NativeRecordCoordinate;

    let opening = adapter.discover(root).unwrap();
    assert!(!opening.root_missing());
    assert!(opening.failures().is_empty());
    assert_eq!(opening.leaves().len(), 1);
    let leaf = opening.leaves()[0].clone();
    let (documents, certified) = collect_test_leaf(adapter, &leaf);
    assert_eq!(
        certified.certificate().counts().indexed_documents,
        documents.len() as u64
    );
    assert_eq!(
        certified.certificate().counts().certified_bytes,
        leaf.source_file.len()
    );
    assert!(certified.certificate().frontier().is_some());
    let document = documents
        .iter()
        .find(|document| document.body.contains(expected_body))
        .unwrap();
    assert_eq!(
        document.provider_session_id.as_deref(),
        Some(expected_native_session_id)
    );
    let expected_parent_session_id = expected_parent_provider_session_id
        .map(|parent| direct_jsonl_session_identity(adapter, parent).unwrap().1);
    let expected_root_session_id =
        direct_jsonl_session_identity(adapter, expected_root_provider_session_id)
            .unwrap()
            .1;
    assert_eq!(document.parent_session_id, expected_parent_session_id);
    assert_eq!(document.root_session_id, expected_root_session_id);
    assert_eq!(document.agent_type, expected_agent_type);
    assert_eq!(document.is_primary, expected_is_primary);
    assert_eq!(document.branch, None);
    assert_eq!(document.source_path.as_deref(), leaf.path.to_str());
    let NativeRecordCoordinate::Jsonl {
        byte_length,
        native_session_key,
        native_event_key,
        ..
    } = document.locator.coordinate()
    else {
        panic!("source-backed fixture did not emit a typed JSONL locator");
    };
    assert_eq!(*byte_length as usize, expected_record.len());
    assert_eq!(
        native_session_key.as_ref(),
        Some(&TypedKey::Utf8(expected_native_session_id.to_owned()))
    );
    assert!(native_event_key.is_some());
    assert_eq!(
        adapter.hydrate(&certified, &document.locator).unwrap(),
        document.body.as_bytes()
    );

    let closing = adapter.discover(root).unwrap();
    let inventory = opening
        .certify_against(&closing, vec![certified.source().clone()])
        .unwrap();
    assert!(inventory.contains(certified.source()));

    let (replayed_documents, replayed) = collect_test_leaf(adapter, &closing.leaves()[0]);
    assert_eq!(
        documents
            .iter()
            .map(|document| (document.event_id, document.session_id))
            .collect::<Vec<_>>(),
        replayed_documents
            .iter()
            .map(|document| (document.event_id, document.session_id))
            .collect::<Vec<_>>()
    );
    assert_eq!(certified.source(), replayed.source());
}

fn collect_test_leaf(
    adapter: DirectJsonlSourceAdapter,
    leaf: &DirectJsonlInventoryLeaf,
) -> (Vec<LexicalDocument>, DirectJsonlCertifiedLeaf) {
    let mut reader = adapter
        .open_leaf(leaf, "2026-07-28T12:00:00Z".parse().unwrap())
        .unwrap();
    let mut documents = Vec::new();
    while let Some(page) = reader.next_page().unwrap() {
        assert!(page.documents.len() <= 64);
        assert_eq!(page.source.provider(), adapter.provider().as_str());
        documents.extend(page.documents);
    }
    (documents, reader.finish().unwrap())
}
