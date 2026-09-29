use super::*;

#[test]
fn failed_differential_route_restores_base_and_keeps_prior_success() {
    for certify in [false, true] {
        let root = tempdir().unwrap();
        let good = source("differential-good.jsonl");
        let failed = source("differential-failed.jsonl");
        let good_route = SourceRouteIdentity::from_sha256("51".repeat(32)).unwrap();
        let failed_route = SourceRouteIdentity::from_sha256("52".repeat(32)).unwrap();
        let good_base = records(&good);
        let failed_base = records(&failed);
        let routes = vec![
            SourceRouteSnapshot::present(good_route.clone(), vec![good.clone()]).unwrap(),
            SourceRouteSnapshot::present(failed_route.clone(), vec![failed.clone()]).unwrap(),
        ];
        let mut initial = open(root.path());
        stage(&mut initial, &good, 1, &good_base);
        stage(&mut initial, &failed, 1, &failed_base);
        initial.set_present_source_routes(routes.clone()).unwrap();
        initial.commit(|_| true).unwrap();

        let mut writer = open(root.path());
        writer
            .set_source_route_plan(
                BTreeSet::from([good_route.clone(), failed_route.clone()]),
                BTreeSet::new(),
            )
            .unwrap();
        writer.begin_source_route_stage(good_route.clone()).unwrap();
        let mut good_current = good_base.clone();
        good_current.push(document_for_session(&good, "session-1", 4, "appended"));
        stage(&mut writer, &good, 2, &good_current);
        writer.finish_source_route_stage(&good_route).unwrap();

        writer
            .begin_source_route_stage(failed_route.clone())
            .unwrap();
        writer.begin_source(failed.clone()).unwrap();
        // A changed event is physically deleted/added before the terminal fence.
        writer
            .add_core_record(document_for_session(&failed, "session-0", 1, "edited"))
            .unwrap();
        writer.add_core_record(failed_base[1].clone()).unwrap();
        assert!(writer.replacement_memory_used > 0);
        if certify {
            writer.certify_source(certificate(&failed, 2, 2)).unwrap();
            assert_eq!(writer.replacement_memory_used, 0);
        }
        assert_eq!(
            writer.replacement_work.event_deletions,
            if certify { 8 } else { 1 }
        );
        writer.rollback_source_route_stage(&failed_route).unwrap();
        assert_eq!(writer.replacement_memory_used, 0);
        assert!(!writer.pending.contains_key(&source_token(&failed)));
        assert!(writer.changed_sessions.keys().all(|id| good_current
            .iter()
            .any(|record| record.session_id.as_uuid() == *id)));
        assert!(writer
            .carry_failed_source_route_from_base(&failed_route)
            .unwrap());
        writer
            .set_present_source_routes(vec![routes[0].clone()])
            .unwrap();
        writer.commit(|_| true).unwrap();
        assert_records(root.path(), &[good_current, failed_base].concat());
    }
}

#[test]
fn differential_cohort_rollback_restores_both_certified_and_unfinished_sources() {
    let root = tempdir().unwrap();
    let first = source("differential-cohort-first.jsonl");
    let second = source("differential-cohort-second.jsonl");
    let first_route = SourceRouteIdentity::from_sha256("61".repeat(32)).unwrap();
    let second_route = SourceRouteIdentity::from_sha256("62".repeat(32)).unwrap();
    let first_base = records(&first);
    let second_base = records(&second);
    let routes = vec![
        SourceRouteSnapshot::present(first_route.clone(), vec![first.clone()]).unwrap(),
        SourceRouteSnapshot::present(second_route.clone(), vec![second.clone()]).unwrap(),
    ];
    let mut initial = open(root.path());
    stage(&mut initial, &first, 1, &first_base);
    stage(&mut initial, &second, 1, &second_base);
    initial.set_present_source_routes(routes).unwrap();
    let previous = initial.commit(|_| true).unwrap().generation_id;

    let mut writer = open(root.path());
    writer
        .set_source_route_plan(
            BTreeSet::from([first_route.clone(), second_route.clone()]),
            BTreeSet::new(),
        )
        .unwrap();
    writer
        .begin_source_route_cohort_stage(first_route.clone())
        .unwrap();
    writer
        .begin_source_route_stage(first_route.clone())
        .unwrap();
    stage(&mut writer, &first, 2, &first_base[1..]);
    writer.finish_source_route_stage(&first_route).unwrap();
    writer
        .begin_source_route_stage(second_route.clone())
        .unwrap();
    writer.begin_source(second.clone()).unwrap();
    writer
        .add_core_record(document_for_session(&second, "session-0", 1, "edited"))
        .unwrap();
    assert!(writer.replacement_memory_used > 0);
    writer.rollback_source_route_stage(&second_route).unwrap();
    writer.rollback_source_route_cohort_stage().unwrap();
    assert_eq!(writer.replacement_memory_used, 0);
    assert!(writer.pending.is_empty());
    assert!(writer.changed_sessions.is_empty());
    assert!(writer
        .carry_failed_source_route_from_base(&first_route)
        .unwrap());
    assert!(writer
        .carry_failed_source_route_from_base(&second_route)
        .unwrap());
    writer.set_present_source_routes(Vec::new()).unwrap();
    let receipt = writer.commit(|_| true).unwrap();
    assert_eq!(receipt.generation_id, previous);
    assert_records(root.path(), &[first_base, second_base].concat());
}

#[test]
fn failed_terminal_revalidation_does_not_publish_retained_or_changed_documents() {
    let root = tempdir().unwrap();
    let source = source("differential-terminal.jsonl");
    let base = records(&source);
    seed(root.path(), &source, &base);
    let previous = VerifiedIndex::open(root.path())
        .unwrap()
        .generation_id()
        .to_owned();
    let mut writer = open(root.path());
    let mut current = base.clone();
    current[4] = document_for_session(&source, "session-1", 2, "edited");
    stage(&mut writer, &source, 2, &current);
    assert_eq!(writer.replacement_work.retained_documents, 8);
    assert!(matches!(
        writer.commit(|_| false),
        Err(IndexError::SourceInvalidated(_))
    ));
    assert_eq!(
        VerifiedIndex::open(root.path()).unwrap().generation_id(),
        previous
    );
    assert_records(root.path(), &base);
}
