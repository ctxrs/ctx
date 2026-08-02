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
        metadata: BTreeMap::new(),
        repository_candidate_evidence: RepositoryCandidateEvidence::default(),
        repository_bindings,
        repository_abstentions: Vec::new(),
        repository_file_observations,
        repository_vcs_observations,
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

#[test]
fn source_acknowledgement_sizing_is_the_exact_complete_frame_at_decimal_boundaries() {
    let response = CoreSourceDeltaPageApplied {
        materialization_id: "d".repeat(64),
        core_generation_id: "a".repeat(64),
        page_index: 0,
        acknowledgement_page_index: 9,
        acknowledgement_terminal: false,
        changed_sources: 0,
        removed_sources: 1,
        reconcile_sources: vec![CoreSourceReconciliation {
            materialize_index: 9,
            delta: CoreSourceDelta::Removed(CoreSourceRemoval { source: source(9) }),
        }],
        replayed: false,
    };
    for sequence in [0, 9, 10, 99, 100, u64::MAX] {
        let mut frame = Vec::new();
        write_frame(
            &mut frame,
            &HelperEnvelope {
                sequence,
                request_id: Uuid::from_u128(1),
                message: HelperMessage::CoreSourceDeltaPageApplied(response.clone()),
            },
        )
        .unwrap();
        assert_eq!(
            core_source_delta_page_applied_frame_wire_bytes(sequence, &response).unwrap(),
            frame.len()
        );
    }
    assert_eq!(
        core_source_delta_page_applied_frame_wire_bytes(10, &response).unwrap(),
        core_source_delta_page_applied_frame_wire_bytes(9, &response).unwrap() + 1
    );
    assert_eq!(
        core_source_delta_page_applied_frame_wire_bytes(100, &response).unwrap(),
        core_source_delta_page_applied_frame_wire_bytes(99, &response).unwrap() + 1
    );

    let mut cursor_ten = response.clone();
    cursor_ten.acknowledgement_page_index = 10;
    assert_eq!(
        core_source_delta_page_applied_frame_wire_bytes(u64::MAX, &cursor_ten).unwrap(),
        core_source_delta_page_applied_frame_wire_bytes(u64::MAX, &response).unwrap() + 1
    );
    let mut cursor_ninety_nine = response;
    cursor_ninety_nine.acknowledgement_page_index = 99;
    let ninety_nine =
        core_source_delta_page_applied_frame_wire_bytes(u64::MAX, &cursor_ninety_nine).unwrap();
    cursor_ninety_nine.acknowledgement_page_index = 100;
    assert_eq!(
        core_source_delta_page_applied_frame_wire_bytes(u64::MAX, &cursor_ninety_nine).unwrap(),
        ninety_nine + 1
    );
}

#[test]
fn source_delta_request_sizing_is_the_exact_complete_host_frame() {
    let request = ApplyCoreSourceDeltaPageRequest {
        page: CoreSourceDeltaPage::new(
            "d".repeat(64),
            "a".repeat(64),
            0,
            true,
            vec![CoreSourceDelta::Present(state(escaped_source(), 1, 0))],
        )
        .unwrap(),
        acknowledgement_page_index: 99,
    };
    for sequence in [0, 9, 10, 99, 100, u64::MAX] {
        let mut frame = Vec::new();
        write_frame(
            &mut frame,
            &HostEnvelope {
                sequence,
                request_id: Uuid::from_u128(1),
                message: HostMessage::ApplyCoreSourceDeltaPage(request.clone()),
            },
        )
        .unwrap();
        assert_eq!(
            apply_core_source_delta_page_request_frame_wire_bytes(sequence, &request).unwrap(),
            frame.len()
        );
    }
}

#[test]
fn source_delta_request_frame_bound_rejects_before_consumer_mutation() {
    let request = ApplyCoreSourceDeltaPageRequest {
        page: CoreSourceDeltaPage::new(
            "d".repeat(64),
            "a".repeat(64),
            0,
            true,
            vec![CoreSourceDelta::Present(state(escaped_source(), 1, 0))],
        )
        .unwrap(),
        acknowledgement_page_index: 0,
    };
    request.page.validate().unwrap();
    let request_bytes = serde_json::to_vec(&request).unwrap().len();
    let frame_bytes =
        apply_core_source_delta_page_request_frame_wire_bytes(u64::MAX, &request).unwrap();
    assert!(request_bytes < frame_bytes);
    request
        .validate_with_control_frame_wire_bound(frame_bytes)
        .unwrap();
    let mut consumer_mutated = false;
    assert_eq!(
        request
            .validate_with_control_frame_wire_bound(frame_bytes - 1)
            .map(|()| consumer_mutated = true)
            .unwrap_err()
            .class,
        ErrorClass::Bounds
    );
    assert!(!consumer_mutated);
    assert_eq!(
        crate::message::apply_core_source_delta_page_request_frame_wire_bytes_from_request_bytes(
            u64::MAX,
            usize::MAX,
        )
        .unwrap_err()
        .class,
        ErrorClass::Bounds
    );
}

#[test]
fn generation_begin_and_finish_fail_closed_on_mismatched_cas() {
    let sources = vec![state(source(1), 1, 0)];
    let request = BeginCoreMaterializationRequest {
        head: head(&sources),
        expected_prior_receipt: None,
    };
    let revision = "materializer-v2";
    let identity = request.acknowledgement_identity().unwrap();
    let expected_materialization_id = canonical_sha256(
        &(&request, revision),
        "Core materialization ID encoding failed",
    )
    .unwrap();
    let mut began = CoreMaterializationBegan {
        materialization_id: core_materialization_id(&request, revision).unwrap(),
        core_generation_id: request.head.core_generation_id.clone(),
        materializer_revision: revision.to_owned(),
        expected_prior_receipt: None,
        replayed: false,
    };
    assert_eq!(began.materialization_id, expected_materialization_id);
    drop(request);
    began.validate_for_identity(&identity).unwrap();
    began.core_generation_id = "f".repeat(64);
    assert!(began.validate_for_identity(&identity).is_err());
}

#[test]
fn receipt_identity_binds_generation_sources_and_materializer_revision() {
    let sources = vec![state(source(1), 1, 4), state(source(2), 2, 5)];
    let head = head(&sources);
    let receipt = CoreMaterializationReceipt {
        core_generation_id: head.core_generation_id.clone(),
        core_record_contract_fingerprint: head.core_record_contract_fingerprint.clone(),
        source_snapshot_sha256: head.source_snapshot_sha256.clone(),
        materializer_revision: "materializer-v1".to_owned(),
        source_count: 2,
        event_count: 9,
    };
    receipt.validate_for_head(&head).unwrap();
    let first = CoreMaterializationReceiptIdentity::from_receipt(&receipt).unwrap();
    let mut revised = receipt.clone();
    revised.materializer_revision = "materializer-v2".to_owned();
    let second = CoreMaterializationReceiptIdentity::from_receipt(&revised).unwrap();
    assert_ne!(first.materializer_revision, second.materializer_revision);
}
