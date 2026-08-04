use super::*;

#[test]
fn retained_generation_hint_seeds_enqueued_generation_identity() {
    let temp = tempfile::tempdir().unwrap();
    let data_root = temp.path().join("data");
    let index_root = source_backed_index_root(&data_root);
    let generation_id = ctx_history_index::GenerationWriter::open(
        &index_root,
        ctx_history_index::WriterOptions::default(),
    )
    .unwrap()
    .into_writer()
    .unwrap()
    .commit(|_| true)
    .unwrap()
    .generation_id;
    write_daemon_job_status(
        &daemon_source_backed_refresh_job_path(&data_root),
        &json!({
            "request_state": "running",
            "published_generation": generation_id,
        }),
    )
    .unwrap();

    let coordinator = CoreRefreshEngine::new();
    let response = coordinator.enqueue_periodic(&data_root).unwrap();

    assert_eq!(
        response["previous_generation"],
        Value::String(generation_id.clone())
    );
    assert_eq!(
        response["published_generation"],
        Value::String(generation_id)
    );
}

#[test]
fn retained_generation_hint_recovers_commit_before_stale_job_receipt() {
    let temp = tempfile::tempdir().unwrap();
    let data_root = temp.path().join("data");
    let index_root = source_backed_index_root(&data_root);
    let generation_id = ctx_history_index::GenerationWriter::open(
        &index_root,
        ctx_history_index::WriterOptions::default(),
    )
    .unwrap()
    .into_writer()
    .unwrap()
    .commit(|_| true)
    .unwrap()
    .generation_id;
    write_daemon_job_status(
        &daemon_source_backed_refresh_job_path(&data_root),
        &json!({
            "request_state": "running",
            "published_generation": "stale-prior-generation",
        }),
    )
    .unwrap();

    let coordinator = CoreRefreshEngine::new();
    let response = coordinator.enqueue_periodic(&data_root).unwrap();

    assert_eq!(
        response["previous_generation"],
        Value::String(generation_id.clone())
    );
    assert_eq!(
        response["published_generation"],
        Value::String(generation_id)
    );
}
