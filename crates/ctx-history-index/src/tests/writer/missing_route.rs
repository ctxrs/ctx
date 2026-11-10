#[test]
fn certified_missing_route_grace_survives_reopen_reappearance_and_final_deletion() {
    const DELETE_AFTER: u32 = 3;

    let temp = tempdir().unwrap();
    let source = source("automatic-missing.jsonl");
    let route_id = SourceRouteIdentity::from_sha256("ab".repeat(32)).unwrap();
    let mut initial = GenerationWriter::open(temp.path(), WriterOptions::default())
        .unwrap()
        .into_writer()
        .unwrap();
    initial.begin_source(source.clone()).unwrap();
    initial
        .add_core_record(document(&source, 1, "last good automatic source"))
        .unwrap();
    initial
        .certify_source(appendable_certificate(&source, 1, 1, 10))
        .unwrap();
    initial
        .set_present_source_routes(vec![SourceRouteSnapshot::present(
            route_id.clone(),
            vec![source.clone()],
        )
        .unwrap()])
        .unwrap();
    let initial = initial.commit(|_| true).unwrap();

    let present_inventory = complete_inventory(&source, 2, vec![source.clone()]);
    let mut noop = GenerationWriter::open(temp.path(), WriterOptions::default())
        .unwrap()
        .into_writer()
        .unwrap();
    let replayed = stage_exact_replay(&mut noop, &source);
    noop.certify_complete_inventory(present_inventory.clone())
        .unwrap();
    noop.set_present_source_routes(vec![SourceRouteSnapshot::present(
        route_id.clone(),
        vec![source.clone()],
    )
    .unwrap()])
        .unwrap();
    let noop = noop
        .commit_with_complete_inventory_revalidation(
            |target| matches!(target, RevalidationTarget::Source(current) if current == &replayed),
            |current| current == &present_inventory,
        )
        .unwrap();
    assert_eq!(noop.generation_id, initial.generation_id);
    assert!(noop
        .manifest()
        .source_route(&route_id)
        .unwrap()
        .missing_state()
        .is_none());

    let observe_missing = |observed_at_unix_ms| {
        let mut writer = GenerationWriter::open(temp.path(), WriterOptions::default())
            .unwrap()
            .into_writer()
            .unwrap();
        writer.set_present_source_routes(Vec::new()).unwrap();
        let outcome = writer
            .observe_certified_missing_route(
                route_id.clone(),
                observed_at_unix_ms,
                DELETE_AFTER,
                || true,
            )
            .unwrap();
        let receipt = writer.commit(|_| true).unwrap();
        (outcome.deleted(), receipt)
    };

    let (deleted, first_missing) = observe_missing(100);
    assert!(!deleted);
    assert_eq!(first_missing.indexed_documents, 1);
    let first_state = first_missing.manifest().source_route(&route_id).unwrap();
    let first_state = first_state.missing_state().unwrap();
    assert_eq!(first_state.consecutive_missing().get(), 1);
    assert_eq!(
        first_state.first_observation().generation_id(),
        initial.generation_id
    );
    assert_eq!(first_state.first_observation().observed_at_unix_ms(), 100);
    assert_eq!(
        first_state.first_observation(),
        first_state.last_observation()
    );

    let (deleted, second_missing) = observe_missing(200);
    assert!(!deleted);
    assert_eq!(second_missing.indexed_documents, 1);
    let second_state = second_missing.manifest().source_route(&route_id).unwrap();
    let second_state = second_state.missing_state().unwrap();
    assert_eq!(second_state.consecutive_missing().get(), 2);
    assert_eq!(
        second_state.first_observation(),
        first_state.first_observation()
    );
    assert_eq!(
        second_state.last_observation().generation_id(),
        first_missing.generation_id
    );
    assert_eq!(second_state.last_observation().observed_at_unix_ms(), 200);

    let reappeared_inventory = complete_inventory(&source, 5, vec![source.clone()]);
    let mut reappeared = GenerationWriter::open(temp.path(), WriterOptions::default())
        .unwrap()
        .into_writer()
        .unwrap();
    let replayed = stage_exact_replay(&mut reappeared, &source);
    reappeared
        .certify_complete_inventory(reappeared_inventory.clone())
        .unwrap();
    reappeared
        .set_present_source_routes(vec![SourceRouteSnapshot::present(
            route_id.clone(),
            vec![source.clone()],
        )
        .unwrap()])
        .unwrap();
    let reappeared = reappeared
        .commit_with_complete_inventory_revalidation(
            |target| matches!(target, RevalidationTarget::Source(current) if current == &replayed),
            |current| current == &reappeared_inventory,
        )
        .unwrap();
    assert_eq!(reappeared.indexed_documents, 1);
    assert!(reappeared
        .manifest()
        .source_route(&route_id)
        .unwrap()
        .missing_state()
        .is_none());

    let (deleted, missing_after_reset) = observe_missing(300);
    assert!(!deleted);
    let reset_state = missing_after_reset
        .manifest()
        .source_route(&route_id)
        .unwrap();
    let reset_state = reset_state.missing_state().unwrap();
    assert_eq!(reset_state.consecutive_missing().get(), 1);
    assert_eq!(
        reset_state.first_observation().generation_id(),
        reappeared.generation_id
    );

    let (deleted, second_after_reset) = observe_missing(400);
    assert!(!deleted);
    assert_eq!(
        second_after_reset
            .manifest()
            .source_route(&route_id)
            .unwrap()
            .missing_state()
            .unwrap()
            .consecutive_missing()
            .get(),
        2
    );

    let (deleted, final_deletion) = observe_missing(500);
    assert!(deleted);
    assert_eq!(final_deletion.indexed_documents, 0);
    assert!(final_deletion.manifest().source_routes().is_empty());
    assert_eq!(
        VerifiedIndex::open(temp.path()).unwrap().document_count(),
        0
    );

    let mut replay = GenerationWriter::open(temp.path(), WriterOptions::default())
        .unwrap()
        .into_writer()
        .unwrap();
    replay.set_present_source_routes(Vec::new()).unwrap();
    assert!(replay
        .observe_certified_missing_route(route_id, 600, DELETE_AFTER, || true)
        .unwrap()
        .retained_sources()
        .is_empty());
    let replay = replay.commit(|_| false).unwrap();
    assert_eq!(replay.generation_id, final_deletion.generation_id);
}

#[test]
fn missing_source_retains_citations_until_explicit_deletion() {
    let temp = tempdir().unwrap();
    let source = source("retained.jsonl");
    let record = document(&source, 1, "durable citation");
    let event_id = record.event_id.as_uuid();
    let mut initial = GenerationWriter::open(temp.path(), WriterOptions::default())
        .unwrap()
        .into_writer()
        .unwrap();
    initial.begin_source(source.clone()).unwrap();
    initial.add_core_record(record.clone()).unwrap();
    initial
        .certify_source(appendable_certificate(&source, 1, 1, 10))
        .unwrap();
    let initial = initial.commit(|_| true).unwrap();
    let inventory = complete_inventory(&source, 2, Vec::new());
    let proof = CertifiedSourceDeletion::from_inventory(source.clone(), &inventory).unwrap();
    let mut generation = None;
    for _ in 0..5 {
        let mut writer = GenerationWriter::open(temp.path(), WriterOptions::default())
            .unwrap()
            .into_writer()
            .unwrap();
        writer
            .retain_missing_source(proof.clone(), inventory.clone())
            .unwrap();
        let receipt = writer
            .commit(|target| matches!(target, RevalidationTarget::Deletion(_)))
            .unwrap();
        assert_eq!(receipt.indexed_documents, 1);
        assert_eq!(
            receipt.manifest().source_routes()[0].missing_sources(),
            std::slice::from_ref(&source)
        );
        assert_ne!(receipt.generation_id, initial.generation_id);
        if let Some(previous) = generation {
            assert_eq!(receipt.generation_id, previous);
        }
        generation = Some(receipt.generation_id);
        let index = VerifiedIndex::open(temp.path()).unwrap();
        assert_eq!(index.core_record_by_id(event_id).unwrap().unwrap(), record);
    }
    let mut writer = GenerationWriter::open(temp.path(), WriterOptions::default())
        .unwrap()
        .into_writer()
        .unwrap();
    writer.delete_source(proof, inventory).unwrap();
    writer.commit(|_| true).unwrap();
    assert!(VerifiedIndex::open(temp.path())
        .unwrap()
        .core_record_by_id(event_id)
        .unwrap()
        .is_none());
}

#[test]
fn partial_reappearance_clears_missing_status() {
    let temp = tempdir().unwrap();
    let source = source("reappearance.jsonl");
    let route = SourceRouteIdentity::from_sha256("58".repeat(32)).unwrap();
    let open = || {
        GenerationWriter::open(temp.path(), WriterOptions::default())
            .unwrap()
            .into_writer()
            .unwrap()
    };
    let mut initial = open();
    initial.begin_source(source.clone()).unwrap();
    initial
        .add_core_record(document(&source, 1, "before"))
        .unwrap();
    initial
        .certify_source(appendable_certificate(&source, 1, 1, 10))
        .unwrap();
    initial
        .set_present_source_routes(vec![SourceRouteSnapshot::present(
            route.clone(),
            vec![source.clone()],
        )
        .unwrap()])
        .unwrap();
    initial.commit(|_| true).unwrap();
    let inventory = complete_inventory(&source, 2, Vec::new());
    let proof = CertifiedSourceDeletion::from_inventory(source.clone(), &inventory).unwrap();
    let mut missing = open();
    missing
        .set_present_source_routes(vec![SourceRouteSnapshot::present(
            route.clone(),
            vec![source.clone()],
        )
        .unwrap()])
        .unwrap();
    missing.retain_missing_source(proof, inventory).unwrap();
    missing.commit(|_| true).unwrap();
    let mut present = open();
    present
        .set_source_route_plan(BTreeSet::from([route.clone()]), BTreeSet::new())
        .unwrap();
    present.begin_source_route_stage(route.clone()).unwrap();
    present
        .retain_unstaged_source_route_members(&route)
        .unwrap();
    present.begin_source(source.clone()).unwrap();
    present
        .add_core_record(document(&source, 1, "after"))
        .unwrap();
    present
        .certify_source(appendable_certificate(&source, 2, 1, 10))
        .unwrap();
    present.finish_source_route_stage(&route).unwrap();
    present.set_present_source_routes(Vec::new()).unwrap();
    let receipt = present.commit(|_| true).unwrap();
    assert!(receipt
        .manifest()
        .source_route(&route)
        .unwrap()
        .missing_sources()
        .is_empty());
}
