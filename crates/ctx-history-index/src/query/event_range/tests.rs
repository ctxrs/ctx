use std::{collections::BTreeSet, path::Path};

use ctx_history_core::{
    derive_event_id, derive_session_id, CertifiedSource, CertifiedSourceDeletion,
    CertifiedSourceInventory, CoreContentPolicyStatus, CoreRecord, EventIdentityInput,
    NativeItemKey, NativeSessionKey, RepositoryBinding, RepositoryEvidence,
    RepositoryEvidenceConfidence, RepositoryEvidenceKind, RepositoryFileObservation,
    RepositoryFileObservationKind, ScannedSourceCounts, SessionIdentityInput, SourceAnchor,
    SourceInventoryObservation, SourceKey, SourceObservation, SubrecordSelector, TypedKey,
};
use tempfile::tempdir;

use super::*;
use crate::{GenerationWriter, WriterOptions};

fn test_source(provider: &str, name: &str) -> SourceKey {
    SourceKey::derive(
        provider,
        format!("{provider}_session_test"),
        "session",
        1,
        SourceAnchor::provider_native("session-file", TypedKey::utf8(name).unwrap()).unwrap(),
    )
    .unwrap()
}

fn record(
    source: &SourceKey,
    nonce: u64,
    sequence: u64,
    occurred_at_unix_ms: Option<i64>,
    body: &str,
) -> CoreRecord {
    let native_session_key =
        NativeSessionKey::native_id("session", TypedKey::utf8("session").unwrap()).unwrap();
    let session_id = derive_session_id(SessionIdentityInput {
        source,
        logical_session_kind: "thread",
        native_session_key: &native_session_key,
    })
    .unwrap();
    let native_item_key = NativeItemKey::native_id("message", TypedKey::U64(nonce)).unwrap();
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
        session_id,
        source.clone(),
        sequence,
        "message",
        "primary",
        true,
        "event-range-test-v1",
        body,
    )
    .unwrap();
    record.occurred_at_unix_ms = occurred_at_unix_ms;
    record.provider_session_id = Some(format!("provider-session-{nonce}"));
    record.native_event_id = Some(TypedKey::U64(nonce));
    record.role = Some("user".to_owned());
    record.workspace = Some("工作区/ctx".to_owned());
    record.cwd = Some("/work/ctx".to_owned());
    record
}

fn certificate(source: &SourceKey, revision: u8, documents: usize) -> CertifiedSource {
    let observation =
        SourceObservation::new(source.clone(), "regular-file-v1", vec![revision]).unwrap();
    CertifiedSource::certify(
        observation.clone(),
        observation,
        "event-range-test-parser-v1",
        [revision; 32],
        ScannedSourceCounts {
            complete_records: documents as u64,
            retained_records: documents as u64,
            indexed_documents: documents as u64,
            certified_bytes: documents as u64 * 10,
            ..ScannedSourceCounts::default()
        },
    )
    .unwrap()
}

fn publish(root: &Path, revision: u8, sources: &[(SourceKey, Vec<CoreRecord>)]) -> String {
    let mut writer = GenerationWriter::open(root, WriterOptions::default()).unwrap();
    for (source, records) in sources {
        writer.begin_source(source.clone()).unwrap();
        for record in records {
            writer.add_core_record(record.clone()).unwrap();
        }
        writer
            .certify_source(certificate(source, revision, records.len()))
            .unwrap();
    }
    writer.commit(|_| true).unwrap().generation_id
}

fn deletion_evidence(
    source: &SourceKey,
    revision: u8,
) -> (CertifiedSourceDeletion, CertifiedSourceInventory) {
    let observation = SourceInventoryObservation::new(
        source.provider(),
        "provider-root",
        TypedKey::utf8("event-range-test-root").unwrap(),
        "tree-inventory-v1",
        vec![revision],
    )
    .unwrap();
    let inventory = CertifiedSourceInventory::certify(
        observation.clone(),
        observation,
        "event-range-test-discovery-v1",
        Vec::new(),
    )
    .unwrap();
    let deletion = CertifiedSourceDeletion::from_inventory(source.clone(), &inventory).unwrap();
    (deletion, inventory)
}

fn sorted_ids(records: &[CoreRecord]) -> Vec<uuid::Uuid> {
    let mut coordinates = records
        .iter()
        .filter_map(|record| {
            record.occurred_at_unix_ms.map(|occurred| {
                (
                    occurred,
                    record.event_sequence,
                    record.event_id.digest(),
                    record.event_id.as_uuid(),
                )
            })
        })
        .collect::<Vec<_>>();
    coordinates.sort_unstable();
    coordinates
        .into_iter()
        .map(|coordinate| coordinate.3)
        .collect()
}

fn traverse(
    index: &VerifiedIndex,
    selection: &CoreEventRangeSelection,
    limit: usize,
) -> Vec<uuid::Uuid> {
    let mut cursor = None;
    let mut ids = Vec::new();
    loop {
        let page = index
            .core_event_range_page(selection, cursor.as_ref(), limit)
            .unwrap();
        ids.extend(page.items.iter().map(|event| event.event_id.as_uuid()));
        if page.terminal {
            assert!(page.next_cursor.is_none());
            return ids;
        }
        cursor = page.next_cursor;
    }
}

#[test]
fn range_is_half_open_timestamped_provider_neutral_and_tie_ordered() {
    let temp = tempdir().unwrap();
    let codex = test_source("codex", "codex");
    let claude = test_source("claude", "claude");
    let since = 1_700_000_000_000_i64;
    let until = since + 10;
    let mut records = [
        record(&codex, 1, 9, None, "timestamp-less"),
        record(&codex, 2, 1, Some(since - 1), "before"),
        record(&codex, 3, 7, Some(since), "codex tie"),
        record(&claude, 4, 7, Some(since), "claude tie"),
        record(&codex, 5, 2, Some(until - 1), "inside"),
        record(&claude, 6, 1, Some(until), "exclusive"),
    ];
    records[4].agent_type = "subagent".to_owned();
    records[4].is_primary = false;
    let codex_records = records
        .iter()
        .filter(|record| record.source.provider() == "codex")
        .cloned()
        .collect::<Vec<_>>();
    let claude_records = records
        .iter()
        .filter(|record| record.source.provider() == "claude")
        .cloned()
        .collect::<Vec<_>>();
    publish(
        temp.path(),
        1,
        &[(codex, codex_records), (claude, claude_records)],
    );
    let index = VerifiedIndex::open(temp.path()).unwrap();
    let selection = CoreEventRangeSelection::new(since, until, Vec::<String>::new()).unwrap();
    let expected = sorted_ids(
        &records
            .iter()
            .filter(|record| {
                record
                    .occurred_at_unix_ms
                    .is_some_and(|timestamp| (since..until).contains(&timestamp))
            })
            .cloned()
            .collect::<Vec<_>>(),
    );
    for limit in 1..=expected.len() {
        assert_eq!(traverse(&index, &selection, limit), expected);
    }

    let codex_only = CoreEventRangeSelection::new(since, until, ["codex"]).unwrap();
    let codex_ids = traverse(&index, &codex_only, 1);
    assert_eq!(codex_ids.len(), 2);
    assert!(codex_ids.contains(&records[4].event_id.as_uuid()));
}

#[test]
fn all_domain_includes_untimestamped_tail_and_descending_is_exact_reverse() {
    let temp = tempdir().unwrap();
    let source = test_source("codex", "all-events");
    let records = vec![
        record(&source, 1, 8, None, "untimed-later"),
        record(&source, 2, 2, Some(101), "timed-later"),
        record(&source, 3, 1, None, "untimed-earlier"),
        record(&source, 4, 9, Some(100), "timed-earlier"),
    ];
    publish(temp.path(), 1, &[(source, records.clone())]);
    let index = VerifiedIndex::open(temp.path()).unwrap();

    let ascending = CoreEventRangeSelection::all(CoreEventRangeFilters::default()).unwrap();
    let mut expected = records
        .iter()
        .map(|record| {
            let encoded = record.encode_stored().unwrap();
            let content = core_content_bytes(&record.content).unwrap();
            (
                EventRangeOrderKey::for_core_record(record, encoded.len(), content).unwrap(),
                record.event_id.as_uuid(),
            )
        })
        .collect::<Vec<_>>();
    expected.sort_unstable_by_key(|(order, _)| *order);
    let expected = expected
        .into_iter()
        .map(|(_, event_id)| event_id)
        .collect::<Vec<_>>();
    assert_eq!(traverse(&index, &ascending, 1), expected);

    let descending = CoreEventRangeSelection::all(CoreEventRangeFilters {
        direction: CoreEventRangeDirection::Descending,
        ..CoreEventRangeFilters::default()
    })
    .unwrap();
    let mut reversed = expected;
    reversed.reverse();
    assert_eq!(traverse(&index, &descending, 2), reversed);
}

#[test]
fn randomized_oracle_has_no_gaps_or_duplicates_at_every_boundary() {
    let temp = tempdir().unwrap();
    let source = test_source("codex", "randomized");
    let mut state = 0x7f4a_7c15_d3e2_91ab_u64;
    let mut records = Vec::new();
    for nonce in 0..96_u64 {
        state = state
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        let timestamp = 10_000 + i64::try_from((state >> 17) % 9).unwrap();
        let sequence = (state >> 29) % 7;
        records.push(record(
            &source,
            nonce,
            sequence,
            Some(timestamp),
            &format!("event-{nonce}"),
        ));
    }
    publish(temp.path(), 1, &[(source, records.clone())]);
    let index = VerifiedIndex::open(temp.path()).unwrap();
    let selection = CoreEventRangeSelection::new(10_000, 10_009, Vec::<String>::new()).unwrap();
    let expected = sorted_ids(&records);
    assert_eq!(
        expected.iter().collect::<BTreeSet<_>>().len(),
        expected.len()
    );
    for limit in 1..=expected.len() {
        assert_eq!(
            traverse(&index, &selection, limit),
            expected,
            "limit {limit}"
        );
    }
}

#[test]
fn cursor_binds_version_generation_selection_coordinate_and_checksum() {
    let temp = tempdir().unwrap();
    let source = test_source("codex", "cursor");
    let records = (0..3)
        .map(|nonce| record(&source, nonce, nonce, Some(100 + nonce as i64), "body"))
        .collect::<Vec<_>>();
    let first_generation = publish(temp.path(), 1, &[(source.clone(), records.clone())]);
    let first = VerifiedIndex::open(temp.path()).unwrap();
    let selection = CoreEventRangeSelection::new(100, 200, ["codex", "codex"]).unwrap();
    let page = first.core_event_range_page(&selection, None, 1).unwrap();
    let cursor = page.next_cursor.unwrap();
    let encoded = cursor.encode();
    assert_eq!(CoreEventRangeCursor::decode(&encoded).unwrap(), cursor);

    let mut tampered = encoded;
    tampered[110] ^= 1;
    assert!(matches!(
        CoreEventRangeCursor::decode(&tampered),
        Err(CoreEventRangeError::InvalidCursor)
    ));
    assert!(matches!(
        CoreEventRangeCursor::decode(&encoded[..encoded.len() - 1]),
        Err(CoreEventRangeError::InvalidCursor)
    ));
    let other_selection = CoreEventRangeSelection::new(100, 201, ["codex"]).unwrap();
    assert!(matches!(
        cursor.validate_selection(&other_selection),
        Err(CoreEventRangeError::CursorSelectionMismatch)
    ));

    let mut invalid_coordinate = cursor.clone();
    let mut invalid_order = invalid_coordinate.after.into_bytes();
    invalid_order[17] ^= 1;
    invalid_coordinate.after = EventRangeOrderKey::decode(&invalid_order).unwrap();
    assert!(matches!(
        first.core_event_range_page(&selection, Some(&invalid_coordinate), 1),
        Err(CoreEventRangeError::InvalidCursorCoordinate)
    ));

    let second_generation = publish(temp.path(), 2, &[(source.clone(), records.clone())]);
    let active = VerifiedIndex::open(temp.path()).unwrap();
    assert_ne!(first_generation, second_generation);
    assert!(matches!(
        active.core_event_range_page(&selection, Some(&cursor), 1),
        Err(CoreEventRangeError::CursorGenerationMismatch { .. })
    ));
    let retained = VerifiedIndex::open_pinned_generation(temp.path(), &first_generation).unwrap();
    assert_eq!(
        retained
            .core_event_range_page(&selection, Some(&cursor), 8)
            .unwrap()
            .items
            .len(),
        2
    );

    publish(temp.path(), 3, &[(source, records)]);
    assert!(matches!(
        VerifiedIndex::open_pinned_generation(temp.path(), &first_generation),
        Err(IndexError::PinnedGenerationNotRetained { .. })
    ));
}

#[test]
fn held_pin_traverses_one_generation_while_rewrites_and_deletes_publish() {
    let temp = tempdir().unwrap();
    let source = test_source("codex", "immutable");
    let original = (0..12)
        .map(|nonce| {
            record(
                &source,
                nonce,
                nonce % 4,
                Some(1_000 + (nonce % 3) as i64),
                &format!("old-{nonce}"),
            )
        })
        .collect::<Vec<_>>();
    publish(temp.path(), 1, &[(source.clone(), original.clone())]);
    let held = VerifiedIndex::open(temp.path()).unwrap();
    let selection = CoreEventRangeSelection::new(1_000, 2_000, Vec::<String>::new()).unwrap();
    let first = held.core_event_range_page(&selection, None, 3).unwrap();
    let mut ids = first
        .items
        .iter()
        .map(|event| event.event_id.as_uuid())
        .collect::<Vec<_>>();
    let mut cursor = first.next_cursor;

    let rewritten = (6..18)
        .map(|nonce| {
            record(
                &source,
                nonce,
                nonce,
                Some(5_000 + nonce as i64),
                &format!("new-{nonce}"),
            )
        })
        .collect::<Vec<_>>();
    publish(temp.path(), 2, &[(source.clone(), rewritten)]);
    publish(temp.path(), 3, &[(source, Vec::new())]);

    while let Some(current) = cursor {
        let page = held
            .core_event_range_page(&selection, Some(&current), 3)
            .unwrap();
        ids.extend(page.items.iter().map(|event| event.event_id.as_uuid()));
        cursor = page.next_cursor;
    }
    assert_eq!(ids, sorted_ids(&original));
}

#[test]
fn complete_unicode_structured_and_policy_content_stays_generation_owned() {
    let temp = tempdir().unwrap();
    let source = test_source("codex", "content");
    let mut selected = record(&source, 1, 1, Some(10), "héllo 🦀");
    selected.content.structured_content = Some(serde_json::json!({
        "valid": "雪",
        "nested": [1, true, {"emoji": "🧭"}]
    }));
    let mut redacted = record(&source, 2, 2, Some(11), "[redacted]");
    redacted.content.policy_status = CoreContentPolicyStatus::Redacted {
        reason: "provider_secret".to_owned(),
    };
    let mut omitted = record(&source, 3, 3, Some(12), "remove me");
    omitted.content.policy_status = CoreContentPolicyStatus::Omitted {
        reason: "unsupported_binary".to_owned(),
    };
    omitted.content.normalized_body = None;
    let records = vec![selected.clone(), redacted.clone(), omitted.clone()];
    publish(temp.path(), 1, &[(source, records)]);
    let index = VerifiedIndex::open(temp.path()).unwrap();
    let selection = CoreEventRangeSelection::new(10, 13, Vec::<String>::new()).unwrap();
    let page = index.core_event_range_page(&selection, None, 8).unwrap();
    assert!(page.terminal);
    assert_eq!(page.items[0].core_record.content, selected.content);
    assert_eq!(page.items[1].core_record.content, redacted.content);
    assert_eq!(page.items[2].core_record.content, omitted.content);
}

#[test]
fn unknown_event_type_full_identity_relationships_and_metadata_roundtrip_exactly() {
    let temp = tempdir().unwrap();
    let source = test_source("future-provider", "full-identity-雪");
    let session = |name: &str| {
        let key = NativeSessionKey::native_id("thread", TypedKey::utf8(name).unwrap()).unwrap();
        derive_session_id(SessionIdentityInput {
            source: &source,
            logical_session_kind: "thread",
            native_session_key: &key,
        })
        .unwrap()
    };
    let root_session_id = session("root");
    let parent_session_id = session("parent");
    let session_id = session("child");
    let native_event_id = TypedKey::Composite(vec![
        TypedKey::utf8("future-key").unwrap(),
        TypedKey::U64(42),
    ]);
    let native_item_key = NativeItemKey::native_id("future-item", native_event_id.clone()).unwrap();
    let selector =
        SubrecordSelector::native_id("future-part", TypedKey::utf8("part-β").unwrap()).unwrap();
    let event_id = derive_event_id(EventIdentityInput {
        source: &source,
        session_id,
        logical_item_kind: "future-item",
        native_item_key: &native_item_key,
        subrecord_selector: Some(&selector),
    })
    .unwrap();
    let mut expected = CoreRecord::new_selected(
        event_id,
        session_id,
        root_session_id,
        source.clone(),
        42,
        "provider_future_event_v99",
        "specialist",
        false,
        "event-range-test-future-parser-v7",
        "complete 雪 🦀 body",
    )
    .unwrap();
    expected.parent_session_id = Some(parent_session_id);
    expected.provider_session_id = Some("provider-thread-β".to_owned());
    expected.native_event_id = Some(native_event_id);
    expected.occurred_at_unix_ms = Some(1_700_000_000_123);
    expected.role = Some("future-role".to_owned());
    expected.workspace = Some("工作区/ctx".to_owned());
    expected.branch = Some("feature/雪".to_owned());
    expected.cwd = Some("/workspace/ctx".to_owned());
    expected.metadata.insert(
        "future_metadata".to_owned(),
        serde_json::json!({"nested": [1, true, {"value": "β"}]}),
    );
    expected.repository_bindings.push(RepositoryBinding {
        binding_id: "binding-future".to_owned(),
        logical_repository_id: "repo-future".to_owned(),
        checkout_id: None,
        worktree_id: None,
        aliases: Vec::new(),
        git_object_format: None,
        local_root_authorization: None,
        evidence: vec![RepositoryEvidence {
            kind: RepositoryEvidenceKind::FileActivity,
            confidence: RepositoryEvidenceConfidence::Explicit,
        }],
        association_policy_revision: ctx_history_core::CORE_REPOSITORY_ASSOCIATION_POLICY_REVISION,
    });
    expected
        .repository_file_observations
        .push(RepositoryFileObservation {
            repository_binding_id: "binding-future".to_owned(),
            relative_path: "src/future.rs".to_owned(),
            kind: RepositoryFileObservationKind::Modified,
            prior_relative_path: None,
        });
    expected.validate_contract().unwrap();

    publish(temp.path(), 1, &[(source.clone(), vec![expected.clone()])]);
    let index = VerifiedIndex::open(temp.path()).unwrap();
    let selection = CoreEventRangeSelection::all(CoreEventRangeFilters::default()).unwrap();
    let page = index.core_event_range_page(&selection, None, 8).unwrap();

    assert!(page.terminal);
    assert_eq!(page.items.len(), 1);
    assert_eq!(page.items[0].event_type, "provider_future_event_v99");
    assert!(page.items[0].source.exact_descriptor_eq(&source));
    assert_eq!(page.items[0].parent_session_id, Some(parent_session_id));
    assert_eq!(page.items[0].root_session_id, root_session_id);
    assert_eq!(page.items[0].core_record, expected);
}

#[test]
fn active_range_tracks_noop_rewrite_and_certified_deletion() {
    let temp = tempdir().unwrap();
    let source = test_source("codex", "lifecycle");
    let original = (0..4)
        .map(|nonce| {
            record(
                &source,
                nonce,
                nonce,
                Some(100 + nonce as i64),
                &format!("original-{nonce}"),
            )
        })
        .collect::<Vec<_>>();
    let first_generation = publish(temp.path(), 1, &[(source.clone(), original.clone())]);
    let selection = CoreEventRangeSelection::all(CoreEventRangeFilters::default()).unwrap();
    assert_eq!(
        traverse(&VerifiedIndex::open(temp.path()).unwrap(), &selection, 2),
        sorted_ids(&original)
    );

    let no_op_generation = publish(temp.path(), 1, &[(source.clone(), original.clone())]);
    assert_eq!(no_op_generation, first_generation);
    assert_eq!(
        traverse(&VerifiedIndex::open(temp.path()).unwrap(), &selection, 1),
        sorted_ids(&original)
    );

    let rewritten = (2..7)
        .map(|nonce| {
            record(
                &source,
                nonce,
                nonce % 3,
                Some(1_000 + nonce as i64),
                &format!("rewritten-{nonce}"),
            )
        })
        .collect::<Vec<_>>();
    let rewritten_generation = publish(temp.path(), 2, &[(source.clone(), rewritten.clone())]);
    assert_ne!(rewritten_generation, first_generation);
    let rewritten_index = VerifiedIndex::open(temp.path()).unwrap();
    assert_eq!(
        traverse(&rewritten_index, &selection, 2),
        sorted_ids(&rewritten)
    );
    assert_eq!(rewritten_index.document_count(), rewritten.len() as u64);

    let (deletion, inventory) = deletion_evidence(&source, 3);
    let mut deleting = GenerationWriter::open(temp.path(), WriterOptions::default()).unwrap();
    deleting.delete_source(deletion, inventory).unwrap();
    let deleted_generation = deleting.commit(|_| true).unwrap().generation_id;
    assert_ne!(deleted_generation, rewritten_generation);
    let deleted = VerifiedIndex::open(temp.path()).unwrap();
    let page = deleted.core_event_range_page(&selection, None, 2).unwrap();
    assert!(page.terminal);
    assert!(page.items.is_empty());
    assert_eq!(deleted.document_count(), 0);
    assert!(deleted.manifest().sources.is_empty());
}

#[test]
fn invalid_ranges_filters_and_limits_fail_before_querying() {
    assert!(matches!(
        CoreEventRangeSelection::new(10, 10, Vec::<String>::new()),
        Err(CoreEventRangeError::InvalidRange { .. })
    ));
    assert!(matches!(
        CoreEventRangeSelection::new(11, 10, Vec::<String>::new()),
        Err(CoreEventRangeError::InvalidRange { .. })
    ));
    assert!(matches!(
        CoreEventRangeSelection::new(0, 1, [" "]),
        Err(CoreEventRangeError::InvalidFilter { field: "provider" })
    ));
    assert!(matches!(
        CoreEventRangeSelection::new(
            0,
            1,
            (0..=MAX_EVENT_RANGE_PROVIDERS).map(|index| format!("provider-{index}")),
        ),
        Err(CoreEventRangeError::InvalidFilter { field: "provider" })
    ));

    let temp = tempdir().unwrap();
    let source = test_source("codex", "limits");
    publish(
        temp.path(),
        1,
        &[(source.clone(), vec![record(&source, 1, 1, Some(0), "one")])],
    );
    let index = VerifiedIndex::open(temp.path()).unwrap();
    let selection = CoreEventRangeSelection::new(0, 1, Vec::<String>::new()).unwrap();
    assert!(matches!(
        index.core_event_range_page(&selection, None, 0),
        Err(CoreEventRangeError::InvalidPageSize { .. })
    ));
}

#[test]
fn complete_filters_apply_before_item_and_byte_limits() {
    let temp = tempdir().unwrap();
    let source = test_source("codex", "filters");
    let mut primary = record(&source, 1, 1, Some(100), "primary");
    primary.branch = Some("main".to_owned());
    primary.event_type = "tool_result".to_owned();
    primary.role = Some("assistant".to_owned());
    primary.agent_type = "codex".to_owned();
    primary.workspace = Some("/Work/CTX".to_owned());
    let mut subagent = record(&source, 2, 2, Some(101), "subagent");
    subagent.is_primary = false;
    subagent.agent_type = "subagent".to_owned();
    let session_id = primary.session_id.as_uuid();
    publish(
        temp.path(),
        1,
        &[(source.clone(), vec![primary.clone(), subagent])],
    );
    let index = VerifiedIndex::open(temp.path()).unwrap();
    let selection = CoreEventRangeSelection::with_filters(
        100,
        102,
        CoreEventRangeFilters {
            providers: vec!["codex".to_owned()],
            source_identity: Some(source.identity().as_uuid()),
            source_format: Some(source.source_format().to_owned()),
            provider_session_id: primary.provider_session_id.clone(),
            session_id: Some(session_id),
            root_session_id: Some(session_id),
            branch: primary.branch.clone(),
            workspace: Some("work/ctx".to_owned()),
            event_type: Some(primary.event_type.clone()),
            role: primary.role.clone(),
            agent_type: Some(primary.agent_type.clone()),
            scope: CoreEventRangeScope::Primary,
            ..CoreEventRangeFilters::default()
        },
    )
    .unwrap();
    let page = index.core_event_range_page(&selection, None, 1).unwrap();
    assert!(page.terminal);
    assert_eq!(page.items.len(), 1);
    assert_eq!(page.items[0].event_id, primary.event_id);

    let subagents = CoreEventRangeSelection::with_filters(
        100,
        102,
        CoreEventRangeFilters {
            scope: CoreEventRangeScope::Subagent,
            ..CoreEventRangeFilters::default()
        },
    )
    .unwrap();
    assert_eq!(
        index
            .core_event_range_page(&subagents, None, 1)
            .unwrap()
            .items
            .len(),
        1
    );
}

#[test]
fn exact_source_filter_skips_nonmatching_records_before_core_decode() {
    let temp = tempdir().unwrap();
    let decoy_source = test_source("codex", "source-filter-decoy");
    let selected_source = test_source("codex", "source-filter-selected");
    let decoys = (0..32)
        .map(|nonce| record(&decoy_source, nonce, nonce, Some(100), "decoy"))
        .collect::<Vec<_>>();
    let selected = record(&selected_source, 100, 100, Some(101), "selected");
    publish(
        temp.path(),
        1,
        &[
            (decoy_source, decoys),
            (selected_source.clone(), vec![selected.clone()]),
        ],
    );
    let index = VerifiedIndex::open(temp.path()).unwrap();
    let selection = CoreEventRangeSelection::with_filters(
        100,
        102,
        CoreEventRangeFilters {
            source_identity: Some(selected_source.identity().as_uuid()),
            ..CoreEventRangeFilters::default()
        },
    )
    .unwrap();
    crate::query::reset_stored_core_event_record_materializations();
    let page = index.core_event_range_page(&selection, None, 1).unwrap();
    assert_eq!(page.items.len(), 1);
    assert_eq!(page.items[0].event_id, selected.event_id);
    assert_eq!(crate::query::stored_core_event_record_materializations(), 1);
}

#[test]
fn custom_source_and_touched_file_filters_apply_before_limit() {
    let temp = tempdir().unwrap();
    let source = test_source("custom", "custom-filters");
    let mut decoy = record(&source, 1, 1, Some(100), "decoy");
    decoy.native_event_id = Some(TypedKey::Composite(vec![
        TypedKey::utf8("other-provider").unwrap(),
        TypedKey::utf8("other-source").unwrap(),
        TypedKey::utf8("event_id:decoy").unwrap(),
    ]));
    let mut matching = record(&source, 2, 2, Some(101), "matching");
    matching.native_event_id = Some(TypedKey::Composite(vec![
        TypedKey::utf8("demo-agent").unwrap(),
        TypedKey::utf8("archive/year/08").unwrap(),
        TypedKey::utf8("event_id:matching").unwrap(),
    ]));
    matching.repository_bindings.push(RepositoryBinding {
        binding_id: "binding-1".to_owned(),
        logical_repository_id: "repo-1".to_owned(),
        checkout_id: None,
        worktree_id: None,
        aliases: Vec::new(),
        git_object_format: None,
        local_root_authorization: None,
        evidence: vec![RepositoryEvidence {
            kind: RepositoryEvidenceKind::FileActivity,
            confidence: RepositoryEvidenceConfidence::Explicit,
        }],
        association_policy_revision: ctx_history_core::CORE_REPOSITORY_ASSOCIATION_POLICY_REVISION,
    });
    matching
        .repository_file_observations
        .push(RepositoryFileObservation {
            repository_binding_id: "binding-1".to_owned(),
            relative_path: "Crates/Feed/SRC/lib.rs".to_owned(),
            kind: RepositoryFileObservationKind::Modified,
            prior_relative_path: None,
        });
    publish(temp.path(), 1, &[(source, vec![decoy, matching.clone()])]);
    let index = VerifiedIndex::open(temp.path()).unwrap();
    let selection = CoreEventRangeSelection::with_filters(
        100,
        102,
        CoreEventRangeFilters {
            providers: vec!["custom".to_owned()],
            history_source: Some("demo-agent/archive/year/08".to_owned()),
            provider_key: Some("demo-agent".to_owned()),
            source_id: Some("archive/year/08".to_owned()),
            file: Some("feed/src/LIB.rs".to_owned()),
            ..CoreEventRangeFilters::default()
        },
    )
    .unwrap();
    let page = index.core_event_range_page(&selection, None, 1).unwrap();
    assert!(page.terminal);
    assert_eq!(page.items.len(), 1);
    assert_eq!(page.items[0].event_id, matching.event_id);
    assert_eq!(page.items[0].touched_files, vec!["Crates/Feed/SRC/lib.rs"]);
}

#[test]
fn valid_oversized_singleton_always_advances() {
    let temp = tempdir().unwrap();
    let source = test_source("codex", "budget");
    let records = vec![
        record(&source, 1, 1, Some(10), &"large".repeat(100)),
        record(&source, 2, 2, Some(11), "next"),
    ];
    publish(temp.path(), 1, &[(source, records.clone())]);
    let index = VerifiedIndex::open(temp.path()).unwrap();
    let selection = CoreEventRangeSelection::new(10, 12, Vec::<String>::new()).unwrap();
    let budget = CoreEventPageBudget::new(1, 1);
    let first = index
        .core_event_range_page_with_budget(&selection, None, 8, budget)
        .unwrap();
    assert_eq!(first.items.len(), 1);
    assert_eq!(first.items[0].event_id, records[0].event_id);
    assert!(first.oversized_singleton);
    assert!(!first.terminal);
    let second = index
        .core_event_range_page_with_budget(&selection, first.next_cursor.as_ref(), 8, budget)
        .unwrap();
    assert_eq!(second.items.len(), 1);
    assert_eq!(second.items[0].event_id, records[1].event_id);
    assert!(second.oversized_singleton);
    assert!(second.terminal);
    assert!(second.next_cursor.is_none());
}

#[test]
fn byte_budget_bounds_every_nonoversized_page_across_cursor_backpressure() {
    let temp = tempdir().unwrap();
    let source = test_source("codex", "bounded-pages");
    let records = (0..17)
        .map(|nonce| record(&source, nonce, nonce, Some(100 + nonce as i64), "fixed"))
        .collect::<Vec<_>>();
    let maximum_encoded = records
        .iter()
        .map(|record| record.encode_stored().unwrap().len())
        .max()
        .unwrap();
    let maximum_content = records
        .iter()
        .map(|record| core_content_bytes(&record.content).unwrap())
        .max()
        .unwrap();
    publish(temp.path(), 1, &[(source, records.clone())]);
    let index = VerifiedIndex::open(temp.path()).unwrap();
    let selection = CoreEventRangeSelection::new(100, 200, Vec::<String>::new()).unwrap();
    let budget = CoreEventPageBudget::new(maximum_encoded, maximum_content);
    let mut cursor = None;
    let mut returned = Vec::new();
    let mut pages = 0;
    loop {
        let page = index
            .core_event_range_page_with_budget(&selection, cursor.as_ref(), 8, budget)
            .unwrap();
        pages += 1;
        assert!(!page.oversized_singleton);
        assert!(page.encoded_core_bytes <= maximum_encoded);
        assert!(page.content_bytes <= maximum_content);
        assert_eq!(page.items.len(), 1);
        returned.push(page.items[0].event_id.as_uuid());
        if page.terminal {
            assert!(page.next_cursor.is_none());
            break;
        }
        cursor = page.next_cursor;
    }
    assert_eq!(pages, records.len());
    assert_eq!(returned, sorted_ids(&records));
}

#[test]
fn keyset_pages_visit_only_forward_range_terms() {
    let temp = tempdir().unwrap();
    let source = test_source("codex", "linear");
    let records = (0..257)
        .map(|nonce| record(&source, nonce, nonce % 5, Some(1_000), "body"))
        .collect::<Vec<_>>();
    publish(temp.path(), 1, &[(source, records)]);
    let index = VerifiedIndex::open(temp.path()).unwrap();
    let selection = CoreEventRangeSelection::new(1_000, 1_001, Vec::<String>::new()).unwrap();
    crate::query::reset_event_range_order_term_visits();
    let ids = traverse(&index, &selection, 7);
    assert_eq!(ids.len(), 257);
    let pages = 257_usize.div_ceil(7);
    assert!(crate::query::event_range_order_term_visits() <= 257 + pages);
}

#[test]
fn descending_keyset_pages_reverse_the_same_total_order() {
    let temp = tempdir().unwrap();
    let source = test_source("codex", "descending");
    let records = (0..97)
        .map(|nonce| {
            record(
                &source,
                nonce,
                nonce % 7,
                Some(1_000 + (nonce % 5) as i64),
                "body",
            )
        })
        .collect::<Vec<_>>();
    publish(temp.path(), 1, &[(source, records.clone())]);
    let index = VerifiedIndex::open(temp.path()).unwrap();
    let ascending = CoreEventRangeSelection::new(1_000, 1_005, Vec::<String>::new()).unwrap();
    let descending = CoreEventRangeSelection::with_filters(
        1_000,
        1_005,
        CoreEventRangeFilters {
            direction: CoreEventRangeDirection::Descending,
            ..CoreEventRangeFilters::default()
        },
    )
    .unwrap();
    let ascending_ids = traverse(&index, &ascending, 11);
    let descending_ids = traverse(&index, &descending, 11);
    assert_eq!(
        descending_ids,
        ascending_ids.into_iter().rev().collect::<Vec<_>>()
    );

    let page = index.core_event_range_page(&descending, None, 1).unwrap();
    assert!(matches!(
        page.next_cursor.unwrap().validate_selection(&ascending),
        Err(CoreEventRangeError::CursorSelectionMismatch)
    ));
}

#[test]
fn indexed_selective_continuation_never_walks_chronology_or_materializes_decoys() {
    let temp = tempdir().unwrap();
    let source = test_source("codex", "indexed-selective");
    let mut records = (0..513)
        .map(|nonce| record(&source, nonce, nonce, Some(10_000 + nonce as i64), "decoy"))
        .collect::<Vec<_>>();
    for record in &mut records {
        record.event_type = "future_decoy_type".to_owned();
    }
    let selected_offsets = [3_usize, 64, 129, 255, 384, 512];
    for offset in selected_offsets {
        records[offset].event_type = "future_selected_type".to_owned();
    }
    let selected_ids = selected_offsets
        .iter()
        .map(|offset| records[*offset].event_id.as_uuid())
        .collect::<Vec<_>>();
    publish(temp.path(), 1, &[(source, records)]);
    let index = VerifiedIndex::open(temp.path()).unwrap();
    let selection = CoreEventRangeSelection::with_filters(
        10_000,
        11_000,
        CoreEventRangeFilters {
            event_type: Some("future_selected_type".to_owned()),
            ..CoreEventRangeFilters::default()
        },
    )
    .unwrap();
    crate::query::reset_event_range_order_term_visits();
    crate::query::reset_stored_core_event_record_materializations();
    let returned = traverse(&index, &selection, 2);
    assert_eq!(returned, selected_ids);
    assert_eq!(crate::query::event_range_order_term_visits(), 0);
    assert_eq!(
        crate::query::stored_core_event_record_materializations(),
        selected_ids.len()
    );
}
