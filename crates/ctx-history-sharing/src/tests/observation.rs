use super::*;
use crate::queue::Pending;

fn scoped_record(session: &str, roots: &[&Path]) -> CoreRecord {
    let mut record = synthetic_record(session, session);
    record.content.activity = Some(CoreActivity {
        revision: CORE_ACTIVITY_REVISION,
        provider_call_id: None,
        invocation: None,
        result: None,
        facts: roots
            .iter()
            .map(|root| ProviderDeclaredFact {
                kind: LiteralFactKind::SessionCwd,
                value: root.to_str().unwrap().into(),
            })
            .collect(),
    });
    record
}

#[test]
fn capture_reports_work_scope_holds_and_backfill_omissions_without_queuing_denied_members() {
    let temp = tempdir().unwrap();
    let data = temp.path().join("data");
    let allowed = temp.path().join("allowed");
    let outside = temp.path().join("outside");
    let old = synthetic_record("omitted historical session", "omitted history body");
    commit_records(&data, std::slice::from_ref(&old), 1);
    let (server, _) = recovery::accepting_remote();
    let store = SharingStore::new(data.join("sharing/team"));
    connect(&store, server.endpoint());
    let mut sources = policy(capture::hex(&old.source.identity().digest())).sources;
    sources[0].whole_source = false;
    sources[0].work_roots = vec![allowed.clone()];
    sources[0].backfill = Backfill::None;
    let policy = store
        .prepare_policy(
            &data,
            &temp.path().join("preview"),
            PublicationMode::Automatic,
            sources,
        )
        .unwrap();
    store.set_policy(policy.clone()).unwrap();
    let selected = scoped_record("allowed session body", &[&allowed.join("nested")]);
    commit_records(
        &data,
        &[
            old,
            synthetic_record("unknown session", "unknown session body"),
            scoped_record("mixed session body", &[&allowed, &outside]),
            scoped_record("outside session body", &[&outside]),
            selected.clone(),
        ],
        2,
    );
    let collector = Collector::new(data.clone(), store.root().to_owned());
    assert_eq!(collector.tick(), TickOutcome::Progress);
    assert!(server.requests().is_empty());
    let status = store.status().unwrap();
    assert_eq!((status.pending, status.held), (1, 0));
    let observed = status.selection.unwrap();
    assert_eq!(
        observed.generation,
        capture::open_index(&data).unwrap().generation_id()
    );
    assert_eq!(observed.policy_revision, policy.revision);
    assert_eq!(
        (observed.selected(), observed.held(), observed.omitted()),
        (1, 3, 1)
    );
    assert_eq!(
        observed.counts,
        BTreeMap::from([
            (SelectionDecision::Selected, 1),
            (SelectionDecision::UnknownWorkRoot, 1),
            (SelectionDecision::OutsideWorkRoots, 2),
            (SelectionDecision::BackfillExcluded, 1),
        ])
    );
    let paths = store.pending_paths().unwrap();
    assert_eq!(paths.len(), 1);
    let pending: Pending = private_file::read(&paths[0].join("pending.json")).unwrap();
    assert_eq!(pending.member.session_id, selected.session_id);
    let json = serde_json::to_value(&observed).unwrap();
    assert_eq!(json["counts"]["unknown_work_root"], 1);
    assert_eq!(json["counts"]["backfill_excluded"], 1);
    assert!(fs::read_dir(store.root()).unwrap().all(|entry| {
        !entry
            .unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with(".capture-")
    }));

    for _ in 0..3 {
        assert_eq!(collector.tick(), TickOutcome::Progress);
    }
    assert_eq!(collector.tick(), TickOutcome::Idle);
    assert_eq!(store.status().unwrap().pending, 0);
    let reopened = SharingStore::new(store.root());
    assert_eq!(reopened.status().unwrap().selection, Some(observed));
    let request_count = server.requests().len();
    let mut widened = policy;
    widened.revision += 1;
    widened.sources[0].whole_source = true;
    widened.sources[0].work_roots.clear();
    store.set_policy(widened).unwrap();
    assert!(store.status().unwrap().selection.is_none()); // No stale old-policy counts.
    assert_eq!(collector.tick(), TickOutcome::Progress);
    let status = store.status().unwrap();
    let observed = status.selection.unwrap();
    assert_eq!(
        (observed.selected(), observed.held(), observed.omitted()),
        (4, 0, 1)
    );
    assert_eq!(status.pending, 3);
    assert_eq!(server.requests().len(), request_count);
}

#[test]
fn wholly_held_observation_survives_restart_and_distinguishes_review_from_intentional_omission() {
    let temp = tempdir().unwrap();
    let data = temp.path().join("data");
    let record = seed(&data);
    let server = Mock::new(|_| panic!("denied selection sent history"));
    let store = SharingStore::new(data.join("sharing/team"));
    connect(&store, server.endpoint());
    let mut policy = policy(capture::hex(&record.source.identity().digest()));
    policy.sources[0].whole_source = false;
    policy.sources[0].work_roots = vec![temp.path().join("allowed")];
    store.set_policy(policy.clone()).unwrap();
    let collector = Collector::new(data.clone(), store.root().to_owned());
    assert_eq!(collector.tick(), TickOutcome::Idle);
    let status = store.status().unwrap();
    assert_eq!((status.pending, status.held), (0, 0));
    let observed = status.selection.unwrap();
    assert_eq!(observed.held(), 1);
    assert_eq!(observed.counts[&SelectionDecision::UnknownWorkRoot], 1);
    assert!(!store.root().join("queue").exists());
    let restarted = Collector::new(data, store.root().to_owned());
    assert_eq!(restarted.tick(), TickOutcome::Idle);
    assert_eq!(
        SharingStore::new(store.root()).status().unwrap().selection,
        Some(observed)
    );

    policy.revision += 1;
    policy.sources[0].whole_source = true;
    policy.sources[0].work_roots.clear();
    policy.mode = PublicationMode::Reviewed {
        revisions: BTreeSet::new(),
    };
    store.set_policy(policy.clone()).unwrap();
    assert!(store.status().unwrap().selection.is_none());
    assert_eq!(restarted.tick(), TickOutcome::Idle);
    let observed = store.status().unwrap().selection.unwrap();
    assert_eq!((observed.held(), observed.omitted()), (1, 0));
    assert_eq!(observed.counts[&SelectionDecision::NeedsReview], 1);

    policy.revision += 1;
    policy.sources[0].whole_source = false;
    policy.sources[0].work_roots = vec![temp.path().join("allowed")];
    policy.sources[0].backfill = Backfill::None;
    policy.sources[0]
        .baseline_revisions
        .insert(capture::hex(&record.session_id.digest()), "original".into());
    store.set_policy(policy.clone()).unwrap();
    assert_eq!(restarted.tick(), TickOutcome::Idle);
    let observed = store.status().unwrap().selection.unwrap();
    assert_eq!((observed.held(), observed.omitted()), (0, 1));
    assert_eq!(observed.counts[&SelectionDecision::BackfillExcluded], 1);

    policy.revision += 1;
    policy.sources[0].baseline_revisions.clear();
    policy.sources[0].include_future = false;
    store.set_policy(policy).unwrap();
    assert_eq!(restarted.tick(), TickOutcome::Idle);
    let observed = store.status().unwrap().selection.unwrap();
    assert_eq!((observed.held(), observed.omitted()), (0, 1));
    assert_eq!(observed.counts[&SelectionDecision::FutureExcluded], 1);
    assert!(!store.root().join("queue").exists());
    assert!(server.requests().is_empty());
}
