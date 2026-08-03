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

    write_codex_setup_session(&temp);
    let automatic =
        json_output(ctx(&temp).args(["import", "--all", "--format=json", "--progress", "none"]));
    assert_eq!(automatic["outcome"], "success", "{automatic:#}");
    let automatic_generation = automatic["sources"][0]["published_generation"]
        .as_str()
        .expect("automatic import generation");
    assert_ne!(automatic_generation, first_generation);
    let retained =
        ctx_history_index::VerifiedIndex::open(data_root(&temp).join("search").join("lexical"))
            .unwrap();
    assert!(retained
        .manifest()
        .source_routes()
        .iter()
        .any(|route| { route.route_identity().as_str() == first_route.as_str().unwrap() }));
    let retained_records = provider_core_records(&data_root(&temp), "custom");
    assert_eq!(retained_records.len(), 1);
    assert_eq!(retained_records[0].source, first_source_identity);
    assert_eq!(retained_records[0].session_id, first_session_identity);
    assert_eq!(retained_records[0].event_id, first_event_identity);

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
fn failed_explicit_replacement_preserves_retained_relocation_witness() {
    let temp = tempdir();
    let _daemon = start_full_source_refresh_daemon(&temp);
    let old_path = temp.path().join("failed-replacement-old.jsonl");
    let failed_path = temp.path().join("failed-replacement-empty.jsonl");
    let moved_path = temp.path().join("failed-replacement-moved.jsonl");
    write_valid_explicit_custom_source(&old_path, "retained after failed replacement oracle");

    let first = import_explicit_custom_source(&temp, &old_path);
    assert_eq!(first["outcome"], "success", "{first:#}");
    let first_generation = published_generation(&first);
    let first_route = first["sources"][0]["route_identity"].clone();
    let first_lineage = first["sources"][0]["catalog_lineage"].clone();
    let first_records = provider_core_records(&data_root(&temp), "custom");
    assert_eq!(first_records.len(), 1);
    let first_source_identity = first_records[0].source.clone();
    let first_session_identity = first_records[0].session_id;
    let first_event_identity = first_records[0].event_id;

    write_codex_setup_session(&temp);
    fs::write(&failed_path, b"").unwrap();
    let failed = import_explicit_custom_source(&temp, &failed_path);
    assert_eq!(
        failed["outcome"], "completed_with_source_failures",
        "{failed:#}"
    );
    assert!(
        failed["sources"][0]["successful_routes"]
            .as_u64()
            .is_some_and(|routes| routes > 0),
        "{failed:#}"
    );
    assert_eq!(failed["sources"][0]["carried_forward"], false, "{failed:#}");
    assert_ne!(published_generation(&failed), first_generation);

    let retained =
        ctx_history_index::VerifiedIndex::open(data_root(&temp).join("search").join("lexical"))
            .unwrap();
    assert!(retained
        .manifest()
        .source_routes()
        .iter()
        .any(|route| route.route_identity().as_str() == first_route.as_str().unwrap()));
    let retained_records = provider_core_records(&data_root(&temp), "custom");
    assert_eq!(retained_records.len(), 1);
    assert_eq!(retained_records[0].source, first_source_identity);
    assert_eq!(retained_records[0].session_id, first_session_identity);
    assert_eq!(retained_records[0].event_id, first_event_identity);

    fs::rename(&old_path, &moved_path).unwrap();
    let relocated = json_output(ctx(&temp).args([
        "import",
        "--input-format",
        "ctx-history-jsonl-v1",
        "--relocate-from",
        old_path.to_str().unwrap(),
        "--path",
        moved_path.to_str().unwrap(),
        "--no-daemon",
        "--format=json",
        "--progress",
        "none",
    ]));
    assert_eq!(relocated["outcome"], "success", "{relocated:#}");
    assert_eq!(relocated["sources"][0]["route_identity"], first_route);
    assert_eq!(relocated["sources"][0]["catalog_lineage"], first_lineage);
    assert_eq!(
        relocated["sources"][0]["path"],
        moved_path.display().to_string()
    );

    let relocated_records = provider_core_records(&data_root(&temp), "custom");
    assert_eq!(relocated_records.len(), 1);
    assert_eq!(relocated_records[0].source, first_source_identity);
    assert_eq!(relocated_records[0].session_id, first_session_identity);
    assert_eq!(relocated_records[0].event_id, first_event_identity);
}

#[test]
fn replaced_explicit_custom_route_cannot_reuse_a_stale_relocation_witness() {
    let temp = tempdir();
    let daemon = start_full_source_refresh_daemon(&temp);
    let first_path = temp.path().join("replaced-first.jsonl");
    let replacement_path = temp.path().join("replacement.jsonl");
    let moved_first_path = temp.path().join("replaced-first-moved.jsonl");
    write_valid_explicit_custom_source(&first_path, "first explicit authority oracle");
    write_valid_explicit_custom_source(&replacement_path, "replacement authority oracle");

    let first = import_explicit_custom_source(&temp, &first_path);
    assert_eq!(first["outcome"], "success", "{first:#}");
    let first_route = first["sources"][0]["route_identity"].clone();
    let replacement = import_explicit_custom_source(&temp, &replacement_path);
    assert_eq!(replacement["outcome"], "success", "{replacement:#}");
    let replacement_generation = published_generation(&replacement);
    let replacement_route = replacement["sources"][0]["route_identity"].clone();
    let replaced =
        ctx_history_index::VerifiedIndex::open(data_root(&temp).join("search").join("lexical"))
            .unwrap();
    assert!(!replaced
        .manifest()
        .source_routes()
        .iter()
        .any(|route| route.route_identity().as_str() == first_route.as_str().unwrap()));
    assert!(replaced
        .manifest()
        .source_routes()
        .iter()
        .any(|route| route.route_identity().as_str() == replacement_route.as_str().unwrap()));
    drop(replaced);

    drop(daemon);
    let _restarted_daemon = start_full_source_refresh_daemon(&temp);
    let replay = import_explicit_custom_source(&temp, &replacement_path);
    assert_eq!(replay["outcome"], "success", "{replay:#}");
    assert_eq!(published_generation(&replay), replacement_generation);

    fs::rename(&first_path, &moved_first_path).unwrap();

    let stderr = failure_stderr(ctx(&temp).args([
        "import",
        "--input-format",
        "ctx-history-jsonl-v1",
        "--relocate-from",
        first_path.to_str().unwrap(),
        "--path",
        moved_first_path.to_str().unwrap(),
        "--no-daemon",
        "--format=json",
        "--progress",
        "none",
    ]));
    assert!(
        stderr.contains("not the active exact catalog lineage/route"),
        "{stderr}"
    );
    let status = json_output(ctx(&temp).args(["status", "--format=json"]));
    assert_eq!(
        status["lexical"]["generation_id"], replacement_generation,
        "{status:#}"
    );
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
