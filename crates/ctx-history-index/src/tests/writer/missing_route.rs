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
