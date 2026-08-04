use std::{collections::BTreeMap, io::Cursor};

use crate::{
    apply_core_source_delta_page_request_frame_wire_bytes,
    core_source_delta_page_applied_frame_wire_bytes, read_frame, write_frame, HelperEnvelope,
    HelperMessage, HostEnvelope, HostMessage,
};
use ctx_history_core::{
    derive_event_id, derive_session_id, CoreContent, CoreContentPolicyStatus, EventIdentityInput,
    NativeItemKey, NativeSessionKey, RepositoryAlias, RepositoryAliasKind, RepositoryBinding,
    RepositoryCandidateEvidence, RepositoryEvidence, RepositoryEvidenceConfidence,
    RepositoryEvidenceKind, RepositoryFileObservation, RepositoryFileObservationKind,
    RepositoryVcsObservation, RepositoryVcsObservationKind, SessionIdentityInput, SourceAnchor,
    TypedKey, CORE_CONTENT_POLICY_REVISION, CORE_REPOSITORY_ASSOCIATION_POLICY_REVISION,
};
use uuid::Uuid;

use super::*;

#[path = "tests/source_page_frames.rs"]
mod source_page_frames;

fn source(lineage: u8) -> SourceKey {
    SourceKey::derive(
        "codex",
        "codex_session_jsonl",
        "fixture-v1",
        1,
        SourceAnchor::CatalogLineage([lineage; 32]),
    )
    .unwrap()
}

fn escaped_source() -> SourceKey {
    SourceKey::derive(
        "fixture",
        "fixture_jsonl",
        "escaped-frame-v1",
        1,
        SourceAnchor::ProviderNative {
            namespace: "fixture-native".to_owned(),
            key: TypedKey::Utf8("\u{001f}\"\\\n".repeat(8)),
        },
    )
    .unwrap()
}

fn state(source: SourceKey, revision: u8, event_count: u64) -> CoreSourceState {
    CoreSourceState {
        source,
        core_record_accumulator: format!("{revision:064x}"),
        event_count,
    }
}

fn binding(id: &str, repository: &str) -> RepositoryBinding {
    RepositoryBinding {
        binding_id: id.to_owned(),
        logical_repository_id: repository.to_owned(),
        checkout_id: None,
        worktree_id: None,
        aliases: vec![RepositoryAlias {
            kind: RepositoryAliasKind::Forge,
            host: "github.com".to_owned(),
            namespace: vec!["ctxrs".to_owned()],
            name: repository.to_owned(),
            remote_name: Some("origin".to_owned()),
        }],
        git_object_format: None,
        local_root_authorization: None,
        evidence: vec![RepositoryEvidence {
            kind: RepositoryEvidenceKind::DeclaredToolWorkdir,
            confidence: RepositoryEvidenceConfidence::High,
        }],
        association_policy_revision: CORE_REPOSITORY_ASSOCIATION_POLICY_REVISION,
    }
}

fn record(source: &SourceKey, sequence: u64, body: String, two_repositories: bool) -> CoreRecord {
    let session_key = NativeSessionKey::native_id("fixture-session", TypedKey::U64(1)).unwrap();
    let session_id = derive_session_id(SessionIdentityInput {
        source,
        logical_session_kind: "thread",
        native_session_key: &session_key,
    })
    .unwrap();
    let event_id = derive_event_id(EventIdentityInput {
        source,
        session_id,
        logical_item_kind: "message",
        native_item_key: &NativeItemKey::native_id("fixture-event", TypedKey::U64(sequence))
            .unwrap(),
        subrecord_selector: None,
    })
    .unwrap();
    let repository_bindings = if two_repositories {
        vec![
            binding("binding-a", "repo-a"),
            binding("binding-b", "repo-b"),
        ]
    } else {
        Vec::new()
    };
    let repository_file_observations = if two_repositories {
        vec![
            RepositoryFileObservation {
                repository_binding_id: "binding-a".to_owned(),
                relative_path: "src/a.rs".to_owned(),
                kind: RepositoryFileObservationKind::Modified,
                prior_relative_path: None,
            },
            RepositoryFileObservation {
                repository_binding_id: "binding-b".to_owned(),
                relative_path: "src/b.rs".to_owned(),
                kind: RepositoryFileObservationKind::Read,
                prior_relative_path: None,
            },
        ]
    } else {
        Vec::new()
    };
    let repository_vcs_observations = if two_repositories {
        vec![RepositoryVcsObservation {
            repository_binding_id: "binding-b".to_owned(),
            kind: RepositoryVcsObservationKind::Branch,
            object_id: None,
            parent_object_ids: Vec::new(),
            reference: Some("refs/heads/main".to_owned()),
            relative_path: None,
        }]
    } else {
        Vec::new()
    };
    CoreRecord {
        record_version: CORE_RECORD_VERSION,
        event_id,
        session_id,
        parent_session_id: None,
        root_session_id: session_id,
        source: source.clone(),
        provider_session_id: Some("provider-session".to_owned()),
        native_event_id: None,
        event_sequence: sequence,
        occurred_at_unix_ms: Some(1_700_000_000_000),
        event_type: "message".to_owned(),
        role: Some("assistant".to_owned()),
        agent_type: "primary".to_owned(),
        is_primary: true,
        workspace: None,
        branch: Some("main".to_owned()),
        cwd: None,
        parser_revision: "fixture-parser-v1".to_owned(),
        normalization_revision: CORE_NORMALIZATION_REVISION,
        content: CoreContent {
            policy_revision: CORE_CONTENT_POLICY_REVISION,
            policy_status: CoreContentPolicyStatus::Selected,
            normalized_body: Some(body),
            structured_content: None,
        },
        mcp_tool_call: None,
        metadata: BTreeMap::new(),
        repository_candidate_evidence: RepositoryCandidateEvidence::default(),
        repository_bindings,
        repository_abstentions: Vec::new(),
        repository_file_invocation_evidence: Vec::new(),
        repository_file_observations,
        repository_vcs_observations,
    }
}

#[test]
fn added_and_replaced_deltas_expose_exact_record_to_leaf_helper() {
    let source = source(1);
    let record = record(
        &source,
        1,
        "exact projection-boundary Core record".to_owned(),
        true,
    );
    let expected = core_record_digests(&record).unwrap();
    let stored_json = record.encode_stored().unwrap();
    let decoded = CoreRecord::decode_stored(&stored_json).unwrap();
    assert_eq!(decoded, record);
    assert_eq!(decoded.encode_stored().unwrap(), stored_json);
    assert_eq!(
        core_record_digests_from_encoded(&decoded, &stored_json).unwrap(),
        expected
    );
    for digest in [
        &expected.core_record_sha256,
        &expected.core_record_leaf_sha256,
    ] {
        assert_eq!(digest.len(), 64);
        assert!(digest
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)));
    }
    assert_eq!(
        expected.core_record_sha256,
        core_record_sha256(&record).unwrap()
    );
    assert_eq!(
        expected.core_record_leaf_sha256,
        core_record_leaf_sha256(&record).unwrap()
    );
    assert_eq!(
        expected.core_record_leaf_sha256,
        ctx_history_core::core_record_leaf_sha256(&record).unwrap()
    );

    for delta in [
        CoreEventDelta::Added(record.clone()),
        CoreEventDelta::Replaced(CoreEventReplacement {
            prior_core_record_sha256: "a".repeat(64),
            record,
        }),
    ] {
        delta.validate_for_source(&source).unwrap();
        assert_eq!(
            core_record_digests(delta.record().unwrap())
                .unwrap()
                .core_record_leaf_sha256,
            expected.core_record_leaf_sha256
        );
    }
}

fn head(sources: &[CoreSourceState]) -> CoreGenerationHead {
    CoreGenerationHead::new(
        "a".repeat(64),
        4,
        1,
        "b".repeat(64),
        3,
        2,
        "c".repeat(64),
        sources,
    )
    .unwrap()
}

fn reconciliation(source: &SourceKey, event_count: usize) -> CoreSourceReconciliation {
    CoreSourceReconciliation {
        materialize_index: 0,
        delta: CoreSourceDelta::Present(state(
            source.clone(),
            1,
            u64::try_from(event_count).unwrap(),
        )),
    }
}

fn ordered_additions(source: &SourceKey, count: usize, body_bytes: usize) -> Vec<CoreEventDelta> {
    let mut deltas = (0..count)
        .map(|index| {
            CoreEventDelta::Added(record(
                source,
                u64::try_from(index + 1).unwrap(),
                "x".repeat(body_bytes),
                false,
            ))
        })
        .collect::<Vec<_>>();
    deltas.sort_by_key(|delta| delta.event_id().digest());
    deltas
}

fn legacy_event_delta_pages(
    reconciliation: &CoreSourceReconciliation,
    deltas: Vec<CoreEventDelta>,
) -> Vec<CoreEventDeltaPage> {
    let materialization_id = "d".repeat(64);
    let generation_id = "a".repeat(64);
    let mut pages = Vec::new();
    let mut pending = Vec::new();
    let mut page_index = 0_u32;
    for delta in deltas {
        if pending.len() == MAX_CORE_EVENT_DELTA_PAGE_ITEMS {
            let page = CoreEventDeltaPage {
                materialization_id: materialization_id.clone(),
                core_generation_id: generation_id.clone(),
                reconciliation: reconciliation.clone(),
                page_index,
                terminal: false,
                deltas: std::mem::take(&mut pending),
            };
            page.validate().unwrap();
            pages.push(page);
            page_index += 1;
        }
        pending.push(delta);
        let candidate = CoreEventDeltaPage {
            materialization_id: materialization_id.clone(),
            core_generation_id: generation_id.clone(),
            reconciliation: reconciliation.clone(),
            page_index,
            terminal: false,
            deltas: pending.clone(),
        };
        if candidate.validate().is_err() {
            let overflow = pending.pop().unwrap();
            assert!(!pending.is_empty(), "legacy singleton exceeded its page");
            let page = CoreEventDeltaPage {
                materialization_id: materialization_id.clone(),
                core_generation_id: generation_id.clone(),
                reconciliation: reconciliation.clone(),
                page_index,
                terminal: false,
                deltas: std::mem::take(&mut pending),
            };
            page.validate().unwrap();
            pages.push(page);
            page_index += 1;
            pending.push(overflow);
        }
    }
    let page = CoreEventDeltaPage {
        materialization_id,
        core_generation_id: generation_id,
        reconciliation: reconciliation.clone(),
        page_index,
        terminal: true,
        deltas: pending,
    };
    page.validate().unwrap();
    pages.push(page);
    pages
}

fn linear_event_delta_pages(
    reconciliation: CoreSourceReconciliation,
    deltas: Vec<CoreEventDelta>,
) -> Vec<CoreEventDeltaPage> {
    let mut builder =
        CoreEventDeltaPageBuilder::new("d".repeat(64), "a".repeat(64), reconciliation, 0).unwrap();
    let mut pages = Vec::new();
    for delta in deltas {
        if let Some(page) = builder.push(delta).unwrap() {
            page.validate().unwrap();
            pages.push(page);
        }
    }
    let page = builder.finish();
    page.validate().unwrap();
    pages.push(page);
    pages
}

fn event_delta_pages_request(
    page_count: usize,
    final_terminal: bool,
) -> ApplyCoreEventDeltaPagesRequest {
    let source = source(7);
    let source_state = state(source.clone(), 2, page_count as u64);
    let mut records = (0..page_count)
        .map(|index| {
            record(
                &source,
                u64::try_from(index + 1).unwrap(),
                format!("body-{index}"),
                false,
            )
        })
        .collect::<Vec<_>>();
    records.sort_by_key(|record| record.event_id.digest());
    let pages = records
        .into_iter()
        .enumerate()
        .map(|(position, record)| CoreEventDeltaPage {
            materialization_id: "d".repeat(64),
            core_generation_id: "a".repeat(64),
            reconciliation: CoreSourceReconciliation {
                materialize_index: 0,
                delta: CoreSourceDelta::Present(source_state.clone()),
            },
            page_index: u32::try_from(position + 3).unwrap(),
            terminal: final_terminal && position + 1 == page_count,
            deltas: vec![CoreEventDelta::Added(record)],
        })
        .collect();
    ApplyCoreEventDeltaPagesRequest { pages }
}

fn multi_source_event_delta_pages_request(
    pages_per_source: &[usize],
) -> ApplyCoreEventDeltaPagesRequest {
    let mut sources = pages_per_source
        .iter()
        .enumerate()
        .map(|(index, page_count)| (source(u8::try_from(index + 32).unwrap()), *page_count))
        .collect::<Vec<_>>();
    sources.sort_by_key(|(source, _)| source.identity().digest());
    let mut pages = Vec::new();
    for (materialize_index, (source, page_count)) in sources.into_iter().enumerate() {
        let source_state = state(source.clone(), 2, u64::try_from(page_count).unwrap());
        let mut records = (0..page_count)
            .map(|index| {
                record(
                    &source,
                    u64::try_from(index + 1).unwrap(),
                    format!("multi-source-body-{index}"),
                    false,
                )
            })
            .collect::<Vec<_>>();
        records.sort_by_key(|record| record.event_id.digest());
        pages.extend(
            records
                .into_iter()
                .enumerate()
                .map(|(index, record)| CoreEventDeltaPage {
                    materialization_id: "d".repeat(64),
                    core_generation_id: "a".repeat(64),
                    reconciliation: CoreSourceReconciliation {
                        materialize_index: u32::try_from(materialize_index).unwrap(),
                        delta: CoreSourceDelta::Present(source_state.clone()),
                    },
                    page_index: u32::try_from(index).unwrap(),
                    terminal: index + 1 == page_count,
                    deltas: vec![CoreEventDelta::Added(record)],
                }),
        );
    }
    ApplyCoreEventDeltaPagesRequest { pages }
}

fn mixed_present_and_removed_event_delta_pages_request(
    present_source: SourceKey,
    removed_source: SourceKey,
) -> ApplyCoreEventDeltaPagesRequest {
    let present_record = record(&present_source, 1, "present source".to_owned(), false);
    let removed_event_id = record(&removed_source, 1, "removed source".to_owned(), false).event_id;
    ApplyCoreEventDeltaPagesRequest {
        pages: vec![
            CoreEventDeltaPage {
                materialization_id: "d".repeat(64),
                core_generation_id: "a".repeat(64),
                reconciliation: CoreSourceReconciliation {
                    materialize_index: 0,
                    delta: CoreSourceDelta::Present(state(present_source, 2, 1)),
                },
                page_index: 0,
                terminal: true,
                deltas: vec![CoreEventDelta::Added(present_record)],
            },
            CoreEventDeltaPage {
                materialization_id: "d".repeat(64),
                core_generation_id: "a".repeat(64),
                reconciliation: CoreSourceReconciliation {
                    materialize_index: 1,
                    delta: CoreSourceDelta::Removed(CoreSourceRemoval {
                        source: removed_source,
                    }),
                },
                page_index: 0,
                terminal: true,
                deltas: vec![CoreEventDelta::Tombstoned(CoreEventTombstone {
                    event_id: removed_event_id,
                    prior_core_record_sha256: "e".repeat(64),
                })],
            },
        ],
    }
}

fn event_delta_pages_applied(
    request: &ApplyCoreEventDeltaPagesRequest,
) -> CoreEventDeltaPagesApplied {
    let pages = request
        .pages
        .iter()
        .map(|page| CoreEventDeltaPageApplied {
            materialization_id: page.materialization_id.clone(),
            core_generation_id: page.core_generation_id.clone(),
            source: page.reconciliation.delta.source().clone(),
            page_index: page.page_index,
            additions: u32::try_from(
                page.deltas
                    .iter()
                    .filter(|delta| matches!(delta, CoreEventDelta::Added(_)))
                    .count(),
            )
            .unwrap(),
            replacements: u32::try_from(
                page.deltas
                    .iter()
                    .filter(|delta| matches!(delta, CoreEventDelta::Replaced(_)))
                    .count(),
            )
            .unwrap(),
            tombstones: u32::try_from(
                page.deltas
                    .iter()
                    .filter(|delta| matches!(delta, CoreEventDelta::Tombstoned(_)))
                    .count(),
            )
            .unwrap(),
            terminal: page.terminal,
            replayed: false,
        })
        .collect();
    CoreEventDeltaPagesApplied { pages }
}

fn request_with_encoded_wire_bytes(target: usize) -> ApplyCoreEventDeltaPagesRequest {
    let source = source(8);
    let page = CoreEventDeltaPage {
        materialization_id: "d".repeat(64),
        core_generation_id: "a".repeat(64),
        reconciliation: CoreSourceReconciliation {
            materialize_index: 0,
            delta: CoreSourceDelta::Present(state(source.clone(), 2, 1)),
        },
        page_index: 0,
        terminal: true,
        deltas: vec![CoreEventDelta::Added(record(
            &source,
            1,
            String::new(),
            false,
        ))],
    };
    let mut request = ApplyCoreEventDeltaPagesRequest { pages: vec![page] };
    let fixed_bytes = serde_json::to_vec(&request).unwrap().len();
    let variable_bytes = target.checked_sub(fixed_bytes).unwrap();
    let escaped_bytes = variable_bytes / 6;
    let plain_bytes = variable_bytes % 6;
    let mut body = "\0".repeat(escaped_bytes);
    body.push_str(&"x".repeat(plain_bytes));
    assert!(body.len() <= MAX_CORE_EVENT_DELTA_PAGE_CONTENT_BYTES);
    let CoreEventDelta::Added(record) = &mut request.pages[0].deltas[0] else {
        panic!("expected added Core event");
    };
    record.content.normalized_body = Some(body);
    assert_eq!(compact_json_encoded_len(&request).unwrap(), target);
    request
}

#[test]
fn complete_core_event_delta_page_transports_long_body_and_two_repository_scopes() {
    let source = source(1);
    let source_state = state(source.clone(), 2, 1);
    let body = format!("{}tail-marker", "x".repeat(20 * 1024));
    let record = record(&source, 1, body.clone(), true);
    let page = CoreEventDeltaPage {
        materialization_id: "d".repeat(64),
        core_generation_id: "a".repeat(64),
        reconciliation: CoreSourceReconciliation {
            materialize_index: 0,
            delta: CoreSourceDelta::Present(source_state),
        },
        page_index: 0,
        terminal: true,
        deltas: vec![CoreEventDelta::Added(record)],
    };
    page.validate().unwrap();

    let CoreEventDelta::Added(record) = &page.deltas[0] else {
        panic!("expected added event");
    };
    assert_eq!(
        record.content.normalized_body.as_deref(),
        Some(body.as_str())
    );
    assert_eq!(record.repository_bindings.len(), 2);
    assert_eq!(record.repository_file_observations.len(), 2);
    assert_eq!(record.repository_vcs_observations.len(), 1);
    let encoded = serde_json::to_string(&page).unwrap();
    assert!(encoded.contains("tail-marker"));
    assert!(!encoded.contains("source_locator"));
    assert!(!encoded.contains("worktree_root_locator"));

    let envelope = HostEnvelope {
        sequence: 1,
        request_id: uuid::Uuid::from_u128(1),
        message: HostMessage::ApplyCoreEventDeltaPage(ApplyCoreEventDeltaPageRequest { page }),
    };
    let mut frame = Vec::new();
    write_frame(&mut frame, &envelope).unwrap();
    assert!(frame.len() > 16 * 1024);
    assert_eq!(
        read_frame::<_, HostEnvelope>(&mut Cursor::new(frame)).unwrap(),
        envelope
    );
}

#[test]
fn event_delta_page_batches_accept_exactly_one_to_sixteen_pages() {
    assert_eq!(MAX_CORE_EVENT_DELTA_PAGES, 16);
    assert_eq!(
        MAX_CORE_EVENT_DELTA_PAGES_PREPARED_OUTPUT_BYTES,
        128 * 1024 * 1024
    );
    for page_count in [1, MAX_CORE_EVENT_DELTA_PAGES] {
        event_delta_pages_request(page_count, true)
            .validate()
            .unwrap();
    }
    assert_eq!(
        event_delta_pages_request(0, true)
            .validate()
            .unwrap_err()
            .class,
        ErrorClass::Bounds
    );
    assert_eq!(
        event_delta_pages_request(MAX_CORE_EVENT_DELTA_PAGES + 1, true)
            .validate()
            .unwrap_err()
            .class,
        ErrorClass::Bounds
    );
}

#[test]
fn event_delta_page_envelope_accepts_ordered_source_pinned_sub_batches() {
    let request = multi_source_event_delta_pages_request(&[2, 1, 13]);
    assert_eq!(request.pages.len(), MAX_CORE_EVENT_DELTA_PAGES);
    request.validate().unwrap();

    let response = event_delta_pages_applied(&request);
    response.validate_for(&request).unwrap();
    assert_eq!(
        response
            .pages
            .windows(2)
            .filter(|pages| !pages[0].source.exact_descriptor_eq(&pages[1].source))
            .count(),
        2
    );
}

#[test]
fn event_delta_page_envelope_accepts_mixed_sources_in_both_digest_orders() {
    let mut sources = [source(64), source(65)];
    sources.sort_by_key(|source| source.identity().digest());
    let lower = sources[0].clone();
    let higher = sources[1].clone();

    for (present, removed, present_digest_is_lower) in [
        (lower.clone(), higher.clone(), true),
        (higher, lower, false),
    ] {
        let present_digest = present.identity().digest();
        let removed_digest = removed.identity().digest();
        let request = mixed_present_and_removed_event_delta_pages_request(present, removed);
        assert_eq!(
            request
                .pages
                .iter()
                .map(|page| page.reconciliation.materialize_index)
                .collect::<Vec<_>>(),
            vec![0, 1]
        );
        assert_eq!(present_digest < removed_digest, present_digest_is_lower);
        request.validate().unwrap();
        event_delta_pages_applied(&request)
            .validate_for(&request)
            .unwrap();
    }
}

#[test]
fn event_delta_page_envelope_rejects_invalid_source_sub_batch_boundaries() {
    let request = multi_source_event_delta_pages_request(&[2, 1]);
    let first_source = request.pages[0].reconciliation.delta.source();
    let first_source_end = request
        .pages
        .iter()
        .position(|page| {
            !page
                .reconciliation
                .delta
                .source()
                .exact_descriptor_eq(first_source)
        })
        .unwrap();

    let mut unterminated = request.clone();
    unterminated.pages[first_source_end - 1].terminal = false;
    assert_eq!(
        unterminated.validate().unwrap_err().class,
        ErrorClass::Sequence
    );

    let mut nonzero_start = request.clone();
    nonzero_start.pages[first_source_end].page_index = 1;
    assert_eq!(
        nonzero_start.validate().unwrap_err().class,
        ErrorClass::Sequence
    );

    let mut out_of_order = request.clone();
    out_of_order.pages.rotate_left(first_source_end);
    assert_eq!(
        out_of_order.validate().unwrap_err().class,
        ErrorClass::Sequence
    );

    let mut mismatched_reconciliation = request;
    let first_source = mismatched_reconciliation.pages[0]
        .reconciliation
        .delta
        .source()
        .clone();
    mismatched_reconciliation.pages[1].reconciliation.delta =
        CoreSourceDelta::Present(state(first_source, 9, 2));
    assert_eq!(
        mismatched_reconciliation.validate().unwrap_err().class,
        ErrorClass::Sequence
    );
}

#[test]
fn event_delta_page_envelope_rejects_malformed_materialize_index_ordering() {
    let mut sources = [source(66), source(67)];
    sources.sort_by_key(|source| source.identity().digest());
    let request =
        mixed_present_and_removed_event_delta_pages_request(sources[0].clone(), sources[1].clone());

    let mut duplicate = request.clone();
    duplicate.pages[1].reconciliation.materialize_index = 0;
    assert_eq!(
        duplicate.validate().unwrap_err().class,
        ErrorClass::Sequence
    );

    let mut descending = request.clone();
    descending.pages[0].reconciliation.materialize_index = 2;
    assert_eq!(
        descending.validate().unwrap_err().class,
        ErrorClass::Sequence
    );

    let mut repeated_source = request;
    let mut repeated_page = repeated_source.pages[0].clone();
    repeated_page.reconciliation.materialize_index = 2;
    repeated_source.pages.push(repeated_page);
    assert_eq!(
        repeated_source.validate().unwrap_err().class,
        ErrorClass::Sequence
    );

    let mut changed_within_source = event_delta_pages_request(2, true);
    changed_within_source.pages[1]
        .reconciliation
        .materialize_index = 1;
    assert_eq!(
        changed_within_source.validate().unwrap_err().class,
        ErrorClass::Sequence
    );
}

#[test]
fn event_delta_page_envelope_acknowledges_each_source_identity() {
    let request = multi_source_event_delta_pages_request(&[1, 1]);
    let identity = request.acknowledgement_identity().unwrap();
    let mut response = event_delta_pages_applied(&request);
    response.pages[1].source = response.pages[0].source.clone();
    assert_eq!(
        response.validate_for_identity(&identity).unwrap_err().class,
        ErrorClass::Sequence
    );
}

#[test]
fn event_delta_page_batch_enforces_exact_aggregate_encoded_request_bound() {
    let mut request =
        request_with_encoded_wire_bytes(MAX_CORE_EVENT_DELTA_PAGES_REQUEST_WIRE_BYTES);
    request.validate().unwrap();

    let CoreEventDelta::Added(record) = &mut request.pages[0].deltas[0] else {
        panic!("expected added Core event");
    };
    record.content.normalized_body.as_mut().unwrap().push('x');
    assert_eq!(
        compact_json_encoded_len(&request).unwrap(),
        MAX_CORE_EVENT_DELTA_PAGES_REQUEST_WIRE_BYTES + 1
    );
    request.pages[0].validate().unwrap();
    assert_eq!(request.validate().unwrap_err().class, ErrorClass::Bounds);
}

#[test]
fn prepared_event_delta_page_batch_checks_complete_pages_and_exact_length() {
    let request = event_delta_pages_request(2, true);
    let encoded_request_bytes = serde_json::to_vec(&request).unwrap().len();
    ApplyCoreEventDeltaPagesRequest::validate_prepared_envelope(request.pages.iter()).unwrap();
    request
        .acknowledgement_identity_for_prepared_request(encoded_request_bytes)
        .unwrap();
    assert_eq!(
        request
            .acknowledgement_identity_for_prepared_request(encoded_request_bytes - 1)
            .unwrap_err()
            .class,
        ErrorClass::InvalidRequest
    );

    let mut invalid = request;
    invalid.pages[0].materialization_id = "not-a-digest".to_owned();
    assert_eq!(
        ApplyCoreEventDeltaPagesRequest::validate_prepared_envelope(invalid.pages.iter())
            .unwrap_err()
            .class,
        ErrorClass::InvalidRequest
    );
}

#[test]
fn encoded_bound_counting_writer_matches_compact_json_bytes_and_errors() {
    let values = [
        serde_json::json!(null),
        serde_json::json!({"plain": "value", "escaped": "\0\n\"\\", "number": 42}),
        serde_json::json!([true, false, {"nested": [1, 2, 3]}]),
    ];
    for value in values {
        assert_eq!(
            compact_json_encoded_len(&value).unwrap(),
            serde_json::to_vec(&value).unwrap().len()
        );
    }

    let invalid_json_key = BTreeMap::from([(vec![1_u8], 1_u8)]);
    let allocated_error = serde_json::to_vec(&invalid_json_key)
        .unwrap_err()
        .to_string();
    let counted_error = compact_json_encoded_len(&invalid_json_key)
        .unwrap_err()
        .to_string();
    assert_eq!(counted_error, allocated_error);
    assert_eq!(
        validate_encoded_bound(&invalid_json_key, usize::MAX, "unused bound").unwrap_err(),
        ProtocolError::new(ErrorClass::Internal, "protocol encoding failed")
    );
}

#[test]
fn event_delta_page_batch_retains_every_constituent_page_bound() {
    let source = source(10);
    let request = |bodies: Vec<String>| {
        let event_count = u64::try_from(bodies.len()).unwrap();
        let mut records = bodies
            .into_iter()
            .enumerate()
            .map(|(index, body)| record(&source, u64::try_from(index + 1).unwrap(), body, false))
            .collect::<Vec<_>>();
        records.sort_by_key(|record| record.event_id.digest());
        ApplyCoreEventDeltaPagesRequest {
            pages: vec![CoreEventDeltaPage {
                materialization_id: "d".repeat(64),
                core_generation_id: "a".repeat(64),
                reconciliation: CoreSourceReconciliation {
                    materialize_index: 0,
                    delta: CoreSourceDelta::Present(state(source.clone(), 2, event_count)),
                },
                page_index: 0,
                terminal: true,
                deltas: records.into_iter().map(CoreEventDelta::Added).collect(),
            }],
        }
    };

    let too_many_items = request(vec!["x".to_owned(); MAX_CORE_EVENT_DELTA_PAGE_ITEMS + 1]);
    assert_eq!(
        too_many_items.validate().unwrap_err().class,
        ErrorClass::Bounds
    );

    let too_much_content = request(vec![
        "x".repeat(MAX_CORE_EVENT_DELTA_PAGE_CONTENT_BYTES / 2 + 1),
        "y".repeat(MAX_CORE_EVENT_DELTA_PAGE_CONTENT_BYTES / 2),
    ]);
    assert!(too_much_content
        .validate()
        .unwrap_err()
        .message
        .contains("selected-content byte bound"));

    let wire_expansion_bytes = MAX_CORE_EVENT_DELTA_PAGE_WIRE_BYTES / 6 + 1;
    assert!(wire_expansion_bytes < MAX_CORE_EVENT_DELTA_PAGE_CONTENT_BYTES);
    let too_many_wire_bytes = request(vec!["\0".repeat(wire_expansion_bytes)]);
    assert!(too_many_wire_bytes
        .validate()
        .unwrap_err()
        .message
        .contains("page exceeds its wire bound"));
}

#[test]
fn event_delta_page_batch_rejects_cross_page_identity_mismatches() {
    let request = event_delta_pages_request(2, true);
    request.validate().unwrap();

    let mut wrong_materialization = request.clone();
    wrong_materialization.pages[1].materialization_id = "e".repeat(64);
    assert_eq!(
        wrong_materialization.validate().unwrap_err().class,
        ErrorClass::Sequence
    );

    let mut wrong_generation = request.clone();
    wrong_generation.pages[1].core_generation_id = "b".repeat(64);
    assert_eq!(
        wrong_generation.validate().unwrap_err().class,
        ErrorClass::Sequence
    );

    let mut wrong_reconciliation = request.clone();
    let CoreSourceDelta::Present(source_state) =
        &mut wrong_reconciliation.pages[1].reconciliation.delta
    else {
        panic!("expected present source");
    };
    source_state.core_record_accumulator = "e".repeat(64);
    assert_eq!(
        wrong_reconciliation.validate().unwrap_err().class,
        ErrorClass::Sequence
    );

    let mut wrong_source = request.clone();
    let other_source = source(9);
    wrong_source.pages[1].reconciliation = CoreSourceReconciliation {
        materialize_index: 0,
        delta: CoreSourceDelta::Present(state(other_source.clone(), 2, 1)),
    };
    wrong_source.pages[1].deltas = vec![CoreEventDelta::Added(record(
        &other_source,
        1,
        "other source".to_owned(),
        false,
    ))];
    assert_eq!(
        wrong_source.validate().unwrap_err().class,
        ErrorClass::Sequence
    );
}

#[test]
fn event_delta_page_batch_rejects_gaps_duplicates_order_regressions_and_early_terminal() {
    let request = event_delta_pages_request(2, true);

    let mut gap = request.clone();
    gap.pages[1].page_index += 1;
    assert_eq!(gap.validate().unwrap_err().class, ErrorClass::Sequence);

    let mut duplicate_index = request.clone();
    duplicate_index.pages[1].page_index = duplicate_index.pages[0].page_index;
    assert_eq!(
        duplicate_index.validate().unwrap_err().class,
        ErrorClass::Sequence
    );

    let mut descending = request.clone();
    let first_deltas = descending.pages[0].deltas.clone();
    descending.pages[0].deltas = descending.pages[1].deltas.clone();
    descending.pages[1].deltas = first_deltas;
    assert_eq!(
        descending.validate().unwrap_err().class,
        ErrorClass::Sequence
    );

    let mut duplicate_event = request.clone();
    duplicate_event.pages[1].deltas = duplicate_event.pages[0].deltas.clone();
    assert_eq!(
        duplicate_event.validate().unwrap_err().class,
        ErrorClass::Sequence
    );

    let mut page_after_terminal = request.clone();
    page_after_terminal.pages[0].terminal = true;
    assert_eq!(
        page_after_terminal.validate().unwrap_err().class,
        ErrorClass::Sequence
    );

    event_delta_pages_request(2, false).validate().unwrap();
}

#[test]
fn event_delta_page_batch_acknowledgement_binds_order_identity_and_counts_compactly() {
    let request = event_delta_pages_request(2, true);
    let identity = request.acknowledgement_identity().unwrap();
    let response = event_delta_pages_applied(&request);
    drop(request);
    response.validate_for_identity(&identity).unwrap();

    let mut reordered = response.clone();
    reordered.pages.swap(0, 1);
    assert_eq!(
        reordered
            .validate_for_identity(&identity)
            .unwrap_err()
            .class,
        ErrorClass::Sequence
    );

    let mut wrong_count = response.clone();
    wrong_count.pages[0].additions += 1;
    assert_eq!(
        wrong_count
            .validate_for_identity(&identity)
            .unwrap_err()
            .class,
        ErrorClass::Sequence
    );

    let mut missing_page = response.clone();
    missing_page.pages.pop();
    assert_eq!(
        missing_page
            .validate_for_identity(&identity)
            .unwrap_err()
            .class,
        ErrorClass::Sequence
    );

    let mut wrong_identity = response;
    wrong_identity.pages[0].materialization_id = "e".repeat(64);
    assert_eq!(
        wrong_identity
            .validate_for_identity(&identity)
            .unwrap_err()
            .class,
        ErrorClass::Sequence
    );
}

#[test]
fn event_delta_acknowledgement_uses_compact_identity_after_request_moves() {
    let source = source(1);
    let source_state = state(source.clone(), 2, 1);
    let page = CoreEventDeltaPage {
        materialization_id: "d".repeat(64),
        core_generation_id: "a".repeat(64),
        reconciliation: CoreSourceReconciliation {
            materialize_index: 0,
            delta: CoreSourceDelta::Present(source_state),
        },
        page_index: 7,
        terminal: true,
        deltas: vec![CoreEventDelta::Added(record(
            &source,
            1,
            "x".repeat(1024 * 1024),
            false,
        ))],
    };
    page.validate().unwrap();
    let identity = page.acknowledgement_identity();
    let request = ApplyCoreEventDeltaPageRequest { page };
    let mut acknowledgement = CoreEventDeltaPageApplied {
        materialization_id: request.page.materialization_id.clone(),
        core_generation_id: request.page.core_generation_id.clone(),
        source: request.page.reconciliation.delta.source().clone(),
        page_index: request.page.page_index,
        additions: 1,
        replacements: 0,
        tombstones: 0,
        terminal: true,
        replayed: false,
    };
    drop(request);

    acknowledgement.validate_for_identity(&identity).unwrap();
    acknowledgement.page_index = 8;
    assert_eq!(
        acknowledgement
            .validate_for_identity(&identity)
            .unwrap_err()
            .class,
        ErrorClass::Sequence
    );
}

#[test]
fn linear_event_delta_builder_preserves_legacy_page_bytes_and_item_boundaries() {
    let source = source(3);
    let deltas = ordered_additions(&source, MAX_CORE_EVENT_DELTA_PAGE_ITEMS + 1, 64);
    let reconciliation = reconciliation(&source, deltas.len());

    let legacy = legacy_event_delta_pages(&reconciliation, deltas.clone());
    let linear = linear_event_delta_pages(reconciliation, deltas);

    assert_eq!(linear, legacy);
    assert_eq!(linear.len(), 2);
    assert_eq!(linear[0].deltas.len(), MAX_CORE_EVENT_DELTA_PAGE_ITEMS);
    assert_eq!(linear[1].deltas.len(), 1);
    assert!(!linear[0].terminal);
    assert!(linear[1].terminal);
    for (linear, legacy) in linear.iter().zip(legacy) {
        assert_eq!(
            serde_json::to_vec(linear).unwrap(),
            serde_json::to_vec(&legacy).unwrap()
        );
    }
}

#[test]
fn linear_event_delta_builder_charges_exact_encoded_bytes_and_rejects_singletons() {
    let source = source(4);
    let mut deltas = ordered_additions(&source, 2, 128);
    let first = deltas.remove(0);
    let second = deltas.remove(0);
    let reconciliation = reconciliation(&source, 2);
    let exact_singleton = CoreEventDeltaPage {
        materialization_id: "d".repeat(64),
        core_generation_id: "a".repeat(64),
        reconciliation: reconciliation.clone(),
        page_index: 0,
        terminal: false,
        deltas: vec![first.clone()],
    };
    let exact_wire_bytes = serde_json::to_vec(&exact_singleton).unwrap().len();

    let mut builder = CoreEventDeltaPageBuilder::with_test_limits(
        "d".repeat(64),
        "a".repeat(64),
        reconciliation.clone(),
        0,
        MAX_CORE_EVENT_DELTA_PAGE_CONTENT_BYTES,
        exact_wire_bytes,
    )
    .unwrap();
    assert!(builder.push(first.clone()).unwrap().is_none());
    builder.finish().validate().unwrap();

    let second_singleton_wire_bytes = serde_json::to_vec(&CoreEventDeltaPage {
        materialization_id: "d".repeat(64),
        core_generation_id: "a".repeat(64),
        reconciliation: reconciliation.clone(),
        page_index: 1,
        terminal: false,
        deltas: vec![second.clone()],
    })
    .unwrap()
    .len();
    let mut splitting = CoreEventDeltaPageBuilder::with_test_limits(
        "d".repeat(64),
        "a".repeat(64),
        reconciliation.clone(),
        0,
        MAX_CORE_EVENT_DELTA_PAGE_CONTENT_BYTES,
        exact_wire_bytes.max(second_singleton_wire_bytes),
    )
    .unwrap();
    assert!(splitting.push(first.clone()).unwrap().is_none());
    let completed = splitting.push(second).unwrap().unwrap();
    assert_eq!(completed, exact_singleton);
    completed.validate().unwrap();
    splitting.finish().validate().unwrap();

    let mut too_small = CoreEventDeltaPageBuilder::with_test_limits(
        "d".repeat(64),
        "a".repeat(64),
        reconciliation,
        0,
        MAX_CORE_EVENT_DELTA_PAGE_CONTENT_BYTES,
        exact_wire_bytes - 1,
    )
    .unwrap();
    let error = too_small.push(first).unwrap_err();
    assert_eq!(error.class, ErrorClass::Bounds);
    assert_eq!(error.message, "one Core event delta exceeds its page bound");
}

#[test]
#[ignore = "manual scaling measurement"]
fn core_event_delta_page_builder_scaling() {
    const ITERATIONS: usize = 64;
    const SAMPLES: usize = 7;

    let source = source(5);
    let deltas = ordered_additions(&source, MAX_CORE_EVENT_DELTA_PAGE_ITEMS, 1024);
    let reconciliation = reconciliation(&source, deltas.len());

    for count in [32_usize, 64, 128, 256] {
        let mut samples = Vec::with_capacity(SAMPLES);
        for _ in 0..SAMPLES {
            let started = std::time::Instant::now();
            for _ in 0..ITERATIONS {
                let mut builder = CoreEventDeltaPageBuilder::new(
                    "d".repeat(64),
                    "a".repeat(64),
                    reconciliation.clone(),
                    0,
                )
                .unwrap();
                for delta in deltas.iter().take(count).cloned() {
                    assert!(builder.push(delta).unwrap().is_none());
                }
                let page = builder.finish();
                page.validate().unwrap();
                let acknowledgement_identity = page.acknowledgement_identity();
                std::hint::black_box((page, acknowledgement_identity));
            }
            samples.push(started.elapsed().as_nanos() / u128::try_from(ITERATIONS).unwrap());
        }
        samples.sort_unstable();
        let median_ns = samples[SAMPLES / 2];
        eprintln!(
            "core_event_delta_page_builder count={count} median_ns={median_ns} ns_per_delta={}",
            median_ns / u128::try_from(count).unwrap()
        );
    }
}

#[test]
fn delta_ack_selects_only_exact_changed_revisions_for_record_materialization() {
    let unchanged = state(source(1), 1, 10_000);
    let changed = state(source(2), 2, 1);
    let page = CoreSourceDeltaPage::new(
        "d".repeat(64),
        "a".repeat(64),
        0,
        true,
        vec![
            CoreSourceDelta::Present(unchanged),
            CoreSourceDelta::Present(changed.clone()),
        ],
    )
    .unwrap();
    let request = ApplyCoreSourceDeltaPageRequest {
        page: page.clone(),
        acknowledgement_page_index: 0,
    };
    let identity = request.acknowledgement_identity();
    CoreSourceDeltaPageApplied {
        materialization_id: page.materialization_id.clone(),
        core_generation_id: page.core_generation_id.clone(),
        page_index: 0,
        acknowledgement_page_index: 0,
        acknowledgement_terminal: true,
        changed_sources: 1,
        removed_sources: 0,
        reconcile_sources: vec![CoreSourceReconciliation {
            materialize_index: 0,
            delta: CoreSourceDelta::Present(changed.clone()),
        }],
        replayed: false,
    }
    .validate_for_identity(&identity)
    .unwrap();

    let mut stale = changed;
    stale.core_record_accumulator = "f".repeat(64);
    let invalid = CoreSourceDeltaPageApplied {
        materialization_id: page.materialization_id.clone(),
        core_generation_id: page.core_generation_id.clone(),
        page_index: 0,
        acknowledgement_page_index: 0,
        acknowledgement_terminal: true,
        changed_sources: 1,
        removed_sources: 0,
        reconcile_sources: vec![CoreSourceReconciliation {
            materialize_index: 0,
            delta: CoreSourceDelta::Present(stale),
        }],
        replayed: false,
    };
    assert_eq!(
        invalid.validate_for_identity(&identity).unwrap_err().class,
        ErrorClass::Sequence
    );
}

#[test]
fn source_snapshot_changes_with_exact_core_record_accumulator() {
    let first = state(source(1), 1, 4);
    let mut changed = first.clone();
    changed.core_record_accumulator = "f".repeat(64);

    let first_head = head(&[first]);
    let changed_head = head(&[changed]);
    assert_ne!(
        first_head.source_snapshot_sha256,
        changed_head.source_snapshot_sha256
    );
}

#[test]
fn source_pages_are_complete_snapshots_with_empty_terminal_and_derived_removal() {
    let materialization_id = "d".repeat(64);
    let generation_id = "a".repeat(64);
    let empty = CoreSourceDeltaPage::new(
        materialization_id.clone(),
        generation_id.clone(),
        0,
        true,
        Vec::new(),
    )
    .unwrap();
    assert!(CoreSourceDeltaPage::new(
        materialization_id.clone(),
        generation_id.clone(),
        0,
        false,
        Vec::new(),
    )
    .is_err());
    assert!(CoreSourceDeltaPage::new(
        materialization_id,
        generation_id,
        0,
        true,
        vec![CoreSourceDelta::Removed(CoreSourceRemoval {
            source: source(9)
        })],
    )
    .is_err());

    let request = ApplyCoreSourceDeltaPageRequest {
        page: empty.clone(),
        acknowledgement_page_index: 0,
    };
    CoreSourceDeltaPageApplied {
        materialization_id: empty.materialization_id.clone(),
        core_generation_id: empty.core_generation_id.clone(),
        page_index: 0,
        acknowledgement_page_index: 0,
        acknowledgement_terminal: true,
        changed_sources: 0,
        removed_sources: 1,
        reconcile_sources: vec![CoreSourceReconciliation {
            materialize_index: 0,
            delta: CoreSourceDelta::Removed(CoreSourceRemoval { source: source(9) }),
        }],
        replayed: false,
    }
    .validate_for(&request)
    .unwrap();
}

#[test]
fn source_delta_pages_require_stable_order_and_exact_page_cas() {
    let first = state(source(1), 1, 2);
    let second = state(source(2), 2, 3);
    let page = CoreSourceDeltaPage::new(
        "d".repeat(64),
        "a".repeat(64),
        0,
        true,
        vec![
            CoreSourceDelta::Present(first),
            CoreSourceDelta::Present(second),
        ],
    )
    .unwrap();
    let request = ApplyCoreSourceDeltaPageRequest {
        page: page.clone(),
        acknowledgement_page_index: 0,
    };
    CoreSourceDeltaPageApplied {
        materialization_id: page.materialization_id.clone(),
        core_generation_id: page.core_generation_id.clone(),
        page_index: 0,
        acknowledgement_page_index: 0,
        acknowledgement_terminal: true,
        changed_sources: 2,
        removed_sources: 0,
        reconcile_sources: page
            .deltas
            .iter()
            .enumerate()
            .filter_map(|(materialize_index, delta)| match delta {
                CoreSourceDelta::Present(state) => Some(CoreSourceReconciliation {
                    materialize_index: u32::try_from(materialize_index).unwrap(),
                    delta: CoreSourceDelta::Present(state.clone()),
                }),
                CoreSourceDelta::Removed(_) => None,
            })
            .collect(),
        replayed: false,
    }
    .validate_for(&request)
    .unwrap();

    let mut reversed = page.deltas.clone();
    reversed.reverse();
    assert!(CoreSourceDeltaPage::new("d".repeat(64), "a".repeat(64), 0, true, reversed,).is_err());
}

#[test]
fn source_acknowledgement_pages_are_bounded_and_cursor_pinned() {
    let page =
        CoreSourceDeltaPage::new("d".repeat(64), "a".repeat(64), 0, true, Vec::new()).unwrap();
    let request = ApplyCoreSourceDeltaPageRequest {
        page,
        acknowledgement_page_index: 7,
    };
    let reconciliations = (0..MAX_CORE_SOURCE_DELTA_PAGE_ITEMS)
        .map(|materialize_index| CoreSourceReconciliation {
            materialize_index: u32::try_from(materialize_index).unwrap(),
            delta: CoreSourceDelta::Removed(CoreSourceRemoval { source: source(9) }),
        })
        .collect::<Vec<_>>();
    let valid = CoreSourceDeltaPageApplied {
        materialization_id: request.page.materialization_id.clone(),
        core_generation_id: request.page.core_generation_id.clone(),
        page_index: request.page.page_index,
        acknowledgement_page_index: 7,
        acknowledgement_terminal: false,
        changed_sources: 0,
        removed_sources: u32::try_from(reconciliations.len()).unwrap(),
        reconcile_sources: reconciliations,
        replayed: false,
    };
    valid.validate_for(&request).unwrap();

    let mut wrong_cursor = valid.clone();
    wrong_cursor.acknowledgement_page_index = 8;
    assert_eq!(
        wrong_cursor.validate_for(&request).unwrap_err().class,
        ErrorClass::Sequence
    );

    let mut oversized = valid.clone();
    oversized.reconcile_sources.push(CoreSourceReconciliation {
        materialize_index: u32::try_from(MAX_CORE_SOURCE_DELTA_PAGE_ITEMS).unwrap(),
        delta: CoreSourceDelta::Removed(CoreSourceRemoval { source: source(9) }),
    });
    oversized.removed_sources = oversized.removed_sources.saturating_add(1);
    assert_eq!(
        oversized.validate_for(&request).unwrap_err().class,
        ErrorClass::Bounds
    );

    let mut empty_nonterminal = valid;
    empty_nonterminal.reconcile_sources.clear();
    empty_nonterminal.removed_sources = 0;
    let empty_error = empty_nonterminal.validate_for(&request).unwrap_err();
    assert_eq!(empty_error.class, ErrorClass::Sequence);
    assert_eq!(
        empty_error.message,
        "Core source delta acknowledgement cannot be empty before terminal"
    );

    let current = state(source(1), 1, 0);
    let terminal_page = CoreSourceDeltaPage::new(
        "d".repeat(64),
        "a".repeat(64),
        0,
        true,
        vec![CoreSourceDelta::Present(current.clone())],
    )
    .unwrap();
    let later_request = ApplyCoreSourceDeltaPageRequest {
        page: terminal_page,
        acknowledgement_page_index: 1,
    };
    let later_present = CoreSourceDeltaPageApplied {
        materialization_id: later_request.page.materialization_id.clone(),
        core_generation_id: later_request.page.core_generation_id.clone(),
        page_index: 0,
        acknowledgement_page_index: 1,
        acknowledgement_terminal: true,
        changed_sources: 1,
        removed_sources: 0,
        reconcile_sources: vec![CoreSourceReconciliation {
            materialize_index: 0,
            delta: CoreSourceDelta::Present(current.clone()),
        }],
        replayed: false,
    };
    let later_error = later_present.validate_for(&later_request).unwrap_err();
    assert_eq!(later_error.class, ErrorClass::Sequence);
    assert_eq!(
        later_error.message,
        "current Core sources are valid only on acknowledgement page zero"
    );

    let nonterminal_page = CoreSourceDeltaPage::new(
        "d".repeat(64),
        "a".repeat(64),
        0,
        false,
        vec![CoreSourceDelta::Present(current.clone())],
    )
    .unwrap();
    let nonterminal_request = ApplyCoreSourceDeltaPageRequest {
        page: nonterminal_page,
        acknowledgement_page_index: 0,
    };
    let nonterminal_acknowledgement = CoreSourceDeltaPageApplied {
        materialization_id: nonterminal_request.page.materialization_id.clone(),
        core_generation_id: nonterminal_request.page.core_generation_id.clone(),
        page_index: 0,
        acknowledgement_page_index: 0,
        acknowledgement_terminal: false,
        changed_sources: 1,
        removed_sources: 0,
        reconcile_sources: vec![CoreSourceReconciliation {
            materialize_index: 0,
            delta: CoreSourceDelta::Present(current),
        }],
        replayed: false,
    };
    let nonterminal_error = nonterminal_acknowledgement
        .validate_for(&nonterminal_request)
        .unwrap_err();
    assert_eq!(nonterminal_error.class, ErrorClass::Sequence);
    assert_eq!(
        nonterminal_error.message,
        "nonterminal Core source delta pages must complete in one acknowledgement page"
    );
}
