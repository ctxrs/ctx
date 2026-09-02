const PI_V10_PARSER_REVISION: &str = "pi-shared-jsonl-v10-child-local-lineage";
const PI_V11_PARSER_REVISION: &str = "pi-shared-jsonl-v11-omp-parent-lineage";
const PI_V10_PARENT_SESSION_ID: &str = "pi-v10-parent";
const PI_V10_CHILD_SESSION_ID: &str = "pi-session-docs-1";

fn pi_parser_revision(data_root: &Path) -> String {
    let index =
        ctx_history_index::VerifiedIndex::open_pinned(data_root.join("search/lexical")).unwrap();
    index
        .manifest()
        .sources
        .iter()
        .find(|certificate| certificate.observation().source().provider() == "pi")
        .expect("published generation must contain the Pi source")
        .parser_revision()
        .to_owned()
}

fn publish_pi_v10_predecessor(data_root: &Path) -> String {
    let index_root = data_root.join("search/lexical");
    let (legacy_sources, routes) = {
        let index = ctx_history_index::VerifiedIndex::open_pinned(&index_root).unwrap();
        let legacy_sources = index
            .manifest()
            .sources
            .iter()
            .filter(|certificate| certificate.observation().source().provider() == "pi")
            .map(|current| {
                assert_eq!(current.parser_revision(), PI_V11_PARSER_REVISION);
                let mut certificate = serde_json::to_value(current).unwrap();
                certificate["parser_revision"] = json!(PI_V10_PARSER_REVISION);
                (
                    current.observation().source().clone(),
                    serde_json::from_value(certificate).unwrap(),
                )
            })
            .collect::<Vec<_>>();
        assert_eq!(legacy_sources.len(), 2);
        (legacy_sources, index.manifest().source_routes().to_vec())
    };

    let mut records = provider_core_records(data_root, "pi");
    for record in &mut records {
        record.parser_revision = PI_V10_PARSER_REVISION.to_owned();
        record.parent_session_id = None;
        record.session_relationship = None;
        assert_eq!(record.root_session_id, None);
        assert_eq!(record.agent_scope, None);
        record.validate_contract().unwrap();
    }

    let mut writer = ctx_history_index::GenerationWriter::open(
        &index_root,
        ctx_history_index::WriterOptions {
            indexer_threads: 1,
            memory_bytes: 32 * 1024 * 1024,
        },
    )
    .unwrap()
    .into_writer()
    .unwrap();
    writer.set_present_source_routes(routes).unwrap();
    let mut published_records = 0;
    for (source, legacy_certificate) in legacy_sources {
        writer.begin_source(source.clone()).unwrap();
        for record in records
            .iter()
            .filter(|record| record.source.exact_descriptor_eq(&source))
        {
            writer.add_core_record(record.clone()).unwrap();
            published_records += 1;
        }
        writer.certify_source(legacy_certificate).unwrap();
    }
    assert_eq!(published_records, records.len());
    let legacy_generation = writer.commit(|_| true).unwrap().generation_id;

    let legacy = ctx_history_index::VerifiedIndex::open_pinned(&index_root).unwrap();
    assert_eq!(legacy.generation_id(), legacy_generation);
    assert_eq!(pi_parser_revision(data_root), PI_V10_PARSER_REVISION);
    for record in provider_core_records(data_root, "pi") {
        assert_eq!(record.parent_session_id, None);
        assert_eq!(record.root_session_id, None);
        assert_eq!(record.session_relationship, None);
        assert_eq!(record.agent_scope, None);
    }

    legacy_generation
}

#[test]
fn pi_cli_import_search_flow() {
    let temp = tempdir();
    let fixture = temp
        .path()
        .join(".pi/agent/sessions/--workspace--/pi-session.jsonl");
    fs::create_dir_all(fixture.parent().unwrap()).unwrap();
    fs::copy(provider_history_fixture("pi-session.jsonl"), &fixture).unwrap();
    let child = fs::read_to_string(&fixture).unwrap();
    let (header, entries) = child.split_once('\n').unwrap();
    let mut header = serde_json::from_str::<Value>(header).unwrap();
    header["parentSession"] = json!(PI_V10_PARENT_SESSION_ID);
    fs::write(&fixture, format!("{header}\n{entries}")).unwrap();
    write_pi_session_jsonl(
        &fixture.parent().unwrap().join("pi-parent.jsonl"),
        PI_V10_PARENT_SESSION_ID,
        "parent session migration oracle",
    );
    let daemon = start_source_refresh_daemon(&temp);

    let imported = json_output(ctx(&temp).args([
        "import",
        "--provider",
        "pi",
        "--no-daemon",
        "--format=json",
    ]));
    assert_authoritative_provider_publication(&imported);
    assert_eq!(imported["totals"]["current_rejected_records"], 0);
    let first_generation = imported["sources"][0]["published_generation"]
        .as_str()
        .expect("Pi provider import must publish a Core generation");

    let search = json_output(ctx(&temp).args([
        "search",
        "provider metadata",
        "--provider",
        "pi",
        "--refresh",
        "off",
        "--format=json",
    ]));
    assert_source_backed_search(&search, "pi", "provider metadata");

    drop(daemon);
    let legacy_generation = publish_pi_v10_predecessor(&data_root(&temp));
    assert_ne!(legacy_generation, first_generation);
    assert_eq!(provider_core_counts(&data_root(&temp), "pi"), (2, 7));
    let _daemon = start_source_refresh_daemon(&temp);

    let second = json_output(ctx(&temp).args([
        "import",
        "--provider",
        "pi",
        "--resume",
        "--no-daemon",
        "--format=json",
    ]));
    assert_eq!(second["resume"], true);
    assert_eq!(second["resume_mode"], "idempotent_rescan");
    assert_authoritative_provider_publication(&second);
    assert_eq!(second["totals"]["current_rejected_records"], 0);
    assert_ne!(
        second["sources"][0]["published_generation"], legacy_generation,
        "an unchanged Pi source must not reuse a v10 projection: {second:#}"
    );
    assert_eq!(
        pi_parser_revision(&data_root(&temp)),
        PI_V11_PARSER_REVISION
    );

    let records = provider_core_records(&data_root(&temp), "pi");
    assert_eq!(provider_core_counts(&data_root(&temp), "pi"), (2, 7));
    let parent_session_id = records
        .iter()
        .find(|record| record.provider_session_id.as_deref() == Some(PI_V10_PARENT_SESSION_ID))
        .unwrap()
        .session_id;
    for record in &records {
        assert_eq!(record.root_session_id, None);
        assert_eq!(record.agent_scope, None);
        if record.provider_session_id.as_deref() == Some(PI_V10_CHILD_SESSION_ID) {
            assert_eq!(record.parent_session_id, Some(parent_session_id));
            assert_eq!(
                serde_json::to_value(record.session_relationship).unwrap(),
                json!("forked")
            );
        } else {
            assert_eq!(record.parent_session_id, None);
            assert_eq!(record.session_relationship, None);
        }
    }
    assert_eq!(
        records
            .iter()
            .filter(|record| {
                record.provider_session_id.as_deref() == Some(PI_V10_CHILD_SESSION_ID)
                    && record.event_type == "message"
                    && record.role.as_deref() == Some("user")
            })
            .count(),
        1
    );
    assert_eq!(
        records
            .iter()
            .filter(|record| {
                record.provider_session_id.as_deref() == Some(PI_V10_CHILD_SESSION_ID)
                    && record.event_type == "message"
                    && record.role.as_deref() == Some("assistant")
            })
            .count(),
        1
    );
    let provider_outputs = records
        .iter()
        .filter(|record| matches!(record.event_type.as_str(), "tool_output" | "command_output"))
        .map(|record| (record.event_type.as_str(), record.content.meaningful_text()))
        .collect::<Vec<_>>();
    assert_eq!(
        provider_outputs,
        [
            ("tool_output", "tests passed"),
            ("command_output", "ok token=fixture-secret"),
        ],
        "Core must preserve provider-native output without adjudicating command success"
    );
    assert!(
        !temp.path().join("work.sqlite").exists(),
        "Pi acceptance must use the Core generation"
    );
    assert!(!data_root(&temp).join("relational.sqlite").exists());
}
