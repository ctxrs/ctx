use super::*;

#[test]
fn pinned_query_api_returns_typed_records_in_deterministic_order() {
    let temp = tempdir().unwrap();
    let source = source("session.jsonl");
    let first = document(&source, 1, "atomic generation");
    let second = document(&source, 2, "atomic generation");
    let mut writer = GenerationWriter::open(temp.path(), WriterOptions::default()).unwrap();
    writer.begin_source(source.clone()).unwrap();
    writer.add_core_record(second.clone()).unwrap();
    writer.add_core_record(first.clone()).unwrap();
    writer.certify_source(certificate(&source, 1, 2)).unwrap();
    writer.commit(|_| true).unwrap();

    let index = VerifiedIndex::open(temp.path()).unwrap();
    let candidates = index
        .search_event_candidates("atomic:generation", 10)
        .unwrap();
    let mut expected_search_ids = vec![first.event_id.as_uuid(), second.event_id.as_uuid()];
    expected_search_ids.sort();
    assert_eq!(
        candidates
            .iter()
            .map(|candidate| candidate.event.event_id.as_uuid())
            .collect::<Vec<_>>(),
        expected_search_ids
    );
    assert_eq!(candidates[0].score, candidates[1].score);

    let exact = index
        .event_by_id(first.event_id.as_uuid())
        .unwrap()
        .unwrap();
    assert_eq!(exact.event_id, first.event_id);
    assert_eq!(exact.session_id, first.session_id);
    assert!(exact.source.exact_descriptor_eq(&first.source));
    assert_eq!(exact.provider, "codex");
    assert_eq!(exact.source_format, "codex_session_jsonl");
    assert_eq!(exact.provider_session_id.as_deref(), Some("session"));
    assert_eq!(exact.event_sequence, 1);
    assert_eq!(exact.occurred_at_unix_ms, first.occurred_at_unix_ms);
    assert_eq!(exact.event_type, "message");
    assert_eq!(exact.role.as_deref(), Some("user"));
    assert_eq!(exact.workspace.as_deref(), Some("ctx"));
    assert_eq!(exact.cwd.as_deref(), Some("/work/ctx"));
    assert!(exact.touched_files.is_empty());

    let event_id = first.event_id.to_string();
    let event_prefix = &event_id[..8];
    assert_eq!(
        index.events_by_id_prefix(event_prefix).unwrap()[0].event_id,
        first.event_id
    );

    let ordered = index
        .events_for_session(first.session_id.as_uuid())
        .unwrap();
    assert_eq!(
        ordered
            .iter()
            .map(|event| event.event_sequence)
            .collect::<Vec<_>>(),
        vec![1, 2]
    );
    let core_ordered = index
        .core_events_for_session(first.session_id.as_uuid())
        .unwrap();
    assert_eq!(
        core_ordered
            .iter()
            .map(|record| record.event_id)
            .collect::<Vec<_>>(),
        ordered
            .iter()
            .map(|event| event.event_id)
            .collect::<Vec<_>>()
    );
    assert_eq!(
        core_ordered[0]
            .core_record
            .content
            .normalized_body
            .as_deref(),
        Some("atomic generation")
    );
    let session = index
        .session_by_id(first.session_id.as_uuid())
        .unwrap()
        .unwrap();
    assert_eq!(session.session_id, first.session_id);
    assert_eq!(session.provider, "codex");
    assert_eq!(session.source_format, "codex_session_jsonl");
    assert_eq!(session.provider_session_id.as_deref(), Some("session"));
    assert_eq!(session.first_event_sequence, 1);

    let session_id = first.session_id.to_string();
    let session_prefix = &session_id[..8];
    assert_eq!(
        index.sessions_by_id_prefix(session_prefix).unwrap(),
        vec![session]
    );
}

#[test]
fn decoded_core_event_reports_searchable_touched_files_deterministically() {
    let temp = tempdir().unwrap();
    let source = source("repository-files.jsonl");
    let mut expected = document(&source, 1, "repository file activity");
    expected.repository_bindings.push(RepositoryBinding {
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
    expected.repository_file_observations = vec![
        RepositoryFileObservation {
            repository_binding_id: "binding-1".to_owned(),
            relative_path: "src/lib.rs".to_owned(),
            kind: RepositoryFileObservationKind::Modified,
            prior_relative_path: None,
        },
        RepositoryFileObservation {
            repository_binding_id: "binding-1".to_owned(),
            relative_path: "src/lib.rs".to_owned(),
            kind: RepositoryFileObservationKind::Read,
            prior_relative_path: None,
        },
        RepositoryFileObservation {
            repository_binding_id: "binding-1".to_owned(),
            relative_path: "src/new.rs".to_owned(),
            kind: RepositoryFileObservationKind::Renamed,
            prior_relative_path: Some("src/old.rs".to_owned()),
        },
    ];
    expected.validate_contract().unwrap();

    let mut writer = GenerationWriter::open(temp.path(), WriterOptions::default()).unwrap();
    writer.begin_source(source.clone()).unwrap();
    writer.add_core_record(expected.clone()).unwrap();
    writer.certify_source(certificate(&source, 1, 1)).unwrap();
    writer.commit(|_| true).unwrap();

    let index = VerifiedIndex::open(temp.path()).unwrap();
    let decoded = index
        .core_event_by_id(expected.event_id.as_uuid())
        .unwrap()
        .unwrap();
    assert_eq!(
        decoded.touched_files,
        vec![
            "src/lib.rs".to_owned(),
            "src/new.rs".to_owned(),
            "src/old.rs".to_owned(),
        ]
    );
    for file in ["SRC/LIB.RS", "src/new.rs", "src/old.rs"] {
        let matches = index
            .search_event_candidates_with_filters(
                "repository:file:activity",
                &EventSearchFilters {
                    file: Some(file.to_owned()),
                    ..EventSearchFilters::default()
                },
                10,
            )
            .unwrap();
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].event.event_id, expected.event_id);
    }
}

#[test]
fn core_valid_escape_heavy_query_metadata_indexes_without_a_narrower_json_bound() {
    const ESCAPED_FIELD_BYTES: usize = 17 * 1024;
    const NATIVE_ID_BYTES: usize = 60 * 1024;

    let temp = tempdir().unwrap();
    let source = source("escape-heavy-query-metadata.jsonl");
    let mut record = document(&source, 1, "small searchable body");
    let escaped = "\u{0001}".repeat(ESCAPED_FIELD_BYTES);
    record.provider_session_id = Some(escaped.clone());
    record.branch = Some(escaped.clone());
    record.agent_type = escaped.clone();
    record.event_type = escaped.clone();
    record.role = Some(escaped.clone());
    record.workspace = Some(escaped.clone());
    record.cwd = Some(escaped);
    record.native_event_id = Some(TypedKey::utf8("\u{0002}".repeat(NATIVE_ID_BYTES)).unwrap());
    record.validate_contract().unwrap();
    let encoded_core = record.encode_stored().unwrap();
    let query_metadata =
        crate::index_document::StoredQueryMetadata::encode(&record, encoded_core.len()).unwrap();
    assert!(query_metadata.len() > 1024 * 1024);
    assert!(query_metadata.len() <= encoded_core.len());

    let mut writer = GenerationWriter::open(temp.path(), WriterOptions::default()).unwrap();
    writer.begin_source(source.clone()).unwrap();
    writer.add_core_record(record.clone()).unwrap();
    writer.certify_source(certificate(&source, 1, 1)).unwrap();
    writer.commit(|_| true).unwrap();

    let index = VerifiedIndex::open(temp.path()).unwrap();
    let indexed = index
        .event_by_id(record.event_id.as_uuid())
        .unwrap()
        .unwrap();
    assert_eq!(indexed.provider_session_id, record.provider_session_id);
    assert_eq!(indexed.native_event_id, record.native_event_id);
    assert_eq!(indexed.cwd, record.cwd);
}

#[test]
fn query_metadata_rejects_valid_json_after_equal_sized_chunk_payloads_are_swapped() {
    use tantivy::schema::Document as _;

    fn encoded_query_metadata(record: &CoreRecord) -> Vec<u8> {
        let encoded_core = record.encode_stored().unwrap();
        crate::index_document::StoredQueryMetadata::encode(record, encoded_core.len()).unwrap()
    }

    fn string_content_offset(encoded: &[u8], field: &str) -> usize {
        let marker = format!("\"{field}\":\"");
        encoded
            .windows(marker.len())
            .position(|window| window == marker.as_bytes())
            .unwrap()
            + marker.len()
    }

    let temp = tempdir().unwrap();
    let source = source("authenticated-query-metadata.jsonl");
    let payload_bytes = crate::index_document::QUERY_METADATA_CHUNK_PAYLOAD_BYTES;
    let mut event = document(&source, 1, "query metadata authentication");
    event.provider_session_id = Some("p".to_owned());
    event.branch = Some("A".repeat(payload_bytes));
    event.role = Some("r".to_owned());
    event.workspace = Some("B".repeat(payload_bytes));

    let encoded = encoded_query_metadata(&event);
    let branch_offset = string_content_offset(&encoded, "branch");
    let branch_padding = (payload_bytes - branch_offset % payload_bytes) % payload_bytes;
    event.provider_session_id = Some("p".repeat(1 + branch_padding));
    let encoded = encoded_query_metadata(&event);
    assert_eq!(string_content_offset(&encoded, "branch") % payload_bytes, 0);

    let workspace_offset = string_content_offset(&encoded, "workspace");
    let workspace_padding = (payload_bytes - workspace_offset % payload_bytes) % payload_bytes;
    event.role = Some("r".repeat(1 + workspace_padding));
    event.validate_contract().unwrap();
    let encoded = encoded_query_metadata(&event);
    let branch_offset = string_content_offset(&encoded, "branch");
    let workspace_offset = string_content_offset(&encoded, "workspace");
    assert_eq!(branch_offset % payload_bytes, 0);
    assert_eq!(workspace_offset % payload_bytes, 0);
    assert!(encoded[branch_offset..branch_offset + payload_bytes]
        .iter()
        .all(|byte| *byte == b'A'));
    assert!(encoded[workspace_offset..workspace_offset + payload_bytes]
        .iter()
        .all(|byte| *byte == b'B'));

    let mut writer = GenerationWriter::open(temp.path(), WriterOptions::default()).unwrap();
    writer.begin_source(source.clone()).unwrap();
    writer.add_core_record(event.clone()).unwrap();
    writer.certify_source(certificate(&source, 1, 1)).unwrap();
    writer.commit(|_| true).unwrap();

    let (searcher, manifest) = open_unverified_generation(temp.path());
    let fields = fields_from_schema(searcher.schema()).unwrap();
    let address = searcher
        .search(&AllQuery, &DocSetCollector)
        .unwrap()
        .into_iter()
        .next()
        .unwrap();
    let original: TantivyDocument = searcher.doc(address).unwrap();
    let mut chunks = original
        .get_all(fields.query_metadata)
        .map(|value| value.as_bytes().unwrap().to_vec())
        .collect::<Vec<_>>();
    let branch_chunk = branch_offset / payload_bytes;
    let workspace_chunk = workspace_offset / payload_bytes;
    assert_ne!(branch_chunk, workspace_chunk);
    let chunk_index = |chunk: &[u8]| usize::from(u16::from_be_bytes([chunk[4], chunk[5]]));
    let branch_position = chunks
        .iter()
        .position(|chunk| chunk_index(chunk) == branch_chunk)
        .unwrap();
    let workspace_position = chunks
        .iter()
        .position(|chunk| chunk_index(chunk) == workspace_chunk)
        .unwrap();
    let branch_payload = chunks[branch_position]
        [crate::index_document::QUERY_METADATA_CHUNK_HEADER_BYTES..]
        .to_vec();
    let workspace_payload = chunks[workspace_position]
        [crate::index_document::QUERY_METADATA_CHUNK_HEADER_BYTES..]
        .to_vec();
    assert_eq!(branch_payload.len(), payload_bytes);
    assert_eq!(workspace_payload.len(), payload_bytes);
    chunks[branch_position][crate::index_document::QUERY_METADATA_CHUNK_HEADER_BYTES..]
        .copy_from_slice(&workspace_payload);
    chunks[workspace_position][crate::index_document::QUERY_METADATA_CHUNK_HEADER_BYTES..]
        .copy_from_slice(&branch_payload);

    let mut ordered_chunks = chunks.iter().collect::<Vec<_>>();
    ordered_chunks.sort_by_key(|chunk| chunk_index(chunk));
    let altered_encoded = ordered_chunks
        .into_iter()
        .flat_map(|chunk| {
            chunk[crate::index_document::QUERY_METADATA_CHUNK_HEADER_BYTES..]
                .iter()
                .copied()
        })
        .collect::<Vec<_>>();
    let altered: crate::index_document::StoredQueryMetadata =
        serde_json::from_slice(&altered_encoded).unwrap();
    assert_eq!(altered.event_id, event.event_id);
    assert_eq!(altered.session_id, event.session_id);
    assert!(altered.source.exact_descriptor_eq(&event.source));
    assert_eq!(
        altered.branch.as_deref(),
        Some("B".repeat(payload_bytes).as_str())
    );
    assert_eq!(
        altered.workspace.as_deref(),
        Some("A".repeat(payload_bytes).as_str())
    );

    let mut forged = TantivyDocument::default();
    for (field, value) in original.iter_fields_and_values() {
        if field != fields.query_metadata {
            forged.add_field_value(field, value);
        }
    }
    for chunk in chunks {
        forged.add_bytes(fields.query_metadata, &chunk);
    }
    drop(searcher);
    let directory = DurableMmapDirectory::open(active_generation_path(temp.path())).unwrap();
    let index = Index::open(directory).unwrap();
    publish_unchecked_generation(
        temp.path(),
        &index,
        manifest,
        std::slice::from_ref(&source),
        vec![forged],
    );

    let error = match VerifiedIndex::open(temp.path()) {
        Ok(_) => panic!("swapped query metadata payloads unexpectedly verified"),
        Err(error) => error,
    };
    assert!(matches!(
        error,
        IndexError::InvalidStoredDocumentField("query_metadata")
    ));
}

#[test]
fn query_metadata_tiny_declared_maximum_header_fails_before_exact_allocation() {
    use tantivy::schema::Document as _;

    let temp = tempdir().unwrap();
    let source = source("tiny-query-metadata-header.jsonl");
    let mut writer = GenerationWriter::open(temp.path(), WriterOptions::default()).unwrap();
    writer.begin_source(source.clone()).unwrap();
    writer
        .add_core_record(document(&source, 1, "bounded malformed metadata"))
        .unwrap();
    writer.certify_source(certificate(&source, 1, 1)).unwrap();
    writer.commit(|_| true).unwrap();

    let (searcher, manifest) = open_unverified_generation(temp.path());
    let fields = fields_from_schema(searcher.schema()).unwrap();
    let address = searcher
        .search(&AllQuery, &DocSetCollector)
        .unwrap()
        .into_iter()
        .next()
        .unwrap();
    let original: TantivyDocument = searcher.doc(address).unwrap();
    let total_bytes = crate::index_document::MAX_QUERY_METADATA_BYTES;
    let chunk_count =
        total_bytes.div_ceil(crate::index_document::QUERY_METADATA_CHUNK_PAYLOAD_BYTES);
    let mut tiny_header =
        Vec::with_capacity(crate::index_document::QUERY_METADATA_CHUNK_HEADER_BYTES);
    tiny_header.extend_from_slice(&crate::index_document::QUERY_METADATA_CHUNK_MAGIC);
    tiny_header.extend_from_slice(&0_u16.to_be_bytes());
    tiny_header.extend_from_slice(&u16::try_from(chunk_count).unwrap().to_be_bytes());
    tiny_header.extend_from_slice(&u32::try_from(total_bytes).unwrap().to_be_bytes());
    tiny_header
        .extend_from_slice(&[0_u8; crate::index_document::QUERY_METADATA_CHUNK_DIGEST_BYTES]);
    assert_eq!(
        tiny_header.len(),
        crate::index_document::QUERY_METADATA_CHUNK_HEADER_BYTES
    );

    let mut forged = TantivyDocument::default();
    for (field, value) in original.iter_fields_and_values() {
        if field != fields.query_metadata {
            forged.add_field_value(field, value);
        }
    }
    forged.add_bytes(fields.query_metadata, &tiny_header);
    drop(searcher);
    let directory = DurableMmapDirectory::open(active_generation_path(temp.path())).unwrap();
    let index = Index::open(directory).unwrap();
    publish_unchecked_generation(
        temp.path(),
        &index,
        manifest,
        std::slice::from_ref(&source),
        vec![forged],
    );

    let (searcher, _) = open_unverified_generation(temp.path());
    let fields = fields_from_schema(searcher.schema()).unwrap();
    let address = searcher
        .search(&AllQuery, &DocSetCollector)
        .unwrap()
        .into_iter()
        .next()
        .unwrap();
    crate::query::reset_query_metadata_decode_work();
    let error = crate::query::stored_event_record(&searcher, address, fields).unwrap_err();
    assert!(matches!(
        error,
        IndexError::InvalidStoredDocumentField("query_metadata")
    ));
    assert_eq!(crate::query::query_metadata_chunk_reads(), 1);
    assert_eq!(crate::query::query_metadata_exact_allocated_bytes(), 0);
}

#[test]
fn semantic_pairing_many_user_turns_uses_bounded_direct_session_pages() {
    const TURNS: u64 = 256;
    const PAIRING_PAGE_ITEMS: usize = 4;

    let temp = tempdir().unwrap();
    let source = source("many-semantic-turns.jsonl");
    let mut writer = GenerationWriter::open(temp.path(), WriterOptions::default()).unwrap();
    writer.begin_source(source.clone()).unwrap();
    for turn in 0..TURNS {
        let user_sequence = turn * 2 + 1;
        writer
            .add_core_record(document(
                &source,
                user_sequence,
                &format!("question {turn}"),
            ))
            .unwrap();
        let mut assistant = document(&source, user_sequence + 1, &format!("answer {turn}"));
        assistant.role = Some("assistant".to_owned());
        writer.add_core_record(assistant).unwrap();
    }
    writer
        .certify_source(certificate(&source, 1, TURNS * 2))
        .unwrap();
    writer.commit(|_| true).unwrap();

    let index = VerifiedIndex::open(temp.path()).unwrap();
    let anchors = index
        .core_events_for_session(document(&source, 1, "anchor").session_id.as_uuid())
        .unwrap()
        .into_iter()
        .filter(|record| record.role.as_deref() == Some("user"))
        .collect::<Vec<_>>();
    assert_eq!(anchors.len(), TURNS as usize);
    crate::query::reset_session_event_order_term_visits();
    for anchor in &anchors {
        let turn = (anchor.event_sequence - 1) / 2;
        let paired = index
            .semantic_lite_turn_assistant(
                anchor,
                PAIRING_PAGE_ITEMS,
                DEFAULT_CORE_EVENT_PAGE_BUDGET,
            )
            .unwrap()
            .unwrap();
        assert_eq!(paired.0, format!("answer {turn}"));
    }
    let term_visits = crate::query::session_event_order_term_visits();
    assert!(
        term_visits <= TURNS as usize * PAIRING_PAGE_ITEMS * crate::LEXICAL_SEGMENT_MERGE_FAN_IN,
        "direct pairing term visits must stay linear in user turns: {term_visits}"
    );
    assert!(term_visits < (TURNS * TURNS) as usize);
}

#[test]
fn semantic_pairing_crosses_more_than_sixty_four_tool_events_body_free() {
    const TOOL_EVENTS: u64 = 96;

    let temp = tempdir().unwrap();
    let source = source("tool-heavy-semantic-turn.jsonl");
    let user = document(&source, 1, "tool-heavy question");
    let mut assistant = document(&source, TOOL_EVENTS + 2, "answer beyond old window");
    assistant.role = Some("assistant".to_owned());
    let next_user = document(&source, TOOL_EVENTS + 3, "next question");
    let mut writer = GenerationWriter::open(temp.path(), WriterOptions::default()).unwrap();
    writer.begin_source(source.clone()).unwrap();
    writer.add_core_record(user.clone()).unwrap();
    for sequence in 2..=TOOL_EVENTS + 1 {
        let mut tool = document(&source, sequence, "large tool body is not hydrated");
        tool.event_type = "tool_output".to_owned();
        tool.role = Some("tool".to_owned());
        writer.add_core_record(tool).unwrap();
    }
    writer.add_core_record(assistant.clone()).unwrap();
    writer.add_core_record(next_user).unwrap();
    writer
        .certify_source(certificate(&source, 1, TOOL_EVENTS + 3))
        .unwrap();
    writer.commit(|_| true).unwrap();

    let index = VerifiedIndex::open(temp.path()).unwrap();
    let anchor = index
        .core_event_by_id(user.event_id.as_uuid())
        .unwrap()
        .unwrap();
    crate::query::reset_stored_core_event_record_materializations();
    crate::query::reset_session_event_order_term_visits();
    let paired = index
        .semantic_lite_turn_assistant(&anchor, 64, DEFAULT_CORE_EVENT_PAGE_BUDGET)
        .unwrap()
        .unwrap();

    assert_eq!(paired.0, "answer beyond old window");
    assert_eq!(paired.1, assistant.occurred_at_unix_ms.unwrap());
    assert_eq!(
        crate::query::stored_core_event_record_materializations(),
        1,
        "tool metadata traversal must hydrate only the paired assistant body"
    );
    let term_visits = crate::query::session_event_order_term_visits();
    assert!(term_visits > 64);
    assert!(
        term_visits <= 2 * 64 * crate::LEXICAL_SEGMENT_MERGE_FAN_IN,
        "tool-heavy pairing must remain page bounded: {term_visits}"
    );
}

#[test]
fn semantic_pairing_many_segments_merges_each_order_term_once_across_pages_and_reopen() {
    const SEGMENTS: u64 = 6;
    const EVENTS_PER_SEGMENT: u64 = 6;
    const TOTAL_EVENTS: u64 = SEGMENTS * EVENTS_PER_SEGMENT;
    const FIRST_ASSISTANT_SEQUENCE: u64 = TOTAL_EVENTS - EVENTS_PER_SEGMENT * 2 + 1;
    const LAST_ASSISTANT_SEQUENCE: u64 = TOTAL_EVENTS - EVENTS_PER_SEGMENT;
    const PAGE_ITEMS: usize = 3;
    const ASSISTANT_EVENTS: usize = SEGMENTS as usize;

    let temp = tempdir().unwrap();
    let source = source("many-segment-semantic-turn.jsonl");
    let mut anchor_id = None;
    let mut latest_assistant = None;
    for segment_index in 0..SEGMENTS {
        let revision = (segment_index + 1) as u8;
        let retained_events = (segment_index + 1) * EVENTS_PER_SEGMENT;
        let retained_bytes = retained_events * 10;
        let mut writer = GenerationWriter::open(temp.path(), WriterOptions::default()).unwrap();
        writer
            .writer_mut()
            .unwrap()
            .set_merge_policy(Box::<NoMergePolicy>::default());
        let append_base = if segment_index == 0 {
            writer.begin_source(source.clone()).unwrap();
            None
        } else {
            Some(writer.begin_source_append(source.clone()).unwrap().clone())
        };

        // Interleave every segment's sequence range and insert each run in
        // reverse so only the indexed key can define global traversal order.
        for sequence in (0..EVENTS_PER_SEGMENT)
            .rev()
            .map(|event_index| segment_index + 1 + event_index * SEGMENTS)
        {
            let is_next_user = sequence == TOTAL_EVENTS;
            let is_assistant =
                (FIRST_ASSISTANT_SEQUENCE..=LAST_ASSISTANT_SEQUENCE).contains(&sequence);
            let mut event = document(
                &source,
                sequence,
                if sequence == 1 {
                    "many-segment question".to_owned()
                } else if is_next_user {
                    "next question".to_owned()
                } else if is_assistant {
                    format!("answer {sequence}")
                } else {
                    format!("tool body {sequence}")
                }
                .as_str(),
            );
            if sequence == 1 {
                anchor_id = Some(event.event_id.as_uuid());
            } else if is_assistant {
                event.role = Some("assistant".to_owned());
                if sequence == LAST_ASSISTANT_SEQUENCE {
                    latest_assistant = Some((
                        format!("answer {sequence}"),
                        event.occurred_at_unix_ms.unwrap(),
                    ));
                }
            } else if !is_next_user {
                event.event_type = "tool_output".to_owned();
                event.role = Some("tool".to_owned());
            }
            writer.add_core_record(event).unwrap();
        }

        let certified = appendable_certificate(&source, revision, retained_events, retained_bytes);
        if let Some(base) = append_base {
            writer
                .certify_source_append(
                    CertifiedSourceAppend::certify(
                        &base,
                        certified,
                        retained_bytes - EVENTS_PER_SEGMENT * 10,
                        [revision - 1; 32],
                    )
                    .unwrap(),
                )
                .unwrap();
        } else {
            writer.certify_source(certified).unwrap();
        }
        writer.commit(|_| true).unwrap();
    }

    let anchor_id = anchor_id.unwrap();
    let expected_latest = latest_assistant.unwrap();
    let assert_traversal = |index: &VerifiedIndex| {
        assert!(
            index.searcher.segment_readers().len() >= SEGMENTS as usize,
            "test requires one live segment per append"
        );
        let anchor = index.core_event_by_id(anchor_id).unwrap().unwrap();
        crate::query::reset_stored_core_event_record_materializations();
        crate::query::reset_session_event_order_term_visits();
        let paired = index
            .semantic_lite_turn_assistant(&anchor, PAGE_ITEMS, DEFAULT_CORE_EVENT_PAGE_BUDGET)
            .unwrap()
            .unwrap();

        assert_eq!(paired, expected_latest);
        assert_eq!(
            crate::query::stored_core_event_record_materializations(),
            ASSISTANT_EVENTS,
            "tool records must remain body-free and no assistant may be skipped or decoded twice"
        );
        assert_eq!(
            crate::query::session_event_order_term_visits(),
            (TOTAL_EVENTS - 1) as usize,
            "each globally considered order term must be decoded exactly once"
        );
        assert_eq!(
            crate::query::session_event_order_visited_sequences(),
            (2..=TOTAL_EVENTS).collect::<Vec<_>>(),
            "merged pages must preserve exact order without skips or duplicates"
        );
    };

    let first_pin = VerifiedIndex::open(temp.path()).unwrap();
    assert_traversal(&first_pin);
    drop(first_pin);
    let reopened = VerifiedIndex::open_pinned(temp.path()).unwrap();
    assert_traversal(&reopened);
}

#[test]
fn metadata_hot_paths_and_ambiguity_collectors_are_body_free_and_bounded() {
    const EVENT_COUNT: u64 = 64;
    const AMBIGUITY_LIMIT: usize = 2;

    let temp = tempdir().unwrap();
    let source = source("metadata-hot-paths.jsonl");
    let mut event_ids = Vec::new();
    let mut session_ids = Vec::new();
    let mut bodies_by_session = std::collections::BTreeMap::new();
    let mut writer = GenerationWriter::open(temp.path(), WriterOptions::default()).unwrap();
    writer.begin_source(source.clone()).unwrap();
    for sequence in 1..=EVENT_COUNT {
        let body = format!("ambiguity needle {sequence}");
        let mut event = document_for_session(
            &source,
            &format!("bounded-session-{sequence}"),
            sequence,
            &body,
        );
        event.provider_session_id = Some("shared-provider-session".to_owned());
        event_ids.push(event.event_id.as_uuid());
        session_ids.push(event.session_id.as_uuid());
        bodies_by_session.insert(event.session_id.as_uuid(), body.len());
        writer.add_core_record(event).unwrap();
    }
    writer
        .certify_source(certificate(&source, 1, EVENT_COUNT))
        .unwrap();
    writer.commit(|_| true).unwrap();

    crate::query::reset_stored_event_record_materializations();
    crate::query::reset_stored_core_event_record_materializations();
    let index = VerifiedIndex::open_pinned(temp.path()).unwrap();
    assert_eq!(crate::query::stored_core_event_record_materializations(), 0);

    session_ids.sort();
    session_ids.dedup();
    crate::query::reset_stored_event_record_materializations();
    let provider_sessions = index
        .sessions_by_provider_session_id("shared-provider-session", Some("codex"))
        .unwrap();
    assert_eq!(provider_sessions.len(), AMBIGUITY_LIMIT);
    assert_eq!(
        provider_sessions
            .iter()
            .map(|session| session.session_id.as_uuid())
            .collect::<Vec<_>>(),
        session_ids[..AMBIGUITY_LIMIT]
    );
    assert_eq!(
        crate::query::stored_event_record_materializations(),
        AMBIGUITY_LIMIT,
        "provider-session ambiguity lookup must decode only one metadata record per retained session"
    );

    let session_prefix = session_ids
        .iter()
        .fold(
            std::collections::BTreeMap::<char, Vec<Uuid>>::new(),
            |mut groups, id| {
                groups
                    .entry(id.to_string().chars().next().unwrap())
                    .or_default()
                    .push(*id);
                groups
            },
        )
        .into_iter()
        .find(|(_, ids)| ids.len() > AMBIGUITY_LIMIT)
        .unwrap();
    crate::query::reset_stored_event_record_materializations();
    let prefix_sessions = index
        .sessions_by_id_prefix(&session_prefix.0.to_string())
        .unwrap();
    assert_eq!(
        prefix_sessions
            .iter()
            .map(|session| session.session_id.as_uuid())
            .collect::<Vec<_>>(),
        session_prefix.1[..AMBIGUITY_LIMIT]
    );
    assert_eq!(
        crate::query::stored_event_record_materializations(),
        AMBIGUITY_LIMIT
    );

    event_ids.sort();
    let event_prefix = event_ids
        .iter()
        .fold(
            std::collections::BTreeMap::<char, Vec<Uuid>>::new(),
            |mut groups, id| {
                groups
                    .entry(id.to_string().chars().next().unwrap())
                    .or_default()
                    .push(*id);
                groups
            },
        )
        .into_iter()
        .find(|(_, ids)| ids.len() > AMBIGUITY_LIMIT)
        .unwrap();
    crate::query::reset_stored_event_record_materializations();
    let prefix_events = index
        .events_by_id_prefix(&event_prefix.0.to_string())
        .unwrap();
    assert_eq!(
        prefix_events
            .iter()
            .map(|event| event.event_id.as_uuid())
            .collect::<Vec<_>>(),
        event_prefix.1[..AMBIGUITY_LIMIT]
    );
    assert_eq!(
        crate::query::stored_event_record_materializations(),
        AMBIGUITY_LIMIT
    );

    crate::query::reset_stored_event_record_materializations();
    let candidates = index.search_event_candidates("ambiguity", 5).unwrap();
    assert_eq!(candidates.len(), 5);
    assert_eq!(crate::query::stored_event_record_materializations(), 5);
    assert_eq!(crate::query::stored_core_event_record_materializations(), 0);

    crate::query::reset_stored_event_record_materializations();
    let source_page = index.source_event_page(&source, None, 5).unwrap();
    assert_eq!(source_page.items.len(), 5);
    assert_eq!(crate::query::stored_event_record_materializations(), 5);
    assert_eq!(crate::query::stored_core_event_record_materializations(), 0);

    crate::query::reset_stored_event_record_materializations();
    let semantic_page = index.semantic_event_page(None, 5).unwrap();
    assert_eq!(semantic_page.items.len(), 5);
    assert_eq!(crate::query::stored_event_record_materializations(), 5);
    assert_eq!(crate::query::stored_core_event_record_materializations(), 0);

    crate::query::reset_stored_event_record_materializations();
    let session_id = session_ids[0];
    assert_eq!(
        index
            .core_content_bytes_for_session_if_bounded(session_id, 1)
            .unwrap(),
        Some(bodies_by_session[&session_id])
    );
    assert_eq!(crate::query::stored_event_record_materializations(), 0);
    assert_eq!(crate::query::stored_core_event_record_materializations(), 0);
}

#[test]
fn exhaustive_open_rejects_an_unreadable_stored_core_body() {
    use tantivy::schema::Document as _;

    let temp = tempdir().unwrap();
    let source = source("body-free-metadata.jsonl");
    let event = document(&source, 1, "metadata survives an unreadable Core body");
    let mut writer = GenerationWriter::open(temp.path(), WriterOptions::default()).unwrap();
    writer.begin_source(source.clone()).unwrap();
    writer.add_core_record(event).unwrap();
    writer.certify_source(certificate(&source, 1, 1)).unwrap();
    writer.commit(|_| true).unwrap();

    let (searcher, manifest) = open_unverified_generation(temp.path());
    let fields = fields_from_schema(searcher.schema()).unwrap();
    let address = searcher
        .search(&AllQuery, &DocSetCollector)
        .unwrap()
        .into_iter()
        .next()
        .unwrap();
    let original: TantivyDocument = searcher.doc(address).unwrap();
    let mut malformed = TantivyDocument::default();
    for (field, value) in original.iter_fields_and_values() {
        if field != fields.core_record {
            malformed.add_field_value(field, value);
        }
    }
    malformed.add_bytes(fields.core_record, b"{");
    drop(searcher);

    let directory = DurableMmapDirectory::open(active_generation_path(temp.path())).unwrap();
    let index = Index::open(directory).unwrap();
    publish_unchecked_generation(
        temp.path(),
        &index,
        manifest,
        std::slice::from_ref(&source),
        vec![malformed],
    );

    assert!(matches!(
        VerifiedIndex::open(temp.path()),
        Err(IndexError::CoreRecord(_))
    ));
}

#[test]
fn bounded_core_event_batch_is_complete_and_requested_ordered() {
    let temp = tempdir().unwrap();
    let source = source("bounded-event-batch.jsonl");
    let first = document(&source, 1, "first complete body");
    let second = document(&source, 2, "second complete body");
    let mut writer = GenerationWriter::open(temp.path(), WriterOptions::default()).unwrap();
    writer.begin_source(source.clone()).unwrap();
    writer.add_core_record(first.clone()).unwrap();
    writer.add_core_record(second.clone()).unwrap();
    writer.certify_source(certificate(&source, 1, 2)).unwrap();
    writer.commit(|_| true).unwrap();

    let index = VerifiedIndex::open(temp.path()).unwrap();
    crate::query::reset_stored_core_event_record_materializations();
    let coordinates = index
        .session_event_coordinates(first.session_id.as_uuid())
        .unwrap();
    assert_eq!(
        coordinates
            .iter()
            .map(|coordinate| coordinate.event_id)
            .collect::<Vec<_>>(),
        vec![first.event_id.as_uuid(), second.event_id.as_uuid()]
    );
    assert_eq!(
        crate::query::stored_core_event_record_materializations(),
        0,
        "session selection metadata must not decode complete Core bodies"
    );

    let requested = [second.event_id.as_uuid(), first.event_id.as_uuid()];
    crate::query::reset_stored_core_event_record_materializations();
    let bounded_batch = index
        .core_events_by_ids_with_budget(&requested, requested.len(), DEFAULT_CORE_EVENT_PAGE_BUDGET)
        .unwrap()
        .unwrap();
    assert_eq!(
        bounded_batch
            .items
            .iter()
            .map(|record| record.event_id.as_uuid())
            .collect::<Vec<_>>(),
        requested
    );
    assert!(bounded_batch.encoded_core_bytes >= bounded_batch.content_bytes);
    assert_eq!(
        bounded_batch.content_bytes,
        "first complete body".len() + "second complete body".len()
    );

    crate::query::reset_stored_core_event_record_materializations();
    let records = index
        .core_events_by_ids_if_bounded(&requested, requested.len(), usize::MAX)
        .unwrap()
        .unwrap();
    assert_eq!(
        records
            .iter()
            .map(|record| record.event_id.as_uuid())
            .collect::<Vec<_>>(),
        requested
    );
    assert_eq!(
        records
            .iter()
            .map(|record| record.core_record.content.meaningful_text())
            .collect::<Vec<_>>(),
        vec!["second complete body", "first complete body",]
    );
    assert_eq!(
        crate::query::stored_core_event_record_materializations(),
        2,
        "each requested event must materialize exactly one stored Core document"
    );

    crate::query::reset_stored_core_event_record_materializations();
    assert!(index
        .core_events_by_ids_if_bounded(&requested, requested.len() - 1, usize::MAX)
        .unwrap()
        .is_none());
    assert_eq!(
        crate::query::stored_core_event_record_materializations(),
        0,
        "an oversized request must be declined before querying stored documents"
    );

    crate::query::reset_stored_core_event_record_materializations();
    assert!(matches!(
        index.core_events_by_ids_if_bounded(
            &[first.event_id.as_uuid(), first.event_id.as_uuid()],
            2,
            usize::MAX,
        ),
        Err(IndexError::DuplicateEventIdentity(_))
    ));
    assert_eq!(
        crate::query::stored_core_event_record_materializations(),
        0,
        "duplicate requested IDs must be rejected before querying stored documents"
    );

    assert!(index
        .core_events_by_ids_if_bounded(&[first.event_id.as_uuid(), Uuid::nil()], 2, usize::MAX,)
        .unwrap()
        .is_none());
    assert!(index
        .core_events_by_ids_if_bounded(&[], 0, 0)
        .unwrap()
        .unwrap()
        .is_empty());
}

#[test]
fn bounded_session_coordinate_queries_ignore_pathological_nonselected_cardinality() {
    const EVENT_COUNT: u64 = 5_000;
    const SELECTED_SEQUENCE: u64 = 2_500;

    let temp = tempdir().unwrap();
    let source = source("bounded-session-coordinates.jsonl");
    let first = document(&source, 1, "first");
    let session_id = first.session_id.as_uuid();
    let mut selected_event_id = None;
    let mut writer = GenerationWriter::open(temp.path(), WriterOptions::default()).unwrap();
    writer.begin_source(source.clone()).unwrap();
    for sequence in (1..=EVENT_COUNT).rev() {
        let event = document(&source, sequence, "small body");
        if sequence == SELECTED_SEQUENCE {
            selected_event_id = Some(event.event_id.as_uuid());
        }
        writer.add_core_record(event).unwrap();
    }
    writer
        .certify_source(certificate(&source, 1, EVENT_COUNT))
        .unwrap();
    writer.commit(|_| true).unwrap();

    let index = VerifiedIndex::open(temp.path()).unwrap();
    crate::query::reset_stored_core_event_record_materializations();
    let prefix = index
        .session_event_coordinate_prefix(session_id, 6)
        .unwrap();
    assert_eq!(
        prefix
            .iter()
            .map(|coordinate| coordinate.event_sequence)
            .collect::<Vec<_>>(),
        vec![1, 2, 3, 4, 5, 6]
    );
    assert_eq!(crate::query::stored_core_event_record_materializations(), 0);

    let selected_event_id = selected_event_id.unwrap();
    let window = index
        .session_event_coordinate_window(session_id, selected_event_id, 50, 50)
        .unwrap()
        .unwrap();
    assert_eq!(window.len(), MAX_SESSION_EVENT_COORDINATE_WINDOW_ITEMS);
    assert_eq!(window.first().unwrap().event_sequence, 2_450);
    assert_eq!(window[50].event_id, selected_event_id);
    assert_eq!(window.last().unwrap().event_sequence, 2_550);
    assert_eq!(crate::query::stored_core_event_record_materializations(), 0);

    assert!(matches!(
        index.session_event_coordinate_prefix(
            session_id,
            MAX_SESSION_EVENT_COORDINATE_PREFIX_ITEMS + 1,
        ),
        Err(IndexError::InvalidSessionEventCoordinateLimit { .. })
    ));
    assert!(matches!(
        index.session_event_coordinate_window(session_id, selected_event_id, 51, 50),
        Err(IndexError::InvalidSessionEventCoordinateLimit { .. })
    ));
}

#[test]
fn custom_source_filters_use_the_core_native_event_identity() {
    let temp = tempdir().unwrap();
    let source = SourceKey::derive(
        "custom",
        "ctx_history_jsonl",
        "catalog",
        1,
        SourceAnchor::CatalogLineage([42; 32]),
    )
    .unwrap();
    let mut record = document(&source, 1, "custom identity needle");
    let native_event_id = TypedKey::composite(vec![
        TypedKey::utf8("fixture-provider").unwrap(),
        TypedKey::utf8("fixture-source").unwrap(),
        TypedKey::utf8("fixture-session").unwrap(),
    ])
    .unwrap();
    record.native_event_id = Some(native_event_id.clone());
    assert_eq!(record.native_event_id.as_ref(), Some(&native_event_id));

    let event_id = record.event_id;
    let mut writer = GenerationWriter::open(temp.path(), WriterOptions::default()).unwrap();
    writer.begin_source(source.clone()).unwrap();
    writer.add_core_record(record).unwrap();
    writer.certify_source(certificate(&source, 1, 1)).unwrap();
    writer.commit(|_| true).unwrap();

    let index = VerifiedIndex::open(temp.path()).unwrap();
    let filters = EventSearchFilters {
        history_source: Some("fixture-provider/fixture-source".to_owned()),
        provider_key: Some("fixture-provider".to_owned()),
        source_id: Some("fixture-source".to_owned()),
        ..EventSearchFilters::default()
    };
    crate::query::reset_stored_core_event_record_materializations();
    let hits = index
        .search_event_candidates_with_filters("identity", &filters, 10)
        .unwrap();
    assert_eq!(
        hits.iter()
            .map(|candidate| candidate.event.event_id)
            .collect::<Vec<_>>(),
        vec![event_id]
    );
    assert_eq!(
        hits[0].event.native_event_id.as_ref(),
        Some(&native_event_id)
    );
    assert!(filters.matches_source_identity(&hits[0].event));
    assert_eq!(
        crate::query::stored_core_event_record_materializations(),
        0,
        "custom source filtering must use bounded indexed identity metadata"
    );

    let misses = index
        .search_event_candidates_with_filters(
            "identity",
            &EventSearchFilters {
                source_id: Some("another-source".to_owned()),
                ..EventSearchFilters::default()
            },
            10,
        )
        .unwrap();
    assert!(misses.is_empty());
}

#[test]
fn bounded_core_event_batch_stops_after_one_large_record_exceeds_byte_budget() {
    let temp = tempdir().unwrap();
    let source = source("large-bounded-event-batch.jsonl");
    let large_body = "large-core-body".repeat(128 * 1024);
    let documents = (1..=3)
        .map(|sequence| document(&source, sequence, &large_body))
        .collect::<Vec<_>>();
    let requested = documents
        .iter()
        .map(|document| document.event_id.as_uuid())
        .collect::<Vec<_>>();
    let mut writer = GenerationWriter::open(temp.path(), WriterOptions::default()).unwrap();
    writer.begin_source(source.clone()).unwrap();
    for document in documents {
        writer.add_core_record(document).unwrap();
    }
    writer.certify_source(certificate(&source, 1, 3)).unwrap();
    writer.commit(|_| true).unwrap();

    let index = VerifiedIndex::open(temp.path()).unwrap();
    crate::query::reset_stored_core_event_record_materializations();
    assert!(index
        .core_events_by_ids_if_bounded(&requested, requested.len(), 1)
        .unwrap()
        .is_none());
    assert_eq!(
        crate::query::stored_core_event_record_materializations(),
        1,
        "byte-budget refusal must stop before decoding the remaining large records"
    );
}

#[test]
fn session_event_budget_declines_before_materializing_an_oversized_session() {
    let temp = tempdir().unwrap();
    let source = source("bounded-session.jsonl");
    let mut writer = GenerationWriter::open(temp.path(), WriterOptions::default()).unwrap();
    writer.begin_source(source.clone()).unwrap();
    for sequence in 1..=3 {
        writer
            .add_core_record(document(&source, sequence, "bounded body"))
            .unwrap();
    }
    writer.certify_source(certificate(&source, 1, 3)).unwrap();
    writer.commit(|_| true).unwrap();

    let index = VerifiedIndex::open(temp.path()).unwrap();
    let session_id = document(&source, 1, "bounded body").session_id.as_uuid();
    assert!(index
        .events_for_session_if_bounded(session_id, 2)
        .unwrap()
        .is_none());
    assert!(index
        .core_events_for_session_if_bounded(session_id, 2)
        .unwrap()
        .is_none());
    assert_eq!(
        index
            .events_for_session_if_bounded(session_id, 3)
            .unwrap()
            .unwrap()
            .len(),
        3
    );
    let core = index
        .core_events_for_session_if_bounded(session_id, 3)
        .unwrap()
        .unwrap();
    assert_eq!(core.len(), 3);
    assert!(core.iter().all(|record| {
        record.core_record.content.normalized_body.as_deref() == Some("bounded body")
    }));
}
