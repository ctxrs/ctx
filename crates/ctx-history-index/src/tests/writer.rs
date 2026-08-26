use super::*;

#[test]
fn cached_source_descriptor_preserves_core_record_and_active_source_faults() {
    fn descriptor(source_format: &str) -> SourceKey {
        SourceKey::derive(
            "codex",
            source_format,
            "session",
            1,
            SourceAnchor::provider_native(
                "session-file",
                TypedKey::utf8("move-backed-document-test").unwrap(),
            )
            .unwrap(),
        )
        .unwrap()
    }

    let active = descriptor("codex_session_jsonl");
    let descriptor_alias = descriptor("codex_prompt_history_jsonl");
    assert_eq!(active, descriptor_alias);
    assert!(!active.exact_descriptor_eq(&descriptor_alias));

    let directory = tempdir().unwrap();
    let mut writer = GenerationWriter::open(directory.path(), WriterOptions::default())
        .unwrap()
        .into_writer()
        .unwrap();
    writer.begin_source(active.clone()).unwrap();

    let mut mismatched_identity = document(&active, 1, "body");
    mismatched_identity.event_id = mismatched_identity.session_id;
    assert!(matches!(
        writer.add_core_record(mismatched_identity),
        Err(IndexError::CoreRecord(_))
    ));
    assert!(matches!(
        writer.add_core_record(document(&descriptor_alias, 1, "body")),
        Err(IndexError::DocumentSourceNotActive)
    ));
}
use ctx_history_core::MAX_ENCODED_CORE_RECORD_BYTES;
use std::{path::Path, sync::Arc};

mod complexity_contract;
mod merge_policy;
mod routes;
mod verification;

#[test]
fn commit_binds_manifest_and_searchable_documents() {
    let temp = tempdir().unwrap();
    let source = source("session.jsonl");
    let mut writer = GenerationWriter::open(temp.path(), WriterOptions::default())
        .unwrap()
        .into_writer()
        .unwrap();
    writer.begin_source(source.clone()).unwrap();
    writer
        .add_core_record(document(&source, 1, "atomic generation"))
        .unwrap();
    writer.certify_source(certificate(&source, 1, 1)).unwrap();
    let receipt = writer.commit(|_| true).unwrap();

    let index = VerifiedIndex::open(temp.path()).unwrap();
    assert_eq!(index.generation_id(), receipt.generation_id);
    assert_eq!(
        receipt.manifest().generation_id().unwrap(),
        receipt.generation_id
    );
    assert_eq!(receipt.manifest().sources, index.manifest().sources);
    assert_eq!(
        receipt.manifest().source_routes(),
        index.manifest().source_routes()
    );
    assert_eq!(index.manifest().indexed_documents, 1);
    assert_eq!(index.count_term("atomic").unwrap(), 1);
}

#[test]
fn prepared_core_draft_retries_final_encoding_under_a_caller_permit() {
    let temp = tempdir().unwrap();
    let source = source("bounded-materialization.jsonl");
    let record = document(&source, 1, "bounded materialization");
    let writer = GenerationWriter::open(temp.path(), WriterOptions::default())
        .unwrap()
        .into_writer()
        .unwrap();
    let preparer = writer.core_record_preparer();

    crate::preparation::reset_final_encoding_count();
    let draft = preparer.prepare_draft(record.clone()).unwrap();
    let draft = match draft.materialize(1).unwrap() {
        PreparedCoreRecordMaterialization::CapacityExceeded(draft) => *draft,
        PreparedCoreRecordMaterialization::Prepared(_) => {
            panic!("one byte unexpectedly admitted a complete Core record")
        }
    };
    assert_eq!(
        crate::preparation::final_encoding_count(),
        0,
        "a failed bounded attempt is not a completed canonical encoding"
    );

    let prepared = match draft.materialize(MAX_ENCODED_CORE_RECORD_BYTES).unwrap() {
        PreparedCoreRecordMaterialization::Prepared(prepared) => prepared,
        PreparedCoreRecordMaterialization::CapacityExceeded(_) => {
            panic!("a valid Core record exceeded the contract maximum")
        }
    };
    assert_eq!(crate::preparation::final_encoding_count(), 1);

    let reference = preparer.prepare(record).unwrap();
    assert_eq!(
        prepared.encoded_core_bytes(),
        reference.encoded_core_bytes(),
        "retrying under a sufficient permit must preserve canonical encoding"
    );
    assert_eq!(crate::preparation::final_encoding_count(), 2);
}

#[test]
fn prepared_core_record_keeps_identity_authority_with_its_document_and_aggregate() {
    let temp = tempdir().unwrap();
    let source = source("prepared-identity-authority.jsonl");
    let parent = document_for_session(&source, "parent", 10, "not published");
    let mut record = document_for_session(&source, "child", 1, "prepared child");
    record.parent_session_id = Some(parent.session_id);
    record.root_session_id = Some(parent.session_id);
    record.session_relationship = Some(ctx_history_core::ProviderNativeSessionRelationship::Forked);
    record.validate_contract().unwrap();
    let expected_event = record.event_id;
    let expected_session = record.session_id;
    let writer = GenerationWriter::open(temp.path(), WriterOptions::default())
        .unwrap()
        .into_writer()
        .unwrap();

    let prepared = writer.core_record_preparer().prepare(record).unwrap();
    let parts = prepared.into_parts();
    assert_eq!(parts.identity_facts.event_id, expected_event);
    assert_eq!(parts.identity_facts.session.session_id, expected_session);
    assert_eq!(
        parts.identity_facts.session.source_owner,
        source.identity().digest()
    );
    assert_eq!(
        parts.identity_facts.session.relationship.parent_session_id,
        Some(parent.session_id)
    );
    assert_eq!(
        parts.identity_facts.session.relationship.root_session_id,
        Some(parent.session_id)
    );
    assert_eq!(
        parts.identity_facts.session.relationship.kind,
        Some(ctx_history_core::ProviderNativeSessionRelationship::Forked)
    );
    let _document_and_aggregate = (parts.document, parts.record_accumulator_leaf);
}

#[test]
fn manifest_accumulator_uses_the_fingerprinted_event_binding() {
    use sha2::{Digest, Sha256};

    let temp = tempdir().unwrap();
    let source = source("event-binding-accumulator.jsonl");
    let record = document(&source, 1, "bound accumulator record");
    let event_id = record.event_id;
    let encoded_record = record.encode_stored().unwrap();
    let record_leaf = ctx_history_core::core_record_leaf_digest(event_id, &encoded_record).unwrap();
    let canonical_event_id = event_id.encode_canonical().unwrap();
    let mut expected = Sha256::new();
    expected.update(ctx_history_core::CORE_RECORD_ACCUMULATOR_IDENTITY);
    expected.update((canonical_event_id.len() as u64).to_be_bytes());
    expected.update(canonical_event_id);
    expected.update(record_leaf);
    let expected: [u8; 32] = expected.finalize().into();

    let mut writer = GenerationWriter::open(temp.path(), WriterOptions::default())
        .unwrap()
        .into_writer()
        .unwrap();
    writer.begin_source(source.clone()).unwrap();
    writer.add_core_record(record).unwrap();
    writer.certify_source(certificate(&source, 1, 1)).unwrap();
    let receipt = writer.commit(|_| true).unwrap();
    let aggregate = receipt.manifest().core_record_aggregates.first().unwrap();

    assert_eq!(aggregate.accumulator_bytes().unwrap(), expected);
    assert_ne!(
        expected, record_leaf,
        "raw Core leaves are not accumulator addends"
    );
}

#[test]
fn logical_generation_identity_excludes_physical_index_topology() {
    let source = source("independent-logical-generation.jsonl");
    let publish = |root: &Path| {
        let mut writer = GenerationWriter::open(root, WriterOptions::default())
            .unwrap()
            .into_writer()
            .unwrap();
        writer.begin_source(source.clone()).unwrap();
        writer
            .add_core_record(document(&source, 1, "same logical publication"))
            .unwrap();
        writer.certify_source(certificate(&source, 1, 1)).unwrap();
        let receipt = writer.commit(|_| true).unwrap();
        let pointer = load_active_generation_pointer(root).unwrap().unwrap();
        (receipt, pointer.active().clone())
    };

    let first = tempdir().unwrap();
    let second = tempdir().unwrap();
    let (first_receipt, first_slot) = publish(first.path());
    let (second_receipt, second_slot) = publish(second.path());

    assert_eq!(first_receipt.generation_id, second_receipt.generation_id);
    assert_eq!(
        serde_json::to_vec(first_receipt.manifest()).unwrap(),
        serde_json::to_vec(second_receipt.manifest()).unwrap()
    );
    assert_ne!(first_slot.directory(), second_slot.directory());
    assert_ne!(
        first_slot.physical_integrity_digest(),
        second_slot.physical_integrity_digest(),
        "independent Tantivy segment names must remain outside logical generation identity"
    );
}

#[test]
fn replacement_preparation_never_imports_prior_generation_semantics() {
    let temp = tempdir().unwrap();
    let source = source("neutral-replacement.jsonl");
    let mut prior = document(&source, 1, "same event body");
    prior
        .content
        .activity
        .as_mut()
        .unwrap()
        .facts
        .push(ctx_history_core::ProviderDeclaredFact {
            kind: ctx_history_core::LiteralFactKind::ProviderDisposition,
            value: "prior-only".to_owned(),
        });

    let mut initial = GenerationWriter::open(temp.path(), WriterOptions::default())
        .unwrap()
        .into_writer()
        .unwrap();
    initial.begin_source(source.clone()).unwrap();
    initial.add_core_record(prior).unwrap();
    initial.certify_source(certificate(&source, 1, 1)).unwrap();
    initial.commit(|_| true).unwrap();

    let mut current = document(&source, 1, "same event body");
    current
        .content
        .activity
        .as_mut()
        .unwrap()
        .facts
        .push(ctx_history_core::ProviderDeclaredFact {
            kind: ctx_history_core::LiteralFactKind::ProviderDisposition,
            value: "current-only".to_owned(),
        });
    let event_id = current.event_id;
    let expected = current.content.activity.as_ref().unwrap().facts.clone();
    let mut replacement = GenerationWriter::open(temp.path(), WriterOptions::default())
        .unwrap()
        .into_writer()
        .unwrap();
    replacement.begin_source(source.clone()).unwrap();
    replacement.add_core_record(current).unwrap();
    replacement
        .certify_source(certificate(&source, 2, 1))
        .unwrap();
    replacement.commit(|_| true).unwrap();

    let stored = VerifiedIndex::open(temp.path())
        .unwrap()
        .core_record_by_id(event_id.as_uuid())
        .unwrap()
        .unwrap();
    assert_eq!(stored.content.activity.unwrap().facts, expected);
}

#[test]
fn prepared_record_requires_matching_active_source_state() {
    let temp = tempdir().unwrap();
    let active_source = source("prepared-source-state.jsonl");
    let other = source("prepared-other-source.jsonl");
    let mut writer = GenerationWriter::open(temp.path(), WriterOptions::default())
        .unwrap()
        .into_writer()
        .unwrap();

    let inactive = writer
        .core_record_preparer()
        .prepare(document(&active_source, 1, "inactive prepared body"))
        .unwrap();
    assert!(matches!(
        writer.add_prepared_core_record(inactive),
        Err(IndexError::DocumentSourceNotActive)
    ));

    writer.begin_source(active_source.clone()).unwrap();
    let wrong_source = writer
        .core_record_preparer()
        .prepare(document(&other, 1, "wrong source prepared body"))
        .unwrap();
    assert!(matches!(
        writer.add_prepared_core_record(wrong_source),
        Err(IndexError::DocumentSourceNotActive)
    ));

    let active = writer
        .core_record_preparer()
        .prepare(document(&active_source, 1, "active prepared body"))
        .unwrap();
    writer.add_prepared_core_record(active).unwrap();
    writer
        .certify_source(certificate(&active_source, 1, 1))
        .unwrap();
    writer.commit(|_| true).unwrap();

    let mut retained = GenerationWriter::open(temp.path(), WriterOptions::default())
        .unwrap()
        .into_writer()
        .unwrap();
    retained
        .retain_source(certificate(&active_source, 1, 1))
        .unwrap();
    let retained_record = retained
        .core_record_preparer()
        .prepare(document(&active_source, 2, "retained source prepared body"))
        .unwrap();
    assert!(matches!(
        retained.add_prepared_core_record(retained_record),
        Err(IndexError::DocumentSourceNotActive)
    ));
}

#[test]
fn unchanged_commit_returns_the_verified_base_without_republication() {
    let temp = tempdir().unwrap();
    let source = source("session.jsonl");
    let mut initial = GenerationWriter::open(temp.path(), WriterOptions::default())
        .unwrap()
        .into_writer()
        .unwrap();
    initial.begin_source(source.clone()).unwrap();
    initial
        .add_core_record(document(&source, 1, "stable generation"))
        .unwrap();
    initial
        .certify_source(appendable_certificate(&source, 1, 1, 10))
        .unwrap();
    let initial_receipt = initial.commit(|_| true).unwrap();

    let active_path = active_generation_path(temp.path());
    let meta_path = active_path.join("meta.json");
    let managed_path = active_path.join(".managed.json");
    let manifest_path = manifest_path(temp.path(), &initial_receipt.generation_id);
    let meta_before = fs::read(&meta_path).unwrap();
    let meta_metadata_before = fs::metadata(&meta_path).unwrap();
    let managed_before = fs::read(&managed_path).unwrap();
    let managed_metadata_before = fs::metadata(&managed_path).unwrap();
    let manifest_before = fs::read(&manifest_path).unwrap();
    let manifest_metadata_before = fs::metadata(&manifest_path).unwrap();

    let mut unchanged_writer = GenerationWriter::open(temp.path(), WriterOptions::default())
        .unwrap()
        .into_writer()
        .unwrap();
    assert!(
        unchanged_writer.writer.is_none(),
        "opening a generation must not construct Tantivy's IndexWriter"
    );
    assert!(
        unchanged_writer.preflight_lock.is_some(),
        "the lazy writer must retain Tantivy's exclusive lock"
    );
    let index_writer_constructions =
        std::sync::Arc::clone(&unchanged_writer.index_writer_constructions);
    let inventory = complete_inventory(&source, 1, vec![source.clone()]);
    unchanged_writer
        .certify_complete_inventory(inventory.clone())
        .unwrap();
    let base = unchanged_writer
        .begin_source_append(source.clone())
        .unwrap()
        .clone();
    let base_frontier = base.frontier().unwrap();
    let replay = CertifiedSourceAppend::certify(
        &base,
        base.clone(),
        base_frontier.certified_prefix_bytes(),
        *base_frontier.certified_prefix_digest(),
    )
    .unwrap();
    unchanged_writer.certify_source_append(replay).unwrap();
    assert!(
        unchanged_writer.writer.is_none(),
        "an exact certified replay must not construct Tantivy's IndexWriter"
    );
    let mut revalidations = 0;
    let unchanged = unchanged_writer
        .commit_with_complete_inventory_revalidation(
            |target| {
                revalidations += 1;
                matches!(
                    target,
                    RevalidationTarget::Source(certificate) if certificate == &base
                )
            },
            |current| current == &inventory,
        )
        .unwrap();

    assert_eq!(
        index_writer_constructions.load(Ordering::SeqCst),
        0,
        "a healthy exact no-op must construct zero Tantivy IndexWriters"
    );
    assert_eq!(revalidations, 1);
    assert_eq!(unchanged.generation_id, initial_receipt.generation_id);
    assert_eq!(unchanged.opstamp, initial_receipt.opstamp);
    assert_eq!(unchanged.indexed_documents, 1);
    assert_eq!(unchanged.certified_sources, 1);
    assert_eq!(unchanged.certified_source_bytes, 10);
    assert_eq!(
        unchanged.manifest().generation_id().unwrap(),
        unchanged.generation_id
    );
    assert_eq!(
        unchanged.manifest().sources,
        initial_receipt.manifest().sources
    );
    assert_eq!(
        unchanged.manifest().source_routes(),
        initial_receipt.manifest().source_routes()
    );
    assert_eq!(fs::read(&meta_path).unwrap(), meta_before);
    assert_eq!(
        fs::metadata(&meta_path).unwrap().modified().unwrap(),
        meta_metadata_before.modified().unwrap()
    );
    assert_eq!(fs::read(&managed_path).unwrap(), managed_before);
    assert_eq!(
        fs::metadata(&managed_path).unwrap().modified().unwrap(),
        managed_metadata_before.modified().unwrap()
    );
    assert_eq!(fs::read(&manifest_path).unwrap(), manifest_before);
    assert_eq!(
        fs::metadata(&manifest_path).unwrap().modified().unwrap(),
        manifest_metadata_before.modified().unwrap()
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt as _;

        assert_eq!(
            fs::metadata(&meta_path).unwrap().ino(),
            meta_metadata_before.ino()
        );
        assert_eq!(
            fs::metadata(&managed_path).unwrap().ino(),
            managed_metadata_before.ino()
        );
        assert_eq!(
            fs::metadata(&manifest_path).unwrap().ino(),
            manifest_metadata_before.ino()
        );
    }

    let verified = VerifiedIndex::open(temp.path()).unwrap();
    assert_eq!(verified.generation_id(), unchanged.generation_id);
    assert_eq!(verified.document_count(), 1);
    assert_eq!(verified.count_term("stable").unwrap(), 1);
}

#[test]
fn logical_rescan_retains_nonappendable_source_without_tantivy_artifacts() {
    let temp = tempdir().unwrap();
    let source = source("logical.sqlite");
    let mut initial = GenerationWriter::open(temp.path(), WriterOptions::default())
        .unwrap()
        .into_writer()
        .unwrap();
    initial.begin_source(source.clone()).unwrap();
    initial
        .add_core_record(document(&source, 1, "logical snapshot"))
        .unwrap();
    let certificate = certificate(&source, 1, 1);
    initial.certify_source(certificate.clone()).unwrap();
    let initial_receipt = initial.commit(|_| true).unwrap();

    let active_path = active_generation_path(temp.path());
    let root_files_before = fs::read_dir(&active_path)
        .unwrap()
        .filter_map(|entry| entry.ok())
        .filter(|entry| entry.path().is_file())
        .map(|entry| {
            let path = entry.path();
            (entry.file_name(), fs::read(path).unwrap())
        })
        .collect::<std::collections::BTreeMap<_, _>>();
    let managed_metadata_before = fs::metadata(active_path.join(".managed.json")).unwrap();

    let mut retained = GenerationWriter::open(temp.path(), WriterOptions::default())
        .unwrap()
        .into_writer()
        .unwrap();
    let constructions = Arc::clone(&retained.index_writer_constructions);
    let inventory = complete_inventory(&source, 1, vec![source.clone()]);
    retained
        .certify_complete_inventory(inventory.clone())
        .unwrap();
    retained.retain_source(certificate.clone()).unwrap();
    let receipt = retained
        .commit_with_complete_inventory_revalidation(
            |target| {
                matches!(
                    target,
                    RevalidationTarget::Source(current) if current == &certificate
                )
            },
            |current| current == &inventory,
        )
        .unwrap();

    let root_files_after = fs::read_dir(&active_path)
        .unwrap()
        .filter_map(|entry| entry.ok())
        .filter(|entry| entry.path().is_file())
        .map(|entry| {
            let path = entry.path();
            (entry.file_name(), fs::read(path).unwrap())
        })
        .collect::<std::collections::BTreeMap<_, _>>();
    let managed_metadata_after = fs::metadata(active_path.join(".managed.json")).unwrap();
    assert_eq!(constructions.load(Ordering::SeqCst), 0);
    assert_eq!(receipt.generation_id, initial_receipt.generation_id);
    assert_eq!(receipt.opstamp, initial_receipt.opstamp);
    assert_eq!(root_files_after, root_files_before);
    assert_eq!(
        managed_metadata_after.modified().unwrap(),
        managed_metadata_before.modified().unwrap()
    );
}

#[test]
fn logical_rescan_advances_only_replay_frontier_without_rewriting_documents() {
    let temp = tempdir().unwrap();
    let source = source("logical-frontier.sqlite");
    let base = appendable_certificate(&source, 1, 1, 10);
    let mut initial = GenerationWriter::open(temp.path(), WriterOptions::default())
        .unwrap()
        .into_writer()
        .unwrap();
    initial.begin_source(source.clone()).unwrap();
    initial
        .add_core_record(document(&source, 1, "retained logical row"))
        .unwrap();
    initial.certify_source(base.clone()).unwrap();
    let initial_receipt = initial.commit(|_| true).unwrap();
    let initial_path = active_generation_path(temp.path());

    let observation = base.observation().clone();
    let current = CertifiedSource::certify_with_frontier(
        observation.clone(),
        observation,
        base.parser_revision(),
        *base.content_digest(),
        base.counts(),
        Some(
            SourceFrontier::new(
                "jsonl-byte-offset",
                TypedKey::U64(11),
                base.counts().certified_bytes,
                *base.content_digest(),
            )
            .unwrap(),
        ),
    )
    .unwrap();
    let mut retained = GenerationWriter::open(temp.path(), WriterOptions::default())
        .unwrap()
        .into_writer()
        .unwrap();
    let constructions = Arc::clone(&retained.index_writer_constructions);
    retained.retain_source(current.clone()).unwrap();
    let receipt = retained
        .commit(|target| matches!(target, RevalidationTarget::Source(source) if source == &current))
        .unwrap();

    assert_eq!(constructions.load(Ordering::SeqCst), 1);
    assert_ne!(receipt.generation_id, initial_receipt.generation_id);
    assert_eq!(receipt.manifest().sources, vec![current]);
    assert_eq!(
        receipt.manifest().core_record_aggregates,
        initial_receipt.manifest().core_record_aggregates
    );
    let published_path = active_generation_path(temp.path());
    assert_ne!(published_path, initial_path);
    let verified = VerifiedIndex::open(temp.path()).unwrap();
    assert_eq!(verified.document_count(), 1);
    assert_eq!(verified.count_term("retained").unwrap(), 1);
}

#[test]
fn logically_identical_one_pass_replacement_is_discarded_without_publication() {
    let temp = tempdir().unwrap();
    let source = source("logical-snapshot.sqlite");
    let certificate = certificate(&source, 1, 1);
    let mut initial = GenerationWriter::open(temp.path(), WriterOptions::default())
        .unwrap()
        .into_writer()
        .unwrap();
    initial.begin_source(source.clone()).unwrap();
    initial
        .add_core_record(document(&source, 1, "stable logical row"))
        .unwrap();
    initial.certify_source(certificate.clone()).unwrap();
    let initial_receipt = initial.commit(|_| true).unwrap();

    let meta_path = active_generation_path(temp.path()).join("meta.json");
    let meta_before = fs::read(&meta_path).unwrap();
    let meta_metadata_before = fs::metadata(&meta_path).unwrap();
    let manifests_before = fs::read_dir(temp.path().join(MANIFEST_DIRECTORY))
        .unwrap()
        .count();

    let mut staged = GenerationWriter::open(temp.path(), WriterOptions::default())
        .unwrap()
        .into_writer()
        .unwrap();
    let constructions = Arc::clone(&staged.index_writer_constructions);
    staged.begin_source(source.clone()).unwrap();
    staged
        .add_core_record(document(&source, 1, "stable logical row"))
        .unwrap();
    staged.certify_source(certificate.clone()).unwrap();
    let inventory = complete_inventory(&source, 1, vec![source.clone()]);
    staged
        .certify_complete_inventory(inventory.clone())
        .unwrap();
    assert_eq!(
        constructions.load(Ordering::SeqCst),
        1,
        "one-pass replacement staging should construct one disposable writer"
    );

    let mut source_revalidations = 0;
    let mut inventory_revalidations = 0;
    let receipt = staged
        .commit_with_complete_inventory_revalidation(
            |target| {
                source_revalidations += 1;
                matches!(
                    target,
                    RevalidationTarget::Source(current) if current == &certificate
                )
            },
            |current| {
                inventory_revalidations += 1;
                current == &inventory
            },
        )
        .unwrap();

    assert_eq!(source_revalidations, 1);
    assert_eq!(inventory_revalidations, 1);
    assert_eq!(receipt.generation_id, initial_receipt.generation_id);
    assert_eq!(receipt.opstamp, initial_receipt.opstamp);
    assert_eq!(fs::read(&meta_path).unwrap(), meta_before);
    assert_eq!(
        fs::metadata(&meta_path).unwrap().modified().unwrap(),
        meta_metadata_before.modified().unwrap()
    );
    assert_eq!(
        fs::read_dir(temp.path().join(MANIFEST_DIRECTORY))
            .unwrap()
            .count(),
        manifests_before
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt as _;

        assert_eq!(
            fs::metadata(&meta_path).unwrap().ino(),
            meta_metadata_before.ino()
        );
    }

    let verified = VerifiedIndex::open(temp.path()).unwrap();
    assert_eq!(verified.generation_id(), initial_receipt.generation_id);
    assert_eq!(verified.document_count(), 1);
    assert_eq!(verified.count_term("stable").unwrap(), 1);
}

#[test]
fn record_only_change_with_identical_source_certificate_publishes_a_new_generation() {
    let temp = tempdir().unwrap();
    let source = source("record-only-change.sqlite");
    let certificate = certificate(&source, 1, 1);
    let mut initial = GenerationWriter::open(temp.path(), WriterOptions::default())
        .unwrap()
        .into_writer()
        .unwrap();
    initial.begin_source(source.clone()).unwrap();
    initial
        .add_core_record(document(&source, 1, "old exact Core record"))
        .unwrap();
    initial.certify_source(certificate.clone()).unwrap();
    let initial_receipt = initial.commit(|_| true).unwrap();

    let mut replacement = GenerationWriter::open(temp.path(), WriterOptions::default())
        .unwrap()
        .into_writer()
        .unwrap();
    replacement.begin_source(source.clone()).unwrap();
    replacement
        .add_core_record(document(&source, 1, "changed exact Core record"))
        .unwrap();
    replacement.certify_source(certificate).unwrap();
    let replacement_receipt = replacement.commit(|_| true).unwrap();

    assert_ne!(
        replacement_receipt.generation_id,
        initial_receipt.generation_id
    );
    assert_ne!(
        replacement_receipt.manifest().core_record_aggregates,
        initial_receipt.manifest().core_record_aggregates
    );
    let verified = VerifiedIndex::open(temp.path()).unwrap();
    assert_eq!(verified.count_term("changed").unwrap(), 1);
    assert_eq!(verified.count_term("old").unwrap(), 0);
}

#[test]
fn exact_replay_omission_fails_with_typed_incomplete_coverage() {
    let temp = tempdir().unwrap();
    let replayed_source = source("replayed.jsonl");
    let omitted_source = source("omitted.jsonl");
    let mut initial = GenerationWriter::open(temp.path(), WriterOptions::default())
        .unwrap()
        .into_writer()
        .unwrap();
    initial.begin_source(replayed_source.clone()).unwrap();
    initial
        .add_core_record(document(&replayed_source, 1, "replayed source"))
        .unwrap();
    initial
        .certify_source(appendable_certificate(&replayed_source, 1, 1, 10))
        .unwrap();
    initial.begin_source(omitted_source.clone()).unwrap();
    initial
        .add_core_record(document(&omitted_source, 1, "omitted source"))
        .unwrap();
    initial
        .certify_source(appendable_certificate(&omitted_source, 1, 1, 10))
        .unwrap();
    let initial_receipt = initial.commit(|_| true).unwrap();

    let mut incomplete = GenerationWriter::open(temp.path(), WriterOptions::default())
        .unwrap()
        .into_writer()
        .unwrap();
    let base = incomplete
        .begin_source_append(replayed_source.clone())
        .unwrap()
        .clone();
    let frontier = base.frontier().unwrap();
    let replay = CertifiedSourceAppend::certify(
        &base,
        base.clone(),
        frontier.certified_prefix_bytes(),
        *frontier.certified_prefix_digest(),
    )
    .unwrap();
    incomplete.certify_source_append(replay).unwrap();
    let inventory = complete_inventory(
        &replayed_source,
        1,
        vec![replayed_source.clone(), omitted_source.clone()],
    );
    incomplete
        .certify_complete_inventory(inventory.clone())
        .unwrap();

    assert!(matches!(
        incomplete.exact_replay_inventory_witness(),
        Err(IndexError::IncompleteExactReplayCoverage { .. })
    ));
    assert!(
        incomplete.writer.is_none(),
        "the regression must reach commit through the lazy preflight state"
    );
    let error = incomplete
        .commit_with_complete_inventory_revalidation(|_| true, |current| current == &inventory)
        .unwrap_err();
    assert!(matches!(
        error,
        IndexError::IncompleteExactReplayCoverage { ref source_id }
            if source_id == &omitted_source.identity().to_string()
    ));

    let verified = VerifiedIndex::open(temp.path()).unwrap();
    assert_eq!(verified.generation_id(), initial_receipt.generation_id);
    assert_eq!(verified.document_count(), 2);
    assert_eq!(verified.count_term("replayed").unwrap(), 1);
    assert_eq!(verified.count_term("omitted").unwrap(), 1);
}

#[test]
fn exact_replay_accepts_independent_source_coverage_beside_complete_inventory() {
    let temp = tempdir().unwrap();
    let inventoried_source = source("inventoried.jsonl");
    let independent_source = source("independent.jsonl");
    let mut initial = GenerationWriter::open(temp.path(), WriterOptions::default())
        .unwrap()
        .into_writer()
        .unwrap();
    for (source, body) in [
        (&inventoried_source, "inventoried source"),
        (&independent_source, "independent source"),
    ] {
        initial.begin_source(source.clone()).unwrap();
        initial.add_core_record(document(source, 1, body)).unwrap();
        initial
            .certify_source(appendable_certificate(source, 1, 1, 10))
            .unwrap();
    }
    let initial_receipt = initial.commit(|_| true).unwrap();

    let mut replay = GenerationWriter::open(temp.path(), WriterOptions::default())
        .unwrap()
        .into_writer()
        .unwrap();
    let inventoried_certificate = stage_exact_replay(&mut replay, &inventoried_source);
    let independent_certificate = stage_exact_replay(&mut replay, &independent_source);
    let inventory = complete_inventory(&inventoried_source, 1, vec![inventoried_source.clone()]);
    replay
        .certify_complete_inventory(inventory.clone())
        .unwrap();
    let constructions = std::sync::Arc::clone(&replay.index_writer_constructions);
    let receipt = replay
        .commit_with_complete_inventory_revalidation(
            |target| match target {
                RevalidationTarget::Source(source) => {
                    source == &inventoried_certificate || source == &independent_certificate
                }
                RevalidationTarget::Deletion(_) => false,
            },
            |current| current == &inventory,
        )
        .unwrap();

    assert_eq!(constructions.load(Ordering::SeqCst), 0);
    assert_eq!(receipt.generation_id, initial_receipt.generation_id);
    assert_eq!(receipt.opstamp, initial_receipt.opstamp);
}

#[test]
fn exact_replay_witness_covers_retained_sources_and_carried_removals() {
    let temp = tempdir().unwrap();
    let retained = source("retained.jsonl");
    let removed = source("removed.jsonl");

    let mut initial = GenerationWriter::open(temp.path(), WriterOptions::default())
        .unwrap()
        .into_writer()
        .unwrap();
    initial.begin_source(retained.clone()).unwrap();
    initial
        .add_core_record(document(&retained, 1, "retained source"))
        .unwrap();
    initial
        .certify_source(appendable_certificate(&retained, 1, 1, 10))
        .unwrap();
    initial.begin_source(removed.clone()).unwrap();
    initial
        .add_core_record(document(&removed, 1, "removed source"))
        .unwrap();
    initial
        .certify_source(appendable_certificate(&removed, 1, 1, 10))
        .unwrap();
    initial.commit(|_| true).unwrap();

    let (deletion, inventory) =
        deletion_evidence_with_retained(&removed, 2, vec![retained.clone()]);
    let current_inventory = inventory.clone();
    let mut deleting = GenerationWriter::open(temp.path(), WriterOptions::default())
        .unwrap()
        .into_writer()
        .unwrap();
    deleting.delete_source(deletion, inventory).unwrap();
    let deleted = deleting.commit(|_| true).unwrap();

    let mut replay = GenerationWriter::open(temp.path(), WriterOptions::default())
        .unwrap()
        .into_writer()
        .unwrap();
    let replayed_source = stage_exact_replay(&mut replay, &retained);
    replay
        .certify_complete_inventory(current_inventory.clone())
        .unwrap();
    let constructions = std::sync::Arc::clone(&replay.index_writer_constructions);
    let mut source_revalidations = 0;
    let mut inventory_revalidations = 0;
    let receipt = replay
        .commit_with_complete_inventory_revalidation(
            |target| match target {
                RevalidationTarget::Source(source) => {
                    source_revalidations += 1;
                    source == &replayed_source
                }
                RevalidationTarget::Deletion(_) => false,
            },
            |inventory| {
                inventory_revalidations += 1;
                inventory == &current_inventory
            },
        )
        .unwrap();

    assert_eq!(constructions.load(Ordering::SeqCst), 0);
    assert_eq!(source_revalidations, 1);
    assert_eq!(inventory_revalidations, 1);
    assert_eq!(receipt.generation_id, deleted.generation_id);
    assert_eq!(receipt.opstamp, deleted.opstamp);
}

#[test]
fn exact_replay_witness_covers_removal_only_and_rejects_stale_inventory() {
    let temp = tempdir().unwrap();
    let removed = source("removed.jsonl");
    let mut initial = GenerationWriter::open(temp.path(), WriterOptions::default())
        .unwrap()
        .into_writer()
        .unwrap();
    initial.begin_source(removed.clone()).unwrap();
    initial
        .add_core_record(document(&removed, 1, "removed source"))
        .unwrap();
    initial
        .certify_source(appendable_certificate(&removed, 1, 1, 10))
        .unwrap();
    initial.commit(|_| true).unwrap();

    let (deletion, inventory) = deletion_evidence(&removed, 2);
    let current_inventory = inventory.clone();
    let mut deleting = GenerationWriter::open(temp.path(), WriterOptions::default())
        .unwrap()
        .into_writer()
        .unwrap();
    deleting.delete_source(deletion, inventory).unwrap();
    let deleted = deleting.commit(|_| true).unwrap();

    let mut replay = GenerationWriter::open(temp.path(), WriterOptions::default())
        .unwrap()
        .into_writer()
        .unwrap();
    replay
        .certify_complete_inventory(current_inventory.clone())
        .unwrap();
    let constructions = std::sync::Arc::clone(&replay.index_writer_constructions);
    let receipt = replay
        .commit_with_complete_inventory_revalidation(
            |_| false,
            |inventory| inventory == &current_inventory,
        )
        .unwrap();
    assert_eq!(constructions.load(Ordering::SeqCst), 0);
    assert_eq!(receipt.generation_id, deleted.generation_id);
    assert_eq!(receipt.opstamp, deleted.opstamp);

    let mut stale = GenerationWriter::open(temp.path(), WriterOptions::default())
        .unwrap()
        .into_writer()
        .unwrap();
    stale
        .certify_complete_inventory(current_inventory.clone())
        .unwrap();
    let terminal_inventory = std::sync::Arc::new(std::sync::Mutex::new(current_inventory.clone()));
    let inventory_after_reappearance = complete_inventory(&removed, 3, vec![removed.clone()]);
    *terminal_inventory.lock().unwrap() = inventory_after_reappearance;
    let raced_constructions = std::sync::Arc::clone(&stale.index_writer_constructions);
    let terminal_inventory_for_commit = std::sync::Arc::clone(&terminal_inventory);
    let mut inventory_revalidations = 0;
    let error = stale
        .commit_with_complete_inventory_revalidation(
            |_| false,
            |inventory| {
                inventory_revalidations += 1;
                inventory == &*terminal_inventory_for_commit.lock().unwrap()
            },
        )
        .unwrap_err();
    assert!(matches!(
        error,
        IndexError::CompleteInventoryInvalidated { .. }
    ));
    assert_eq!(inventory_revalidations, 1);
    assert_eq!(raced_constructions.load(Ordering::SeqCst), 0);
    assert_eq!(
        VerifiedIndex::open(temp.path()).unwrap().generation_id(),
        deleted.generation_id
    );
}

include!("writer/missing_route.rs");

#[test]
fn empty_inventory_requires_terminal_witness_and_rejects_discovered_source_race() {
    let temp = tempdir().unwrap();
    let discovered = source("discovered-after-opening.jsonl");
    let initial = GenerationWriter::open(temp.path(), WriterOptions::default())
        .unwrap()
        .into_writer()
        .unwrap()
        .commit(|_| true)
        .unwrap();

    let unwitnessed = GenerationWriter::open(temp.path(), WriterOptions::default())
        .unwrap()
        .into_writer()
        .unwrap();
    let unwitnessed_constructions = std::sync::Arc::clone(&unwitnessed.index_writer_constructions);
    let unwitnessed_receipt = unwitnessed.commit(|_| true).unwrap();
    assert_eq!(unwitnessed_receipt.generation_id, initial.generation_id);
    assert_eq!(
        unwitnessed_constructions.load(Ordering::SeqCst),
        1,
        "an empty base without current inventory authority must enter the ordinary writer path"
    );

    let opening_empty = complete_inventory(&discovered, 1, Vec::new());
    let mut empty = GenerationWriter::open(temp.path(), WriterOptions::default())
        .unwrap()
        .into_writer()
        .unwrap();
    empty
        .certify_complete_inventory(opening_empty.clone())
        .unwrap();
    let constructions = std::sync::Arc::clone(&empty.index_writer_constructions);
    let mut revalidations = 0;
    let replay = empty
        .commit_with_complete_inventory_revalidation(
            |_| false,
            |inventory| {
                revalidations += 1;
                inventory == &opening_empty
            },
        )
        .unwrap();

    assert_eq!(constructions.load(Ordering::SeqCst), 0);
    assert_eq!(revalidations, 1);
    assert_eq!(replay.generation_id, initial.generation_id);
    assert_eq!(replay.opstamp, unwitnessed_receipt.opstamp);
    assert_eq!(replay.indexed_documents, 0);
    assert_eq!(replay.certified_sources, 0);

    let mut raced = GenerationWriter::open(temp.path(), WriterOptions::default())
        .unwrap()
        .into_writer()
        .unwrap();
    raced
        .certify_complete_inventory(opening_empty.clone())
        .unwrap();
    let raced_constructions = std::sync::Arc::clone(&raced.index_writer_constructions);
    let terminal_inventory = std::sync::Arc::new(std::sync::Mutex::new(opening_empty));
    let inventory_after_discovery = complete_inventory(&discovered, 2, vec![discovered.clone()]);
    *terminal_inventory.lock().unwrap() = inventory_after_discovery;
    let terminal_inventory_for_commit = std::sync::Arc::clone(&terminal_inventory);
    let mut inventory_revalidations = 0;
    let error = raced
        .commit_with_complete_inventory_revalidation(
            |_| false,
            |inventory| {
                inventory_revalidations += 1;
                inventory == &*terminal_inventory_for_commit.lock().unwrap()
            },
        )
        .unwrap_err();
    assert!(matches!(
        error,
        IndexError::CompleteInventoryInvalidated { .. }
    ));
    assert_eq!(inventory_revalidations, 1);
    assert_eq!(raced_constructions.load(Ordering::SeqCst), 0);
    assert_eq!(
        VerifiedIndex::open(temp.path()).unwrap().generation_id(),
        unwitnessed_receipt.generation_id
    );
}

include!("writer/downgrade.rs");

include!("writer/reclamation.rs");

include!("writer/core_records.rs");
