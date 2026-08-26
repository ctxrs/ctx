use super::*;
use ctx_history_index_query::{
    SearchFamilyBasis, SearchFamilyKey, MAX_SESSION_GROUPING_COORDINATES,
};

fn publish_records(
    temp: &TempDir,
    source: &SourceKey,
    records: impl IntoIterator<Item = CoreRecord>,
) -> VerifiedIndex {
    let records = records.into_iter().collect::<Vec<_>>();
    let mut writer = GenerationWriter::open(temp.path(), WriterOptions::default())
        .unwrap()
        .into_writer()
        .unwrap();
    writer.begin_source(source.clone()).unwrap();
    for record in records.iter().cloned() {
        writer.add_core_record(record).unwrap();
    }
    writer
        .certify_source(certificate(source, 1, records.len() as u64))
        .unwrap();
    writer.commit(|_| true).unwrap();
    VerifiedIndex::open_pinned(temp.path()).unwrap()
}

fn authority_document(record: CoreRecord) -> TantivyDocument {
    let authority = ctx_history_index_format::SessionAuthorityKey::exact(
        record.session_id,
        record.source.identity(),
    )
    .unwrap();
    let fields = fields_from_schema(&lexical_schema()).unwrap();
    let mut document = indexed_document(record);
    document.add_bytes(fields.session_authority, authority.as_bytes());
    document
}

fn replace_source_unchecked(temp: &TempDir, source: &SourceKey, documents: Vec<TantivyDocument>) {
    let (searcher, manifest) = open_unverified_generation(temp.path());
    drop(searcher);
    let directory = DurableMmapDirectory::open(active_generation_path(temp.path())).unwrap();
    let index = Index::open(directory).unwrap();
    publish_unchecked_generation(
        temp.path(),
        &index,
        manifest,
        std::slice::from_ref(source),
        documents,
    );
}

#[test]
fn exact_batch_coalesces_direct_claims_once_in_request_order() {
    let temp = tempdir().unwrap();
    let source = source("grouping-coalescing.jsonl");
    let root = document_for_session(&source, "root", 1, "root");
    let child_without_claim = document_for_session(&source, "child", 2, "child start");
    let mut child_with_claim = document_for_session(&source, "child", 3, "child linked");
    child_with_claim.parent_session_id = Some(root.session_id);
    child_with_claim.root_session_id = Some(root.session_id);
    child_with_claim.session_relationship = Some(ProviderNativeSessionRelationship::Delegated);
    child_with_claim.validate_contract().unwrap();
    let index = publish_records(
        &temp,
        &source,
        [root.clone(), child_without_claim, child_with_claim.clone()],
    );

    ctx_history_index_query::reset_session_grouping_authority_queries();
    let claims = index
        .session_grouping_claims(&[
            (child_with_claim.session_id, source.identity()),
            (root.session_id, source.identity()),
        ])
        .unwrap();
    assert_eq!(
        ctx_history_index_query::session_grouping_authority_queries(),
        0,
        "grouping authority must not invoke generic query execution"
    );
    assert_eq!(
        claims
            .iter()
            .map(|claim| claim.session_id)
            .collect::<Vec<_>>(),
        [child_with_claim.session_id, root.session_id]
    );
    assert_eq!(claims[0].source_owner, source.identity());
    assert_eq!(claims[0].parent_session_id, Some(root.session_id));
    assert_eq!(claims[0].root_session_id, Some(root.session_id));
    assert_eq!(
        claims[0].relationship,
        Some(ProviderNativeSessionRelationship::Delegated)
    );
    let child_family = SearchFamilyKey::from_claims(&claims[0]);
    let root_family = SearchFamilyKey::from_claims(&claims[1]);
    assert_eq!(child_family.session_id, root.session_id);
    assert_eq!(child_family.basis, SearchFamilyBasis::LiteralProviderRoot);
    assert_eq!(root_family.session_id, root.session_id);
    assert_eq!(root_family.basis, SearchFamilyBasis::OwnSessionFallback);
    assert_eq!(
        child_family, root_family,
        "evidence basis is not part of family identity"
    );
    assert_eq!(
        index
            .session_grouping_claims_by_id(child_with_claim.session_id.as_uuid())
            .unwrap(),
        Some(claims[0])
    );
    assert_eq!(
        index
            .session_grouping_claims_claiming_lineage_to_any(&[root.session_id.as_uuid()], 2,)
            .unwrap(),
        vec![claims[0]]
    );
}

#[test]
fn exact_batch_accepts_equal_positive_witnesses() {
    let temp = tempdir().unwrap();
    let source = source("grouping-equal-positives.jsonl");
    let root = document_for_session(&source, "root", 1, "root");
    let mut first = document_for_session(&source, "child", 2, "first");
    let mut second = document_for_session(&source, "child", 3, "second");
    for child in [&mut first, &mut second] {
        child.parent_session_id = Some(root.session_id);
        child.root_session_id = Some(root.session_id);
        child.session_relationship = Some(ProviderNativeSessionRelationship::WorkflowChild);
        child.validate_contract().unwrap();
    }
    publish_records(
        &temp,
        &source,
        [root.clone(), first.clone(), second.clone()],
    );
    replace_source_unchecked(
        &temp,
        &source,
        vec![
            authority_document(root),
            authority_document(first.clone()),
            authority_document(second),
        ],
    );
    let index = VerifiedIndex::open_pinned(temp.path()).unwrap();

    let claims = index
        .session_grouping_claims(&[(first.session_id, source.identity())])
        .unwrap();
    assert_eq!(claims.len(), 1);
    assert_eq!(claims[0].parent_session_id, first.parent_session_id);
    assert_eq!(claims[0].root_session_id, first.root_session_id);
    assert_eq!(claims[0].relationship, first.session_relationship);
}

#[test]
fn exact_batch_rejects_conflicting_forged_witnesses() {
    let temp = tempdir().unwrap();
    let source = source("grouping-conflict.jsonl");
    let first_root = document_for_session(&source, "first-root", 1, "first root");
    let second_root = document_for_session(&source, "second-root", 2, "second root");
    let mut first = document_for_session(&source, "child", 3, "first");
    let mut second = document_for_session(&source, "child", 4, "second");
    for child in [&mut first, &mut second] {
        child.parent_session_id = Some(first_root.session_id);
        child.validate_contract().unwrap();
    }
    publish_records(
        &temp,
        &source,
        [
            first_root.clone(),
            second_root.clone(),
            first.clone(),
            second.clone(),
        ],
    );
    second.parent_session_id = Some(second_root.session_id);
    second.validate_contract().unwrap();
    replace_source_unchecked(
        &temp,
        &source,
        vec![
            authority_document(first_root),
            authority_document(second_root),
            authority_document(first.clone()),
            authority_document(second),
        ],
    );
    let index = VerifiedIndex::open_pinned(temp.path()).unwrap();

    assert!(matches!(
        index.session_grouping_claims(&[(first.session_id, source.identity())]),
        Err(IndexError::ConflictingProviderNativeSessionClaim(_))
    ));
}

#[test]
fn exact_batch_enforces_coordinate_and_witness_cardinality() {
    let temp = tempdir().unwrap();
    let source = source("grouping-cardinality.jsonl");
    let root = document_for_session(&source, "root", 1, "root");
    let mut records = Vec::new();
    for sequence in 2..=6 {
        let mut child = document_for_session(&source, "child", sequence, "child");
        match sequence {
            3 => child.parent_session_id = Some(root.session_id),
            4 => {
                child.parent_session_id = Some(root.session_id);
                child.root_session_id = Some(root.session_id);
            }
            5 | 6 => {
                child.parent_session_id = Some(root.session_id);
                child.root_session_id = Some(root.session_id);
                child.session_relationship = Some(ProviderNativeSessionRelationship::Delegated);
            }
            _ => {}
        }
        child.validate_contract().unwrap();
        records.push(child);
    }
    let child_id = records[0].session_id;
    let mut published = vec![root.clone()];
    published.extend(records.iter().cloned());
    let index = publish_records(&temp, &source, published);
    assert_eq!(
        index
            .session_grouping_claims(&[(child_id, source.identity())])
            .unwrap()[0]
            .relationship,
        Some(ProviderNativeSessionRelationship::Delegated)
    );

    let mut forged = vec![authority_document(root)];
    forged.extend(records.into_iter().map(authority_document));
    replace_source_unchecked(&temp, &source, forged);
    let index = VerifiedIndex::open_pinned(temp.path()).unwrap();
    assert!(matches!(
        index.session_grouping_claims(&[(child_id, source.identity())]),
        Err(IndexError::InvalidStoredDocumentField("session_authority"))
    ));

    let duplicate = [(child_id, source.identity()), (child_id, source.identity())];
    assert!(matches!(
        index.session_grouping_claims(&duplicate),
        Err(IndexError::DuplicateSessionGroupingCoordinate(_))
    ));
    let oversized = vec![(child_id, source.identity()); MAX_SESSION_GROUPING_COORDINATES + 1];
    assert!(matches!(
        index.session_grouping_claims(&oversized),
        Err(IndexError::InvalidSessionGroupingCoordinateCount { .. })
    ));
}

#[test]
fn exact_batch_rejects_missing_or_mismatched_source_coordinates() {
    let temp = tempdir().unwrap();
    let source_key = source("grouping-missing.jsonl");
    let present = document_for_session(&source_key, "present", 1, "present");
    let missing = document_for_session(&source_key, "missing", 2, "missing");
    let index = publish_records(&temp, &source_key, [present.clone()]);
    assert!(matches!(
        index.session_grouping_claims(&[(missing.session_id, source_key.identity())]),
        Err(IndexError::MissingSessionGroupingCoordinate(_))
    ));

    let foreign = source("grouping-foreign.jsonl");
    assert!(matches!(
        index.session_grouping_claims(&[(present.session_id, foreign.identity())]),
        Err(IndexError::InvalidStoredDocumentField("session_authority"))
    ));
}

#[test]
fn exact_batch_skips_malformed_tombstoned_witnesses() {
    let temp = tempdir().unwrap();
    let source = source("grouping-tombstone.jsonl");
    let record = document_for_session(&source, "session", 1, "current");
    publish_records(&temp, &source, [record.clone()]);

    let fields = fields_from_schema(&lexical_schema()).unwrap();
    let mut malformed = authority_document(record.clone());
    let mut replacement = TantivyDocument::default();
    for (field, value) in malformed.iter_fields_and_values() {
        if field != fields.core_record && field != fields.core_record_encoded_bytes {
            replacement.add_field_value(field, value);
        }
    }
    replacement.add_u64(fields.core_record_encoded_bytes, 1);
    replacement.add_bytes(fields.core_record, b"{");
    malformed = replacement;
    replace_source_unchecked(&temp, &source, vec![malformed]);
    replace_source_unchecked(&temp, &source, vec![authority_document(record.clone())]);

    let index = VerifiedIndex::open_pinned(temp.path()).unwrap();
    assert_eq!(
        index
            .session_grouping_claims(&[(record.session_id, source.identity())])
            .unwrap()[0]
            .session_id,
        record.session_id
    );
}

#[test]
fn exact_batch_rejects_preflight_core_byte_exhaustion_before_stored_decode() {
    let temp = tempdir().unwrap();
    let source = source("grouping-core-byte-limit.jsonl");
    let records = (0..8)
        .map(|index| {
            document_for_session(
                &source,
                if index < 4 { "session-a" } else { "session-b" },
                index + 1,
                "small body",
            )
        })
        .collect::<Vec<_>>();
    publish_records(&temp, &source, records.iter().cloned());

    let fields = fields_from_schema(&lexical_schema()).unwrap();
    let forged = records
        .iter()
        .cloned()
        .map(authority_document)
        .map(|complete| {
            let mut forged = TantivyDocument::default();
            for (field, value) in complete.iter_fields_and_values() {
                if field != fields.core_record_encoded_bytes {
                    forged.add_field_value(field, value);
                }
            }
            // Each forged fact remains under the valid per-document maximum,
            // but the complete eight-witness batch exceeds 256 MiB.
            forged.add_u64(fields.core_record_encoded_bytes, 32 * 1024 * 1024 + 1);
            forged
        })
        .collect::<Vec<_>>();
    replace_source_unchecked(&temp, &source, forged);
    let index = VerifiedIndex::open_pinned(temp.path()).unwrap();

    let coordinates = [
        (records[0].session_id, source.identity()),
        (records[4].session_id, source.identity()),
    ];
    ctx_history_index_query::reset_core_record_decodes();
    assert!(matches!(
        index.session_grouping_claims(&coordinates),
        Err(IndexError::SessionGroupingAuthorityWorkLimitExceeded {
            operation: "encoded Core bytes",
            ..
        })
    ));
    assert_eq!(
        ctx_history_index_query::core_record_decodes(),
        0,
        "the byte admission failure must precede stored Core decoding"
    );
}
