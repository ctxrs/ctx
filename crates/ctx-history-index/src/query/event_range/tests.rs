use std::{collections::BTreeSet, path::Path};

use ctx_history_core::{
    derive_event_id, derive_session_id, CertifiedSource, CoreContentPolicyStatus, CoreRecord,
    EventIdentityInput, NativeItemKey, NativeSessionKey, ScannedSourceCounts, SessionIdentityInput,
    SourceAnchor, SourceKey, SourceObservation, TypedKey,
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

fn sorted_ids(records: &[CoreRecord]) -> Vec<uuid::Uuid> {
    let mut coordinates = records
        .iter()
        .filter_map(|record| {
            record
                .occurred_at_unix_ms
                .map(|occurred| (occurred, record.event_sequence, record.event_id.as_uuid()))
        })
        .collect::<Vec<_>>();
    coordinates.sort_unstable();
    coordinates
        .into_iter()
        .map(|coordinate| coordinate.2)
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
    invalid_coordinate.after.event_id = uuid::Uuid::nil();
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
        Err(CoreEventRangeError::InvalidProviderSelection)
    ));
    assert!(matches!(
        CoreEventRangeSelection::new(
            0,
            1,
            (0..=MAX_EVENT_RANGE_PROVIDERS).map(|index| format!("provider-{index}")),
        ),
        Err(CoreEventRangeError::InvalidProviderSelection)
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
