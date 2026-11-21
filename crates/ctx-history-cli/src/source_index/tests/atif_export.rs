use std::collections::{BTreeMap, HashSet};

use ctx_history_capture::atif::{
    parse_atif_bytes, parse_atif_str, project_trajectory_to_core_records, AtifProjectionInput,
    SchemaVersion, Trajectory, ATIF_SOURCE_FORMAT, MAX_SUBAGENT_NESTING_DEPTH,
};
use uuid::Uuid;

use super::*;
use crate::source_index::atif_export::{
    export_archive_atif, export_cli_session_atif, has_selected_embedding_ancestor,
    ArchiveAtifRequest,
};

fn write_core_records_generation(data_root: &Path, records: &[CoreRecord], revision: u8) {
    let source = records.first().expect("non-empty records").source.clone();
    assert!(records.iter().all(|record| record.source == source));
    let mut writer = GenerationWriter::open(
        index_root(data_root),
        WriterOptions {
            indexer_threads: 1,
            memory_bytes: 32 * 1024 * 1024,
        },
    )
    .unwrap()
    .into_writer()
    .unwrap();
    writer.begin_source(source.clone()).unwrap();
    for record in records {
        writer.add_core_record(record.clone()).unwrap();
    }
    let observation =
        SourceObservation::new(source, "atif-test-generation-v1", vec![revision]).unwrap();
    writer
        .certify_source(
            CertifiedSource::certify(
                observation.clone(),
                observation,
                "atif-test-parser-v1",
                [revision; 32],
                ScannedSourceCounts {
                    complete_records: records.len() as u64,
                    retained_records: records.len() as u64,
                    indexed_documents: records.len() as u64,
                    certified_bytes: 1,
                    ..ScannedSourceCounts::default()
                },
            )
            .unwrap(),
        )
        .unwrap();
    writer.commit(|_| true).unwrap();
}

fn atif_test_source(lineage: u8) -> SourceKey {
    SourceKey::derive(
        CaptureProvider::Custom.as_str(),
        ATIF_SOURCE_FORMAT,
        "atif-v1-source-backed-v1",
        1,
        SourceAnchor::CatalogLineage([lineage; 32]),
    )
    .unwrap()
}

fn collect_all_step_messages(traj: &Trajectory, out: &mut Vec<String>) {
    if let Some(step) = traj.steps.first() {
        out.push(step.message.to_searchable_text());
    }
    if let Some(subagents) = traj.subagent_trajectories.as_ref() {
        for sub in subagents {
            collect_all_step_messages(sub, out);
        }
    }
}

#[test]
fn archive_atif_exports_cyclic_parent_session_ids_without_dropping_sessions() {
    let temp = tempdir().unwrap();
    let source = atif_test_source(71);

    let make_session_id = |name: &str| {
        let key = NativeSessionKey::native_id("atif.cycle", TypedKey::utf8(name).unwrap()).unwrap();
        derive_session_id(SessionIdentityInput {
            source: &source,
            logical_session_kind: "atif-cycle",
            native_session_key: &key,
        })
        .unwrap()
    };
    let id_a = make_session_id("cycle-a");
    let id_b = make_session_id("cycle-b");

    let make_record = |sid: StableEntityId, parent: StableEntityId, name: &str| {
        let item = NativeItemKey::native_id("msg", TypedKey::utf8(name).unwrap()).unwrap();
        let eid = derive_event_id(EventIdentityInput {
            source: &source,
            session_id: sid,
            logical_item_kind: "message",
            native_item_key: &item,
            subrecord_selector: None,
        })
        .unwrap();
        let mut rec =
            CoreRecord::new_selected(eid, sid, source.clone(), 0, "message", "test-v1", name)
                .unwrap();
        rec.parent_session_id = Some(parent);
        rec.root_session_id = Some(parent);
        rec.session_relationship = Some(ProviderNativeSessionRelationship::Delegated);
        rec.agent_scope = Some(CoreAgentScope::Subagent);
        rec.provider_session_id = Some(name.to_owned());
        rec.occurred_at_unix_ms = Some(1_760_000_000_000);
        rec.role = Some("user".to_owned());
        rec.validate_contract().unwrap();
        rec
    };

    let rec_a = make_record(id_a, id_b, "cycle-a");
    let rec_b = make_record(id_b, id_a, "cycle-b");
    write_core_records_generation(temp.path(), &[rec_a, rec_b], 1);

    let index = open_index(temp.path()).unwrap();
    let sess_a = index.session_by_id(id_a.as_uuid()).unwrap().unwrap();
    let sess_b = index.session_by_id(id_b.as_uuid()).unwrap().unwrap();
    let mut selected = BTreeMap::new();
    selected.insert(id_a.as_uuid(), sess_a.clone());
    selected.insert(id_b.as_uuid(), sess_b.clone());
    assert!(
        !has_selected_embedding_ancestor(&index, &sess_a, &selected).unwrap(),
        "cyclic member A must not be suppressed as embedded"
    );
    assert!(
        !has_selected_embedding_ancestor(&index, &sess_b, &selected).unwrap(),
        "cyclic member B must not be suppressed as embedded"
    );

    let out_dir = temp.path().join("cycle-out");
    let receipt = export_archive_atif(
        &ArchiveAtifRequest {
            out_dir: out_dir.clone(),
            sessions: Vec::new(),
            providers: Vec::new(),
            workspace: None,
            since: None,
            until: None,
            include_subagents: true,
        },
        temp.path(),
    )
    .unwrap();

    // Neither cyclic session is dropped: total exported (roots + embedded) == 2.
    assert_eq!(receipt.sessions_exported + receipt.subagents_embedded, 2);
    let mut exported_ids = Vec::new();
    for file in &receipt.files {
        let traj = parse_atif_bytes(&fs::read(&file.path).unwrap()).unwrap();
        collect_all_step_messages(&traj, &mut exported_ids);
    }
    let exported_set: HashSet<&str> = exported_ids.iter().map(String::as_str).collect();
    assert!(exported_set.contains("cycle-a"), "{exported_ids:?}");
    assert!(exported_set.contains("cycle-b"), "{exported_ids:?}");
}

#[test]
fn archive_atif_exports_sessions_beyond_max_subagent_nesting_depth_as_fallback_roots() {
    let temp = tempdir().unwrap();
    let source = atif_test_source(72);

    // Build a 35-session chain (depths 0..=34), exceeding MAX_SUBAGENT_NESTING_DEPTH (32).
    let total_sessions = MAX_SUBAGENT_NESTING_DEPTH + 3;
    let mut session_ids = Vec::with_capacity(total_sessions);
    let mut records = Vec::with_capacity(total_sessions);

    for depth in 0..total_sessions {
        let name = format!("depth-{depth:02}");
        let key = NativeSessionKey::native_id("atif.depth", TypedKey::utf8(name.clone()).unwrap())
            .unwrap();
        let sid = derive_session_id(SessionIdentityInput {
            source: &source,
            logical_session_kind: "atif-depth",
            native_session_key: &key,
        })
        .unwrap();
        let item = NativeItemKey::native_id("msg", TypedKey::U64(depth as u64)).unwrap();
        let eid = derive_event_id(EventIdentityInput {
            source: &source,
            session_id: sid,
            logical_item_kind: "message",
            native_item_key: &item,
            subrecord_selector: None,
        })
        .unwrap();
        let mut rec =
            CoreRecord::new_selected(eid, sid, source.clone(), 0, "message", "test-v1", &name)
                .unwrap();
        if depth == 0 {
            rec.session_relationship = Some(ProviderNativeSessionRelationship::Root);
            rec.agent_scope = Some(CoreAgentScope::Primary);
        } else {
            rec.parent_session_id = Some(session_ids[depth - 1]);
            rec.root_session_id = Some(session_ids[0]);
            rec.session_relationship = Some(ProviderNativeSessionRelationship::Delegated);
            rec.agent_scope = Some(CoreAgentScope::Subagent);
        }
        rec.provider_session_id = Some(name);
        // Deliberately give deeper sessions earlier timestamps to verify depth ordering
        // still exports depth-33 before depth-34 (so depth-33 embeds depth-34).
        rec.occurred_at_unix_ms = Some(1_760_000_100_000 - (depth as i64) * 1_000);
        rec.role = Some("user".to_owned());
        rec.validate_contract().unwrap();
        session_ids.push(sid);
        records.push(rec);
    }

    write_core_records_generation(temp.path(), &records, 2);

    let index = open_index(temp.path()).unwrap();
    let mut selected = BTreeMap::new();
    for sid in &session_ids {
        selected.insert(
            sid.as_uuid(),
            index.session_by_id(sid.as_uuid()).unwrap().unwrap(),
        );
    }
    assert!(
        has_selected_embedding_ancestor(
            &index,
            &selected[&session_ids[MAX_SUBAGENT_NESTING_DEPTH].as_uuid()],
            &selected
        )
        .unwrap(),
        "depth == 32 is embedded by root"
    );
    assert!(
        !has_selected_embedding_ancestor(
            &index,
            &selected[&session_ids[MAX_SUBAGENT_NESTING_DEPTH + 1].as_uuid()],
            &selected
        )
        .unwrap(),
        "depth == 33 exceeds MAX_SUBAGENT_NESTING_DEPTH and must become a fallback root"
    );

    let out_dir = temp.path().join("deep-out");
    let receipt = export_archive_atif(
        &ArchiveAtifRequest {
            out_dir,
            sessions: Vec::new(),
            providers: Vec::new(),
            workspace: None,
            since: None,
            until: None,
            include_subagents: true,
        },
        temp.path(),
    )
    .unwrap();

    // depth-00 embeds depths 01..=32 (32 subagents), depth-33 becomes fallback root embedding depth-34 (1 subagent).
    assert_eq!(receipt.sessions_exported, 2, "{receipt:#?}");
    assert_eq!(
        receipt.subagents_embedded,
        total_sessions - 2,
        "{receipt:#?}"
    );

    let mut exported_names = Vec::new();
    for file in &receipt.files {
        let traj = parse_atif_bytes(&fs::read(&file.path).unwrap()).unwrap();
        collect_all_step_messages(&traj, &mut exported_names);
    }
    assert_eq!(exported_names.len(), total_sessions);
    let unique: HashSet<&str> = exported_names.iter().map(String::as_str).collect();
    assert_eq!(unique.len(), total_sessions);
    for depth in 0..total_sessions {
        let expected = format!("depth-{depth:02}");
        assert!(
            unique.contains(expected.as_str()),
            "missing `{expected}` from archive export"
        );
    }
}

#[test]
fn show_session_atif_max_events_preserves_untimestamped_header_and_bounds_subagents() {
    let temp = tempdir().unwrap();
    let source = atif_test_source(73);

    // Step 1 (event_sequence 0) has NO timestamp and holds StoredAtifTrajectoryHeader;
    // Step 2 (event_sequence 1) HAS a timestamp and delegates to embedded subagents.
    let doc = json!({
        "schema_version": "ATIF-v1.8",
        "session_id": "untimestamped-run-001",
        "trajectory_id": "untimestamped-root",
        "notes": "Header metadata that must survive --max-events 1",
        "agent": {
            "name": "harbor-untimestamped-agent",
            "version": "9.9.9",
            "model_name": "gemini-4-neon"
        },
        "steps": [
            {
                "step_id": 1,
                "source": "user",
                "message": "Untimestamped opening prompt"
            },
            {
                "step_id": 2,
                "timestamp": "2026-06-24T12:00:05Z",
                "source": "agent",
                "message": "Delegating to two subagents",
                "observation": {
                    "results": [
                        {
                            "content": "delegated",
                            "subagent_trajectory_ref": [
                                { "trajectory_id": "sub-one" },
                                { "trajectory_id": "sub-two" }
                            ]
                        }
                    ]
                }
            }
        ],
        "subagent_trajectories": [
            {
                "schema_version": "ATIF-v1.8",
                "trajectory_id": "sub-one",
                "agent": { "name": "worker-one", "version": "1.0.0" },
                "steps": [
                    { "step_id": 1, "timestamp": "2026-06-24T12:00:06Z", "source": "user", "message": "sub one step 1" },
                    { "step_id": 2, "timestamp": "2026-06-24T12:00:07Z", "source": "agent", "message": "sub one step 2" }
                ]
            },
            {
                "schema_version": "ATIF-v1.8",
                "trajectory_id": "sub-two",
                "agent": { "name": "worker-two", "version": "1.0.0" },
                "steps": [
                    { "step_id": 1, "timestamp": "2026-06-24T12:00:08Z", "source": "user", "message": "sub two step 1" }
                ]
            }
        ]
    });

    let traj = parse_atif_str(&doc.to_string()).unwrap();
    let records = project_trajectory_to_core_records(&AtifProjectionInput {
        source: &source,
        document_key: "untimestamped.json",
        trajectory: &traj,
        lineage: None,
    })
    .unwrap();
    let root_session_id = records[0].session_id.as_uuid().to_string();
    write_core_records_generation(temp.path(), &records, 3);

    // 1. --max-events 1 must pick event_sequence 0 (untimestamped), preserving ATIF header
    //    metadata, and must NOT load or synthesize subagents.
    let (mut ui_one, stdout_one) = test_ui();
    let res_one = export_cli_session_atif(
        temp.path(),
        Some(root_session_id.clone()),
        None,
        None,
        None,
        None,
        Some(1),
        None,
        &mut ui_one,
    )
    .unwrap();
    assert_eq!(res_one.events_returned, 1);
    let traj_one = parse_atif_bytes(&stdout_one.bytes()).unwrap();
    assert_eq!(traj_one.schema_version, SchemaVersion::V1_8);
    assert_eq!(traj_one.agent.name, "harbor-untimestamped-agent");
    assert_eq!(traj_one.agent.version, "9.9.9");
    assert_eq!(
        traj_one.notes.as_deref(),
        Some("Header metadata that must survive --max-events 1")
    );
    assert_eq!(traj_one.steps.len(), 1);
    assert_eq!(
        traj_one.steps[0].message.to_searchable_text(),
        "Untimestamped opening prompt"
    );
    assert!(
        traj_one.subagent_trajectories.is_none(),
        "subagents referenced only after the truncated prefix must not be loaded or synthesized"
    );
    assert!(traj_one.steps[0].observation.is_none());

    // 2. --max-events 4: root has 3 CoreRecords (step 1 msg, step 2 msg, step 2 obs_result),
    //    leaving remaining budget == 1 for subagents -> sub-one gets 1 event, sub-two gets 0.
    let (mut ui_four, stdout_four) = test_ui();
    let res_four = export_cli_session_atif(
        temp.path(),
        Some(root_session_id),
        None,
        None,
        None,
        None,
        Some(4),
        None,
        &mut ui_four,
    )
    .unwrap();
    assert_eq!(res_four.events_returned, 4);
    let traj_four = parse_atif_bytes(&stdout_four.bytes()).unwrap();
    let embedded = traj_four
        .subagent_trajectories
        .as_ref()
        .expect("one subagent fits in remaining budget");
    assert_eq!(embedded.len(), 1);
    assert_eq!(embedded[0].trajectory_id.as_deref(), Some("sub-one"));
    assert_eq!(embedded[0].steps.len(), 1);
}

#[test]
fn atif_show_and_archive_resolve_compact_session_refs_with_retained_peer() {
    let temp = tempdir().unwrap();
    write_test_generation(temp.path());
    let (retained, active) = compact_collision_pair(temp.path(), "atif retained peer needle");
    let active_session = active.event.session_id.as_uuid().simple().to_string();

    // 8-char colliding prefix must fail as ambiguous naming both retained and active UUIDs.
    let (mut ui_ambig, _) = test_ui();
    let show_err = export_cli_session_atif(
        temp.path(),
        Some(active_session[..8].to_owned()),
        None,
        None,
        None,
        None,
        None,
        None,
        &mut ui_ambig,
    )
    .map_err(externalize_query_error)
    .err()
    .expect("8-char prefix collides with retained peer");
    let show_detail = show_err.to_string();
    assert!(show_detail.contains("ambiguous"), "{show_detail}");
    assert!(
        show_detail.contains(&retained.event.session_id.to_string()),
        "{show_detail}"
    );
    assert!(
        show_detail.contains(&active.event.session_id.to_string()),
        "{show_detail}"
    );

    let archive_err = export_archive_atif(
        &ArchiveAtifRequest {
            out_dir: temp.path().join("ambig-out"),
            sessions: vec![active_session[..8].to_owned()],
            providers: Vec::new(),
            workspace: None,
            since: None,
            until: None,
            include_subagents: true,
        },
        temp.path(),
    )
    .unwrap_err();
    let archive_detail = archive_err.to_string();
    assert!(archive_detail.contains("ambiguous"), "{archive_detail}");
    assert!(
        archive_detail.contains(&retained.event.session_id.to_string()),
        "{archive_detail}"
    );
    assert!(
        archive_detail.contains(&active.event.session_id.to_string()),
        "{archive_detail}"
    );

    // 9-char unambiguous compact prefix must succeed for both show --format atif and archive atif.
    let (mut ui_ok, stdout_ok) = test_ui();
    let show_ok = export_cli_session_atif(
        temp.path(),
        Some(active_session[..9].to_owned()),
        None,
        None,
        None,
        None,
        None,
        None,
        &mut ui_ok,
    )
    .unwrap();
    assert_eq!(show_ok.events_returned, 1);
    let traj = parse_atif_bytes(&stdout_ok.bytes()).unwrap();
    assert_eq!(traj.schema_version, SchemaVersion::V1_8);

    let archive_ok = export_archive_atif(
        &ArchiveAtifRequest {
            out_dir: temp.path().join("compact-ok-out"),
            sessions: vec![active_session[..9].to_owned()],
            providers: Vec::new(),
            workspace: None,
            since: None,
            until: None,
            include_subagents: true,
        },
        temp.path(),
    )
    .unwrap();
    assert_eq!(archive_ok.sessions_exported, 1);
    assert_eq!(
        archive_ok.files[0].ctx_session_id,
        active.event.session_id.as_uuid().to_string()
    );
    let _ = Uuid::parse_str(&archive_ok.files[0].ctx_session_id).unwrap();
}
