use super::*;
use ctx_history_core::CaptureProvider;

#[test]
fn discovery_policy_only_change_publishes_and_preserves_carried_documents() {
    let temp = tempdir().unwrap();
    let source = source("automatic-root.jsonl");
    let route = SourceRouteIdentity::from_sha256("90".repeat(32)).unwrap();

    let mut initial = GenerationWriter::open(temp.path(), WriterOptions::default())
        .unwrap()
        .into_writer()
        .unwrap();
    initial
        .set_applied_provider_roots(true, provider_source_config_digest(true, &[]), Vec::new())
        .unwrap();
    initial.begin_source(source.clone()).unwrap();
    initial
        .add_core_record(document(&source, 1, "carried automatic history"))
        .unwrap();
    initial.certify_source(certificate(&source, 1, 1)).unwrap();
    initial
        .set_present_source_routes(vec![SourceRouteSnapshot::present(
            route.clone(),
            vec![source],
        )
        .unwrap()])
        .unwrap();
    let initial = initial.commit(|_| true).unwrap();

    let mut policy_change = GenerationWriter::open(temp.path(), WriterOptions::default())
        .unwrap()
        .into_writer()
        .unwrap();
    policy_change
        .set_applied_provider_roots(false, provider_source_config_digest(false, &[]), Vec::new())
        .unwrap();
    policy_change
        .set_source_route_plan(BTreeSet::new(), BTreeSet::from([route.clone()]))
        .unwrap();
    policy_change.set_present_source_routes(Vec::new()).unwrap();
    let changed = policy_change.commit(|_| true).unwrap();

    assert_ne!(changed.generation_id, initial.generation_id);
    let published = VerifiedIndex::open(temp.path()).unwrap();
    assert!(!published.manifest().automatic_provider_discovery());
    assert!(published.manifest().source_route(&route).is_some());
    assert_eq!(published.manifest().indexed_documents, 1);
    assert_eq!(published.count_term("carried").unwrap(), 1);
}

#[test]
fn provider_root_metadata_change_with_source_replacement_reopens_exactly() {
    let temp = tempdir().unwrap();
    let source = source("provider-root-metadata.jsonl");
    let route = SourceRouteIdentity::from_sha256("8f".repeat(32)).unwrap();
    let definition = |path: &str| ProviderRootDefinition {
        id: "personal".to_owned(),
        provider: CaptureProvider::Claude,
        path: PathBuf::from(path),
        group: Some("personal".to_owned()),
        kind: None,
    };
    let initial_definition = definition("/home/example/.claude-personal");

    let mut initial = GenerationWriter::open(temp.path(), WriterOptions::default())
        .unwrap()
        .into_writer()
        .unwrap();
    initial
        .set_applied_provider_roots(
            true,
            provider_source_config_digest(true, std::slice::from_ref(&initial_definition)),
            vec![AppliedProviderRoot::new(initial_definition, vec![route.clone()]).unwrap()],
        )
        .unwrap();
    initial.begin_source(source.clone()).unwrap();
    initial
        .add_core_record(document(&source, 1, "initial provider-root document"))
        .unwrap();
    initial.certify_source(certificate(&source, 1, 1)).unwrap();
    initial
        .set_present_source_routes(vec![SourceRouteSnapshot::present(
            route.clone(),
            vec![source.clone()],
        )
        .unwrap()])
        .unwrap();
    initial.commit(|_| true).unwrap();

    let moved_definition = definition("/mnt/history/.claude-personal");
    let mut moved = GenerationWriter::open(temp.path(), WriterOptions::default())
        .unwrap()
        .into_writer()
        .unwrap();
    moved
        .set_applied_provider_roots(
            true,
            provider_source_config_digest(true, std::slice::from_ref(&moved_definition)),
            vec![AppliedProviderRoot::new(moved_definition.clone(), vec![route.clone()]).unwrap()],
        )
        .unwrap();
    moved.begin_source(source.clone()).unwrap();
    moved
        .add_core_record(document(&source, 2, "moved provider-root document"))
        .unwrap();
    moved.certify_source(certificate(&source, 2, 1)).unwrap();
    moved
        .set_present_source_routes(vec![
            SourceRouteSnapshot::present(route, vec![source]).unwrap()
        ])
        .unwrap();
    let committed = moved.commit(|_| true).unwrap();

    let reopened = VerifiedIndex::open(temp.path()).unwrap();
    assert!(committed.manifest().exact_snapshot_eq(reopened.manifest()));
    assert_eq!(
        reopened.manifest().provider_roots()[0].definition(),
        &moved_definition
    );
    assert_eq!(reopened.count_term("moved").unwrap(), 1);
}

#[test]
fn valid_provider_root_removal_retires_its_last_route_without_a_replacement() {
    let temp = tempdir().unwrap();
    let source = source("configured-root.jsonl");
    let expected_source_token = source_token(&source);
    let route = SourceRouteIdentity::from_sha256("91".repeat(32)).unwrap();
    let definition = ProviderRootDefinition {
        id: "personal".to_owned(),
        provider: CaptureProvider::Claude,
        path: PathBuf::from("/home/example/.claude-personal"),
        group: Some("personal".to_owned()),
        kind: None,
    };

    let mut initial = GenerationWriter::open(temp.path(), WriterOptions::default())
        .unwrap()
        .into_writer()
        .unwrap();
    initial
        .set_applied_provider_roots(
            true,
            provider_source_config_digest(true, std::slice::from_ref(&definition)),
            vec![AppliedProviderRoot::new(definition, vec![route.clone()]).unwrap()],
        )
        .unwrap();
    initial.begin_source(source.clone()).unwrap();
    initial
        .add_core_record(document(&source, 1, "removed provider root"))
        .unwrap();
    initial.certify_source(certificate(&source, 1, 1)).unwrap();
    initial
        .set_present_source_routes(vec![SourceRouteSnapshot::present(
            route.clone(),
            vec![source],
        )
        .unwrap()])
        .unwrap();
    initial.commit(|_| true).unwrap();

    let pinned = VerifiedIndex::open(temp.path()).unwrap();
    assert_eq!(
        pinned
            .manifest()
            .provider_root_source_tokens(&["personal".to_owned()], &[])
            .unwrap(),
        vec![expected_source_token.clone()]
    );
    assert_eq!(
        pinned
            .manifest()
            .provider_root_source_tokens(&[], &["personal".to_owned()])
            .unwrap(),
        vec![expected_source_token]
    );

    let mut removal = GenerationWriter::open(temp.path(), WriterOptions::default())
        .unwrap()
        .into_writer()
        .unwrap();
    removal
        .set_applied_provider_roots(true, provider_source_config_digest(true, &[]), Vec::new())
        .unwrap();
    removal
        .set_source_route_plan(BTreeSet::new(), BTreeSet::new())
        .unwrap();
    removal.set_present_source_routes(Vec::new()).unwrap();
    removal.commit(|_| true).unwrap();

    let published = VerifiedIndex::open(temp.path()).unwrap();
    assert_eq!(published.count_term("removed").unwrap(), 0);
    assert!(published.manifest().sources.is_empty());
    assert!(published.manifest().source_routes().is_empty());
    assert!(published.manifest().provider_roots().is_empty());
}

#[test]
fn authenticated_topology_transfer_reowns_an_unchanged_source_without_deleting_its_documents() {
    let temp = tempdir().unwrap();
    let source = source("topology-transfer.jsonl");
    let old_route = SourceRouteIdentity::from_sha256("92".repeat(32)).unwrap();
    let new_route = SourceRouteIdentity::from_sha256("93".repeat(32)).unwrap();

    let mut initial = GenerationWriter::open(temp.path(), WriterOptions::default())
        .unwrap()
        .into_writer()
        .unwrap();
    initial.begin_source(source.clone()).unwrap();
    initial
        .add_core_record(document(&source, 1, "topology transfer retained"))
        .unwrap();
    initial.certify_source(certificate(&source, 1, 1)).unwrap();
    initial
        .set_present_source_routes(vec![SourceRouteSnapshot::present(
            old_route.clone(),
            vec![source.clone()],
        )
        .unwrap()])
        .unwrap();
    initial.commit(|_| true).unwrap();

    let mut unauthorized = GenerationWriter::open(temp.path(), WriterOptions::default())
        .unwrap()
        .into_writer()
        .unwrap();
    assert!(matches!(
        unauthorized.set_source_route_plan(BTreeSet::from([new_route.clone()]), BTreeSet::new(),),
        Err(IndexError::InvalidSourceRoutePlan(_))
    ));
    drop(unauthorized);

    let mut transfer = GenerationWriter::open(temp.path(), WriterOptions::default())
        .unwrap()
        .into_writer()
        .unwrap();
    transfer
        .set_authorized_topology_route_retirements(BTreeSet::from([old_route.clone()]))
        .unwrap();
    transfer
        .set_source_route_plan(BTreeSet::from([new_route.clone()]), BTreeSet::new())
        .unwrap();
    transfer
        .begin_source_route_stage(new_route.clone())
        .unwrap();
    transfer.retain_source(certificate(&source, 1, 1)).unwrap();
    transfer.finish_source_route_stage(&new_route).unwrap();
    transfer
        .set_present_source_routes(vec![SourceRouteSnapshot::present(
            new_route.clone(),
            vec![source],
        )
        .unwrap()])
        .unwrap();
    transfer.commit(|_| true).unwrap();

    let published = VerifiedIndex::open(temp.path()).unwrap();
    assert_eq!(published.count_term("topology").unwrap(), 1);
    assert!(published.manifest().source_route(&old_route).is_none());
    assert!(published.manifest().source_route(&new_route).is_some());
}

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
    let route_b_record = document(&source_b, 2, "successful route b");
    let route_b_session_uuid = route_b_record.session_id.as_uuid();
    writer.add_core_record(route_b_record).unwrap();
    writer.certify_source(certificate(&source_b, 2, 1)).unwrap();
    writer.finish_source_route_stage(&route_b).unwrap();
    assert!(writer.changed_sessions.contains_key(&route_b_session_uuid));

    writer.begin_source_route_stage(route_c.clone()).unwrap();
    writer.begin_source(source_c.clone()).unwrap();
    let first_parent = document_for_session(&source_c, "first-parent", 30, "not published");
    let second_parent = document_for_session(&source_c, "second-parent", 31, "not published");
    let mut first_attempt = document(&source_c, 3, "partial failed route c");
    first_attempt
        .set_session_relationship(
            SessionRelationshipKind::Forked,
            Some(first_parent.session_id),
            first_parent.session_id,
        )
        .unwrap();
    let route_c_session_uuid = first_attempt.session_id.as_uuid();
    writer.add_core_record(first_attempt).unwrap();
    assert_eq!(
        writer
            .active_source_route_stage
            .as_ref()
            .unwrap()
            .changed_session_insertions,
        vec![route_c_session_uuid]
    );
    writer.rollback_source_route_stage(&route_c).unwrap();
    assert!(writer.changed_sessions.contains_key(&route_b_session_uuid));
    assert!(!writer.changed_sessions.contains_key(&route_c_session_uuid));

    // A second attempt may legitimately reconstruct the rolled-back session
    // with a different claim. This succeeds only when rollback restores the
    // route's changed-session insertions alongside Tantivy and manifest state.
    writer.begin_source_route_stage(route_c.clone()).unwrap();
    writer.begin_source(source_c.clone()).unwrap();
    let mut second_attempt = document(&source_c, 4, "second failed route c");
    second_attempt
        .set_session_relationship(
            SessionRelationshipKind::Forked,
            Some(second_parent.session_id),
            second_parent.session_id,
        )
        .unwrap();
    writer.add_core_record(second_attempt).unwrap();
    writer.rollback_source_route_stage(&route_c).unwrap();
    assert!(writer.changed_sessions.contains_key(&route_b_session_uuid));
    assert!(!writer.changed_sessions.contains_key(&route_c_session_uuid));
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
fn route_cohort_rollback_discards_every_member_but_keeps_prior_route_work() {
    let temp = tempdir().unwrap();
    let base_source = source("cohort-base.jsonl");
    let peer_source = source("cohort-peer.jsonl");
    let first_source = source("cohort-first.jsonl");
    let second_source = source("cohort-second.jsonl");
    let base_route = SourceRouteIdentity::from_sha256("11".repeat(32)).unwrap();
    let peer_route = SourceRouteIdentity::from_sha256("22".repeat(32)).unwrap();
    let first_route = SourceRouteIdentity::from_sha256("33".repeat(32)).unwrap();
    let second_route = SourceRouteIdentity::from_sha256("44".repeat(32)).unwrap();

    let mut initial = GenerationWriter::open(temp.path(), WriterOptions::default())
        .unwrap()
        .into_writer()
        .unwrap();
    initial.begin_source(base_source.clone()).unwrap();
    initial
        .add_core_record(document(&base_source, 1, "cohort retained base"))
        .unwrap();
    initial
        .certify_source(certificate(&base_source, 1, 1))
        .unwrap();
    initial
        .set_present_source_routes(vec![SourceRouteSnapshot::present(
            base_route.clone(),
            vec![base_source],
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
            BTreeSet::from([
                peer_route.clone(),
                first_route.clone(),
                second_route.clone(),
            ]),
            BTreeSet::from([base_route.clone()]),
        )
        .unwrap();

    writer.begin_source_route_stage(peer_route.clone()).unwrap();
    writer.begin_source(peer_source.clone()).unwrap();
    writer
        .add_core_record(document(&peer_source, 2, "cohort healthy peer"))
        .unwrap();
    writer
        .certify_source(certificate(&peer_source, 2, 1))
        .unwrap();
    writer.finish_source_route_stage(&peer_route).unwrap();

    writer
        .begin_source_route_cohort_stage(first_route.clone())
        .unwrap();
    writer
        .begin_source_route_stage(first_route.clone())
        .unwrap();
    writer.begin_source(first_source.clone()).unwrap();
    writer
        .add_core_record(document(&first_source, 3, "cohort provisional first"))
        .unwrap();
    writer
        .certify_source(certificate(&first_source, 3, 1))
        .unwrap();
    writer.finish_source_route_stage(&first_route).unwrap();
    writer
        .begin_source_route_stage(second_route.clone())
        .unwrap();
    writer.begin_source(second_source.clone()).unwrap();
    writer
        .add_core_record(document(&second_source, 4, "cohort provisional second"))
        .unwrap();
    writer
        .certify_source(certificate(&second_source, 4, 1))
        .unwrap();
    writer.finish_source_route_stage(&second_route).unwrap();
    writer.rollback_source_route_cohort_stage().unwrap();

    assert!(!writer
        .carry_failed_source_route_from_base(&first_route)
        .unwrap());
    assert!(!writer
        .carry_failed_source_route_from_base(&second_route)
        .unwrap());
    writer
        .set_present_source_routes(vec![SourceRouteSnapshot::present(
            peer_route.clone(),
            vec![peer_source],
        )
        .unwrap()])
        .unwrap();
    writer.commit(|_| true).unwrap();

    let published = VerifiedIndex::open(temp.path()).unwrap();
    assert_eq!(published.count_term("retained").unwrap(), 1);
    assert_eq!(published.count_term("healthy").unwrap(), 1);
    assert_eq!(published.count_term("provisional").unwrap(), 0);
    assert!(published.manifest().source_route(&base_route).is_some());
    assert!(published.manifest().source_route(&peer_route).is_some());
    assert!(published.manifest().source_route(&first_route).is_none());
    assert!(published.manifest().source_route(&second_route).is_none());

    let published_generation = published.generation_id().to_owned();
    drop(published);
    let abandoned_source = source("cohort-abandoned.jsonl");
    let abandoned_route = SourceRouteIdentity::from_sha256("55".repeat(32)).unwrap();
    {
        let mut abandoned = GenerationWriter::open(temp.path(), WriterOptions::default())
            .unwrap()
            .into_writer()
            .unwrap();
        abandoned
            .set_source_route_plan(
                BTreeSet::from([abandoned_route.clone()]),
                BTreeSet::from([base_route, peer_route]),
            )
            .unwrap();
        abandoned
            .begin_source_route_cohort_stage(abandoned_route.clone())
            .unwrap();
        abandoned
            .begin_source_route_stage(abandoned_route.clone())
            .unwrap();
        abandoned.begin_source(abandoned_source.clone()).unwrap();
        abandoned
            .add_core_record(document(
                &abandoned_source,
                5,
                "cohort abandoned provisional",
            ))
            .unwrap();
        abandoned
            .certify_source(certificate(&abandoned_source, 5, 1))
            .unwrap();
        abandoned
            .finish_source_route_stage(&abandoned_route)
            .unwrap();
    }
    let reopened = VerifiedIndex::open(temp.path()).unwrap();
    assert_eq!(reopened.generation_id(), published_generation);
    assert_eq!(reopened.count_term("abandoned").unwrap(), 0);
}

#[test]
fn many_route_checkpoints_record_only_route_local_session_insertions() {
    const ROUTES: usize = 64;

    let temp = tempdir().unwrap();
    let routes = (1..=ROUTES)
        .map(|index| SourceRouteIdentity::from_sha256(format!("{index:064x}")).unwrap())
        .collect::<Vec<_>>();
    let sources = (1..=ROUTES)
        .map(|index| source(&format!("route-local-session-{index}.jsonl")))
        .collect::<Vec<_>>();
    let mut writer = GenerationWriter::open(temp.path(), WriterOptions::default())
        .unwrap()
        .into_writer()
        .unwrap();
    writer
        .set_source_route_plan(routes.iter().cloned().collect(), BTreeSet::new())
        .unwrap();

    let mut total_undo_entries = 0;
    for (offset, (route, source)) in routes.iter().zip(&sources).enumerate() {
        writer.begin_source_route_stage(route.clone()).unwrap();
        assert!(writer
            .active_source_route_stage
            .as_ref()
            .unwrap()
            .changed_session_insertions
            .is_empty());
        writer.begin_source(source.clone()).unwrap();
        let record = document_for_session(
            source,
            &format!("route-local-session-{offset}"),
            offset as u64 + 1,
            "route-local insertion",
        );
        let session_uuid = record.session_id.as_uuid();
        writer.add_core_record(record).unwrap();

        let checkpoint = writer.active_source_route_stage.as_ref().unwrap();
        assert_eq!(checkpoint.changed_session_insertions, vec![session_uuid]);
        total_undo_entries += checkpoint.changed_session_insertions.len();
        assert_eq!(writer.changed_sessions.len(), offset + 1);

        writer
            .certify_source(certificate(source, offset as u8 + 1, 1))
            .unwrap();
        writer.finish_source_route_stage(route).unwrap();
        assert!(writer.active_source_route_stage.is_none());
    }

    assert_eq!(total_undo_entries, ROUTES);
    assert_eq!(writer.changed_sessions.len(), ROUTES);
    assert_eq!(ROUTES * (ROUTES - 1) / 2, 2016);
    assert!(
        total_undo_entries < ROUTES * (ROUTES - 1) / 2,
        "route checkpoints copied prior-session registry state"
    );
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
fn route_retirement_uses_logarithmic_canonical_member_lookup() {
    const RETIRED_SOURCES: usize = 65;
    const OTHER_ROUTE_SOURCES: usize = 513;

    let temp = tempdir().unwrap();
    let retired_route = SourceRouteIdentity::from_sha256("81".repeat(32)).unwrap();
    let other_route = SourceRouteIdentity::from_sha256("82".repeat(32)).unwrap();
    let replacement_route = SourceRouteIdentity::from_sha256("83".repeat(32)).unwrap();
    let mut retired_sources = (0..RETIRED_SOURCES)
        .map(|index| source(&format!("retirement-retired-{index:04}.jsonl")))
        .collect::<Vec<_>>();
    retired_sources.sort_by_key(source_sort_key);
    let mut other_sources = (0..OTHER_ROUTE_SOURCES)
        .map(|index| source(&format!("retirement-other-{index:04}.jsonl")))
        .collect::<Vec<_>>();
    other_sources.sort_by_key(source_sort_key);

    let mut initial = GenerationWriter::open(temp.path(), WriterOptions::default())
        .unwrap()
        .into_writer()
        .unwrap();
    for source in retired_sources.iter().chain(&other_sources) {
        initial.begin_source(source.clone()).unwrap();
        initial.certify_source(certificate(source, 1, 0)).unwrap();
    }
    initial
        .set_present_source_routes(vec![
            SourceRouteSnapshot::present(retired_route.clone(), retired_sources.clone()).unwrap(),
            SourceRouteSnapshot::present(other_route.clone(), other_sources.clone()).unwrap(),
        ])
        .unwrap();
    initial.commit(|_| true).unwrap();

    let mut replacement = GenerationWriter::open(temp.path(), WriterOptions::default())
        .unwrap()
        .into_writer()
        .unwrap();
    replacement
        .set_source_route_plan(
            BTreeSet::from([replacement_route.clone()]),
            BTreeSet::from([retired_route.clone(), other_route.clone()]),
        )
        .unwrap();
    replacement
        .begin_source_route_stage(replacement_route.clone())
        .unwrap();
    replacement
        .authorize_carried_source_route_retirement(&replacement_route, &retired_route)
        .unwrap();
    crate::writer_routes::reset_route_retirement_membership_probes();
    assert_eq!(
        replacement
            .retire_carried_source_route(&replacement_route, &retired_route)
            .unwrap(),
        retired_sources
    );
    let (lookups, comparisons) = crate::writer_routes::route_retirement_membership_probes();
    assert_eq!(lookups, RETIRED_SOURCES as u64);
    assert!(
        comparisons <= (RETIRED_SOURCES * 11) as u64,
        "canonical binary membership lookup made {comparisons} comparisons"
    );
    assert!(
        comparisons < (RETIRED_SOURCES * OTHER_ROUTE_SOURCES) as u64,
        "retirement regressed to a retired-sources by other-route-members scan"
    );

    replacement
        .finish_source_route_stage(&replacement_route)
        .unwrap();
    replacement
        .set_present_source_routes(vec![SourceRouteSnapshot::present(
            replacement_route,
            Vec::new(),
        )
        .unwrap()])
        .unwrap();
    replacement.commit(|_| true).unwrap();

    let published = VerifiedIndex::open(temp.path()).unwrap();
    assert!(published.manifest().source_route(&retired_route).is_none());
    assert_eq!(
        published
            .manifest()
            .source_route(&other_route)
            .unwrap()
            .sources(),
        other_sources
    );
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
fn writerless_failed_route_reuses_identical_publication_metadata() {
    let temp = tempdir().unwrap();
    let old_source = source("writerless-failed-route.jsonl");
    let old_route = SourceRouteIdentity::from_sha256("5c".repeat(32)).unwrap();
    let failed_route = SourceRouteIdentity::from_sha256("6d".repeat(32)).unwrap();

    let mut initial = GenerationWriter::open(temp.path(), WriterOptions::default())
        .unwrap()
        .into_writer()
        .unwrap();
    initial.begin_source(old_source.clone()).unwrap();
    initial
        .add_core_record(document(&old_source, 1, "writerless retained body"))
        .unwrap();
    initial
        .certify_source(appendable_certificate(&old_source, 1, 1, 10))
        .unwrap();
    initial
        .set_present_source_routes(vec![SourceRouteSnapshot::present(
            old_route.clone(),
            vec![old_source.clone()],
        )
        .unwrap()])
        .unwrap();
    let initial = initial
        .commit_with_publication_metadata(|_| true, |_| Ok(b"initial metadata".to_vec()))
        .unwrap();
    let meta_path = active_generation_path(temp.path()).join("meta.json");
    let meta_before = fs::read(&meta_path).unwrap();

    let mut failed = GenerationWriter::open(temp.path(), WriterOptions::default())
        .unwrap()
        .into_writer()
        .unwrap();
    failed
        .set_source_route_plan(
            BTreeSet::from([old_route.clone(), failed_route.clone()]),
            BTreeSet::new(),
        )
        .unwrap();
    failed.begin_source_route_stage(old_route.clone()).unwrap();
    let replayed = stage_exact_replay(&mut failed, &old_source);
    failed.finish_source_route_stage(&old_route).unwrap();
    assert!(!failed
        .carry_failed_source_route_from_base(&failed_route)
        .unwrap());
    failed
        .set_present_source_routes(vec![SourceRouteSnapshot::present(
            old_route.clone(),
            vec![old_source],
        )
        .unwrap()])
        .unwrap();
    assert!(
        failed.writer.is_none(),
        "carrying the verified base must not construct a Tantivy writer"
    );

    let replay = failed
        .commit_with_publication_metadata(
            |target| matches!(target, RevalidationTarget::Source(current) if current == &replayed),
            |_| -> Result<Vec<u8>> {
                panic!("writerless identical staging must skip the metadata factory")
            },
        )
        .unwrap();

    assert_eq!(replay.disposition(), PublicationDisposition::Reused);
    assert_eq!(
        replay.receipt().generation_id,
        initial.receipt().generation_id
    );
    assert_eq!(fs::read(&meta_path).unwrap(), meta_before);
    assert_eq!(
        replay.receipt().manifest().source_route(&old_route),
        initial.receipt().manifest().source_route(&old_route)
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

#[test]
fn incremental_route_delta_exposes_full_manifest_materialization() {
    let temp = tempdir().unwrap();
    let route = SourceRouteIdentity::from_sha256("58".repeat(32)).unwrap();
    let base_sources = (0_u8..3)
        .map(|index| source(&format!("materialized-base-{index}.jsonl")))
        .collect::<Vec<_>>();
    let mut initial = GenerationWriter::open(temp.path(), WriterOptions::default())
        .unwrap()
        .into_writer()
        .unwrap();
    for (index, current) in base_sources.iter().enumerate() {
        initial.begin_source(current.clone()).unwrap();
        initial
            .add_core_record(document(current, index as u64 + 1, "base materialization"))
            .unwrap();
        initial
            .certify_source(certificate(current, index as u8 + 1, 1))
            .unwrap();
    }
    initial
        .set_present_source_routes(vec![SourceRouteSnapshot::present(
            route.clone(),
            base_sources.clone(),
        )
        .unwrap()])
        .unwrap();
    initial.commit(|_| true).unwrap();

    let appended = source("materialized-append.jsonl");
    let mut incremental = GenerationWriter::open(temp.path(), WriterOptions::default())
        .unwrap()
        .into_writer()
        .unwrap();
    incremental
        .set_source_route_plan(BTreeSet::from([route.clone()]), BTreeSet::new())
        .unwrap();
    incremental.begin_source_route_stage(route.clone()).unwrap();
    incremental
        .retain_unstaged_source_route_members(&route)
        .unwrap();
    incremental.begin_source(appended.clone()).unwrap();
    incremental
        .add_core_record(document(&appended, 4, "delta materialization"))
        .unwrap();
    incremental
        .certify_source(certificate(&appended, 4, 1))
        .unwrap();
    incremental.finish_source_route_stage(&route).unwrap();
    incremental.set_present_source_routes(Vec::new()).unwrap();

    crate::writer_publication::reset_manifest_materialization_visits();
    let published = incremental.commit(|_| true).unwrap();
    assert_eq!(
        crate::writer_publication::manifest_materialization_visits(),
        (3, 3, 0),
        "the current self-contained manifest format still materializes every base certificate and partial-route member once"
    );
    assert_eq!(
        published
            .manifest()
            .source_route(&route)
            .unwrap()
            .sources()
            .len(),
        4
    );
}
