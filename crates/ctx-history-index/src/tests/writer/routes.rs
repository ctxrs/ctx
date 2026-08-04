use super::*;

#[test]
fn route_checkpoint_rolls_back_partial_route_and_keeps_prior_route_work() {
    let temp = tempdir().unwrap();
    let source_a = source("route-a.jsonl");
    let source_b = source("route-b.jsonl");
    let source_c = source("route-c.jsonl");
    let route_a = SourceRouteIdentity::from_sha256("a1".repeat(32)).unwrap();
    let route_b = SourceRouteIdentity::from_sha256("b2".repeat(32)).unwrap();
    let route_c = SourceRouteIdentity::from_sha256("c3".repeat(32)).unwrap();

    let mut initial = GenerationWriter::open(temp.path(), WriterOptions::default())
        .unwrap()
        .into_writer()
        .unwrap();
    initial.begin_source(source_a.clone()).unwrap();
    initial
        .add_core_record(document(&source_a, 1, "retained route a"))
        .unwrap();
    initial
        .certify_source(certificate(&source_a, 1, 1))
        .unwrap();
    initial
        .set_present_source_routes(vec![SourceRouteSnapshot::present(
            route_a.clone(),
            vec![source_a.clone()],
        )
        .unwrap()])
        .unwrap();
    initial.commit(|_| true).unwrap();

    let mut writer = GenerationWriter::open(temp.path(), WriterOptions::default())
        .unwrap()
        .into_writer()
        .unwrap();
    writer
        .set_source_route_plan(
            BTreeSet::from([route_b.clone(), route_c.clone()]),
            BTreeSet::from([route_a.clone()]),
        )
        .unwrap();
    writer.begin_source_route_stage(route_b.clone()).unwrap();
    writer.begin_source(source_b.clone()).unwrap();
    writer
        .add_core_record(document(&source_b, 2, "successful route b"))
        .unwrap();
    writer.certify_source(certificate(&source_b, 2, 1)).unwrap();
    writer.finish_source_route_stage(&route_b).unwrap();

    writer.begin_source_route_stage(route_c.clone()).unwrap();
    writer.begin_source(source_c.clone()).unwrap();
    writer
        .add_core_record(document(&source_c, 3, "partial failed route c"))
        .unwrap();
    writer.rollback_source_route_stage(&route_c).unwrap();
    assert!(!writer
        .carry_failed_source_route_from_base(&route_c)
        .unwrap());
    writer
        .set_present_source_routes(vec![SourceRouteSnapshot::present(
            route_b.clone(),
            vec![source_b],
        )
        .unwrap()])
        .unwrap();
    writer.commit(|_| true).unwrap();

    let published = VerifiedIndex::open(temp.path()).unwrap();
    assert_eq!(published.count_term("retained").unwrap(), 1);
    assert_eq!(published.count_term("successful").unwrap(), 1);
    assert_eq!(published.count_term("partial").unwrap(), 0);
    assert!(published.manifest().source_route(&route_a).is_some());
    assert!(published.manifest().source_route(&route_b).is_some());
    assert!(published.manifest().source_route(&route_c).is_none());
}

#[test]
fn selected_route_exact_noop_carries_unselected_base_without_revalidation() {
    let temp = tempdir().unwrap();
    let source_a = source("selected-noop-a.jsonl");
    let source_b = source("selected-noop-b.jsonl");
    let route_a = SourceRouteIdentity::from_sha256("d4".repeat(32)).unwrap();
    let route_b = SourceRouteIdentity::from_sha256("e5".repeat(32)).unwrap();
    let mut initial = GenerationWriter::open(temp.path(), WriterOptions::default())
        .unwrap()
        .into_writer()
        .unwrap();
    for (source, sequence) in [(&source_a, 1), (&source_b, 2)] {
        initial.begin_source(source.clone()).unwrap();
        initial
            .add_core_record(document(source, sequence, "selected no-op"))
            .unwrap();
        initial
            .certify_source(certificate(source, sequence as u8, 1))
            .unwrap();
    }
    initial
        .set_present_source_routes(vec![
            SourceRouteSnapshot::present(route_a.clone(), vec![source_a.clone()]).unwrap(),
            SourceRouteSnapshot::present(route_b.clone(), vec![source_b.clone()]).unwrap(),
        ])
        .unwrap();
    let initial = initial.commit(|_| true).unwrap();
    let base_route_a = initial.manifest().source_route(&route_a).unwrap().clone();

    let mut selected = GenerationWriter::open(temp.path(), WriterOptions::default())
        .unwrap()
        .into_writer()
        .unwrap();
    selected
        .set_source_route_plan(
            BTreeSet::from([route_b.clone()]),
            BTreeSet::from([route_a.clone()]),
        )
        .unwrap();
    selected.begin_source_route_stage(route_b.clone()).unwrap();
    selected
        .retain_source(
            initial
                .manifest()
                .sources
                .iter()
                .find(|certificate| {
                    certificate
                        .observation()
                        .source()
                        .exact_descriptor_eq(&source_b)
                })
                .unwrap()
                .clone(),
        )
        .unwrap();
    selected.finish_source_route_stage(&route_b).unwrap();
    selected
        .set_present_source_routes(vec![SourceRouteSnapshot::present(
            route_b.clone(),
            vec![source_b],
        )
        .unwrap()])
        .unwrap();
    let mut revalidated = 0;
    let noop = selected
        .commit(|_| {
            revalidated += 1;
            true
        })
        .unwrap();
    assert_eq!(noop.generation_id, initial.generation_id);
    assert_eq!(revalidated, 1, "only the selected route is revalidated");
    assert_eq!(noop.manifest().source_route(&route_a), Some(&base_route_a));

    let mut mutation = GenerationWriter::open(temp.path(), WriterOptions::default())
        .unwrap()
        .into_writer()
        .unwrap();
    mutation
        .set_source_route_plan(BTreeSet::new(), BTreeSet::from([route_a, route_b]))
        .unwrap();
    assert!(matches!(
        mutation.begin_source(source_a),
        Err(IndexError::CarriedSourceRouteMutation { .. })
    ));
}

#[test]
fn successful_route_atomically_retires_one_exact_carried_route() {
    let temp = tempdir().unwrap();
    let old_source = source("retired-route-old.jsonl");
    let replacement_source = source("retired-route-new.jsonl");
    let old_route = SourceRouteIdentity::from_sha256("18".repeat(32)).unwrap();
    let replacement_route = SourceRouteIdentity::from_sha256("29".repeat(32)).unwrap();

    let mut initial = GenerationWriter::open(temp.path(), WriterOptions::default())
        .unwrap()
        .into_writer()
        .unwrap();
    initial.begin_source(old_source.clone()).unwrap();
    initial
        .add_core_record(document(&old_source, 1, "retired route old body"))
        .unwrap();
    initial
        .certify_source(certificate(&old_source, 1, 1))
        .unwrap();
    initial
        .set_present_source_routes(vec![SourceRouteSnapshot::present(
            old_route.clone(),
            vec![old_source.clone()],
        )
        .unwrap()])
        .unwrap();
    initial.commit(|_| true).unwrap();

    let mut replacement = GenerationWriter::open(temp.path(), WriterOptions::default())
        .unwrap()
        .into_writer()
        .unwrap();
    replacement
        .set_source_route_plan(
            BTreeSet::from([replacement_route.clone()]),
            BTreeSet::from([old_route.clone()]),
        )
        .unwrap();
    replacement
        .begin_source_route_stage(replacement_route.clone())
        .unwrap();
    replacement
        .begin_source(replacement_source.clone())
        .unwrap();
    replacement
        .add_core_record(document(
            &replacement_source,
            2,
            "replacement route new body",
        ))
        .unwrap();
    replacement
        .certify_source(certificate(&replacement_source, 2, 1))
        .unwrap();
    assert_eq!(
        replacement
            .retire_carried_source_route(&replacement_route, &old_route)
            .unwrap(),
        vec![old_source]
    );
    replacement
        .finish_source_route_stage(&replacement_route)
        .unwrap();
    replacement
        .set_present_source_routes(vec![SourceRouteSnapshot::present(
            replacement_route.clone(),
            vec![replacement_source],
        )
        .unwrap()])
        .unwrap();
    replacement.commit(|_| true).unwrap();

    let published = VerifiedIndex::open(temp.path()).unwrap();
    assert_eq!(published.count_term("old").unwrap(), 0);
    assert_eq!(published.count_term("new").unwrap(), 1);
    assert!(published.manifest().source_route(&old_route).is_none());
    assert!(published
        .manifest()
        .source_route(&replacement_route)
        .is_some());
}

#[test]
fn failed_route_rollback_restores_its_carried_retirement() {
    let temp = tempdir().unwrap();
    let old_source = source("rollback-retired-route.jsonl");
    let old_route = SourceRouteIdentity::from_sha256("3a".repeat(32)).unwrap();
    let failed_route = SourceRouteIdentity::from_sha256("4b".repeat(32)).unwrap();

    let mut initial = GenerationWriter::open(temp.path(), WriterOptions::default())
        .unwrap()
        .into_writer()
        .unwrap();
    initial.begin_source(old_source.clone()).unwrap();
    initial
        .add_core_record(document(&old_source, 1, "rollback retained body"))
        .unwrap();
    initial
        .certify_source(certificate(&old_source, 1, 1))
        .unwrap();
    initial
        .set_present_source_routes(vec![SourceRouteSnapshot::present(
            old_route.clone(),
            vec![old_source.clone()],
        )
        .unwrap()])
        .unwrap();
    let initial = initial.commit(|_| true).unwrap();

    let mut failed = GenerationWriter::open(temp.path(), WriterOptions::default())
        .unwrap()
        .into_writer()
        .unwrap();
    failed
        .set_source_route_plan(
            BTreeSet::from([failed_route.clone()]),
            BTreeSet::from([old_route.clone()]),
        )
        .unwrap();
    failed
        .begin_source_route_stage(failed_route.clone())
        .unwrap();
    assert_eq!(
        failed
            .retire_carried_source_route(&failed_route, &old_route)
            .unwrap(),
        vec![old_source]
    );
    failed.rollback_source_route_stage(&failed_route).unwrap();
    assert!(!failed
        .carry_failed_source_route_from_base(&failed_route)
        .unwrap());
    failed.set_present_source_routes(Vec::new()).unwrap();
    let replay = failed.commit(|_| true).unwrap();

    assert_eq!(replay.generation_id, initial.generation_id);
    assert_eq!(
        replay.manifest().source_route(&old_route),
        initial.manifest().source_route(&old_route)
    );
    assert_eq!(
        VerifiedIndex::open(temp.path())
            .unwrap()
            .count_term("retained")
            .unwrap(),
        1
    );
}

#[test]
fn unpublished_route_checkpoint_is_reclaimed_after_reopen() {
    let temp = tempdir().unwrap();
    let source_a = source("checkpoint-crash-a.jsonl");
    let source_b = source("checkpoint-crash-b.jsonl");
    let route_a = SourceRouteIdentity::from_sha256("f6".repeat(32)).unwrap();
    let route_b = SourceRouteIdentity::from_sha256("07".repeat(32)).unwrap();
    let mut initial = GenerationWriter::open(temp.path(), WriterOptions::default())
        .unwrap()
        .into_writer()
        .unwrap();
    initial.begin_source(source_a.clone()).unwrap();
    initial
        .add_core_record(document(&source_a, 1, "active before checkpoint"))
        .unwrap();
    initial
        .certify_source(certificate(&source_a, 1, 1))
        .unwrap();
    initial
        .set_present_source_routes(vec![SourceRouteSnapshot::present(
            route_a.clone(),
            vec![source_a],
        )
        .unwrap()])
        .unwrap();
    let initial = initial.commit(|_| true).unwrap();

    {
        let mut abandoned = GenerationWriter::open(temp.path(), WriterOptions::default())
            .unwrap()
            .into_writer()
            .unwrap();
        abandoned
            .set_source_route_plan(BTreeSet::from([route_b.clone()]), BTreeSet::from([route_a]))
            .unwrap();
        abandoned.begin_source_route_stage(route_b.clone()).unwrap();
        abandoned.begin_source(source_b.clone()).unwrap();
        abandoned
            .add_core_record(document(&source_b, 2, "unpublished checkpoint"))
            .unwrap();
        abandoned
            .certify_source(certificate(&source_b, 2, 1))
            .unwrap();
        abandoned.finish_source_route_stage(&route_b).unwrap();
    }

    let still_active = VerifiedIndex::open(temp.path()).unwrap();
    assert_eq!(still_active.generation_id(), initial.generation_id);
    assert_eq!(still_active.count_term("unpublished").unwrap(), 0);
    drop(
        GenerationWriter::open(temp.path(), WriterOptions::default())
            .unwrap()
            .into_writer()
            .unwrap(),
    );
    assert_eq!(
        VerifiedIndex::open(temp.path()).unwrap().generation_id(),
        initial.generation_id
    );
}
