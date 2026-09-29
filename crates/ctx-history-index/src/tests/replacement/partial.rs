use super::*;

struct Pair {
    root: TempDir,
    a: CoreRecord,
    b: CoreRecord,
    generation: String,
}

fn two_sources() -> Pair {
    let root = tempdir().unwrap();
    let a = document(&source("partial-a.jsonl"), 1, "before");
    let b = document(&source("partial-b.jsonl"), 1, "untouched");
    let mut writer = open(root.path());
    for record in [&a, &b] {
        writer.begin_source(record.source.clone()).unwrap();
        writer.add_core_record(record.clone()).unwrap();
        writer
            .certify_source(appendable_certificate(&record.source, 1, 1, 10))
            .unwrap();
    }
    let generation = writer.commit(|_| true).unwrap().generation_id;
    Pair {
        root,
        a,
        b,
        generation,
    }
}

#[test]
fn partial_identical_replacement_preserves_unstaged_source_in_both_modes() {
    for differential in [true, false] {
        for revision in [1, 2] {
            let pair = two_sources();
            let mut writer = open(pair.root.path());
            if !differential {
                writer.replacement_memory_bytes = 0;
            }
            writer.begin_source(pair.a.source.clone()).unwrap();
            writer.add_core_record(pair.a.clone()).unwrap();
            let current = appendable_certificate(&pair.a.source, revision, 1, 10);
            writer.certify_source(current.clone()).unwrap();
            assert_eq!(writer.writer.is_none(), differential);
            assert!(writer.exact_replay_inventory_witness().unwrap().is_none());
            assert_eq!(
                writer.replacement_work.retained_documents,
                usize::from(differential)
            );
            let mut revalidated = Vec::new();
            let receipt = writer
                .commit(|target| {
                    if let RevalidationTarget::Source(certificate) = target {
                        revalidated.push(certificate.clone());
                        certificate == &current
                    } else {
                        false
                    }
                })
                .unwrap();
            assert_eq!(revalidated, vec![current.clone()]);
            // A complete replacement of A makes no complete-replay claim for B.
            assert!(receipt.manifest().sources.contains(&current));
            assert!(receipt.manifest().sources.contains(&appendable_certificate(
                &pair.b.source,
                1,
                1,
                10
            )));
            assert_records(pair.root.path(), &[pair.a, pair.b]);
        }
    }
}

#[test]
fn partial_changed_replacement_preserves_unstaged_source_in_both_modes() {
    for differential in [true, false] {
        let pair = two_sources();
        let mut writer = open(pair.root.path());
        if !differential {
            writer.replacement_memory_bytes = 0;
        }
        let changed = document(&pair.a.source, 1, "edited");
        stage(
            &mut writer,
            &pair.a.source,
            2,
            std::slice::from_ref(&changed),
        );
        assert_eq!(writer.replacement_work.document_adds, 1);
        let mut revalidations = 0;
        writer
            .commit(|target| {
                revalidations += 1;
                matches!(target, RevalidationTarget::Source(certificate)
                if certificate.observation().source().exact_descriptor_eq(&pair.a.source))
            })
            .unwrap();
        assert_eq!(revalidations, 1);
        assert_records(pair.root.path(), &[changed, pair.b]);
        let published = VerifiedIndex::open(pair.root.path()).unwrap();
        assert_eq!(published.count_term("before").unwrap(), 0);
        assert_eq!(published.count_term("edited").unwrap(), 1);
        assert_eq!(published.count_term("untouched").unwrap(), 1);
    }
}

#[test]
fn actual_exact_replay_still_rejects_omitted_source() {
    let pair = two_sources();
    let mut writer = open(pair.root.path());
    stage_exact_replay(&mut writer, &pair.a.source);
    let inventory = complete_inventory(
        &pair.a.source,
        1,
        vec![pair.a.source.clone(), pair.b.source.clone()],
    );
    writer
        .certify_complete_inventory(inventory.clone())
        .unwrap();
    assert!(writer.writer.is_none());
    let mut revalidations = 0;
    let error = writer
        .commit_with_complete_inventory_revalidation(
            |_| {
                revalidations += 1;
                true
            },
            |current| current == &inventory,
        )
        .unwrap_err();
    assert!(
        matches!(error, IndexError::IncompleteExactReplayCoverage { source_id }
        if source_id == pair.b.source.identity().to_string())
    );
    assert_eq!(revalidations, 0);
    assert_eq!(
        VerifiedIndex::open(pair.root.path())
            .unwrap()
            .generation_id(),
        pair.generation
    );
    assert_records(pair.root.path(), &[pair.a, pair.b]);
}
