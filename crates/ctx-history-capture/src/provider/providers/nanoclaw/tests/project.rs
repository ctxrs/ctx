use std::time::SystemTime;

use super::super::project::{NanoClawProjectSnapshot, NanoClawSqliteSnapshot};
use super::super::{
    position::{nanoclaw_message_locator, NanoClawMessageSource},
    NanoClawCompleteProject,
};
use super::*;
use crate::complete_content::sqlite::{
    CompleteContentSqliteBoundError, CompleteContentSqliteQueryBudget,
};

#[test]
fn project_snapshot_freezes_constituent_identities_in_row_and_direction_order() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let root = create_project(&temp, "inventory-order", 2);
    create_message_stores(&root, "session-0001");
    create_message_stores(&root, "session-0000");
    let central_path = root.join("data").join("v2.db");

    let snapshot = NanoClawProjectSnapshot::read(&root, &central_path).unwrap();
    assert_eq!(
        snapshot.database_paths(),
        vec![
            root.join("data/v2-sessions/ag-1/session-0000/inbound.db"),
            root.join("data/v2-sessions/ag-1/session-0000/outbound.db"),
            root.join("data/v2-sessions/ag-1/session-0001/inbound.db"),
            root.join("data/v2-sessions/ag-1/session-0001/outbound.db"),
        ]
    );
    assert!(snapshot.revalidate().unwrap());

    let revision = snapshot.source_revision(7, "schema-oracle");
    assert!(revision.starts_with(
        "nanoclaw-project-snapshot-v1:capture=2;policy=4;user_version=7;schema=schema-oracle;"
    ));
}

fn restore_modified_time(path: &Path, modified: SystemTime) {
    fs::File::options()
        .write(true)
        .open(path)
        .unwrap()
        .set_times(fs::FileTimes::new().set_modified(modified))
        .unwrap();
}

fn update_same_sized_value(path: &Path, component: &str, iteration: usize) {
    let alternate = iteration.is_multiple_of(2);
    let (sql, value) = match component {
        "central" => (
            "update sessions set status = ?1 where id = 'session-0000'",
            if alternate { "paused" } else { "active" },
        ),
        "inbound" => (
            "update messages_in set content = ?1 where id = 'in-1'",
            if alternate { "change" } else { "before" },
        ),
        "outbound" => (
            "update messages_out set content = ?1 where id = 'out-1'",
            if alternate { "change" } else { "before" },
        ),
        _ => unreachable!(),
    };
    assert_eq!(
        Connection::open(path)
            .unwrap()
            .execute(sql, [value])
            .unwrap(),
        1
    );
}

fn assert_rapid_component_mutations_detected(yield_between_steps: bool) {
    const ITERATIONS: usize = 16;

    let temp = crate::test_support_paths::tempdir().unwrap();
    for (name, component) in [
        ("central-mutation", "central"),
        ("inbound-mutation", "inbound"),
        ("outbound-mutation", "outbound"),
    ] {
        let root = create_project(&temp, name, 1);
        let central_path = root.join("data").join("v2.db");
        let (inbound, outbound) = create_message_stores(&root, "session-0000");
        insert_inbound(&inbound, "in-1", 1, 1_000, "before");
        insert_outbound(&outbound, "out-1", 2, 2_000, "before");
        let component_path = match component {
            "central" => &central_path,
            "inbound" => &inbound,
            "outbound" => &outbound,
            _ => unreachable!(),
        };

        for iteration in 0..ITERATIONS {
            let project_before = NanoClawProjectSnapshot::read(&root, &central_path).unwrap();
            let component_before = NanoClawSqliteSnapshot::read(component_path).unwrap();
            let metadata_before = fs::metadata(component_path).unwrap();
            let modified_before = metadata_before.modified().unwrap();

            update_same_sized_value(component_path, component, iteration);
            restore_modified_time(component_path, modified_before);
            if yield_between_steps {
                std::thread::yield_now();
            }

            let metadata_after = fs::metadata(component_path).unwrap();
            assert_eq!(metadata_after.len(), metadata_before.len());
            assert_eq!(metadata_after.modified().unwrap(), modified_before);

            let component_after = NanoClawSqliteSnapshot::read(component_path).unwrap();
            assert_ne!(
                component_after.database_change_token(),
                component_before.database_change_token(),
                "{component} iteration {iteration} must detect a same-page mutation"
            );
            assert!(
                !project_before.revalidate().unwrap(),
                "{component} iteration {iteration} must invalidate the project snapshot"
            );
        }
    }
}

#[test]
fn rapid_same_page_mutations_invalidate_without_scheduler_assistance() {
    assert_rapid_component_mutations_detected(false);
}

#[test]
fn rapid_same_page_mutations_invalidate_with_scheduler_yields() {
    assert_rapid_component_mutations_detected(true);
}

#[test]
fn complete_project_rejects_mutation_after_compound_snapshot_open() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let root = create_project(&temp, "complete-mutation", 1);
    let (inbound, _) = create_message_stores(&root, "session-0000");
    insert_inbound(&inbound, "in-1", 1, 1_000, "before");
    let locator = nanoclaw_message_locator(1, NanoClawMessageSource::Inbound, 1).unwrap();
    let project = NanoClawCompleteProject::open(
        &root,
        std::slice::from_ref(&locator),
        CompleteContentSqliteQueryBudget::new(),
    )
    .unwrap();

    Connection::open(&inbound)
        .unwrap()
        .execute(
            "update messages_in set content = 'after!' where rowid = 1",
            [],
        )
        .unwrap();

    assert!(matches!(
        project.resolve(&locator),
        Err(CompleteContentSqliteBoundError::Capture(
            CaptureError::SourceChangedDuringCapture
        ))
    ));
}

fn add_schema_objects(path: &Path, prefix: &str, count: usize) {
    let ddl = (0..count)
        .map(|index| format!("create table {prefix}_{index:04} (value integer);"))
        .collect::<String>();
    Connection::open(path).unwrap().execute_batch(&ddl).unwrap();
}

fn assert_progress_interrupted(
    result: std::result::Result<NanoClawCompleteProject, CompleteContentSqliteBoundError>,
) {
    match result {
        Err(CompleteContentSqliteBoundError::Capture(CaptureError::Sqlite(
            rusqlite::Error::SqliteFailure(failure, _),
        ))) => assert_eq!(failure.code, rusqlite::ErrorCode::OperationInterrupted),
        _ => panic!("expected deterministic SQLite progress interruption"),
    }
}

#[test]
fn complete_project_applies_schema_cap_to_central_and_selected_component() {
    let temp = crate::test_support_paths::tempdir().unwrap();

    let central_root = create_project(&temp, "oversized-central-schema", 1);
    let (central_inbound, _) = create_message_stores(&central_root, "session-0000");
    insert_inbound(&central_inbound, "in-1", 1, 1_000, "body");
    add_schema_objects(&central_root.join("data/v2.db"), "extra", 1_025);
    let locator = nanoclaw_message_locator(1, NanoClawMessageSource::Inbound, 1).unwrap();
    assert!(matches!(
        NanoClawCompleteProject::open(
            &central_root,
            std::slice::from_ref(&locator),
            CompleteContentSqliteQueryBudget::new(),
        ),
        Err(CompleteContentSqliteBoundError::ContentTooLarge)
    ));

    let component_root = create_project(&temp, "oversized-component-schema", 1);
    let (component_inbound, _) = create_message_stores(&component_root, "session-0000");
    insert_inbound(&component_inbound, "in-1", 1, 1_000, "body");
    add_schema_objects(&component_inbound, "extra", 1_025);
    let project = NanoClawCompleteProject::open(
        &component_root,
        std::slice::from_ref(&locator),
        CompleteContentSqliteQueryBudget::new(),
    )
    .unwrap();
    assert!(matches!(
        project.resolve(&locator),
        Err(CompleteContentSqliteBoundError::ContentTooLarge)
    ));
}

#[test]
fn complete_project_progress_budget_interrupts_central_and_component_without_sleeping() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let root = create_project(&temp, "progress-budget", 1);
    let (inbound, _) = create_message_stores(&root, "session-0000");
    insert_inbound(&inbound, "in-1", 1, 1_000, "body");
    let locator = nanoclaw_message_locator(1, NanoClawMessageSource::Inbound, 1).unwrap();

    assert_progress_interrupted(NanoClawCompleteProject::open(
        &root,
        std::slice::from_ref(&locator),
        CompleteContentSqliteQueryBudget::interrupted_for_test(),
    ));

    let mut project = NanoClawCompleteProject::open(
        &root,
        std::slice::from_ref(&locator),
        CompleteContentSqliteQueryBudget::new(),
    )
    .unwrap();
    project.replace_query_budget_for_test(CompleteContentSqliteQueryBudget::interrupted_for_test());
    match project.resolve(&locator) {
        Err(CompleteContentSqliteBoundError::Capture(CaptureError::Sqlite(
            rusqlite::Error::SqliteFailure(failure, _),
        ))) => assert_eq!(failure.code, rusqlite::ErrorCode::OperationInterrupted),
        _ => panic!("expected component SQLite progress interruption"),
    }
}

#[test]
fn rapid_append_survives_source_reset_and_store_reopen() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let root = create_project(&temp, "append-reopen", 1);
    let (inbound, _) = create_message_stores(&root, "session-0000");
    insert_inbound(&inbound, "in-1", 1, 1_000, "before");
    let store_path = temp.path().join("append-reopen.sqlite");
    let options = || NormalizedProviderImportOptions {
        history_record_id: None,
        persist_cursors: true,
        wrap_transaction: true,
        fast_event_inserts: true,
        capture_work_limit: crate::CaptureWorkLimit::Drain,
        inventory_observation_token: None,
    };
    let mut store = Store::open(&store_path).unwrap();
    let first =
        import_nanoclaw_project_batched(&root, &mut store, context(&root), options()).unwrap();
    assert_eq!(first.failed, 0, "{:?}", first.failures);

    let metadata_before = fs::metadata(&inbound).unwrap();
    let modified_before = metadata_before.modified().unwrap();
    insert_inbound(
        &inbound,
        "in-2",
        2,
        2_000,
        "rapid appended row survives reopen",
    );
    restore_modified_time(&inbound, modified_before);
    let metadata_after = fs::metadata(&inbound).unwrap();
    assert_eq!(metadata_after.len(), metadata_before.len());
    assert_eq!(metadata_after.modified().unwrap(), modified_before);

    let appended =
        import_nanoclaw_project_batched(&root, &mut store, context(&root), options()).unwrap();
    assert_eq!(appended.failed, 0, "{:?}", appended.failures);
    assert!(store
        .search_event_hits("rapid appended row survives reopen", 10)
        .unwrap()
        .iter()
        .any(|hit| hit.provider == Some(CaptureProvider::NanoClaw)));

    drop(store);
    let reopened = Store::open(store_path).unwrap();
    assert!(reopened
        .search_event_hits("rapid appended row survives reopen", 10)
        .unwrap()
        .iter()
        .any(|hit| hit.provider == Some(CaptureProvider::NanoClaw)));
}
