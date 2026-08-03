use super::*;

fn import_explicit_custom_source(temp: &TempDir, path: &Path) -> Value {
    json_output(ctx(temp).args([
        "import",
        "--input-format",
        "ctx-history-jsonl-v1",
        "--path",
        path.to_str().unwrap(),
        "--no-daemon",
        "--format=json",
        "--progress",
        "none",
    ]))
}

#[test]
fn explicit_custom_relocation_preserves_route_source_session_and_event_identity() {
    let temp = tempdir();
    let _daemon = start_full_source_refresh_daemon(&temp);
    let old_path = temp.path().join("relocation-old.jsonl");
    let new_path = temp.path().join("relocation-new.jsonl");
    write_valid_explicit_custom_source(&old_path, "certified explicit relocation oracle");

    let first = import_explicit_custom_source(&temp, &old_path);
    assert_eq!(first["outcome"], "success", "{first:#}");
    let first_source = &first["sources"][0];
    let first_generation = published_generation(&first);
    let first_route = first_source["route_identity"].clone();
    let first_lineage = first_source["catalog_lineage"].clone();
    let first_records = provider_core_records(&data_root(&temp), "custom");
    assert_eq!(first_records.len(), 1);
    let first_record = &first_records[0];
    let first_source_identity = first_record.source.clone();
    let first_session_identity = first_record.session_id;
    let first_event_identity = first_record.event_id;

    fs::rename(&old_path, &new_path).unwrap();
    let relocated = json_output(ctx(&temp).args([
        "import",
        "--input-format",
        "ctx-history-jsonl-v1",
        "--relocate-from",
        old_path.to_str().unwrap(),
        "--path",
        new_path.to_str().unwrap(),
        "--no-daemon",
        "--format=json",
        "--progress",
        "none",
    ]));
    assert_eq!(relocated["outcome"], "success", "{relocated:#}");
    let relocated_source = &relocated["sources"][0];
    assert_eq!(
        relocated_source["route_identity"], first_route,
        "{relocated:#}"
    );
    assert_eq!(
        relocated_source["catalog_lineage"], first_lineage,
        "{relocated:#}"
    );
    assert_eq!(
        relocated_source["request_overlay"]["entries"][0]["route_identity"], first_route,
        "{relocated:#}"
    );
    assert_eq!(
        relocated_source["path"],
        new_path.display().to_string(),
        "{relocated:#}"
    );
    assert_ne!(published_generation(&relocated), first_generation);

    let relocated_records = provider_core_records(&data_root(&temp), "custom");
    assert_eq!(relocated_records.len(), 1);
    assert_eq!(relocated_records[0].source, first_source_identity);
    assert_eq!(relocated_records[0].session_id, first_session_identity);
    assert_eq!(relocated_records[0].event_id, first_event_identity);
}

#[test]
fn failed_explicit_relocation_keeps_the_old_generation_authoritative() {
    let temp = tempdir();
    let _daemon = start_full_source_refresh_daemon(&temp);
    let old_path = temp.path().join("relocation-active.jsonl");
    let new_path = temp.path().join("relocation-uncertified.jsonl");
    write_valid_explicit_custom_source(&old_path, "active relocation authority oracle");
    write_valid_explicit_custom_source(&new_path, "uncertified relocation candidate");

    let first = import_explicit_custom_source(&temp, &old_path);
    let first_generation = published_generation(&first);
    let first_records = provider_core_records(&data_root(&temp), "custom");
    let stderr = failure_stderr(ctx(&temp).args([
        "import",
        "--input-format",
        "ctx-history-jsonl-v1",
        "--relocate-from",
        old_path.to_str().unwrap(),
        "--path",
        new_path.to_str().unwrap(),
        "--no-daemon",
        "--format=json",
        "--progress",
        "none",
    ]));
    assert!(
        stderr.contains("old exact source is still available"),
        "{stderr}"
    );

    let status = json_output(ctx(&temp).args(["status", "--format=json"]));
    assert_eq!(status["lexical"]["generation_id"], first_generation);
    assert_eq!(
        provider_core_records(&data_root(&temp), "custom"),
        first_records
    );
}
