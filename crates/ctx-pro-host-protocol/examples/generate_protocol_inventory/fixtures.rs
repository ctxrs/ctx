use super::*;
use ctx_history_core::{
    core_record_contract_fingerprint, derive_event_id, derive_session_id, CoreContent,
    CoreContentPolicyStatus, EventIdentityInput, NativeItemKey, NativeSessionKey,
    RepositoryCandidateEvidence, RepositoryCandidateKind, SessionIdentityInput, SourceAnchor,
    TypedKey, CORE_CONTENT_POLICY_REVISION, CORE_NORMALIZATION_REVISION, CORE_RECORD_VERSION,
};

pub(super) fn source(lineage: u8) -> SourceKey {
    SourceKey::derive(
        "golden",
        "golden_jsonl",
        "golden-v1",
        1,
        SourceAnchor::CatalogLineage([lineage; 32]),
    )
    .expect("golden source")
}

pub(super) fn source_state(lineage: u8, revision: u8, event_count: u64) -> CoreSourceState {
    CoreSourceState {
        source: source(lineage),
        core_record_accumulator: format!("{revision:064x}"),
        event_count,
    }
}

pub(super) fn source_removal() -> CoreSourceRemoval {
    CoreSourceRemoval { source: source(2) }
}

pub(super) fn source_deltas() -> Vec<CoreSourceDelta> {
    let mut deltas = vec![CoreSourceDelta::Present(source_state(1, 1, 1))];
    deltas.sort_by_key(|delta| delta.source().identity().digest());
    deltas
}

pub(super) fn head() -> CoreGenerationHead {
    CoreGenerationHead::new(
        "a".repeat(64),
        4,
        1,
        core_record_contract_fingerprint(),
        3,
        2,
        "c".repeat(64),
        &[source_state(1, 1, 1)],
    )
    .expect("golden head")
}

pub(super) fn receipt() -> CoreMaterializationReceipt {
    let head = head();
    CoreMaterializationReceipt {
        core_generation_id: head.core_generation_id,
        core_record_contract_fingerprint: head.core_record_contract_fingerprint,
        source_snapshot_sha256: head.source_snapshot_sha256,
        materializer_revision: "golden-core-materializer-v1".to_owned(),
        source_count: head.source_count,
        event_count: head.event_count,
    }
}

pub(super) fn begin_request() -> BeginCoreMaterializationRequest {
    BeginCoreMaterializationRequest {
        head: head(),
        expected_prior_receipt: None,
    }
}

pub(super) fn materialization_id() -> String {
    core_materialization_id(&begin_request(), "golden-core-materializer-v1")
        .expect("golden materialization ID")
}

pub(super) fn delta_page() -> CoreSourceDeltaPage {
    CoreSourceDeltaPage::new(
        materialization_id(),
        "a".repeat(64),
        0,
        true,
        source_deltas(),
    )
    .expect("golden delta page")
}

pub(super) fn record() -> CoreRecord {
    let source = source(1);
    let session_id = derive_session_id(SessionIdentityInput {
        source: &source,
        logical_session_kind: "thread",
        native_session_key: &NativeSessionKey::native_id("session", TypedKey::U64(1))
            .expect("session key"),
    })
    .expect("session ID");
    let event_id = derive_event_id(EventIdentityInput {
        source: &source,
        session_id,
        logical_item_kind: "message",
        native_item_key: &NativeItemKey::native_id("event", TypedKey::U64(1)).expect("event key"),
        subrecord_selector: None,
    })
    .expect("event ID");
    let mut repository_candidate_evidence = RepositoryCandidateEvidence::default();
    repository_candidate_evidence.insert(
        RepositoryCandidateKind::FileActivityPath,
        "/golden/repo/src/lib.rs".to_owned(),
    );
    repository_candidate_evidence.insert(
        RepositoryCandidateKind::SessionCwd,
        "/golden/repo".to_owned(),
    );
    CoreRecord {
        record_version: CORE_RECORD_VERSION,
        event_id,
        session_id,
        parent_session_id: None,
        root_session_id: session_id,
        source,
        provider_session_id: Some("golden-session".to_owned()),
        native_event_id: None,
        event_sequence: 1,
        occurred_at_unix_ms: Some(1_700_000_000_000),
        event_type: "message".to_owned(),
        role: Some("assistant".to_owned()),
        agent_type: "primary".to_owned(),
        is_primary: true,
        workspace: None,
        branch: None,
        cwd: None,
        parser_revision: "golden-parser-v1".to_owned(),
        normalization_revision: CORE_NORMALIZATION_REVISION,
        content: CoreContent {
            policy_revision: CORE_CONTENT_POLICY_REVISION,
            policy_status: CoreContentPolicyStatus::Selected,
            normalized_body: Some("complete golden Core body".to_owned()),
            structured_content: None,
        },
        metadata: BTreeMap::new(),
        repository_candidate_evidence,
        repository_bindings: Vec::new(),
        repository_abstentions: Vec::new(),
        repository_file_invocation_evidence: Vec::new(),
        repository_file_observations: Vec::new(),
        repository_vcs_observations: Vec::new(),
    }
}

pub(super) fn reconciliation() -> CoreSourceReconciliation {
    CoreSourceReconciliation {
        materialize_index: 0,
        delta: CoreSourceDelta::Present(source_state(1, 1, 1)),
    }
}

pub(super) fn event_state_request() -> CoreEventStatePageRequest {
    CoreEventStatePageRequest {
        materialization_id: materialization_id(),
        core_generation_id: "a".repeat(64),
        reconciliation: reconciliation(),
        page_index: 0,
        after_event_id: None,
        maximum_items: MAX_CORE_EVENT_STATE_PAGE_ITEMS as u32,
    }
}

pub(super) fn event_state_page() -> CoreEventStatePage {
    let request = event_state_request();
    CoreEventStatePage {
        materialization_id: request.materialization_id,
        core_generation_id: request.core_generation_id,
        reconciliation: request.reconciliation,
        page_index: request.page_index,
        after_event_id: request.after_event_id,
        states: Vec::new(),
        terminal: true,
        replayed: false,
    }
}

pub(super) fn event_delta_page() -> CoreEventDeltaPage {
    CoreEventDeltaPage {
        materialization_id: materialization_id(),
        core_generation_id: "a".repeat(64),
        reconciliation: reconciliation(),
        page_index: 0,
        terminal: true,
        deltas: vec![CoreEventDelta::Added(record())],
    }
}

pub(super) fn event_delta_pages_request() -> ApplyCoreEventDeltaPagesRequest {
    ApplyCoreEventDeltaPagesRequest {
        pages: vec![event_delta_page()],
    }
}

pub(super) fn finish_request() -> FinishCoreMaterializationRequest {
    FinishCoreMaterializationRequest {
        materialization_id: materialization_id(),
        head: head(),
        expected_prior_receipt: None,
        source_delta_pages: 1,
        changed_sources: 1,
        removed_sources: 1,
        event_delta_pages: 2,
        event_mutations: 2,
    }
}

pub(super) fn authorization() -> AuthorizationRequest {
    AuthorizationRequest {
        entitlement: SignedEntitlement {
            grant: EntitlementGrant {
                schema_version: ENTITLEMENT_SCHEMA_VERSION,
                issuer: "https://commercial.ctx.rs".to_owned(),
                key_id: "fixture-v1".to_owned(),
                grant_id: "grant-1".to_owned(),
                subject: "user-1".to_owned(),
                account_id: "account-1".to_owned(),
                product: "ctx-local-pro".to_owned(),
                access_kind: EntitlementAccessKind::Trial,
                installation_key_thumbprint: base64url(&[1; 32]),
                issued_at_unix: 100,
                not_before_unix: 90,
                refresh_after_unix: 150,
                access_deadline_unix: 200,
                grace_deadline_unix: 250,
                expires_at_unix: 175,
                minimum_helper_protocol: PROTOCOL_VERSION,
                revocation_epoch: 0,
                capabilities: BTreeSet::from([
                    EntitlementCapability::GraphRead,
                    EntitlementCapability::GraphWrite,
                ]),
            },
            signature_base64url: base64url(&[2; ED25519_SIGNATURE_BYTES]),
        },
        installation_public_key_base64url: base64url(&[3; INSTALLATION_PUBLIC_KEY_BYTES]),
        challenge_base64url: base64url(&[4; AUTHORIZATION_CHALLENGE_BYTES]),
        proof_signature_base64url: base64url(&[5; ED25519_SIGNATURE_BYTES]),
    }
}

pub(super) fn blame_request() -> BlameRequest {
    BlameRequest {
        target: BlameTarget::Commit {
            oid: "0123456789abcdef".to_owned(),
            repository: Some("ctxrs/ctx".to_owned()),
        },
        limit: 10,
        cursor: None,
        expected_snapshot: QuerySnapshotExpectation::Core {
            receipt: CoreMaterializationReceiptIdentity::from_receipt(&receipt())
                .expect("golden receipt identity"),
        },
    }
}

pub(super) fn blame_result() -> BlameResult {
    BlameResult {
        snapshot: blame_request().expected_snapshot,
        target: ResolvedBlameTarget::Commit {
            commit: ResourceRef {
                id: "commit:golden".to_owned(),
                kind: ResourceKind::Commit,
                display: "0123456789abcdef".to_owned(),
            },
            repository: ResourceRef {
                id: "repository:ctxrs-ctx".to_owned(),
                kind: ResourceKind::Repository,
                display: "ctxrs/ctx".to_owned(),
            },
        },
        git_snapshot: None,
        matches: Vec::new(),
        evidence: Vec::new(),
        next: None,
    }
}
