use super::*;
use crate::writer_replacement::ReplacementWork;

mod boundaries;
mod partial;
mod rollback;

fn open(root: &Path) -> GenerationWriter {
    GenerationWriter::open(
        root,
        WriterOptions {
            indexer_threads: 1,
            memory_bytes: 64 * 1024 * 1024,
        },
    )
    .unwrap()
    .into_writer()
    .unwrap()
}

fn stage(writer: &mut GenerationWriter, source: &SourceKey, revision: u8, records: &[CoreRecord]) {
    writer.begin_source(source.clone()).unwrap();
    for record in records {
        writer.add_core_record(record.clone()).unwrap();
    }
    writer
        .certify_source(certificate(source, revision, records.len() as u64))
        .unwrap();
}

fn records(source: &SourceKey) -> Vec<CoreRecord> {
    (0..3)
        .flat_map(|session| {
            (1..=3).map(move |sequence| {
                document_for_session(source, &format!("session-{session}"), sequence, "before")
            })
        })
        .collect()
}

fn seed(root: &Path, source: &SourceKey, records: &[CoreRecord]) {
    let mut writer = open(root);
    assert_eq!(writer.replacement_memory_bytes, 0);
    stage(&mut writer, source, 1, records);
    assert_eq!(writer.replacement_work.base_documents, 0);
    writer.commit(|_| true).unwrap();
}

fn assert_records(root: &Path, expected: &[CoreRecord]) {
    let index = VerifiedIndex::open(root).unwrap();
    assert_eq!(index.manifest().indexed_documents, expected.len() as u64);
    for record in expected {
        assert_eq!(
            index
                .core_record_by_id(record.event_id.as_uuid())
                .unwrap()
                .as_ref(),
            Some(record)
        );
    }
}

type StoredProjection = BTreeMap<[u8; 32], (Vec<u8>, Option<Vec<u8>>)>;

fn stored_projection(root: &Path) -> StoredProjection {
    let (searcher, _) = open_unverified_generation(root);
    let fields = fields_from_schema(searcher.schema()).unwrap();
    searcher
        .search(&AllQuery, &DocSetCollector)
        .unwrap()
        .into_iter()
        .map(|address| {
            let document: TantivyDocument = searcher.doc(address).unwrap();
            let encoded = document
                .get_first(fields.core_record)
                .unwrap()
                .as_bytes()
                .unwrap();
            let core = CoreRecord::decode_stored(encoded).unwrap();
            let mut authorities = document.get_all(fields.session_authority);
            let authority = authorities
                .next()
                .map(|value| value.as_bytes().unwrap().to_vec());
            assert!(authorities.next().is_none());
            (core.event_id.digest(), (encoded.to_vec(), authority))
        })
        .collect()
}

// Both arms start from the same source, then one deliberately uses the ordinary
// full replacement. Check authored expected records as well as paired output.
fn paired(
    source: &SourceKey,
    base: &[CoreRecord],
    current: &[CoreRecord],
) -> (TempDir, ReplacementWork) {
    let differential = tempdir().unwrap();
    let ordinary = tempdir().unwrap();
    for root in [&differential, &ordinary] {
        seed(root.path(), source, base);
    }
    let mut work = ReplacementWork::default();
    for (root, reuse) in [(&differential, true), (&ordinary, false)] {
        let mut writer = open(root.path());
        if !reuse {
            writer.replacement_memory_bytes = 0;
        }
        stage(&mut writer, source, 2, current);
        assert_eq!(writer.replacement_memory_used, 0);
        if reuse {
            work = writer.replacement_work;
            assert_eq!(work.source_deletions, 0);
        } else {
            assert_eq!(writer.replacement_work.source_deletions, 1);
            assert_eq!(writer.replacement_work.document_adds, current.len());
        }
        writer.commit(|_| true).unwrap();
        assert_records(root.path(), current);
    }
    assert_eq!(
        stored_projection(differential.path()),
        stored_projection(ordinary.path())
    );
    let actual = VerifiedIndex::open(differential.path()).unwrap();
    let control = VerifiedIndex::open(ordinary.path()).unwrap();
    assert!(actual.manifest().exact_snapshot_eq(control.manifest()));
    for term in ["before", "edited", "appended", "inserted"] {
        assert_eq!(
            actual.count_term(term).unwrap(),
            control.count_term(term).unwrap()
        );
    }
    (differential, work)
}

#[test]
fn middle_session_append_indexes_only_the_new_record() {
    let source = source("differential.jsonl");
    let base = records(&source);
    let mut current = base.clone();
    current.insert(6, document_for_session(&source, "session-1", 4, "appended"));
    let (_, work) = paired(&source, &base, &current);
    assert_eq!(
        work,
        ReplacementWork {
            base_documents: 9,
            document_adds: 1,
            retained_documents: 9,
            ..ReplacementWork::default()
        }
    );
}

#[test]
fn same_size_old_edit_and_old_deletions_preserve_unchanged_peers() {
    let source = source("differential-edits.jsonl");
    let base = records(&source);
    for case in ["edit", "middle", "last", "session", "insert"] {
        let mut current = base.clone();
        let expected = match case {
            "edit" => {
                current[4] = document_for_session(&source, "session-1", 2, "edited");
                assert_eq!(
                    base[4].encode_stored().unwrap().len(),
                    current[4].encode_stored().unwrap().len()
                );
                assert_eq!(base[4].occurred_at_unix_ms, current[4].occurred_at_unix_ms);
                (1, 1, 8)
            }
            "middle" => {
                current.remove(4);
                current[4].event_sequence = 2;
                (1, 2, 7)
            }
            "last" => {
                current.remove(5);
                (0, 1, 8)
            }
            "session" => {
                current.drain(3..6);
                (0, 3, 6)
            }
            _ => {
                let mut inserted = document_for_session(&source, "session-1", 4, "inserted");
                inserted.event_sequence = 2;
                current[4].event_sequence = 3;
                current[5].event_sequence = 4;
                current.insert(4, inserted);
                (3, 2, 7)
            }
        };
        let (_, work) = paired(&source, &base, &current);
        assert_eq!(
            (
                work.document_adds,
                work.event_deletions,
                work.retained_documents
            ),
            expected,
            "{case}"
        );
    }
}

#[test]
fn deleted_and_reordered_authority_carriers_are_replaced_even_with_equal_core() {
    let source = source("differential-authority.jsonl");
    let base = records(&source);
    let mut current = base.clone();
    current.remove(3); // Keep surviving Core bytes, including their old sequences.
    let (_, work) = paired(&source, &base, &current);
    assert_eq!(
        (
            work.document_adds,
            work.event_deletions,
            work.retained_documents
        ),
        (1, 2, 7)
    );

    let mut reordered = base.clone();
    reordered.swap(3, 4);
    let (root, work) = paired(&source, &base, &reordered);
    assert_eq!(
        (
            work.document_adds,
            work.event_deletions,
            work.retained_documents
        ),
        (2, 2, 7)
    );
    let projection = stored_projection(root.path());
    assert_eq!(
        projection
            .values()
            .filter(|(_, authority)| authority.is_some())
            .count(),
        3
    );
    for revision in 3..=4 {
        let mut writer = open(root.path());
        stage(&mut writer, &source, revision, &reordered);
        assert_eq!(writer.replacement_work.document_adds, 0);
        assert_eq!(writer.replacement_work.retained_documents, 9);
        writer.commit(|_| true).unwrap();
        assert_eq!(stored_projection(root.path()), projection);
    }
}

#[test]
fn replacement_claim_changes_do_not_merge_obsolete_base_relationships() {
    let source = source("differential-claims.jsonl");
    let mut base = records(&source);
    let old_parent = document_for_session(&source, "old-parent", 1, "unused").session_id;
    let new_parent = document_for_session(&source, "new-parent", 1, "unused").session_id;
    for record in &mut base[3..6] {
        record
            .set_session_relationship(
                SessionRelationshipKind::Forked,
                Some(old_parent),
                old_parent,
            )
            .unwrap();
    }
    for parent in [Some(new_parent), None] {
        let mut current = base.clone();
        for record in &mut current[3..6] {
            record.parent_session_id = parent;
            record.root_session_id = parent;
            record.session_relationship = parent.map(|_| SessionRelationshipKind::Forked);
        }
        let (_, work) = paired(&source, &base, &current);
        assert_eq!(
            (
                work.document_adds,
                work.event_deletions,
                work.retained_documents
            ),
            (3, 3, 6)
        );
    }
}

#[test]
fn complete_identical_rescan_reuses_generation_without_constructing_a_writer() {
    let root = tempdir().unwrap();
    let source = source("differential-noop.jsonl");
    let records = records(&source);
    seed(root.path(), &source, &records);
    let previous = VerifiedIndex::open(root.path())
        .unwrap()
        .generation_id()
        .to_owned();
    let mut writer = open(root.path());
    let constructions = Arc::clone(&writer.index_writer_constructions);
    stage(&mut writer, &source, 1, &records);
    assert!(writer.writer.is_none());
    let receipt = writer.commit(|_| true).unwrap();
    assert_eq!(receipt.generation_id, previous);
    assert_eq!(constructions.load(Ordering::SeqCst), 0);
}
