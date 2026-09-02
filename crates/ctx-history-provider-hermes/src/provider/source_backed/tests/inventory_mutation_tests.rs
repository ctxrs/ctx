use std::{cell::Cell, collections::BTreeMap};

use super::*;

type TestLifecycle = crate::registration::tests::NoopLifecycle;

const SESSION_COUNT: usize = 129;

#[derive(Debug, Clone, PartialEq, Eq)]
struct ObservedSession {
    fingerprint: [u8; 32],
    certificate: CertifiedSource,
    events: Vec<CoreRecord>,
}

struct TestReconciliationContext<'a> {
    demand: SourceBackedReconciliationDemand,
    route_control: Option<&'a [u8]>,
    base: &'a BTreeMap<String, ObservedSession>,
    base_source_visits: Cell<u64>,
}

impl HermesReconciliationContext<TestLifecycle> for TestReconciliationContext<'_> {
    fn reconciliation_demand(&self) -> SourceBackedReconciliationDemand {
        self.demand
    }

    fn route_control(&self) -> Option<&[u8]> {
        self.route_control
    }

    fn exact_base_source(&self, source: &SourceKey) -> Option<DocumentAppendBase<TestLifecycle>> {
        self.base_source_visits
            .set(self.base_source_visits.get().saturating_add(1));
        self.base
            .values()
            .find(|base| {
                base.certificate
                    .observation()
                    .source()
                    .exact_descriptor_eq(source)
            })
            .map(|base| DocumentAppendBase::Certificate(Box::new(base.certificate.clone())))
    }

    fn report_progress(
        &mut self,
        _progress: SourceBackedCurrentSourceProgress,
    ) -> SourceBackedRouteResult<()> {
        Ok(())
    }
}

fn create_many_session_fixture(path: &Path) {
    fs::create_dir_all(path.parent().expect("Hermes fixture parent")).unwrap();
    let mut connection = Connection::open(path).unwrap();
    connection
        .execute_batch(
            "create table sessions (
                 id text primary key,
                 source text not null,
                 parent_session_id text,
                 started_at real not null,
                 message_count integer default 0
             );
             create table messages (
                 id integer primary key,
                 session_id text not null,
                 role text not null,
                 content text,
                 timestamp real not null
             );",
        )
        .unwrap();
    let transaction = connection.transaction().unwrap();
    {
        let mut insert_session = transaction
            .prepare(
                "insert into sessions (id, source, started_at, message_count)
                 values (?1, 'acp', ?2, 1)",
            )
            .unwrap();
        let mut insert_message = transaction
            .prepare(
                "insert into messages (id, session_id, role, content, timestamp)
                 values (?1, ?2, 'assistant', ?3, ?4)",
            )
            .unwrap();
        for index in 0..SESSION_COUNT {
            let session = format!("session-{index:04}");
            insert_session
                .execute((&session, 1_782_259_200_f64 + index as f64))
                .unwrap();
            insert_message
                .execute((
                    i64::try_from(index + 1).unwrap(),
                    &session,
                    format!("body {index:04}"),
                    1_782_260_000_f64 + index as f64,
                ))
                .unwrap();
        }
    }
    transaction.commit().unwrap();
}

fn reset_work_counters() {
    reset_logical_row_traversals();
    crate::provider::sqlite::reset_exact_message_query_counters();
}

fn extend_events(records: Vec<HermesSourceBackedRecord>, events: &mut Vec<CoreRecord>) {
    events.extend(records.into_iter().filter_map(|record| match record {
        HermesSourceBackedRecord::Event(event) => Some(event),
        HermesSourceBackedRecord::Session(_) | HermesSourceBackedRecord::Rejected(_) => None,
    }));
}

fn observe_exact(
    candidate: &HermesSourceCandidate,
    snapshot: &SqliteSourceReadSnapshot,
    base: &BTreeMap<String, ObservedSession>,
) -> (BTreeMap<String, ObservedSession>, Vec<u8>) {
    let mut context = TestReconciliationContext {
        demand: SourceBackedReconciliationDemand::Exhaustive,
        route_control: None,
        base,
        base_source_visits: Cell::new(0),
    };
    let mut inventory = observe_hermes_reconciliation_inventory(
        candidate,
        snapshot.connection().unwrap(),
        None,
        SourceBackedReconciliationDemand::Exhaustive,
        HermesPhysicalSourceRevision {
            database_identity: *snapshot.evidence().identity(),
            physical_revision: *snapshot.evidence().physical_revision(),
        },
        1_000,
        &mut context,
    )
    .unwrap();
    let route_control =
        serde_json::to_vec(inventory.publication_receipt.as_ref().unwrap()).unwrap();
    let mut observed = BTreeMap::new();
    for leaf in &inventory.leaves {
        let session = &leaf.provider_leaf.provider_session_id;
        let fingerprint = leaf.fingerprint.as_bytes();
        if let Some(base) = base
            .get(session)
            .filter(|base| base.fingerprint == fingerprint)
        {
            observed.insert(session.clone(), base.clone());
            continue;
        }
        let mut events = Vec::new();
        let projection = project_hermes_session_snapshot(
            candidate,
            &leaf.provider_leaf,
            &inventory.schema,
            snapshot.connection().unwrap(),
            inventory.message_spool.as_mut().unwrap(),
            &mut |page| {
                extend_events(page.records, &mut events);
                Ok(())
            },
        )
        .unwrap();
        observed.insert(
            session.clone(),
            ObservedSession {
                fingerprint,
                certificate: projection.certificate,
                events,
            },
        );
    }
    (observed, route_control)
}

fn exact_snapshot(
    data_root: &Path,
    candidate: &HermesSourceCandidate,
    base: &BTreeMap<String, ObservedSession>,
) -> (BTreeMap<String, ObservedSession>, Vec<u8>) {
    let (_authority, snapshot) = open_root_authorized_snapshot(data_root, &candidate.path).unwrap();
    let observed = observe_exact(candidate, &snapshot, base);
    snapshot.finish().unwrap();
    observed
}

fn event_containing<'a>(observed: &'a ObservedSession, needle: &str) -> &'a CoreRecord {
    observed
        .events
        .iter()
        .find(|event| {
            event
                .content
                .normalized_body
                .as_deref()
                .is_some_and(|body| body.contains(needle))
        })
        .expect("Hermes projected event")
}

#[test]
fn exact_refresh_retains_unchanged_sessions_and_bounds_changed_body_work() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let data_root = temp.path().join("data-root");
    let database = temp.path().join("source/state.db");
    create_many_session_fixture(&database);
    let candidate = candidate(&data_root, &database);

    reset_work_counters();
    let (cold, _) = exact_snapshot(&data_root, &candidate, &BTreeMap::new());
    assert_eq!(cold.len(), SESSION_COUNT);
    assert_eq!(logical_row_traversals(), SESSION_COUNT as u64);
    assert_eq!(inventory_observation_rows(), (SESSION_COUNT * 2) as u64);
    assert_eq!(
        crate::provider::sqlite::exact_message_query_counters(),
        (1, 0)
    );
    assert_eq!(
        crate::provider::sqlite::exact_message_spool_counters(),
        (1, 0, 1, SESSION_COUNT as u64, SESSION_COUNT as u64)
    );

    Connection::open(&database)
        .unwrap()
        .execute(
            "update messages
             set content = 'one changed body', timestamp = 1782269999.0
             where id = 65",
            [],
        )
        .unwrap();
    reset_work_counters();
    let (exact, _) = exact_snapshot(&data_root, &candidate, &cold);

    assert_eq!(exact.len(), SESSION_COUNT);
    assert_eq!(
        exact
            .iter()
            .filter(|(session, current)| cold.get(*session).is_some_and(|base| base == *current))
            .count(),
        SESSION_COUNT - 1
    );
    assert_ne!(exact["session-0064"], cold["session-0064"]);
    assert_eq!(inventory_observation_rows(), (SESSION_COUNT * 2) as u64);
    assert_eq!(logical_row_traversals(), 1);
    assert_eq!(
        crate::provider::sqlite::exact_message_query_counters(),
        (1, 0)
    );
    assert_eq!(
        crate::provider::sqlite::exact_message_spool_counters(),
        (1, 0, 1, 1, 1)
    );
    assert_eq!(
        session_scan_receipts().keys().collect::<Vec<_>>(),
        vec!["session-0064"]
    );
    let (parsed_rows, body_queries) = session_scan_receipts()["session-0064"];
    assert_eq!(parsed_rows, 2);
    assert!(body_queries > 0);
}

#[test]
fn incremental_inventory_touches_only_the_appended_session() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let data_root = temp.path().join("data-root");
    let database = temp.path().join("source/state.db");
    create_many_session_fixture(&database);
    let candidate = candidate(&data_root, &database);
    let (cold, route_control) = exact_snapshot(&data_root, &candidate, &BTreeMap::new());

    Connection::open(&database)
        .unwrap()
        .execute(
            "insert into messages (id, session_id, role, content, timestamp)
             values (?1, 'session-0128', 'assistant', 'incremental append', 1782269999.0)",
            [i64::try_from(SESSION_COUNT + 1).unwrap()],
        )
        .unwrap();
    reset_work_counters();
    let (_authority, snapshot) = open_root_authorized_snapshot(&data_root, &database).unwrap();
    let mut context = TestReconciliationContext {
        demand: SourceBackedReconciliationDemand::Incremental,
        route_control: Some(&route_control),
        base: &cold,
        base_source_visits: Cell::new(0),
    };
    let inventory = observe_hermes_reconciliation_inventory(
        &candidate,
        snapshot.connection().unwrap(),
        Some(&route_control),
        SourceBackedReconciliationDemand::Incremental,
        HermesPhysicalSourceRevision {
            database_identity: *snapshot.evidence().identity(),
            physical_revision: *snapshot.evidence().physical_revision(),
        },
        2_000,
        &mut context,
    )
    .unwrap();

    assert_eq!(
        inventory.reconciliation_demand,
        SourceBackedReconciliationDemand::Incremental
    );
    assert_eq!(inventory.leaves.len(), 1);
    assert_eq!(context.base_source_visits.get(), 1);
    let leaf = &inventory.leaves[0].provider_leaf;
    assert_eq!(leaf.provider_session_id, "session-0128");
    let mut events = Vec::new();
    let projection = project_hermes_incremental_leaf_with_progress(
        &candidate,
        leaf,
        leaf.incremental.as_ref().unwrap(),
        &mut |output| {
            if let HermesSnapshotProjectionOutput::Page(page) = output {
                extend_events(page.records, &mut events);
            }
            Ok(())
        },
    )
    .unwrap();
    assert_eq!(projection.certificate.counts().complete_records, 3);
    assert_eq!(
        events
            .iter()
            .filter_map(|event| event.content.normalized_body.as_deref())
            .collect::<Vec<_>>(),
        vec!["incremental append"]
    );
    assert_eq!(inventory_observation_rows(), 1);
    assert_eq!(logical_row_traversals(), 1);
    assert_eq!(
        session_scan_receipts().keys().collect::<Vec<_>>(),
        vec!["session-0128"]
    );
    snapshot.finish().unwrap();
}

#[test]
fn exact_deletion_removes_one_source_and_retains_every_peer() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let data_root = temp.path().join("data-root");
    let database = temp.path().join("source/state.db");
    create_many_session_fixture(&database);
    let candidate = candidate(&data_root, &database);
    let (cold, _) = exact_snapshot(&data_root, &candidate, &BTreeMap::new());

    Connection::open(&database)
        .unwrap()
        .execute_batch(
            "delete from messages where session_id = 'session-0064';
             delete from sessions where id = 'session-0064';",
        )
        .unwrap();
    reset_work_counters();
    let (deleted, _) = exact_snapshot(&data_root, &candidate, &cold);

    assert_eq!(deleted.len(), SESSION_COUNT - 1);
    assert!(!deleted.contains_key("session-0064"));
    assert!(deleted
        .iter()
        .all(|(session, current)| cold.get(session) == Some(current)));
    assert_eq!(
        inventory_observation_rows(),
        ((SESSION_COUNT - 1) * 2) as u64
    );
    assert_eq!(logical_row_traversals(), 0);
    assert_eq!(
        crate::provider::sqlite::exact_message_query_counters(),
        (1, 0)
    );
    assert_eq!(
        crate::provider::sqlite::exact_message_spool_counters(),
        (1, 0, 0, 0, 0)
    );
    assert!(session_scan_receipts().is_empty());
}

#[test]
fn unsupported_schema_is_rejected_during_provider_inventory() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let data_root = temp.path().join("data-root");
    let database = temp.path().join("source/state.db");
    fs::create_dir_all(database.parent().unwrap()).unwrap();
    Connection::open(&database)
        .unwrap()
        .execute_batch("create table unsupported(value text)")
        .unwrap();
    let candidate = candidate(&data_root, &database);
    let (_authority, snapshot) = open_root_authorized_snapshot(&data_root, &database).unwrap();

    let result = observe_hermes_session_inventory::<TestLifecycle>(
        &candidate,
        snapshot.connection().unwrap(),
        &mut |_| Ok(()),
    );
    let error = match result {
        Ok(_) => panic!("unsupported Hermes schema was accepted"),
        Err(error) => error,
    };
    assert!(error
        .to_string()
        .contains("missing required sessions table"));
    snapshot.abort().unwrap();
}

#[test]
fn parent_mutation_keeps_related_child_session_byte_identical() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let data_root = temp.path().join("data-root");
    let database = temp.path().join("source/state.db");
    create_fixture(&database);
    let candidate = candidate(&data_root, &database);
    let (cold, _) = exact_snapshot(&data_root, &candidate, &BTreeMap::new());
    let parent = event_containing(&cold[PARENT], "parent stable needle");
    let child = event_containing(&cold[CHILD], "child stable needle");
    assert_eq!(child.parent_session_id, Some(parent.session_id));
    assert_eq!(child.root_session_id, None);
    assert_eq!(
        child.session_relationship,
        Some(ctx_history_core::ProviderNativeSessionRelationship::Delegated)
    );

    Connection::open(&database)
        .unwrap()
        .execute_batch(
            "insert into messages (id, session_id, role, content, timestamp)
                 values (11, 'parent-session', 'assistant', 'parent appended', 1782259204.0);
             update sessions set message_count = 2, ended_at = 1782259204.0
                 where id = 'parent-session';",
        )
        .unwrap();
    let (refreshed, _) = exact_snapshot(&data_root, &candidate, &cold);

    assert_eq!(refreshed[CHILD].certificate, cold[CHILD].certificate);
    assert_eq!(refreshed[CHILD].events, cold[CHILD].events);
    assert_eq!(
        serde_json::to_vec(&refreshed[CHILD].events).unwrap(),
        serde_json::to_vec(&cold[CHILD].events).unwrap()
    );
    assert_ne!(refreshed[PARENT], cold[PARENT]);
}

#[test]
fn profile_removal_cannot_claim_sibling_inventory_or_control() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let data_root = temp.path().join("data-root");
    let first_database = temp.path().join("first/state.db");
    let second_database = temp.path().join("second/state.db");
    create_fixture(&first_database);
    create_fixture(&second_database);
    Connection::open(&second_database)
        .unwrap()
        .execute(
            "update messages set content = 'sibling profile needle' where id = 10",
            [],
        )
        .unwrap();
    let first_candidate = automatic_candidate(&data_root, &first_database);
    let second_candidate = automatic_candidate(&data_root, &second_database);
    let (first_cold, _) = exact_snapshot(&data_root, &first_candidate, &BTreeMap::new());
    let (second_cold, second_control) =
        exact_snapshot(&data_root, &second_candidate, &BTreeMap::new());

    Connection::open(&first_database)
        .unwrap()
        .execute_batch(
            "delete from messages where session_id = 'parent-session';
             delete from sessions where id = 'parent-session';",
        )
        .unwrap();
    let (first_refreshed, _) = exact_snapshot(&data_root, &first_candidate, &first_cold);
    let (second_refreshed, refreshed_control) =
        exact_snapshot(&data_root, &second_candidate, &second_cold);

    assert!(!first_refreshed.contains_key(PARENT));
    assert!(first_refreshed.contains_key(CHILD));
    assert_eq!(second_refreshed, second_cold);
    assert_eq!(refreshed_control, second_control);
    assert_eq!(
        event_containing(&second_refreshed[PARENT], "sibling profile needle"),
        event_containing(&second_cold[PARENT], "sibling profile needle")
    );
}
