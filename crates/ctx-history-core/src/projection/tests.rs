use std::collections::HashSet;

use super::*;

mod serde_compat;

fn source(lineage: u8) -> SourceKey {
    SourceKey::derive(
        "codex",
        "codex_session_jsonl",
        "session",
        1,
        SourceAnchor::CatalogLineage([lineage; 32]),
    )
    .unwrap()
}

fn native_id(value: &str) -> NativeItemKey {
    NativeItemKey::native_id("message", TypedKey::utf8(value).unwrap()).unwrap()
}

fn native_session_id(value: &str) -> NativeSessionKey {
    NativeSessionKey::native_id("session", TypedKey::utf8(value).unwrap()).unwrap()
}

#[test]
fn source_lineage_disambiguates_equal_provider_session_ids() {
    let first = source(1);
    let second = source(2);
    let session_key = native_session_id("provider-thread-123");
    let first_id = derive_session_id(SessionIdentityInput {
        source: &first,
        logical_session_kind: "thread",
        native_session_key: &session_key,
    })
    .unwrap();
    let second_id = derive_session_id(SessionIdentityInput {
        source: &second,
        logical_session_kind: "thread",
        native_session_key: &session_key,
    })
    .unwrap();
    assert_ne!(first_id, second_id);
}

#[test]
fn exact_catalog_lineage_survives_source_relocation() {
    let before_move = source(1);
    let after_move = source(1);
    let session_key = native_session_id("provider-thread-123");
    assert_eq!(
        derive_session_id(SessionIdentityInput {
            source: &before_move,
            logical_session_kind: "thread",
            native_session_key: &session_key,
        })
        .unwrap(),
        derive_session_id(SessionIdentityInput {
            source: &after_move,
            logical_session_kind: "thread",
            native_session_key: &session_key,
        })
        .unwrap()
    );
}

#[test]
fn source_format_and_parser_classification_do_not_rotate_lineage_identity() {
    let anchor = SourceAnchor::CatalogLineage([7; 32]);
    let before = SourceKey::derive(
        "codex",
        "codex_session_jsonl",
        "session-v1",
        1,
        anchor.clone(),
    )
    .unwrap();
    let after = SourceKey::derive(
        "codex",
        "codex_session_jsonl_tree_leaf",
        "session-v2",
        2,
        anchor,
    )
    .unwrap();
    assert_eq!(before.identity(), after.identity());
    assert_eq!(before, after);
    assert!(!before.exact_descriptor_eq(&after));
    assert!(before.is_same_lineage_descriptor_replacement(&after));
    assert!(!before.is_same_lineage_descriptor_replacement(&before));
    assert!(!before.is_same_lineage_descriptor_replacement(&source(8)));
    assert_eq!(
        before.validate_exact_descriptor(&after).unwrap_err(),
        ProjectionContractError::SourceDescriptorChanged
    );

    let session_key = native_session_id("provider-thread-123");
    let before_session = derive_session_id(SessionIdentityInput {
        source: &before,
        logical_session_kind: "thread",
        native_session_key: &session_key,
    })
    .unwrap();
    let after_session = derive_session_id(SessionIdentityInput {
        source: &after,
        logical_session_kind: "thread",
        native_session_key: &session_key,
    })
    .unwrap();
    assert_eq!(before_session, after_session);
    assert_ne!(
        before_session.source_descriptor_digest(),
        after_session.source_descriptor_digest()
    );
}

#[test]
fn scans_and_identity_inputs_reject_same_lineage_with_a_different_descriptor() {
    let anchor = SourceAnchor::CatalogLineage([7; 32]);
    let opening_source = SourceKey::derive(
        "codex",
        "codex_session_jsonl",
        "session-v1",
        1,
        anchor.clone(),
    )
    .unwrap();
    let closing_source = SourceKey::derive(
        "codex",
        "codex_session_jsonl_tree_leaf",
        "session-v2",
        2,
        anchor,
    )
    .unwrap();
    let opening =
        SourceObservation::new(opening_source.clone(), "regular-file-v1", vec![1]).unwrap();
    let closing =
        SourceObservation::new(closing_source.clone(), "regular-file-v1", vec![1]).unwrap();
    assert_eq!(
        CertifiedSource::certify(
            opening,
            closing,
            "parser-v1",
            [3; 32],
            ScannedSourceCounts::default(),
        )
        .unwrap_err(),
        ProjectionContractError::SourceDescriptorChanged
    );

    let session_key = native_session_id("provider-thread-123");
    let session_id = derive_session_id(SessionIdentityInput {
        source: &opening_source,
        logical_session_kind: "thread",
        native_session_key: &session_key,
    })
    .unwrap();
    let item = native_id("event-1");
    assert_eq!(
        derive_event_id(EventIdentityInput {
            source: &closing_source,
            session_id,
            logical_item_kind: "message",
            native_item_key: &item,
            subrecord_selector: None,
        })
        .unwrap_err(),
        ProjectionContractError::SourceDescriptorChanged
    );
}

#[test]
fn stable_native_item_identity_excludes_mutable_content_and_locator() {
    let source = source(1);
    let session_key = native_session_id("session");
    let session_id = derive_session_id(SessionIdentityInput {
        source: &source,
        logical_session_kind: "thread",
        native_session_key: &session_key,
    })
    .unwrap();
    let key = native_id("event-1");
    let first = derive_event_id(EventIdentityInput {
        source: &source,
        session_id,
        logical_item_kind: "message",
        native_item_key: &key,
        subrecord_selector: None,
    })
    .unwrap();
    let second = derive_event_id(EventIdentityInput {
        source: &source,
        session_id,
        logical_item_kind: "message",
        native_item_key: &key,
        subrecord_selector: None,
    })
    .unwrap();
    assert_eq!(first, second);
    assert_eq!(first.as_uuid().get_version_num(), 8);
    assert_ne!(first.digest(), [0; 32]);
}

#[test]
fn stable_entity_id_canonical_encoding_is_exact_and_round_trips() {
    let source = source(1);
    let session_key = native_session_id("session");
    let session_id = derive_session_id(SessionIdentityInput {
        source: &source,
        logical_session_kind: "thread",
        native_session_key: &session_key,
    })
    .unwrap();
    let event_id = derive_event_id(EventIdentityInput {
        source: &source,
        session_id,
        logical_item_kind: "message",
        native_item_key: &native_id("event-1"),
        subrecord_selector: None,
    })
    .unwrap();

    let encoded = event_id.encode_canonical().unwrap();
    assert_eq!(encoded.len(), STABLE_ENTITY_ID_CANONICAL_LEN);
    assert_eq!(
        &encoded[..STABLE_ENTITY_ID_KIND_OFFSET],
        &IDENTITY_VERSION.to_be_bytes()
    );
    assert_eq!(
        encoded[STABLE_ENTITY_ID_KIND_OFFSET],
        StableEntityKind::Event as u8
    );
    assert_eq!(
        &encoded[STABLE_ENTITY_ID_DIGEST_OFFSET..STABLE_ENTITY_ID_SOURCE_DIGEST_OFFSET],
        &event_id.digest()
    );
    assert_eq!(
        &encoded[STABLE_ENTITY_ID_SOURCE_DIGEST_OFFSET
            ..STABLE_ENTITY_ID_SOURCE_DESCRIPTOR_DIGEST_OFFSET],
        &event_id.source_digest()
    );
    assert_eq!(
        &encoded[STABLE_ENTITY_ID_SOURCE_DESCRIPTOR_DIGEST_OFFSET..STABLE_ENTITY_ID_UUID_OFFSET],
        &event_id.source_descriptor_digest()
    );
    assert_eq!(
        &encoded[STABLE_ENTITY_ID_UUID_OFFSET..],
        event_id.as_uuid().as_bytes()
    );

    let decoded = StableEntityId::decode_canonical(&encoded).unwrap();
    assert_eq!(decoded.contract_version(), event_id.contract_version());
    assert_eq!(decoded.entity_kind(), event_id.entity_kind());
    assert_eq!(decoded.digest(), event_id.digest());
    assert_eq!(decoded.source_digest(), event_id.source_digest());
    assert_eq!(
        decoded.source_descriptor_digest(),
        event_id.source_descriptor_digest()
    );
    assert_eq!(decoded.as_uuid(), event_id.as_uuid());
    assert_eq!(decoded.encode_canonical().unwrap(), encoded);
}

#[test]
fn stable_entity_id_canonical_decode_rejects_corruption() {
    fn assert_invalid(encoded: &[u8]) {
        assert_eq!(
            StableEntityId::decode_canonical(encoded).unwrap_err(),
            ProjectionContractError::InvalidDerivedIdentity
        );
    }

    let source = source(1);
    let session_key = native_session_id("session");
    let session_id = derive_session_id(SessionIdentityInput {
        source: &source,
        logical_session_kind: "thread",
        native_session_key: &session_key,
    })
    .unwrap();
    let mut encoded = session_id.encode_canonical().unwrap();

    assert_invalid(&encoded[..STABLE_ENTITY_ID_CANONICAL_LEN - 1]);
    let mut extended = encoded.to_vec();
    extended.push(0);
    assert_invalid(&extended);

    encoded[..STABLE_ENTITY_ID_KIND_OFFSET].copy_from_slice(&2_u16.to_be_bytes());
    assert_invalid(&encoded);
    encoded = session_id.encode_canonical().unwrap();

    encoded[STABLE_ENTITY_ID_KIND_OFFSET] = 0;
    assert_invalid(&encoded);
    encoded[STABLE_ENTITY_ID_KIND_OFFSET] = u8::MAX;
    assert_invalid(&encoded);
    encoded = session_id.encode_canonical().unwrap();

    encoded[STABLE_ENTITY_ID_DIGEST_OFFSET] ^= 1;
    assert_invalid(&encoded);
    encoded = session_id.encode_canonical().unwrap();

    encoded[STABLE_ENTITY_ID_UUID_OFFSET + 6] ^= 0x10;
    assert_invalid(&encoded);
    encoded = session_id.encode_canonical().unwrap();

    encoded[STABLE_ENTITY_ID_UUID_OFFSET + 8] ^= 0x40;
    assert_invalid(&encoded);

    let source_id = source.identity();
    encoded = source_id.encode_canonical().unwrap();
    encoded[STABLE_ENTITY_ID_SOURCE_DIGEST_OFFSET] ^= 1;
    assert_invalid(&encoded);
    encoded = source_id.encode_canonical().unwrap();
    encoded[STABLE_ENTITY_ID_SOURCE_DESCRIPTOR_DIGEST_OFFSET] = 1;
    assert_invalid(&encoded);
}

#[test]
fn typed_native_keys_do_not_collapse_storage_classes() {
    let source = source(1);
    let session_key = native_session_id("session");
    let session_id = derive_session_id(SessionIdentityInput {
        source: &source,
        logical_session_kind: "thread",
        native_session_key: &session_key,
    })
    .unwrap();
    let keys = [
        NativeItemKey::native_id("sqlite-key", TypedKey::I64(1)).unwrap(),
        NativeItemKey::native_id("sqlite-key", TypedKey::utf8("1").unwrap()).unwrap(),
        NativeItemKey::native_id("sqlite-key", TypedKey::bytes(vec![0x31]).unwrap()).unwrap(),
        NativeItemKey::native_id("sqlite-key", TypedKey::from_f64(1.0)).unwrap(),
    ];
    let ids = keys
        .iter()
        .map(|key| {
            derive_event_id(EventIdentityInput {
                source: &source,
                session_id,
                logical_item_kind: "message",
                native_item_key: key,
                subrecord_selector: None,
            })
            .unwrap()
        })
        .collect::<HashSet<_>>();
    assert_eq!(ids.len(), keys.len());
}

#[test]
fn framed_identity_fields_do_not_have_concatenation_collisions() {
    let first = SourceKey::derive(
        "ab",
        "c",
        "schema",
        1,
        SourceAnchor::CatalogLineage([1; 32]),
    )
    .unwrap();
    let second = SourceKey::derive(
        "a",
        "bc",
        "schema",
        1,
        SourceAnchor::CatalogLineage([1; 32]),
    )
    .unwrap();
    assert_ne!(first.identity(), second.identity());
}

#[test]
fn positional_stability_is_an_explicit_identity_input() {
    let source = source(1);
    let session_key = native_session_id("session");
    let session_id = derive_session_id(SessionIdentityInput {
        source: &source,
        logical_session_kind: "thread",
        native_session_key: &session_key,
    })
    .unwrap();
    let append = NativeItemKey::certified_position(
        "jsonl-record",
        TypedKey::U64(4),
        PositionStability::AppendStable,
    )
    .unwrap();
    let missing_scope = NativeItemKey::certified_position(
        "jsonl-record",
        TypedKey::U64(4),
        PositionStability::RevisionScoped,
    )
    .unwrap_err();
    assert_eq!(
        missing_scope,
        ProjectionContractError::RevisionScopeRequired
    );
    let revision_one = NativeItemKey::revision_scoped_position(
        "jsonl-record",
        TypedKey::U64(4),
        TypedKey::bytes(vec![1; 32]).unwrap(),
    )
    .unwrap();
    let revision_two = NativeItemKey::revision_scoped_position(
        "jsonl-record",
        TypedKey::U64(4),
        TypedKey::bytes(vec![2; 32]).unwrap(),
    )
    .unwrap();
    let derive = |key: &NativeItemKey| {
        derive_event_id(EventIdentityInput {
            source: &source,
            session_id,
            logical_item_kind: "message",
            native_item_key: key,
            subrecord_selector: None,
        })
        .unwrap()
    };
    assert_ne!(derive(&append), derive(&revision_one));
    assert_ne!(derive(&revision_one), derive(&revision_two));
}

#[test]
fn session_positions_require_an_explicit_stability_contract() {
    let source = source(1);
    let append = NativeSessionKey::certified_position(
        "session-array-index",
        TypedKey::U64(4),
        PositionStability::AppendStable,
    )
    .unwrap();
    let stable_slot = NativeSessionKey::certified_position(
        "session-array-index",
        TypedKey::U64(4),
        PositionStability::StableSlot,
    )
    .unwrap();
    assert_eq!(
        NativeSessionKey::certified_position(
            "session-array-index",
            TypedKey::U64(4),
            PositionStability::RevisionScoped,
        )
        .unwrap_err(),
        ProjectionContractError::RevisionScopeRequired
    );
    let revision_one = NativeSessionKey::revision_scoped_position(
        "session-array-index",
        TypedKey::U64(4),
        TypedKey::bytes(vec![1; 32]).unwrap(),
    )
    .unwrap();
    let revision_two = NativeSessionKey::revision_scoped_position(
        "session-array-index",
        TypedKey::U64(4),
        TypedKey::bytes(vec![2; 32]).unwrap(),
    )
    .unwrap();
    let derive = |key: &NativeSessionKey| {
        derive_session_id(SessionIdentityInput {
            source: &source,
            logical_session_kind: "thread",
            native_session_key: key,
        })
        .unwrap()
    };
    let ids = [&append, &stable_slot, &revision_one, &revision_two]
        .into_iter()
        .map(derive)
        .collect::<HashSet<_>>();
    assert_eq!(ids.len(), 4);

    let unscoped_revision = NativeSessionKey::CertifiedPosition {
        kind: "session-array-index".to_owned(),
        coordinate: TypedKey::U64(4),
        stability: PositionStability::RevisionScoped,
        revision_scope: None,
    };
    assert_eq!(
        derive_session_id(SessionIdentityInput {
            source: &source,
            logical_session_kind: "thread",
            native_session_key: &unscoped_revision,
        })
        .unwrap_err(),
        ProjectionContractError::RevisionScopeRequired
    );
}

#[test]
fn subrecord_positions_require_an_explicit_stability_contract() {
    let source = source(1);
    let session_key = native_session_id("session");
    let session_id = derive_session_id(SessionIdentityInput {
        source: &source,
        logical_session_kind: "thread",
        native_session_key: &session_key,
    })
    .unwrap();
    let item = native_id("event-1");
    let append = SubrecordSelector::certified_position(
        "content-block",
        TypedKey::U64(2),
        PositionStability::AppendStable,
    )
    .unwrap();
    let stable_slot = SubrecordSelector::certified_position(
        "content-block",
        TypedKey::U64(2),
        PositionStability::StableSlot,
    )
    .unwrap();
    assert_eq!(
        SubrecordSelector::certified_position(
            "content-block",
            TypedKey::U64(2),
            PositionStability::RevisionScoped,
        )
        .unwrap_err(),
        ProjectionContractError::RevisionScopeRequired
    );
    let revision_one = SubrecordSelector::revision_scoped_position(
        "content-block",
        TypedKey::U64(2),
        TypedKey::bytes(vec![1; 32]).unwrap(),
    )
    .unwrap();
    let revision_two = SubrecordSelector::revision_scoped_position(
        "content-block",
        TypedKey::U64(2),
        TypedKey::bytes(vec![2; 32]).unwrap(),
    )
    .unwrap();
    let derive = |selector: &SubrecordSelector| {
        derive_event_id(EventIdentityInput {
            source: &source,
            session_id,
            logical_item_kind: "message",
            native_item_key: &item,
            subrecord_selector: Some(selector),
        })
        .unwrap()
    };
    let ids = [&append, &stable_slot, &revision_one, &revision_two]
        .into_iter()
        .map(derive)
        .collect::<HashSet<_>>();
    assert_eq!(ids.len(), 4);

    let unexpectedly_scoped = SubrecordSelector::CertifiedPosition {
        kind: "content-block".to_owned(),
        coordinate: TypedKey::U64(2),
        stability: PositionStability::AppendStable,
        revision_scope: Some(TypedKey::U64(9)),
    };
    assert_eq!(
        unexpectedly_scoped.validate_contract().unwrap_err(),
        ProjectionContractError::UnexpectedRevisionScope
    );
}

#[test]
fn stable_identity_key_variants_keep_native_item_framing() {
    let item_native = NativeItemKey::native_id("id", TypedKey::U64(7)).unwrap();
    let session_native = NativeSessionKey::native_id("id", TypedKey::U64(7)).unwrap();
    let subrecord_native = SubrecordSelector::native_id("id", TypedKey::U64(7)).unwrap();
    let item_composite =
        NativeItemKey::composite("id", vec![TypedKey::U64(7), TypedKey::Bool(true)]).unwrap();
    let session_composite =
        NativeSessionKey::composite("id", vec![TypedKey::U64(7), TypedKey::Bool(true)]).unwrap();
    let subrecord_composite =
        SubrecordSelector::composite("id", vec![TypedKey::U64(7), TypedKey::Bool(true)]).unwrap();

    let mut expected_native = vec![1];
    expected_native.extend_from_slice(&2_u64.to_be_bytes());
    expected_native.extend_from_slice(b"id");
    expected_native.push(4);
    expected_native.extend_from_slice(&7_u64.to_be_bytes());

    let mut expected_composite = vec![2];
    expected_composite.extend_from_slice(&2_u64.to_be_bytes());
    expected_composite.extend_from_slice(b"id");
    expected_composite.extend_from_slice(&2_u32.to_be_bytes());
    expected_composite.push(4);
    expected_composite.extend_from_slice(&7_u64.to_be_bytes());
    expected_composite.extend_from_slice(&[6, 1]);

    let mut item_native_encoded = Vec::new();
    let mut session_native_encoded = Vec::new();
    let mut subrecord_native_encoded = Vec::new();
    encode_native_item_key(&mut item_native_encoded, &item_native).unwrap();
    encode_native_session_key(&mut session_native_encoded, &session_native).unwrap();
    encode_subrecord_selector(&mut subrecord_native_encoded, &subrecord_native).unwrap();
    assert_eq!(item_native_encoded, expected_native);
    assert_eq!(session_native_encoded, expected_native);
    assert_eq!(subrecord_native_encoded, expected_native);

    let mut item_composite_encoded = Vec::new();
    let mut session_composite_encoded = Vec::new();
    let mut subrecord_composite_encoded = Vec::new();
    encode_native_item_key(&mut item_composite_encoded, &item_composite).unwrap();
    encode_native_session_key(&mut session_composite_encoded, &session_composite).unwrap();
    encode_subrecord_selector(&mut subrecord_composite_encoded, &subrecord_composite).unwrap();
    assert_eq!(item_composite_encoded, expected_composite);
    assert_eq!(session_composite_encoded, expected_composite);
    assert_eq!(subrecord_composite_encoded, expected_composite);
}

#[test]
fn event_identity_rejects_a_non_session_parent() {
    let source = source(1);
    let key = native_id("event-1");
    let error = derive_event_id(EventIdentityInput {
        source: &source,
        session_id: source.identity(),
        logical_item_kind: "message",
        native_item_key: &key,
        subrecord_selector: None,
    })
    .unwrap_err();
    assert_eq!(
        error,
        ProjectionContractError::EntityKindMismatch {
            expected: StableEntityKind::Session,
            actual: StableEntityKind::Source,
        }
    );
}

#[test]
fn event_identity_rejects_a_session_from_another_source() {
    let first = source(1);
    let second = source(2);
    let session_key = native_session_id("session");
    let first_session = derive_session_id(SessionIdentityInput {
        source: &first,
        logical_session_kind: "thread",
        native_session_key: &session_key,
    })
    .unwrap();
    let item = native_id("event-1");
    let error = derive_event_id(EventIdentityInput {
        source: &second,
        session_id: first_session,
        logical_item_kind: "message",
        native_item_key: &item,
        subrecord_selector: None,
    })
    .unwrap_err();
    assert_eq!(error, ProjectionContractError::SourceChanged);
}

#[test]
fn certification_rejects_a_source_mutation() {
    let source = source(1);
    let opening = SourceObservation::new(source.clone(), "regular-file-v1", vec![1, 2, 3]).unwrap();
    let closing = SourceObservation::new(source, "regular-file-v1", vec![1, 2, 4]).unwrap();
    let error = CertifiedSource::certify(
        opening,
        closing,
        "codex-parser-v1",
        [9; 32],
        ScannedSourceCounts {
            complete_records: 1,
            retained_records: 1,
            indexed_documents: 1,
            certified_bytes: 10,
            ..ScannedSourceCounts::default()
        },
    )
    .unwrap_err();
    assert_eq!(error, ProjectionContractError::SourceRevisionChanged);
}

#[test]
fn certification_reconciles_record_counts() {
    let source = source(1);
    let observation = SourceObservation::new(source, "regular-file-v1", vec![1, 2, 3]).unwrap();
    let error = CertifiedSource::certify(
        observation.clone(),
        observation,
        "codex-parser-v1",
        [9; 32],
        ScannedSourceCounts {
            complete_records: 3,
            retained_records: 1,
            rejected_records: 1,
            ignored_records: 0,
            indexed_documents: 1,
            certified_bytes: 10,
        },
    )
    .unwrap_err();
    assert_eq!(error, ProjectionContractError::CountMismatch);
}

#[test]
fn deletion_requires_one_unchanged_authoritative_inventory() {
    let opening = SourceInventoryObservation::new(
        "codex",
        "sessions-root",
        TypedKey::utf8("root-lineage").unwrap(),
        "tree-inventory-v1",
        vec![1],
    )
    .unwrap();
    let closing = SourceInventoryObservation::new(
        "codex",
        "sessions-root",
        TypedKey::utf8("root-lineage").unwrap(),
        "tree-inventory-v1",
        vec![2],
    )
    .unwrap();
    let error = CertifiedSourceInventory::certify(opening, closing, "codex-discovery-v1", vec![])
        .unwrap_err();
    assert_eq!(error, ProjectionContractError::InventoryRevisionChanged);
}

#[test]
fn deletion_inventory_must_own_the_source_provider() {
    let observation = SourceInventoryObservation::new(
        "claude_code",
        "projects-root",
        TypedKey::utf8("root-lineage").unwrap(),
        "tree-inventory-v1",
        vec![1],
    )
    .unwrap();
    let inventory = CertifiedSourceInventory::certify(
        observation.clone(),
        observation,
        "claude-discovery-v1",
        vec![],
    )
    .unwrap();
    let error = CertifiedSourceDeletion::from_inventory(source(1), &inventory).unwrap_err();
    assert_eq!(error, ProjectionContractError::InventoryProviderMismatch);
}

#[test]
fn deletion_inventory_must_prove_the_source_is_absent() {
    let source = source(1);
    let observation = SourceInventoryObservation::new(
        "codex",
        "sessions-root",
        TypedKey::utf8("root-lineage").unwrap(),
        "tree-inventory-v1",
        vec![1],
    )
    .unwrap();
    let inventory = CertifiedSourceInventory::certify(
        observation.clone(),
        observation,
        "codex-discovery-v1",
        vec![source.clone()],
    )
    .unwrap();
    let error = CertifiedSourceDeletion::from_inventory(source, &inventory).unwrap_err();
    assert_eq!(
        error,
        ProjectionContractError::InventoryContainsDeletedSource
    );
}

#[test]
fn deletion_witness_verifies_only_the_exact_complete_inventory() {
    let deleted = source(1);
    let retained = source(2);
    let observation = SourceInventoryObservation::new(
        "codex",
        "sessions-root",
        TypedKey::utf8("root-lineage").unwrap(),
        "tree-inventory-v1",
        vec![1],
    )
    .unwrap();
    let inventory = CertifiedSourceInventory::certify(
        observation.clone(),
        observation,
        "codex-discovery-v1",
        vec![retained],
    )
    .unwrap();
    let witness = CertifiedSourceDeletion::from_inventory(deleted.clone(), &inventory).unwrap();

    assert!(witness.verifies(&inventory));

    let mut wrong_provider = inventory.clone();
    wrong_provider.observation.provider = "claude_code".to_owned();
    assert!(!witness.verifies(&wrong_provider));

    let mut wrong_authority = inventory.clone();
    wrong_authority.observation.authority_key = TypedKey::utf8("other-root").unwrap();
    assert!(!witness.verifies(&wrong_authority));

    let mut wrong_discovery_contract = inventory.clone();
    wrong_discovery_contract.discovery_revision = "codex-discovery-v2".to_owned();
    assert!(!witness.verifies(&wrong_discovery_contract));

    let mut wrong_digest = inventory.clone();
    wrong_digest.inventory_digest = [9; 32];
    assert!(!witness.verifies(&wrong_digest));

    let mut wrong_count = inventory.clone();
    wrong_count.source_digests.clear();
    assert!(!witness.verifies(&wrong_count));

    let mut containing_inventory = inventory;
    containing_inventory.source_digests = vec![deleted.identity.digest];
    assert!(!witness.verifies(&containing_inventory));
}

#[test]
fn append_requires_an_exact_committed_prefix() {
    let source = source(1);
    let base_observation =
        SourceObservation::new(source.clone(), "regular-file-v1", vec![1]).unwrap();
    let base = CertifiedSource::certify_with_frontier(
        base_observation.clone(),
        base_observation,
        "parser-v1",
        [3; 32],
        ScannedSourceCounts {
            complete_records: 2,
            retained_records: 2,
            indexed_documents: 2,
            certified_bytes: 100,
            ..ScannedSourceCounts::default()
        },
        Some(SourceFrontier::new("jsonl-byte-offset", TypedKey::U64(100), 100, [3; 32]).unwrap()),
    )
    .unwrap();
    let current_observation = SourceObservation::new(source, "regular-file-v1", vec![2]).unwrap();
    let current = CertifiedSource::certify_with_frontier(
        current_observation.clone(),
        current_observation,
        "parser-v1",
        [4; 32],
        ScannedSourceCounts {
            complete_records: 3,
            retained_records: 3,
            indexed_documents: 3,
            certified_bytes: 150,
            ..ScannedSourceCounts::default()
        },
        Some(SourceFrontier::new("jsonl-byte-offset", TypedKey::U64(150), 150, [4; 32]).unwrap()),
    )
    .unwrap();
    let error = CertifiedSourceAppend::certify(&base, current.clone(), 100, [9; 32]).unwrap_err();
    assert_eq!(error, ProjectionContractError::AppendPrefixMismatch);
    assert!(CertifiedSourceAppend::certify(&base, current, 100, [3; 32]).is_ok());
}

#[test]
fn frontier_must_bind_the_certified_byte_prefix() {
    let source = source(1);
    let observation = SourceObservation::new(source, "regular-file-v1", vec![1]).unwrap();
    let error = CertifiedSource::certify_with_frontier(
        observation.clone(),
        observation,
        "parser-v1",
        [3; 32],
        ScannedSourceCounts {
            certified_bytes: 100,
            ..ScannedSourceCounts::default()
        },
        Some(SourceFrontier::new("jsonl-byte-offset", TypedKey::U64(99), 99, [3; 32]).unwrap()),
    )
    .unwrap_err();
    assert_eq!(error, ProjectionContractError::FrontierMismatch);
}
