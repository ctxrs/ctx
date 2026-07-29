use super::*;
use ctx_history_core::{
    derive_event_id, derive_session_id, CertifiedSource, CertifiedSourceDeletion,
    CertifiedSourceInventory, EventIdentityInput, LocatorRevisionPolicy, NativeItemKey,
    NativeRecordCoordinate, NativeSessionKey, ScannedSourceCounts, SessionIdentityInput,
    SourceAnchor, SourceFrontier, SourceInventoryObservation, SourceObservation,
    SourceRecordLocator, TypedKey,
};

fn certified_source() -> CertifiedSource {
    let source = ctx_history_core::SourceKey::derive(
        "golden",
        "golden_jsonl",
        "golden-v1",
        1,
        SourceAnchor::CatalogLineage([3; 32]),
    )
    .expect("golden source key");
    let observation =
        SourceObservation::new(source, "golden-revision-v1", vec![7]).expect("golden observation");
    let counts = ScannedSourceCounts {
        complete_records: 1,
        retained_records: 1,
        indexed_documents: 1,
        certified_bytes: 10,
        ..ScannedSourceCounts::default()
    };
    let frontier = SourceFrontier::new("golden-frontier-v1", TypedKey::U64(1), 10, [9; 32])
        .expect("golden frontier");
    CertifiedSource::certify_with_frontier(
        observation.clone(),
        observation,
        "golden-parser-v1",
        [9; 32],
        counts,
        Some(frontier),
    )
    .expect("golden certified source")
}

pub(super) fn source_manifest() -> SourceManifest {
    SourceManifest::new("a".repeat(64), vec![certified_source()], Vec::new())
        .expect("golden source manifest")
}

pub(super) fn source_manifest_header() -> SourceManifestHeader {
    let manifest = source_manifest();
    SourceManifestHeader::new(
        manifest.core_generation_id,
        1,
        1,
        1,
        1,
        "b".repeat(64),
        1,
        &manifest.sources,
        &manifest.removals,
    )
    .expect("golden source manifest header")
}

pub(super) fn source_manifest_page() -> SourceManifestPage {
    let header = source_manifest_header();
    SourceManifestPage::new(
        &header,
        SourceManifestAdmissionCursor::initial(&header).next_page_previous_sha256,
        0,
        0,
        SourceManifestPageEntries::Sources(vec![certified_source()]),
    )
    .expect("golden source manifest page")
}

pub(super) fn source_manifest_admission_receipt() -> SourceManifestAdmissionReceipt {
    let page = source_manifest_page();
    SourceManifestAdmissionReceipt {
        header: source_manifest_header(),
        page_count: 1,
        terminal_chain_sha256: page.page_sha256,
    }
}

pub(super) fn source_progress(terminal: bool) -> SourceProgress {
    let source = certified_source();
    SourceProgress {
        source: source.observation().source().clone(),
        source_epoch: 1,
        certified_revision_sha256: certified_source_revision_sha256(&source)
            .expect("golden certified source revision"),
        frontier: terminal.then(|| source.frontier().expect("golden frontier").clone()),
        materializer_revision: "golden-source-materializer-v1".to_owned(),
        terminal,
    }
}

pub(super) fn source_record() -> SourceRecord {
    let source = certified_source().observation().source().clone();
    let session_key =
        NativeSessionKey::native_id("golden-session", TypedKey::utf8("session-1").unwrap())
            .expect("golden session key");
    let session_id = derive_session_id(SessionIdentityInput {
        source: &source,
        logical_session_kind: "golden-session",
        native_session_key: &session_key,
    })
    .expect("golden session ID");
    let item_key =
        NativeItemKey::native_id("golden-event", TypedKey::U64(1)).expect("golden event key");
    let event_id = derive_event_id(EventIdentityInput {
        source: &source,
        session_id,
        logical_item_kind: "golden-event",
        native_item_key: &item_key,
        subrecord_selector: None,
    })
    .expect("golden event ID");
    let locator = source_locator();
    SourceRecord::new(
        event_id,
        session_id,
        locator,
        SourceSessionRelationships {
            direct_session_id: session_id,
            root_session_id: session_id,
            parent_session_id: None,
            provider_session_id: Some("provider-session-1".to_owned()),
            agent_id: Some("agent-1".to_owned()),
        },
        Some(SourceRepositoryContext {
            repository_id: "repository-1".to_owned(),
            checkout_id: Some("checkout-1".to_owned()),
            worktree_id: Some("worktree-1".to_owned()),
            object_format: Some("sha1".to_owned()),
        }),
        SourceRecordMetadata {
            event_sequence: 1,
            occurred_at_unix_ms: Some(1_753_232_400_000),
            event_type: "assistant_message".to_owned(),
            role: Some("assistant".to_owned()),
            workspace: Some("/workspace".to_owned()),
            cwd: Some("/workspace/ctx".to_owned()),
            touched_files: vec!["src/lib.rs".to_owned()],
        },
        vec![
            TransientSourceFact::Message(SourceMessageFact {
                content: TransientSourceContent::from_bytes(b"golden message")
                    .expect("golden message content"),
            }),
            TransientSourceFact::Command(SourceCommandFact {
                call_id: Some("call-1".to_owned()),
                tool_name: Some("exec_command".to_owned()),
                command: TransientSourceContent::from_bytes(b"cargo check")
                    .expect("golden command content"),
                working_directory: Some("/workspace/ctx".to_owned()),
            }),
            TransientSourceFact::Result(SourceResultFact {
                call_id: Some("call-1".to_owned()),
                outcome: SourceOutcome::Success,
                exit_code: Some(0),
                duration_ms: Some(42),
                content: TransientSourceContent::from_bytes(b"ok").expect("golden result content"),
            }),
        ],
    )
    .expect("golden source record")
}

pub(super) fn source_locator() -> SourceRecordLocator {
    SourceRecordLocator::new(
        certified_source().observation().source().clone(),
        NativeRecordCoordinate::ProviderNative {
            namespace: "golden-record".to_owned(),
            coordinate: TypedKey::U64(1),
        },
        LocatorRevisionPolicy::ExactSourceRevision,
        Some([8; 32]),
        [6; 32],
    )
    .expect("golden locator")
}

pub(super) fn source_removal() -> SourceRemoval {
    let source = certified_source().observation().source().clone();
    let observation = SourceInventoryObservation::new(
        source.provider(),
        "golden-root",
        TypedKey::utf8("golden-authority").expect("golden authority"),
        "golden-inventory-v1",
        vec![1],
    )
    .expect("golden inventory observation");
    let inventory = CertifiedSourceInventory::certify(
        observation.clone(),
        observation,
        "golden-discovery-v1",
        Vec::new(),
    )
    .expect("golden certified inventory");
    let deletion = CertifiedSourceDeletion::from_inventory(source, &inventory)
        .expect("golden certified deletion");
    SourceRemoval::new(deletion, inventory).expect("golden source removal")
}

pub(super) fn source_receipt() -> SourceManifestReceipt {
    SourceManifestReceipt {
        core_generation_id: "a".repeat(64),
        manifest_aggregate_sha256: source_manifest_header().aggregate_sha256,
        materializer_revision: "golden-source-materializer-v1".to_owned(),
        progress: vec![source_progress(true)],
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

pub(super) fn blame_request(
    target: BlameTarget,
    cursor: Option<String>,
    _fingerprint: &str,
) -> BlameRequest {
    BlameRequest {
        target,
        limit: 10,
        cursor,
        expected_snapshot: QuerySnapshotExpectation::Source {
            receipt: SourceManifestReceiptIdentity::from_receipt(&source_receipt())
                .expect("golden source receipt identity"),
        },
    }
}

pub(super) fn blame(cursor: Option<String>, fingerprint: &str) -> BlameRequest {
    blame_request(
        BlameTarget::Commit {
            oid: "0123456789abcdef".to_owned(),
            repository: Some("ctxrs/ctx".to_owned()),
        },
        cursor,
        fingerprint,
    )
}
