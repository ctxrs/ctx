use super::*;

#[test]
fn retained_generation_hint_avoids_reopening_the_large_index_when_enqueuing() {
    let temp = tempfile::tempdir().unwrap();
    let data_root = temp.path().join("data");
    let index_root = source_backed_index_root(&data_root);
    fs::create_dir_all(&index_root).unwrap();
    fs::write(
        index_root.join("meta.json"),
        br#"{"payload":"{\"version\":1,\"generation_id\":\"retained-generation\"}"}"#,
    )
    .unwrap();
    write_daemon_job_status(
        &daemon_source_backed_refresh_job_path(&data_root),
        &json!({
            "request_state": "running",
            "published_generation": "retained-generation",
        }),
    )
    .unwrap();

    let coordinator = CoreRefreshEngine::new();
    let response = coordinator.enqueue_periodic(&data_root).unwrap();

    assert_eq!(
        response["previous_generation"],
        Value::String("retained-generation".to_owned())
    );
    assert_eq!(
        response["published_generation"],
        Value::String("retained-generation".to_owned())
    );
}

#[test]
fn retained_generation_hint_recovers_commit_before_stale_job_receipt() {
    let temp = tempfile::tempdir().unwrap();
    let data_root = temp.path().join("data");
    let index_root = source_backed_index_root(&data_root);
    fs::create_dir_all(&index_root).unwrap();
    fs::write(
        index_root.join("meta.json"),
        br#"{"payload":"{\"version\":1,\"generation_id\":\"committed-generation\"}"}"#,
    )
    .unwrap();
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
        Value::String("committed-generation".to_owned())
    );
    assert_eq!(
        response["published_generation"],
        Value::String("committed-generation".to_owned())
    );
}
