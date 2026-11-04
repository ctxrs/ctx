use super::*;

#[test]
fn claude_ownership_revision_reimports_lossy_records_with_stable_identities() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let root = temp.path().join("projects");
    let index = temp.path().join("index");
    let path = root.join("project/primary.jsonl");
    let mut rows = Vec::new();
    for (id, markers) in [
        ("child", json!({"isSidechain":true,"agentId":"child"})),
        ("primary", json!({})),
    ] {
        let mut call = markers.clone();
        call["type"] = json!("assistant");
        call["uuid"] = json!(format!("{id}-call"));
        call["sessionId"] = json!("primary");
        call["message"] = json!({"role":"assistant", "content":[{
            "type":"tool_use", "id":id, "name":"Bash", "input":{"command":"pwd"}
        }]});
        let mut result = markers;
        result["type"] = json!("user");
        result["uuid"] = json!(format!("{id}-result"));
        result["sessionId"] = json!("primary");
        result["message"] = json!({"role":"user", "content":[{
            "type":"tool_result", "tool_use_id":id, "content":"result", "is_error":false
        }]});
        rows.extend([call, result]);
    }
    write_transcript(&path, &rows);
    let original = fs::read(&path).unwrap();
    let registry = registry(CaptureProvider::Claude, "claude_projects_jsonl_tree", &root);
    refresh_source_backed_generation(&index, &registry, writer_options()).unwrap();
    let current = indexed_records(&index, CaptureProvider::Claude, "primary");
    assert_eq!(current.len(), 4);
    for (record, sidechain, agent) in [
        (&current[0], "true", true),
        (&current[1], "true", true),
        (&current[2], "absent", false),
        (&current[3], "absent", false),
    ] {
        assert_eq!(
            record.parser_revision,
            "claude-shared-jsonl-core-activity-v3-record-ownership"
        );
        assert_eq!(
            record.content.structured_content.as_ref().unwrap()["record_ownership"],
            json!({"sidechain":sidechain, "agent_id_present":agent})
        );
    }

    // Seed the predecessor's actual projection shape, including its missing
    // row markers and old source certificate, without changing source bytes.
    let revision = "claude-shared-jsonl-core-activity-v2-record-rejections";
    let verified = VerifiedIndex::open_pinned(&index).unwrap();
    let certificate = &verified.manifest().sources[0];
    let source = certificate.observation().source().clone();
    let mut certificate = serde_json::to_value(certificate).unwrap();
    certificate["parser_revision"] = json!(revision);
    let routes = verified.manifest().source_routes().to_vec();
    drop(verified);
    let mut writer = GenerationWriter::open(&index, writer_options())
        .unwrap()
        .into_writer()
        .unwrap();
    writer.set_present_source_routes(routes).unwrap();
    writer.begin_source(source).unwrap();
    let mut predecessor = current.clone();
    for record in &mut predecessor {
        record.parser_revision = revision.to_owned();
        record.content.structured_content = record
            .content
            .structured_content
            .take()
            .map(|mut value| value["entry"].take());
        writer.add_core_record(record.clone()).unwrap();
    }
    writer
        .certify_source(serde_json::from_value(certificate).unwrap())
        .unwrap();
    let seeded = writer.commit(|_| true).unwrap();
    assert_eq!(
        indexed_records(&index, CaptureProvider::Claude, "primary"),
        predecessor
    );

    let migrated = refresh_source_backed_generation(&index, &registry, writer_options()).unwrap();
    assert!(migrated.failed_routes.is_empty());
    assert_ne!(migrated.commit.generation_id, seeded.generation_id);
    assert_eq!(
        indexed_records(&index, CaptureProvider::Claude, "primary"),
        current
    );
    let repeated = refresh_source_backed_generation(&index, &registry, writer_options()).unwrap();
    assert_eq!(repeated.commit.generation_id, migrated.commit.generation_id);
    assert_eq!(
        indexed_records(&index, CaptureProvider::Claude, "primary"),
        current
    );
    assert_eq!(fs::read(path).unwrap(), original);
}
