use super::*;

#[test]
fn failed_final_revalidation_keeps_the_previous_generation() {
    let temp = tempdir().unwrap();
    let source = source("session.jsonl");
    let mut first = GenerationWriter::open(temp.path(), WriterOptions::default())
        .unwrap()
        .into_writer()
        .unwrap();
    first.begin_source(source.clone()).unwrap();
    first
        .add_core_record(document(&source, 1, "previous generation"))
        .unwrap();
    first.certify_source(certificate(&source, 1, 1)).unwrap();
    let first_receipt = first.commit(|_| true).unwrap();

    let mut replacement = GenerationWriter::open(temp.path(), WriterOptions::default())
        .unwrap()
        .into_writer()
        .unwrap();
    replacement.begin_source(source.clone()).unwrap();
    replacement
        .add_core_record(document(&source, 1, "uncommitted replacement"))
        .unwrap();
    replacement
        .certify_source(certificate(&source, 2, 1))
        .unwrap();
    let error = replacement.commit(|_| false).unwrap_err();
    assert!(matches!(error, IndexError::SourceInvalidated(_)));

    let index = VerifiedIndex::open(temp.path()).unwrap();
    assert_eq!(index.generation_id(), first_receipt.generation_id);
    assert_eq!(index.count_term("previous").unwrap(), 1);
    assert_eq!(index.count_term("uncommitted").unwrap(), 0);
}

#[test]
fn crash_after_candidate_commit_before_verification_keeps_old_pointer_and_restarts() {
    let temp = tempdir().unwrap();
    let source = source("candidate-crash.jsonl");
    let mut initial = GenerationWriter::open(temp.path(), WriterOptions::default())
        .unwrap()
        .into_writer()
        .unwrap();
    initial.begin_source(source.clone()).unwrap();
    initial
        .add_core_record(document(&source, 1, "audited baseline"))
        .unwrap();
    initial.certify_source(certificate(&source, 1, 1)).unwrap();
    let baseline = initial.commit(|_| true).unwrap();
    let pointer_before = fs::read(temp.path().join("active-generation.json")).unwrap();

    let mut candidate = GenerationWriter::open(temp.path(), WriterOptions::default())
        .unwrap()
        .into_writer()
        .unwrap();
    candidate.begin_source(source.clone()).unwrap();
    candidate
        .add_core_record(document(&source, 1, "unverified candidate"))
        .unwrap();
    candidate
        .certify_source(certificate(&source, 2, 1))
        .unwrap();
    candidate.after_candidate_commit = Some(Box::new(|_| {
        panic!("simulated process death after candidate meta commit")
    }));
    let crash = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _ = candidate.commit(|_| true);
    }));
    assert!(crash.is_err());
    assert_eq!(
        fs::read(temp.path().join("active-generation.json")).unwrap(),
        pointer_before
    );
    let still_active = VerifiedIndex::open(temp.path()).unwrap();
    assert_eq!(still_active.generation_id(), baseline.generation_id);
    assert_eq!(still_active.count_term("baseline").unwrap(), 1);
    assert_eq!(still_active.count_term("unverified").unwrap(), 0);

    let restarted = GenerationWriter::open(temp.path(), WriterOptions::default())
        .unwrap()
        .into_writer()
        .unwrap();
    assert_eq!(
        restarted.base_manifest().unwrap().generation_id().unwrap(),
        baseline.generation_id
    );
}

#[test]
fn structural_verification_fault_never_switches_the_active_pointer() {
    let temp = tempdir().unwrap();
    let source = source("candidate-verification-fault.jsonl");
    let mut initial = GenerationWriter::open(temp.path(), WriterOptions::default())
        .unwrap()
        .into_writer()
        .unwrap();
    initial.begin_source(source.clone()).unwrap();
    initial
        .add_core_record(document(&source, 1, "verified baseline"))
        .unwrap();
    initial.certify_source(certificate(&source, 1, 1)).unwrap();
    let baseline = initial.commit(|_| true).unwrap();
    let pointer_before = fs::read(temp.path().join("active-generation.json")).unwrap();

    let mut candidate = GenerationWriter::open(temp.path(), WriterOptions::default())
        .unwrap()
        .into_writer()
        .unwrap();
    candidate.begin_source(source.clone()).unwrap();
    candidate
        .add_core_record(document(&source, 1, "corrupt candidate"))
        .unwrap();
    candidate
        .certify_source(certificate(&source, 2, 1))
        .unwrap();
    let source_token = source_token(&source);
    candidate.before_pointer_switch = Some(Box::new(move |candidate_path| {
        let directory = DurableMmapDirectory::open(candidate_path).unwrap();
        let index = Index::open(directory).unwrap();
        let payload = index.load_metas().unwrap().payload;
        let source_key = required_field(&index.schema(), "source_key").unwrap();
        let mut writer = index
            .writer_with_num_threads::<TantivyDocument>(1, INDEX_MEMORY_MIN_PER_THREAD)
            .unwrap();
        writer.delete_term(Term::from_field_text(source_key, &source_token));
        let mut prepared = writer.prepare_commit().unwrap();
        if let Some(payload) = payload {
            prepared.set_payload(&payload);
        }
        prepared.commit().unwrap();
        writer.wait_merging_threads().unwrap();
    }));
    let error = candidate.commit(|_| true).unwrap_err();
    assert!(
        matches!(error, IndexError::ConcurrentGenerationChange),
        "{error:?}"
    );
    assert_eq!(
        fs::read(temp.path().join("active-generation.json")).unwrap(),
        pointer_before
    );
    assert_eq!(
        VerifiedIndex::open(temp.path()).unwrap().generation_id(),
        baseline.generation_id
    );
    drop(
        GenerationWriter::open(temp.path(), WriterOptions::default())
            .unwrap()
            .into_writer()
            .unwrap(),
    );
}

fn assert_post_verification_file_mutation_keeps_previous_generation(managed: bool) {
    use std::io::{Seek as _, Write as _};

    let temp = tempdir().unwrap();
    let source = source(if managed {
        "terminal-managed-mutation.jsonl"
    } else {
        "terminal-segment-mutation.jsonl"
    });
    let mut initial = GenerationWriter::open(temp.path(), WriterOptions::default())
        .unwrap()
        .into_writer()
        .unwrap();
    initial.begin_source(source.clone()).unwrap();
    initial
        .add_core_record(document(&source, 1, "terminal fence baseline"))
        .unwrap();
    initial.certify_source(certificate(&source, 1, 1)).unwrap();
    let baseline = initial.commit(|_| true).unwrap();
    let pointer_before = fs::read(temp.path().join("active-generation.json")).unwrap();

    let mut candidate = GenerationWriter::open(temp.path(), WriterOptions::default())
        .unwrap()
        .into_writer()
        .unwrap();
    candidate.begin_source(source.clone()).unwrap();
    candidate
        .add_core_record(document(&source, 1, "terminal fence candidate"))
        .unwrap();
    candidate
        .certify_source(certificate(&source, 2, 1))
        .unwrap();
    candidate.before_pointer_publication = Some(Box::new(move |candidate_path| {
        let path = if managed {
            candidate_path.join(".managed.json")
        } else {
            fs::read_dir(candidate_path)
                .unwrap()
                .map(|entry| entry.unwrap().path())
                .find(|path| {
                    path.extension()
                        .is_some_and(|extension| extension == "store")
                })
                .unwrap()
        };
        let mut file = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(&path)
            .unwrap();
        if managed {
            let mut bytes = fs::read(&path).unwrap();
            let offset = bytes
                .windows(b"meta.json".len())
                .position(|window| window == b"meta.json")
                .unwrap();
            bytes[offset] = b'n';
            file.write_all(&bytes).unwrap();
        } else {
            let mut byte = [0_u8; 1];
            std::io::Read::read_exact(&mut file, &mut byte).unwrap();
            file.seek(std::io::SeekFrom::Start(0)).unwrap();
            byte[0] ^= 0x5a;
            file.write_all(&byte).unwrap();
        }
        file.sync_all().unwrap();
    }));

    let error = candidate.commit(|_| true).unwrap_err();
    assert!(
        matches!(
            error,
            IndexError::ChecksumMismatch | IndexError::ConcurrentGenerationChange
        ),
        "{error:?}"
    );
    assert_eq!(
        fs::read(temp.path().join("active-generation.json")).unwrap(),
        pointer_before
    );
    assert_eq!(
        VerifiedIndex::open_pinned(temp.path())
            .unwrap()
            .generation_id(),
        baseline.generation_id
    );
}

#[test]
fn terminal_activation_fence_rejects_post_verification_segment_mutation() {
    assert_post_verification_file_mutation_keeps_previous_generation(false);
}

#[test]
fn terminal_activation_fence_rejects_post_verification_same_size_managed_mutation() {
    assert_post_verification_file_mutation_keeps_previous_generation(true);
}

#[test]
fn terminal_activation_fence_rejects_candidate_manifest_replacement() {
    use std::io::{Seek as _, Write as _};

    let temp = tempdir().unwrap();
    let source = source("terminal-manifest-replacement.jsonl");
    let mut initial = GenerationWriter::open(temp.path(), WriterOptions::default())
        .unwrap()
        .into_writer()
        .unwrap();
    initial.begin_source(source.clone()).unwrap();
    initial
        .add_core_record(document(&source, 1, "manifest fence baseline"))
        .unwrap();
    initial.certify_source(certificate(&source, 1, 1)).unwrap();
    let baseline = initial.commit(|_| true).unwrap();
    let pointer_before = fs::read(temp.path().join("active-generation.json")).unwrap();

    let mut candidate = GenerationWriter::open(temp.path(), WriterOptions::default())
        .unwrap()
        .into_writer()
        .unwrap();
    candidate.begin_source(source.clone()).unwrap();
    candidate
        .add_core_record(document(&source, 1, "manifest fence candidate"))
        .unwrap();
    candidate
        .certify_source(certificate(&source, 2, 1))
        .unwrap();
    let root = temp.path().to_path_buf();
    let baseline_manifest = format!("{}.json", baseline.generation_id);
    candidate.before_pointer_publication = Some(Box::new(move |_| {
        let manifest = fs::read_dir(root.join(MANIFEST_DIRECTORY))
            .unwrap()
            .map(|entry| entry.unwrap())
            .find(|entry| {
                entry.file_type().unwrap().is_file()
                    && entry.file_name().to_str() != Some(baseline_manifest.as_str())
                    && entry
                        .path()
                        .extension()
                        .is_some_and(|extension| extension == "json")
            })
            .unwrap()
            .path();
        let mut file = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(manifest)
            .unwrap();
        file.seek(std::io::SeekFrom::Start(0)).unwrap();
        file.write_all(b"[").unwrap();
        file.sync_all().unwrap();
    }));

    let error = candidate.commit(|_| true).unwrap_err();
    assert!(
        matches!(
            error,
            IndexError::ChecksumMismatch | IndexError::ManifestDigestMismatch { .. }
        ),
        "{error:?}"
    );
    assert_eq!(
        fs::read(temp.path().join("active-generation.json")).unwrap(),
        pointer_before
    );
    assert_eq!(
        VerifiedIndex::open_pinned(temp.path())
            .unwrap()
            .generation_id(),
        baseline.generation_id
    );
}

#[test]
fn terminal_activation_fence_rejects_post_verification_directory_substitution() {
    let temp = tempdir().unwrap();
    let source = source("terminal-directory-substitution.jsonl");
    let mut initial = GenerationWriter::open(temp.path(), WriterOptions::default())
        .unwrap()
        .into_writer()
        .unwrap();
    initial.begin_source(source.clone()).unwrap();
    initial
        .add_core_record(document(&source, 1, "directory fence baseline"))
        .unwrap();
    initial.certify_source(certificate(&source, 1, 1)).unwrap();
    let baseline = initial.commit(|_| true).unwrap();
    let pointer_before = fs::read(temp.path().join("active-generation.json")).unwrap();

    let mut candidate = GenerationWriter::open(temp.path(), WriterOptions::default())
        .unwrap()
        .into_writer()
        .unwrap();
    candidate.begin_source(source.clone()).unwrap();
    candidate
        .add_core_record(document(&source, 1, "directory fence candidate"))
        .unwrap();
    candidate
        .certify_source(certificate(&source, 2, 1))
        .unwrap();
    candidate.before_pointer_publication = Some(Box::new(|candidate_path| {
        let displaced = candidate_path.with_extension("authenticated-candidate");
        fs::rename(candidate_path, displaced).unwrap();
        fs::create_dir(candidate_path).unwrap();
    }));

    let error = candidate.commit(|_| true).unwrap_err();
    assert!(
        matches!(
            error,
            IndexError::CurrentRepublishSourceTopology(_)
                | IndexError::ConcurrentGenerationChange
                | IndexError::ChecksumMismatch
                | IndexError::Tantivy(_)
        ),
        "{error:?}"
    );
    assert_eq!(
        fs::read(temp.path().join("active-generation.json")).unwrap(),
        pointer_before
    );
    assert_eq!(
        VerifiedIndex::open_pinned(temp.path())
            .unwrap()
            .generation_id(),
        baseline.generation_id
    );
}

#[test]
fn omitted_managed_body_projection_fault_before_pointer_switch_keeps_previous_generation() {
    let temp = tempdir().unwrap();
    let source = source("candidate-checksum-fault.jsonl");
    let mut initial = GenerationWriter::open(temp.path(), WriterOptions::default())
        .unwrap()
        .into_writer()
        .unwrap();
    initial.begin_source(source.clone()).unwrap();
    initial
        .add_core_record(document(&source, 1, "checksum baseline"))
        .unwrap();
    initial.certify_source(certificate(&source, 1, 1)).unwrap();
    let baseline = initial.commit(|_| true).unwrap();
    let pointer_before = fs::read(temp.path().join("active-generation.json")).unwrap();

    let mut candidate = GenerationWriter::open(temp.path(), WriterOptions::default())
        .unwrap()
        .into_writer()
        .unwrap();
    candidate.begin_source(source.clone()).unwrap();
    candidate
        .add_core_record(document(&source, 1, "checksum candidate"))
        .unwrap();
    candidate
        .certify_source(certificate(&source, 2, 1))
        .unwrap();
    candidate.before_pointer_switch = Some(Box::new(|candidate_path| {
        omit_managed_and_corrupt_body_projection(candidate_path);
    }));

    assert!(matches!(
        candidate.commit(|_| true),
        Err(IndexError::ChecksumMismatch)
    ));
    assert_eq!(
        fs::read(temp.path().join("active-generation.json")).unwrap(),
        pointer_before
    );
    let still_active = VerifiedIndex::open(temp.path()).unwrap();
    assert_eq!(still_active.generation_id(), baseline.generation_id);
    assert_eq!(still_active.count_term("baseline").unwrap(), 1);
    assert_eq!(still_active.count_term("candidate").unwrap(), 0);
}

fn assert_incremental_segment_corruption_is_rejected(corrupt_retained_segment: bool) {
    let temp = tempdir().unwrap();
    let source = source(if corrupt_retained_segment {
        "retained-segment-corruption.jsonl"
    } else {
        "changed-segment-corruption.jsonl"
    });
    let mut initial = GenerationWriter::open(temp.path(), WriterOptions::default())
        .unwrap()
        .into_writer()
        .unwrap();
    initial.begin_source(source.clone()).unwrap();
    initial
        .add_core_record(document(&source, 1, "uncorrupted retained body"))
        .unwrap();
    initial
        .certify_source(appendable_certificate(&source, 1, 1, 10))
        .unwrap();
    let baseline = initial.commit(|_| true).unwrap();
    let pointer_before = fs::read(temp.path().join("active-generation.json")).unwrap();
    let base_segment_ids = {
        let (searcher, _) = open_unverified_generation(temp.path());
        searcher
            .segment_readers()
            .iter()
            .map(|segment| segment.segment_id().uuid_string())
            .collect::<std::collections::HashSet<_>>()
    };

    let mut append = GenerationWriter::open(temp.path(), WriterOptions::default())
        .unwrap()
        .into_writer()
        .unwrap();
    let base = append.begin_source_append(source.clone()).unwrap().clone();
    append
        .add_core_record(document(&source, 2, "uncorrupted changed body"))
        .unwrap();
    append
        .certify_source_append(
            CertifiedSourceAppend::certify(
                &base,
                appendable_certificate(&source, 2, 2, 20),
                10,
                [1; 32],
            )
            .unwrap(),
        )
        .unwrap();
    append.before_pointer_switch = Some(Box::new(move |candidate_path| {
        corrupt_candidate_segment_store(
            candidate_path,
            &base_segment_ids,
            corrupt_retained_segment,
        );
    }));

    assert!(matches!(
        append.commit(|_| true),
        Err(IndexError::ChecksumMismatch)
    ));
    assert_eq!(
        fs::read(temp.path().join("active-generation.json")).unwrap(),
        pointer_before
    );
    let retained = VerifiedIndex::open(temp.path()).unwrap();
    assert_eq!(retained.generation_id(), baseline.generation_id);
    assert_eq!(retained.count_term("retained").unwrap(), 1);
    assert_eq!(retained.count_term("changed").unwrap(), 0);
}

#[test]
fn final_candidate_hash_rejects_changed_segment_byte_corruption() {
    assert_incremental_segment_corruption_is_rejected(false);
}

#[test]
fn final_candidate_hash_rejects_retained_segment_byte_corruption() {
    assert_incremental_segment_corruption_is_rejected(true);
}

#[test]
fn stored_core_aggregate_fault_before_pointer_switch_keeps_the_previous_generation() {
    const DOCUMENTS: u64 = 5;

    let temp = tempdir().unwrap();
    let source = source("candidate-core-aggregate-fault.jsonl");
    let mut initial = GenerationWriter::open(temp.path(), WriterOptions::default())
        .unwrap()
        .into_writer()
        .unwrap();
    initial.begin_source(source.clone()).unwrap();
    for sequence in 1..=DOCUMENTS {
        initial
            .add_core_record(document(&source, sequence, "aggregate baseline"))
            .unwrap();
    }
    initial
        .certify_source(certificate(&source, 1, DOCUMENTS))
        .unwrap();
    let baseline = initial.commit(|_| true).unwrap();
    let pointer_before = fs::read(temp.path().join("active-generation.json")).unwrap();

    let mut candidate = GenerationWriter::open(temp.path(), WriterOptions::default())
        .unwrap()
        .into_writer()
        .unwrap();
    candidate.begin_source(source.clone()).unwrap();
    for sequence in 1..=DOCUMENTS {
        candidate
            .add_core_record(document(&source, sequence, "aggregate candidate"))
            .unwrap();
    }
    candidate
        .certify_source(certificate(&source, 2, DOCUMENTS))
        .unwrap();
    candidate.before_pointer_switch = Some(Box::new(|candidate_path| {
        let directory = DurableMmapDirectory::open(candidate_path).unwrap();
        let index = Index::open(directory).unwrap();
        let payload = index.load_metas().unwrap().payload;
        let reader = index
            .reader_builder()
            .reload_policy(ReloadPolicy::Manual)
            .try_into()
            .unwrap();
        let searcher = reader.searcher();
        let address = searcher
            .search(&AllQuery, &DocSetCollector)
            .unwrap()
            .into_iter()
            .next()
            .unwrap();
        let mut forged = decoded_stored_core(&searcher, address);
        forged.content.normalized_body = Some("forged stored Core bytes".to_owned());
        let forged_event_id = forged.event_id;
        drop(searcher);
        drop(reader);

        let event_id = required_field(&index.schema(), "event_id").unwrap();
        let mut writer = index
            .writer_with_num_threads::<TantivyDocument>(1, INDEX_MEMORY_MIN_PER_THREAD)
            .unwrap();
        writer.set_merge_policy(Box::<NoMergePolicy>::default());
        writer.delete_term(Term::from_field_text(
            event_id,
            &forged_event_id.to_string(),
        ));
        writer.add_document(indexed_document(forged)).unwrap();
        let mut prepared = writer.prepare_commit().unwrap();
        if let Some(payload) = payload {
            prepared.set_payload(&payload);
        }
        prepared.commit().unwrap();
        writer.wait_merging_threads().unwrap();
    }));

    assert!(matches!(
        candidate.commit(|_| true),
        Err(IndexError::ConcurrentGenerationChange)
    ));
    assert_eq!(
        fs::read(temp.path().join("active-generation.json")).unwrap(),
        pointer_before
    );
    let still_active = VerifiedIndex::open(temp.path()).unwrap();
    assert_eq!(still_active.generation_id(), baseline.generation_id);
    assert_eq!(
        still_active.count_term("baseline").unwrap(),
        DOCUMENTS as usize
    );
    assert_eq!(still_active.count_term("forged").unwrap(), 0);
}

fn assert_valid_recommit_without_projection_is_rejected(field_name: &'static str) {
    let temp = tempdir().unwrap();
    let source = source(&format!("missing-{field_name}.jsonl"));
    let mut writer = GenerationWriter::open(temp.path(), WriterOptions::default())
        .unwrap()
        .into_writer()
        .unwrap();
    writer.begin_source(source.clone()).unwrap();
    writer
        .add_core_record(document(&source, 1, "projection authority body"))
        .unwrap();
    writer.certify_source(certificate(&source, 1, 1)).unwrap();
    writer.commit(|_| true).unwrap();

    let (searcher, manifest) = open_unverified_generation(temp.path());
    let address = searcher
        .search(&AllQuery, &DocSetCollector)
        .unwrap()
        .into_iter()
        .next()
        .unwrap();
    let complete = indexed_document(decoded_stored_core(&searcher, address));
    let omitted = required_field(searcher.schema(), field_name).unwrap();
    let mut forged = TantivyDocument::default();
    for (field, value) in complete.field_values() {
        if field != omitted {
            forged.add_field_value(field, value);
        }
    }
    let index = searcher.index().clone();
    drop(searcher);
    publish_unchecked_generation(
        temp.path(),
        &index,
        manifest,
        std::slice::from_ref(&source),
        vec![forged],
    );

    assert!(matches!(
        VerifiedIndex::open(temp.path()),
        Err(IndexError::InvalidStoredDocumentField(actual))
            if actual == field_name || actual == "query_projection"
    ));
}

#[test]
fn checksum_valid_recommit_without_body_search_is_rejected() {
    assert_valid_recommit_without_projection_is_rejected("body_search");
}

#[test]
fn checksum_valid_recommit_without_session_order_is_rejected() {
    assert_valid_recommit_without_projection_is_rejected("session_event_order");
}

#[test]
fn checksum_valid_recommit_without_neutral_core_order_is_rejected() {
    assert_valid_recommit_without_projection_is_rejected("semantic_event_order");
}

#[derive(Clone, Copy)]
enum QueryProjectionMutation {
    Omit(&'static str),
    Text(&'static str, &'static str),
    U64(&'static str, u64),
    I64(&'static str, i64),
    Bytes(&'static str, &'static [u8]),
}

impl QueryProjectionMutation {
    fn field_name(self) -> &'static str {
        match self {
            Self::Omit(field)
            | Self::Text(field, _)
            | Self::U64(field, _)
            | Self::I64(field, _)
            | Self::Bytes(field, _) => field,
        }
    }
}

fn query_projection_fixture_record(source: &SourceKey, body: &str) -> CoreRecord {
    let mut record = document(source, 1, body);
    record.content.activity.as_mut().unwrap().facts.extend([
        ProviderDeclaredFact {
            kind: LiteralFactKind::File,
            value: "src/current.rs".to_owned(),
        },
        ProviderDeclaredFact {
            kind: LiteralFactKind::Commit,
            value: "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".to_owned(),
        },
    ]);
    record.content.structured_content = Some(serde_json::json!({
        "provider_literal": "retained exact content"
    }));
    record.validate_contract().unwrap();
    record
}

fn recommit_candidate_with_query_projection_mutation(
    candidate_path: &Path,
    mutation: QueryProjectionMutation,
    target_event_id: Option<ctx_history_core::StableEntityId>,
) {
    let directory = DurableMmapDirectory::open(candidate_path).unwrap();
    let index = Index::open(directory).unwrap();
    let payload = index.load_metas().unwrap().payload;
    let reader = index
        .reader_builder()
        .reload_policy(ReloadPolicy::Manual)
        .try_into()
        .unwrap();
    let searcher = reader.searcher();
    let address = if let Some(event_id) = target_event_id {
        let event_id_field = required_field(&index.schema(), "event_id").unwrap();
        searcher
            .search(
                &tantivy::query::TermQuery::new(
                    Term::from_field_text(event_id_field, &event_id.to_string()),
                    tantivy::schema::IndexRecordOption::Basic,
                ),
                &DocSetCollector,
            )
            .unwrap()
            .into_iter()
            .next()
            .unwrap()
    } else {
        searcher
            .search(&AllQuery, &DocSetCollector)
            .unwrap()
            .into_iter()
            .next()
            .unwrap()
    };
    let core = decoded_stored_core(&searcher, address);
    let event_id = core.event_id;
    let complete = indexed_document(core);
    let target = required_field(&index.schema(), mutation.field_name()).unwrap();
    let mut forged = TantivyDocument::default();
    for (field, value) in complete.field_values() {
        if field != target {
            forged.add_field_value(field, value);
        }
    }
    match mutation {
        QueryProjectionMutation::Omit(_) => {}
        QueryProjectionMutation::Text(_, value) => forged.add_text(target, value),
        QueryProjectionMutation::U64(_, value) => forged.add_u64(target, value),
        QueryProjectionMutation::I64(_, value) => forged.add_i64(target, value),
        QueryProjectionMutation::Bytes(_, value) => forged.add_bytes(target, value),
    }
    drop(searcher);
    drop(reader);

    let event_id_field = required_field(&index.schema(), "event_id").unwrap();
    let mut writer = index
        .writer_with_num_threads::<TantivyDocument>(1, INDEX_MEMORY_MIN_PER_THREAD)
        .unwrap();
    writer.set_merge_policy(Box::<NoMergePolicy>::default());
    writer.delete_term(Term::from_field_text(event_id_field, &event_id.to_string()));
    writer.add_document(forged).unwrap();
    let mut prepared = writer.prepare_commit().unwrap();
    if let Some(payload) = payload {
        prepared.set_payload(&payload);
    }
    prepared.commit().unwrap();
    writer.wait_merging_threads().unwrap();
}

#[test]
fn candidate_publication_rejects_every_query_authoritative_projection_mutation() {
    const CORRUPT_EVENT_RANGE_ORDER: [u8; ctx_history_index_format::EVENT_RANGE_ORDER_KEY_LEN] =
        [0x5a; ctx_history_index_format::EVENT_RANGE_ORDER_KEY_LEN];
    let cases = [
        ("event_type", QueryProjectionMutation::Omit("event_type")),
        ("role", QueryProjectionMutation::Text("role", "assistant")),
        (
            "provider",
            QueryProjectionMutation::Text("provider", "forged-provider"),
        ),
        (
            "timestamp",
            QueryProjectionMutation::I64("occurred_at_unix_ms", 42),
        ),
        (
            "agent_scope",
            QueryProjectionMutation::Text("agent_scope", "subagent"),
        ),
        (
            "workspace_fact",
            QueryProjectionMutation::Text("fact_workspace", "/forged/workspace"),
        ),
        (
            "file_fact",
            QueryProjectionMutation::Text("fact_file", "src/forged.rs"),
        ),
        (
            "commit_fact",
            QueryProjectionMutation::Text(
                "fact_commit",
                "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
            ),
        ),
        (
            "size",
            QueryProjectionMutation::U64("core_content_bytes", 1),
        ),
        (
            "event_range_order_omitted",
            QueryProjectionMutation::Omit("event_range_order"),
        ),
        (
            "event_range_order_corrupt",
            QueryProjectionMutation::Bytes("event_range_order", &CORRUPT_EVENT_RANGE_ORDER),
        ),
    ];

    for (name, mutation) in cases {
        let temp = tempdir().unwrap();
        let source = source(&format!("projection-{name}.jsonl"));
        let mut initial = GenerationWriter::open(temp.path(), WriterOptions::default())
            .unwrap()
            .into_writer()
            .unwrap();
        initial.begin_source(source.clone()).unwrap();
        initial
            .add_core_record(query_projection_fixture_record(&source, "prior body"))
            .unwrap();
        initial.certify_source(certificate(&source, 1, 1)).unwrap();
        let baseline = initial.commit(|_| true).unwrap();
        let pointer_before = fs::read(temp.path().join("active-generation.json")).unwrap();

        let mut candidate = GenerationWriter::open(temp.path(), WriterOptions::default())
            .unwrap()
            .into_writer()
            .unwrap();
        candidate.begin_source(source.clone()).unwrap();
        candidate
            .add_core_record(query_projection_fixture_record(&source, "candidate body"))
            .unwrap();
        candidate
            .certify_source(certificate(&source, 2, 1))
            .unwrap();
        candidate.before_pointer_switch = Some(Box::new(move |candidate_path| {
            recommit_candidate_with_query_projection_mutation(candidate_path, mutation, None);
        }));

        assert!(matches!(
            candidate.commit(|_| true),
            Err(IndexError::ConcurrentGenerationChange)
        ));
        assert_eq!(
            fs::read(temp.path().join("active-generation.json")).unwrap(),
            pointer_before,
            "{name} mutation switched the active pointer"
        );
        let retained = VerifiedIndex::open(temp.path()).unwrap();
        assert_eq!(retained.generation_id(), baseline.generation_id, "{name}");
        assert_eq!(retained.count_term("prior").unwrap(), 1, "{name}");
        assert_eq!(retained.count_term("candidate").unwrap(), 0, "{name}");
    }
}

fn assert_malicious_incremental_copy_is_rejected_by_deep_scrub(
    mutation: QueryProjectionMutation,
    expected_error_field: &'static str,
) {
    let temp = tempdir().unwrap();
    let source = source("malicious-incremental-copy.jsonl");
    let original = document_for_session(&source, "original-session", 1, "shared copied body");
    let mut copied = document_for_session(&source, "copied-session", 2, "shared copied body");
    copied.parent_session_id = Some(original.session_id);
    copied.root_session_id = Some(original.session_id);
    copied.session_relationship = Some(ctx_history_core::ProviderNativeSessionRelationship::Forked);
    copied.event_copy = Some(ctx_history_core::ProviderNativeEventCopy {
        ancestor_session_id: original.session_id,
        ancestor_event_id: original.event_id,
        proof: ctx_history_core::ProviderNativeCopyProof::NativeCopiedFromField,
    });
    copied.validate_contract().unwrap();
    let copied_event_id = copied.event_id;

    let mut initial = GenerationWriter::open(temp.path(), WriterOptions::default())
        .unwrap()
        .into_writer()
        .unwrap();
    initial.begin_source(source.clone()).unwrap();
    initial.add_core_record(original).unwrap();
    initial
        .certify_source(appendable_certificate(&source, 1, 1, 10))
        .unwrap();
    let baseline = initial.commit(|_| true).unwrap();
    let pointer_before = fs::read(temp.path().join("active-generation.json")).unwrap();
    let (base_searcher, _) = open_unverified_generation(temp.path());
    let mut candidate = GenerationWriter::open(temp.path(), WriterOptions::default())
        .unwrap()
        .into_writer()
        .unwrap();
    let base = candidate
        .begin_source_append(source.clone())
        .unwrap()
        .clone();
    candidate.add_core_record(copied).unwrap();
    candidate
        .certify_source_append(
            CertifiedSourceAppend::certify(
                &base,
                appendable_certificate(&source, 2, 2, 20),
                10,
                [1; 32],
            )
            .unwrap(),
        )
        .unwrap();
    let scrub_result = std::sync::Arc::new(std::sync::Mutex::new(None));
    let result_for_hook = std::sync::Arc::clone(&scrub_result);
    let root = temp.path().to_path_buf();
    candidate.before_pointer_switch = Some(Box::new(move |candidate_path| {
        recommit_candidate_with_query_projection_mutation(
            candidate_path,
            mutation,
            Some(copied_event_id),
        );
        let directory = DurableMmapDirectory::open(candidate_path).unwrap();
        let index = Index::open(directory).unwrap();
        let metas = index.load_metas().unwrap();
        let manifest = load_publication_for_metas(&root, &metas)
            .unwrap()
            .into_parts()
            .1;
        let reader = index
            .reader_builder()
            .reload_policy(ReloadPolicy::Manual)
            .try_into()
            .unwrap();
        let searcher = reader.searcher();
        assert!(
            crate::publication::verify_publication_candidate(
                &searcher,
                &manifest,
                Some(&base_searcher),
            )
            .is_ok(),
            "the sealed writer boundary must not replay impossible post-seal projection mutation"
        );
        *result_for_hook.lock().unwrap() =
            Some(crate::publication::verify_searcher(&searcher, &manifest));
    }));

    assert!(matches!(
        candidate.commit(|_| true),
        Err(IndexError::ConcurrentGenerationChange)
    ));
    assert!(matches!(
        scrub_result.lock().unwrap().take().unwrap(),
        Err(IndexError::InvalidStoredDocumentField(field)) if field == expected_error_field
    ));
    assert_eq!(
        fs::read(temp.path().join("active-generation.json")).unwrap(),
        pointer_before
    );
    let retained = VerifiedIndex::open(temp.path()).unwrap();
    assert_eq!(retained.generation_id(), baseline.generation_id);
    assert_eq!(retained.count_term("shared").unwrap(), 1);
    assert!(retained
        .event_by_id(copied_event_id.as_uuid())
        .unwrap()
        .is_none());
}

#[test]
fn explicit_deep_scrub_rejects_incremental_copied_lineage_projection_mismatches() {
    for mutation in [
        QueryProjectionMutation::Text("provider_native_session_relationship", "delegated"),
        QueryProjectionMutation::Text(
            "event_copy_ancestor_event_id",
            "00000000-0000-0000-0000-000000000000",
        ),
    ] {
        assert_malicious_incremental_copy_is_rejected_by_deep_scrub(mutation, "query_projection");
    }
}

#[test]
fn explicit_deep_scrub_rejects_incremental_injected_copied_body_posting() {
    assert_malicious_incremental_copy_is_rejected_by_deep_scrub(
        QueryProjectionMutation::Text("body_search", "injectedcopybodyposting"),
        "body_search",
    );
}

mod additional;
