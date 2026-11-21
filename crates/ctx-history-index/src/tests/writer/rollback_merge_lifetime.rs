use super::*;
use std::sync::atomic::AtomicUsize;
use tantivy::indexer::{MergeCandidate, MergePolicy};
use tantivy::SegmentMeta;

#[derive(Debug)]
struct ObserveCandidates {
    inner: Arc<dyn MergePolicy>,
    scheduled: Arc<AtomicUsize>,
}

impl MergePolicy for ObserveCandidates {
    fn compute_merge_candidates(&self, segments: &[SegmentMeta]) -> Vec<MergeCandidate> {
        let candidates = self.inner.compute_merge_candidates(segments);
        self.scheduled.fetch_add(candidates.len(), Ordering::SeqCst);
        candidates
    }
}

fn observe(writer: &mut GenerationWriter) -> Arc<AtomicUsize> {
    let scheduled = Arc::new(AtomicUsize::new(0));
    let actual = writer.writer_mut().unwrap();
    actual.set_merge_policy(Box::new(ObserveCandidates {
        inner: actual.get_merge_policy(),
        scheduled: Arc::clone(&scheduled),
    }));
    scheduled
}

fn assert_stage_merge_free(writer: &mut GenerationWriter, scheduled: &AtomicUsize) {
    // The GC task fences the real updater queue after the checkpoint's merge decisions.
    writer
        .writer
        .as_ref()
        .unwrap()
        .garbage_collect_files()
        .wait()
        .unwrap();
    let count = scheduled.load(Ordering::SeqCst);
    if count != 0 {
        // Join this updater before failing; rollback would lose its merge inventory.
        writer
            .writer
            .take()
            .unwrap()
            .wait_merging_threads()
            .unwrap();
    }
    assert_eq!(
        count, 0,
        "rollbackable ctx staging scheduled real merge work"
    );
}

fn checkpoint(
    writer: &mut GenerationWriter,
    route: &SourceRouteIdentity,
    source: &SourceKey,
    body: &str,
) {
    writer.begin_source_route_stage(route.clone()).unwrap();
    writer.begin_source(source.clone()).unwrap();
    writer.add_core_record(document(source, 1, body)).unwrap();
    writer.certify_source(certificate(source, 1, 1)).unwrap();
    writer.finish_source_route_stage(route).unwrap();
}

fn candidate_snapshot(path: &Path) -> BTreeMap<PathBuf, u64> {
    fs::read_dir(path)
        .unwrap()
        .map(|entry| {
            let entry = entry.unwrap();
            (
                PathBuf::from(entry.file_name()),
                entry.metadata().unwrap().len(),
            )
        })
        .collect()
}

fn rollback_publication(cohort: bool, disable_terminal_merges: bool) {
    let temp = tempdir().unwrap();
    let route_count = if cohort { 19 } else { 18 };
    let routes = (1..=route_count)
        .map(|value| SourceRouteIdentity::from_sha256(format!("{value:02x}").repeat(32)).unwrap())
        .collect::<Vec<_>>();
    let sources = (1..=route_count)
        .map(|value| source(&format!("rollback-merge-{value}.jsonl")))
        .collect::<Vec<_>>();
    let mut writer = GenerationWriter::open(
        temp.path(),
        WriterOptions {
            indexer_threads: 1,
            memory_bytes: 64 * 1024 * 1024,
        },
    )
    .unwrap()
    .into_writer()
    .unwrap();
    writer
        .set_source_route_plan(routes.iter().cloned().collect(), BTreeSet::new())
        .unwrap();
    let scheduled = observe(&mut writer);
    for ordinal in 0..16 {
        checkpoint(&mut writer, &routes[ordinal], &sources[ordinal], "survivor");
        assert_stage_merge_free(&mut writer, &scheduled);
    }

    if cohort {
        writer
            .begin_source_route_cohort_stage(routes[16].clone())
            .unwrap();
        checkpoint(&mut writer, &routes[16], &sources[16], "discardedcohort");
        writer.begin_source_route_stage(routes[17].clone()).unwrap();
        writer.begin_source(sources[17].clone()).unwrap();
        writer
            .add_core_record(document(&sources[17], 1, "discarded"))
            .unwrap();
        writer.rollback_source_route_stage(&routes[17]).unwrap();
        writer.rollback_source_route_cohort_stage().unwrap();
        assert!(!writer
            .carry_failed_source_route_from_base(&routes[16])
            .unwrap());
        assert!(!writer
            .carry_failed_source_route_from_base(&routes[17])
            .unwrap());
    } else {
        writer.begin_source_route_stage(routes[16].clone()).unwrap();
        writer.begin_source(sources[16].clone()).unwrap();
        writer
            .add_core_record(document(&sources[16], 1, "discarded"))
            .unwrap();
        writer.rollback_source_route_stage(&routes[16]).unwrap();
        assert!(!writer
            .carry_failed_source_route_from_base(&routes[16])
            .unwrap());
    }
    if disable_terminal_merges {
        writer.test_disable_merges().unwrap();
    }
    let replacement_scheduled = observe(&mut writer);
    let last = route_count - 1;
    checkpoint(&mut writer, &routes[last], &sources[last], "afterrollback");
    assert_stage_merge_free(&mut writer, &replacement_scheduled);

    let present = (0..16)
        .chain(std::iter::once(last))
        .map(|ordinal| {
            SourceRouteSnapshot::present(routes[ordinal].clone(), vec![sources[ordinal].clone()])
                .unwrap()
        })
        .collect();
    writer.set_present_source_routes(present).unwrap();
    let candidate_path = writer
        .root
        .join(ctx_history_index_generation::INDEX_GENERATIONS_DIRECTORY)
        .join(writer.candidate_directory_name.as_ref().unwrap());
    let candidate = writer.index.clone();
    writer.commit(|_| true).unwrap();

    let published = VerifiedIndex::open(temp.path()).unwrap();
    assert_eq!(published.try_manifest().unwrap().indexed_documents, 17);
    assert_eq!(published.count_term("survivor").unwrap(), 16);
    assert_eq!(published.count_term("afterrollback").unwrap(), 1);
    assert_eq!(published.count_term("discarded").unwrap(), 0);
    assert_eq!(published.count_term("discardedcohort").unwrap(), 0);
    assert!(published
        .try_manifest()
        .unwrap()
        .source_route(&routes[16])
        .is_none());
    if cohort {
        assert!(published
            .try_manifest()
            .unwrap()
            .source_route(&routes[17])
            .is_none());
    }
    for ordinal in (0..16).chain(std::iter::once(last)) {
        assert!(published
            .try_manifest()
            .unwrap()
            .source_route(&routes[ordinal])
            .is_some());
    }
    let metas = candidate.load_metas().unwrap();
    assert_eq!(
        metas
            .segments
            .iter()
            .map(SegmentMeta::num_docs)
            .sum::<u32>(),
        17
    );
    if disable_terminal_merges {
        assert_eq!(metas.segments.len(), 17);
    } else {
        assert!(
            metas.segments.len() < 16,
            "terminal publication must coalesce eligible segments"
        );
    }
    for file in ctx_history_index_generation::active_index_files(&candidate).unwrap() {
        assert!(candidate_path.join(file).is_file());
    }
    let files = candidate_snapshot(&candidate_path);
    let meta_bytes = fs::read(candidate_path.join("meta.json")).unwrap();
    let reopened = VerifiedIndex::open(temp.path()).unwrap();
    assert_eq!(reopened.count_term("survivor").unwrap(), 16);
    assert_eq!(
        fs::read(candidate_path.join("meta.json")).unwrap(),
        meta_bytes
    );
    assert_eq!(candidate_snapshot(&candidate_path), files);
}

#[test]
fn route_rollback_defers_real_merge_work_until_publication() {
    rollback_publication(false, false);
}

#[test]
fn cohort_rollback_defers_real_merge_work_until_publication() {
    rollback_publication(true, false);
}

#[test]
fn explicit_merge_disabling_preserves_fragmented_publication() {
    rollback_publication(false, true);
}

fn pending_terminal_candidate() -> (TempDir, GenerationWriter, Arc<AtomicUsize>) {
    let temp = tempdir().unwrap();
    let mut writer = GenerationWriter::open(
        temp.path(),
        WriterOptions {
            indexer_threads: 1,
            memory_bytes: 64 * 1024 * 1024,
        },
    )
    .unwrap()
    .into_writer()
    .unwrap();
    let scheduled = observe(&mut writer);
    for ordinal in 0..17 {
        let current = source(&format!("terminal-abort-{ordinal}.jsonl"));
        writer.begin_source(current.clone()).unwrap();
        writer
            .add_core_record(document(&current, 1, "pending"))
            .unwrap();
        writer.certify_source(certificate(&current, 1, 1)).unwrap();
        if ordinal != 16 {
            writer.writer.as_mut().unwrap().commit().unwrap();
            assert_stage_merge_free(&mut writer, &scheduled);
        }
    }
    (temp, writer, scheduled)
}

#[test]
fn invalidated_terminal_candidate_does_not_start_merges() {
    let (temp, writer, scheduled) = pending_terminal_candidate();
    let result = writer.commit(|_| false);
    assert!(matches!(result, Err(IndexError::SourceInvalidated(_))));
    assert_eq!(scheduled.load(Ordering::SeqCst), 0);
    assert!(!temp
        .path()
        .join(ctx_history_index_generation::ACTIVE_GENERATION_POINTER_FILE)
        .exists());
}

#[test]
fn failed_merging_progress_does_not_start_merges() {
    let (temp, writer, scheduled) = pending_terminal_candidate();
    let result = writer.commit_with_generation_state(
        |_| true,
        |_| false,
        |context| Ok(context.manifest().generation_state().cloned().unwrap()),
        |stage| match stage {
            PublicationStage::Merging => Err(IndexError::WriterInvariant(
                "injected merging progress failure",
            )),
            _ => Ok(()),
        },
    );
    assert!(matches!(
        result,
        Err(IndexError::WriterInvariant(
            "injected merging progress failure"
        ))
    ));
    assert_eq!(scheduled.load(Ordering::SeqCst), 0);
    assert!(!temp
        .path()
        .join(ctx_history_index_generation::ACTIVE_GENERATION_POINTER_FILE)
        .exists());
}

#[test]
fn staging_compaction_bounds_segments_respects_merge_thread_cap_and_quiesces_on_rollback() {
    const COMMITTED_ROUTES: usize = crate::merge_policy::STAGING_SEGMENT_CEILING + 10;
    const TOTAL_ROUTES: usize = COMMITTED_ROUTES + 2;

    let temp = tempdir().unwrap();
    let routes = (1..=TOTAL_ROUTES)
        .map(|value| SourceRouteIdentity::from_sha256(format!("{value:02x}").repeat(32)).unwrap())
        .collect::<Vec<_>>();
    let sources = (1..=TOTAL_ROUTES)
        .map(|value| source(&format!("bounded-staging-{value}.jsonl")))
        .collect::<Vec<_>>();
    let options =
        WriterOptions::for_indexing_limits_with_parallelism(Some(15 * 1024 * 1024), Some(8), 16);
    assert_eq!(options.indexer_threads, 1);
    assert_eq!(options.merge_threads(), 1);

    let mut writer = GenerationWriter::open(temp.path(), options)
        .unwrap()
        .into_writer()
        .unwrap();
    writer
        .set_source_route_plan(routes.iter().cloned().collect(), BTreeSet::new())
        .unwrap();

    let mut peak_segments = 0_usize;
    for ordinal in 0..COMMITTED_ROUTES {
        checkpoint(
            &mut writer,
            &routes[ordinal],
            &sources[ordinal],
            "boundedstaging",
        );
        writer
            .writer
            .as_ref()
            .unwrap()
            .garbage_collect_files()
            .wait()
            .unwrap();
        peak_segments = peak_segments.max(writer.index.load_metas().unwrap().segments.len());
    }
    assert_eq!(writer.applied_merge_threads.load(Ordering::SeqCst), 1);
    assert!(
        writer
            .merge_control
            .as_ref()
            .unwrap()
            .staging_merges_started(),
        "staging past the segment ceiling must start bounded incremental compaction"
    );

    // Wait briefly by rolling back a failed route; quiesced rollback must join
    // any in-flight staging merge, preserve prior routes, and discard the failed one.
    let failed_index = COMMITTED_ROUTES;
    let survivor_index = COMMITTED_ROUTES + 1;
    writer
        .begin_source_route_stage(routes[failed_index].clone())
        .unwrap();
    writer.begin_source(sources[failed_index].clone()).unwrap();
    writer
        .add_core_record(document(
            &sources[failed_index],
            1,
            "rolledbackafterstagingmerge",
        ))
        .unwrap();
    writer
        .rollback_source_route_stage(&routes[failed_index])
        .unwrap();
    assert!(!writer
        .carry_failed_source_route_from_base(&routes[failed_index])
        .unwrap());
    assert_eq!(writer.applied_merge_threads.load(Ordering::SeqCst), 1);

    let segments_after_quiesce = writer.index.load_metas().unwrap().segments.len();
    assert!(
        segments_after_quiesce < crate::merge_policy::STAGING_SEGMENT_CEILING,
        "quiesced staging segment count {segments_after_quiesce} must stay below ceiling {} (peak={peak_segments})",
        crate::merge_policy::STAGING_SEGMENT_CEILING
    );

    checkpoint(
        &mut writer,
        &routes[survivor_index],
        &sources[survivor_index],
        "survivorafterquiescedrollback",
    );
    let present = (0..COMMITTED_ROUTES)
        .chain(std::iter::once(survivor_index))
        .map(|ordinal| {
            SourceRouteSnapshot::present(routes[ordinal].clone(), vec![sources[ordinal].clone()])
                .unwrap()
        })
        .collect();
    writer.set_present_source_routes(present).unwrap();
    writer.commit(|_| true).unwrap();

    let published = VerifiedIndex::open(temp.path()).unwrap();
    assert_eq!(published.document_count(), (COMMITTED_ROUTES + 1) as u64);
    assert_eq!(
        published.count_term("boundedstaging").unwrap(),
        COMMITTED_ROUTES
    );
    assert_eq!(
        published
            .count_term("survivorafterquiescedrollback")
            .unwrap(),
        1
    );
    assert_eq!(
        published.count_term("rolledbackafterstagingmerge").unwrap(),
        0
    );
}
