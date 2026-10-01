//! Native terminal multiplicity is settled before any replay order can join it.
use super::*;
use ctx_attribution::{
    core_materialization::CORE_MATERIALIZER_REVISION,
    materializer::{CoreGenerationStart, SegmentMaterializer},
    protocol::{
        CoreEventDelta, CoreEventDeltaPage, CoreGenerationHead, CoreSourceDelta,
        CoreSourceDeltaPage, CoreSourceState,
    },
    query::{AttributionOutcome, BlameEntry, BlameService, QueryBounds, QueryError, ResourceKind},
    CoreMaterializationSyncOutcome, SegmentGraph,
};
use serde_json::{json, Value};

fn next_commit(repository: &Path, message: &str) -> (String, String) {
    let output = git(repository, &["commit", "--allow-empty", "-m", message]);
    (output, git(repository, &["rev-parse", "HEAD"]))
}

fn commit_call(repository: &Path, call: &str) -> Value {
    let mut event = exec_call_with_command(call, "git commit --allow-empty -m witness");
    event["payload"]["arguments"] = json!(json!({
        "cmd":"git commit --allow-empty -m witness", "workdir":repository
    })
    .to_string());
    event
}

fn duplicate(output: &str, conflicting: &str, mode: &str) -> Value {
    let mut event = exact_exec_result(
        "commit-call",
        if mode == "conflicting" {
            conflicting
        } else {
            output
        },
    );
    if mode == "failed" {
        event["payload"]["status"] = json!("failed");
        event["payload"]["output"] = json!(format!(
            "Process exited with code 1\nFinal output:\n{output}"
        ));
    }
    event
}

fn assert_unlinked(record: &CoreRecord) {
    let activity = record.content.activity.as_ref().unwrap();
    assert_eq!(activity.provider_call_id, None);
    assert_eq!(activity.invocation, None);
    assert_eq!(activity.result, None);
    assert!(record.content.structured_content.is_some());
    record.validate_contract().unwrap();
    assert!(record.native_event_id.is_some());
    assert_eq!(record.provider_session_id.as_deref(), Some(OWNER));
    assert_eq!(record.content.discovery_exclusion, None);
    assert!(!record.content.normalized_body.as_ref().unwrap().is_empty());
}

fn assert_graph_blame(graph: &SegmentGraph, oid: &str, witness: Option<&CoreRecord>) {
    let result = BlameService::new(graph, QueryBounds::default())
        .unwrap()
        .execute(
            &BlameTarget::Commit {
                oid: oid.to_owned(),
                repository: None,
            },
            None,
        );
    let Some(witness) = witness else {
        assert!(matches!(
            result,
            Err(QueryError::TargetNotFound(ResourceKind::Commit))
        ));
        return;
    };
    let page = result.unwrap();
    assert!(!page.entries.is_empty());
    for entry in page.entries {
        let BlameEntry::Commit(entry) = entry else {
            panic!("commit match")
        };
        assert_eq!(entry.attribution, AttributionOutcome::Possible);
        assert_eq!(
            entry.direct_actor.unwrap().display,
            witness.session_id.to_string()
        );
        assert!(entry.fact.citations.iter().any(|citation| {
            citation.0.event_id == witness.event_id && citation.0.session_id == witness.session_id
        }));
    }
}

fn join_owner(pair: &[CoreRecord]) -> &CoreRecord {
    pair.iter()
        .max_by_key(|record| record.event_id.digest())
        .unwrap()
}

// One record per page, always in the materializer's stable-ID cursor order.
fn replay_in_stable_order(
    root: &Path,
    snapshot: &CoreSnapshot,
    records: &[CoreRecord],
) -> SegmentGraph {
    let manifest = snapshot.source_manifest_page(None, 256).unwrap();
    assert!(manifest.terminal);
    assert_eq!(manifest.items.len(), 1);
    let state = &manifest.items[0];
    let source = CoreSourceState {
        source: state.source.clone(),
        core_record_accumulator: state.aggregate.core_record_accumulator().to_owned(),
        event_count: state.aggregate.indexed_documents(),
    };
    let contract = snapshot.contract();
    let schema = &contract.schema;
    let head = CoreGenerationHead::new(
        snapshot.generation_id(),
        schema.manifest_version,
        schema.identity_version,
        &contract.core_record_fingerprint,
        schema.lexical_schema_version,
        schema.lexical_analyzer_version,
        &schema.policy_schema_hash,
        std::slice::from_ref(&source),
    )
    .unwrap();
    let mut materializer =
        SegmentMaterializer::open_for_revision(root, CORE_MATERIALIZER_REVISION).unwrap();
    let CoreGenerationStart::Started(mut session) =
        materializer.start_core_generation(head).unwrap()
    else {
        panic!("fresh replay must materialize")
    };
    let reconciliations = session
        .reconcile_source_page(
            CoreSourceDeltaPage::new(
                "0".repeat(64),
                snapshot.generation_id(),
                0,
                true,
                vec![CoreSourceDelta::Present(source)],
            )
            .unwrap(),
        )
        .unwrap();
    assert_eq!(reconciliations.len(), 1);
    let mut ordered = records.iter().collect::<Vec<_>>();
    ordered.sort_by_key(|record| record.event_id.digest());
    for (page, record) in ordered.iter().enumerate() {
        session
            .ingest_event_pages(vec![CoreEventDeltaPage {
                materialization_id: "0".repeat(64),
                core_generation_id: snapshot.generation_id().to_owned(),
                reconciliation: reconciliations[0].clone(),
                page_index: page as u32,
                terminal: page + 1 == ordered.len(),
                deltas: vec![CoreEventDelta::Added((**record).clone())],
            }])
            .unwrap();
    }
    session.activate().unwrap();
    SegmentGraph::from_pinned(
        materializer
            .flat_store()
            .open_active(SegmentGraph::flat_open_policy())
            .unwrap(),
        None,
    )
}

#[test]
fn native_terminal_multiplicity_withholds_every_duplicate_in_stable_blame_replay() {
    let temp = tempdir().unwrap();
    let repository = temp.path().join("repository");
    let (output, oid) = real_commit(&repository);
    let (conflicting, conflicting_oid) = next_commit(&repository, "conflicting witness");
    let (unrelated, unrelated_oid) = next_commit(&repository, "unrelated witness");
    for scope in ["primary", "delegated"] {
        for mode in ["unique", "identical", "conflicting", "failed"] {
            let root = temp.path().join(format!("{scope}-{mode}"));
            let sessions = root.join("sessions");
            fs::create_dir_all(&sessions).unwrap();
            let mut bytes = envelope(&repository, &output, scope);
            if mode != "unique" {
                bytes.extend(jsonl_bytes([duplicate(&output, &conflicting, mode)]));
            }
            bytes.extend(jsonl_bytes([
                commit_call(&repository, "unrelated-call"),
                exact_exec_result("unrelated-call", &unrelated),
            ]));
            fs::write(session_path(&sessions, OWNER), bytes).unwrap();
            let data = data_root(&root);
            let index_root = data.join("search/lexical");
            let receipt = refresh_source_backed_generation(
                &index_root,
                &register_tree(&[&sessions]),
                writer_options(),
            )
            .unwrap();
            assert!(receipt.failed_routes.is_empty());
            assert!(receipt.logical_source_failures.is_empty());
            let index = VerifiedIndex::open_pinned(&index_root).unwrap();
            let records = records_for(&index, OWNER);
            let unique = mode == "unique";
            assert_eq!(records.len(), if unique { 4 } else { 5 });
            if !unique {
                assert_unlinked(&records[1]);
                assert_unlinked(&records[2]);
                assert!(records[1]
                    .content
                    .normalized_body
                    .as_ref()
                    .unwrap()
                    .contains(&output));
                assert_ne!(records[1].event_id, records[2].event_id);
            }
            assert_call_blame(
                &data,
                &receipt.commit.generation_id,
                &oid,
                &records,
                unique.then_some("commit-call"),
            );
            assert_call_blame(
                &data,
                &receipt.commit.generation_id,
                &conflicting_oid,
                &records,
                None,
            );
            assert_call_blame(
                &data,
                &receipt.commit.generation_id,
                &unrelated_oid,
                &records,
                Some("unrelated-call"),
            );
            let snapshot = CoreSnapshot::open(
                &data,
                &receipt.commit.generation_id,
                &SnapshotContract::current().unwrap(),
            )
            .unwrap();
            let graph = replay_in_stable_order(&root.join("replay"), &snapshot, &records);
            assert_graph_blame(&graph, &oid, unique.then_some(join_owner(&records[..2])));
            assert_graph_blame(&graph, &conflicting_oid, None);
            assert_graph_blame(
                &graph,
                &unrelated_oid,
                Some(join_owner(&records[records.len() - 2..])),
            );
        }
    }
}

fn attribution_generation(data: &Path) -> String {
    let materializer = SegmentMaterializer::open_for_revision(
        data.join("search/attribution"),
        CORE_MATERIALIZER_REVISION,
    )
    .unwrap();
    let graph = SegmentGraph::from_pinned(
        materializer
            .flat_store()
            .open_active(SegmentGraph::flat_open_policy())
            .unwrap(),
        None,
    );
    graph.generation_id().to_owned()
}

fn assert_attribution_noop(data: &Path, generation: &str) {
    let prior = attribution_generation(data);
    let snapshot =
        CoreSnapshot::open(data, generation, &SnapshotContract::current().unwrap()).unwrap();
    assert!(matches!(
        ctx_attribution::catch_up(data, &snapshot, &|| false).unwrap(),
        CoreMaterializationSyncOutcome::Finished {
            did_work: false,
            ..
        }
    ));
    assert_eq!(attribution_generation(data), prior);
}

// Choose authored native call IDs, never alter emitted Core IDs. Use the
// observed source/session/key shape; the actual cursor order is asserted below.
fn native_event_id_for_call(record: &CoreRecord, call: &str) -> ctx_history_core::StableEntityId {
    use ctx_history_core::{derive_event_id, EventIdentityInput, NativeItemKey};
    let Some(TypedKey::Composite(mut parts)) = record.native_event_id.clone() else {
        panic!("native event key")
    };
    assert_eq!(parts[1], TypedKey::Utf8("call_id".to_owned()));
    parts[2] = TypedKey::Utf8(call.to_owned());
    derive_event_id(EventIdentityInput {
        source: &record.source,
        session_id: record.session_id,
        logical_item_kind: "codex-event",
        native_item_key: &NativeItemKey::composite("codex.event.v1", parts).unwrap(),
        subrecord_selector: None,
    })
    .unwrap()
}

fn native_call_in_order(pair: &[CoreRecord], reverse: bool) -> String {
    for record in pair {
        assert_eq!(
            native_event_id_for_call(record, "commit-call"),
            record.event_id
        );
    }
    (0..128)
        .map(|index| format!("ordered-native-call-{index}"))
        .find(|call| {
            (native_event_id_for_call(&pair[1], call).digest()
                < native_event_id_for_call(&pair[0], call).digest())
                == reverse
        })
        .expect("fixture native IDs include both stable cursor directions")
}

#[test]
fn conflicting_original_exec_and_mcp_pair_abstain_in_legal_blame_cursor_orders() {
    let temp = tempdir().unwrap();
    let repository = temp.path().join("repository");
    let (output, oid) = real_commit(&repository);
    for custom in [false, true] {
        for reverse in [false, true] {
            for direct in [false, true] {
                for result_in_prefix in [false, true] {
                    let root = temp
                        .path()
                        .join(format!("{custom}-{reverse}-{direct}-{result_in_prefix}"));
                    let sessions = root.join("sessions");
                    fs::create_dir_all(&sessions).unwrap();
                    let path = session_path(&sessions, OWNER);
                    let mut rows: Vec<Value> = envelope(&repository, &output, "primary")
                        .split(|byte| *byte == b'\n')
                        .filter(|line| !line.is_empty())
                        .map(|line| serde_json::from_slice(line).unwrap())
                        .collect();
                    if custom {
                        let invocation = &mut rows[1]["payload"];
                        invocation["type"] = json!("custom_tool_call");
                        let arguments = invocation
                            .as_object_mut()
                            .unwrap()
                            .remove("arguments")
                            .unwrap();
                        assert!(arguments.is_string());
                        invocation["input"] = arguments;
                    }
                    let model = rows.last_mut().unwrap();
                    model["payload"].as_object_mut().unwrap().remove("status");
                    model["payload"]["output"] =
                        json!(format!("Wall time: 0.0000 seconds\nOutput:\n{output}"));
                    let mut mcp = exact_mcp_result("commit-call", "not an execution of git");
                    mcp["payload"]["result"] = json!({"Err":"failed"});
                    rows.push(mcp);
                    fs::write(&path, jsonl_bytes(rows.clone())).unwrap();
                    let data = data_root(&root);
                    let index_root = data.join("search/lexical");
                    let registry = register_tree(&[&sessions]);
                    let seed =
                        refresh_source_backed_generation(&index_root, &registry, writer_options())
                            .unwrap();
                    assert!(seed.failed_routes.is_empty());
                    let index = VerifiedIndex::open_pinned(&index_root).unwrap();
                    let shape = records_for(&index, OWNER);
                    assert_eq!(shape.len(), 3);
                    for record in &shape {
                        assert_eq!(
                            native_event_id_for_call(record, "commit-call"),
                            record.event_id
                        );
                    }
                    let call = (0..256)
                        .map(|i| format!("mcp-conflict-native-{i}"))
                        .find(|call| {
                            let ids: Vec<_> = shape
                                .iter()
                                .map(|record| native_event_id_for_call(record, call))
                                .collect();
                            (ids[1].digest() < ids[0].digest()) == reverse
                                && ids[0].digest() < ids[2].digest()
                                && ids[1].digest() < ids[2].digest()
                        })
                        .expect("authored native IDs give I,F,M and F,I,M cursor orders");
                    drop(index);
                    for row in &mut rows {
                        if row["payload"]["call_id"] == "commit-call" {
                            row["payload"]["call_id"] = json!(call);
                        }
                    }
                    let prefix_len = rows.len() - if result_in_prefix { 1 } else { 2 };
                    fs::write(&path, jsonl_bytes(rows[..prefix_len].iter().cloned())).unwrap();
                    let prefix =
                        refresh_source_backed_generation(&index_root, &registry, writer_options())
                            .unwrap();
                    assert!(prefix.failed_routes.is_empty());
                    let index = VerifiedIndex::open_pinned(&index_root).unwrap();
                    let before = records_for(&index, OWNER);
                    assert_eq!(before.len(), if result_in_prefix { 2 } else { 1 });
                    assert_call_blame(
                        &data,
                        &prefix.commit.generation_id,
                        &oid,
                        &before,
                        result_in_prefix.then_some(call.as_str()),
                    );
                    if result_in_prefix {
                        let answer = ctx_attribution::query(
                            &data,
                            &BlameTarget::Commit {
                                oid: oid.clone(),
                                repository: None,
                            },
                            10,
                            None,
                        )
                        .unwrap()
                        .result;
                        assert!(answer
                            .evidence
                            .iter()
                            .any(|evidence| evidence.citation.event_id
                                == before[usize::from(!reverse)].event_id));
                    }
                    drop(index);
                    for row in &rows[prefix_len..] {
                        append_event(&path, row.clone());
                    }
                    let appended = if direct {
                        let (receipt, completed) =
                            incremental_refresh(&index_root, &registry, &prefix);
                        assert_eq!(
                            completed, 3,
                            "prefix identity requires complete replacement"
                        );
                        receipt
                    } else {
                        refresh_source_backed_generation(&index_root, &registry, writer_options())
                            .unwrap()
                    };
                    assert!(appended.failed_routes.is_empty());
                    let index = VerifiedIndex::open_pinned(&index_root).unwrap();
                    let records = records_for(&index, OWNER);
                    assert_eq!(records.len(), 3);
                    for record in &records {
                        assert_unlinked(record);
                    }
                    for (old, new) in before.iter().zip(&records) {
                        assert_eq!(old.event_id, new.event_id);
                        assert_eq!(old.native_event_id, new.native_event_id);
                        assert_eq!(
                            old.content.structured_content,
                            new.content.structured_content
                        );
                        assert_eq!(old.content.normalized_body, new.content.normalized_body);
                        assert_eq!(
                            old.content.activity.as_ref().unwrap().facts,
                            new.content.activity.as_ref().unwrap().facts
                        );
                    }
                    let page = index
                        .source_event_page(&records[0].source, None, 256)
                        .unwrap();
                    assert!(page.terminal);
                    assert_eq!(
                        page.items
                            .iter()
                            .map(|item| item.event_id)
                            .collect::<Vec<_>>(),
                        [
                            records[usize::from(reverse)].event_id,
                            records[usize::from(!reverse)].event_id,
                            records[2].event_id
                        ]
                    );
                    assert_call_blame(&data, &appended.commit.generation_id, &oid, &records, None);
                    let snapshot = CoreSnapshot::open(
                        &data,
                        &appended.commit.generation_id,
                        &SnapshotContract::current().unwrap(),
                    )
                    .unwrap();
                    let graph = replay_in_stable_order(&root.join("replay"), &snapshot, &records);
                    assert_graph_blame(&graph, &oid, None);
                    drop(graph);
                    drop(snapshot);
                    drop(index);
                    assert_eq!(
                        records_for(&VerifiedIndex::open_pinned(&index_root).unwrap(), OWNER),
                        records
                    );
                    let repeated =
                        refresh_source_backed_generation(&index_root, &registry, writer_options())
                            .unwrap();
                    assert_eq!(repeated.commit.generation_id, appended.commit.generation_id);
                    assert_attribution_noop(&data, &repeated.commit.generation_id);
                    let cold_root = root.join("cold");
                    fs::create_dir(&cold_root).unwrap();
                    let cold_data = data_root(&cold_root);
                    let cold_index_root = cold_data.join("search/lexical");
                    let cold = refresh_source_backed_generation(
                        &cold_index_root,
                        &registry,
                        writer_options(),
                    )
                    .unwrap();
                    let cold_records = records_for(
                        &VerifiedIndex::open_pinned(&cold_index_root).unwrap(),
                        OWNER,
                    );
                    assert_eq!(cold_records, records);
                    assert_call_blame(
                        &cold_data,
                        &cold.commit.generation_id,
                        &oid,
                        &cold_records,
                        None,
                    );
                }
            }
        }
    }
}

fn envelope_for_call(repository: &Path, output: &str, call: &str) -> Vec<u8> {
    jsonl_bytes(
        envelope(repository, output, "primary")
            .split(|byte| *byte == b'\n')
            .filter(|line| !line.is_empty())
            .map(|line| {
                let mut row: Value = serde_json::from_slice(line).unwrap();
                if row["payload"]["call_id"] == "commit-call" {
                    row["payload"]["call_id"] = json!(call);
                }
                row
            }),
    )
}

#[test]
fn appended_duplicate_withdraws_prior_blame_and_matches_cold_reopen_and_noop() {
    let temp = tempdir().unwrap();
    let repository = temp.path().join("repository");
    let (output, oid) = real_commit(&repository);
    let (conflicting, _) = next_commit(&repository, "conflicting witness");
    let (unrelated, unrelated_oid) = next_commit(&repository, "unrelated witness");
    for reverse in [false, true] {
        for mode in ["identical", "conflicting", "failed"] {
            let root = temp.path().join(format!("{reverse}-{mode}"));
            let sessions = root.join("sessions");
            fs::create_dir_all(&sessions).unwrap();
            let path = session_path(&sessions, OWNER);
            let mut bytes = envelope(&repository, &output, "primary");
            bytes.extend(jsonl_bytes([
                commit_call(&repository, "unrelated-call"),
                exact_exec_result("unrelated-call", &unrelated),
            ]));
            fs::write(&path, bytes).unwrap();
            let data = data_root(&root);
            let index_root = data.join("search/lexical");
            let registry = register_tree(&[&sessions]);
            let seed =
                refresh_source_backed_generation(&index_root, &registry, writer_options()).unwrap();
            let index = VerifiedIndex::open_pinned(&index_root).unwrap();
            assert!(seed.failed_routes.is_empty());
            let call = native_call_in_order(&records_for(&index, OWNER)[..2], reverse);
            drop(index);
            let mut bytes = envelope_for_call(&repository, &output, &call);
            bytes.extend(jsonl_bytes([
                commit_call(&repository, "unrelated-call"),
                exact_exec_result("unrelated-call", &unrelated),
                commit_call(&repository, "pending-without-terminal"),
            ]));
            fs::write(&path, bytes).unwrap();
            let prefix =
                refresh_source_backed_generation(&index_root, &registry, writer_options()).unwrap();
            assert!(prefix.failed_routes.is_empty());
            let index = VerifiedIndex::open_pinned(&index_root).unwrap();
            let before = records_for(&index, OWNER);
            let page = index
                .source_event_page(&before[0].source, None, 256)
                .unwrap();
            assert!(page.terminal);
            let position = |record: &CoreRecord| {
                page.items
                    .iter()
                    .position(|item| item.event_id == record.event_id)
                    .unwrap()
            };
            assert_eq!(position(&before[1]) < position(&before[0]), reverse);
            assert_call_blame(
                &data,
                &prefix.commit.generation_id,
                &oid,
                &before,
                Some(&call),
            );
            let result = ctx_attribution::query(
                &data,
                &BlameTarget::Commit {
                    oid: oid.clone(),
                    repository: None,
                },
                10,
                None,
            )
            .unwrap()
            .result;
            let expected_owner = &before[if reverse { 0 } else { 1 }];
            assert!(result
                .evidence
                .iter()
                .any(|evidence| evidence.citation.event_id == expected_owner.event_id));
            let snapshot = CoreSnapshot::open(
                &data,
                &prefix.commit.generation_id,
                &SnapshotContract::current().unwrap(),
            )
            .unwrap();
            let graph = replay_in_stable_order(&root.join("prefix-pages"), &snapshot, &before);
            assert_graph_blame(&graph, &oid, Some(expected_owner));
            drop(graph);
            drop(snapshot);
            drop(index);
            let mut terminal = duplicate(&output, &conflicting, mode);
            terminal["payload"]["call_id"] = json!(call);
            append_event(&path, terminal);
            // Cover both suffix-only continuation and ordinary refresh, whose
            // physical preflight rereads the certified prefix before its suffix.
            let (appended, completed) = if reverse {
                (
                    refresh_source_backed_generation(&index_root, &registry, writer_options())
                        .unwrap(),
                    None,
                )
            } else {
                let (receipt, completed) = incremental_refresh(&index_root, &registry, &prefix);
                (receipt, Some(completed))
            };
            assert!(appended.failed_routes.is_empty());
            assert_ne!(appended.commit.generation_id, prefix.commit.generation_id);
            let index = VerifiedIndex::open_pinned(&index_root).unwrap();
            let records = records_for(&index, OWNER);
            if let Some(completed) = completed {
                assert_eq!(
                    completed,
                    records.len() as u64,
                    "prefix overlap must replace before publication"
                );
            }
            assert_unlinked(&records[1]);
            assert_unlinked(records.last().unwrap());
            let mut expected_invocation = before[0].clone();
            expected_invocation
                .content
                .activity
                .as_mut()
                .unwrap()
                .provider_call_id = None;
            expected_invocation
                .content
                .activity
                .as_mut()
                .unwrap()
                .invocation = None;
            assert_eq!(records[0], expected_invocation);
            assert_eq!(
                records[2..5],
                before[2..5],
                "unrelated and pending invocations stay unchanged"
            );
            assert_eq!(records[1].event_id, before[1].event_id);
            assert_eq!(records[1].native_event_id, before[1].native_event_id);
            assert_eq!(
                records[1].content.normalized_body,
                before[1].content.normalized_body
            );
            assert_blame(&data, &appended.commit.generation_id, &oid, &records, false);
            assert_call_blame(
                &data,
                &appended.commit.generation_id,
                &unrelated_oid,
                &records,
                Some("unrelated-call"),
            );
            drop(index);
            let reopened = VerifiedIndex::open_pinned(&index_root).unwrap();
            assert_eq!(records_for(&reopened, OWNER), records);
            drop(reopened);
            let (repeated, _) =
                incremental_refresh(&index_root, &register_tree(&[&sessions]), &appended);
            assert_eq!(repeated.commit.generation_id, appended.commit.generation_id);
            assert_attribution_noop(&data, &repeated.commit.generation_id);
            let cold_root = root.join("cold");
            fs::create_dir(&cold_root).unwrap();
            let cold_data = data_root(&cold_root);
            let cold_index_root = cold_data.join("search/lexical");
            let cold =
                refresh_source_backed_generation(&cold_index_root, &registry, writer_options())
                    .unwrap();
            let cold_index = VerifiedIndex::open_pinned(&cold_index_root).unwrap();
            let cold_records = records_for(&cold_index, OWNER);
            assert_eq!(cold_records, records);
            assert_blame(
                &cold_data,
                &cold.commit.generation_id,
                &oid,
                &cold_records,
                false,
            );
            assert_call_blame(
                &cold_data,
                &cold.commit.generation_id,
                &unrelated_oid,
                &cold_records,
                Some("unrelated-call"),
            );
        }
    }
}

fn assert_saturated_checkpoint(index: &VerifiedIndex) {
    let (_, _, _, checkpoint) = provider_checkpoint_envelope(index, OWNER);
    assert_current_provider_checkpoint(&checkpoint);
    let encoded = checkpoint["Utf8"].as_str().unwrap();
    let value: Value = serde_json::from_str(
        encoded
            .strip_prefix("codex.projector-checkpoint.v8:")
            .unwrap(),
    )
    .unwrap();
    assert_eq!(value["terminal_authority"]["state"], "saturated");
}

#[test]
fn more_than_4096_unique_terminals_keep_blame_across_saturated_unique_and_duplicate_appends() {
    for direct_append in [false, true] {
        let temp = tempdir().unwrap();
        let repository = temp.path().join("repository");
        let (output, oid) = real_commit(&repository);
        let (after_output, after_oid) = next_commit(&repository, "after checkpoint boundary");
        let (appended_output, appended_oid) = next_commit(&repository, "after saturated prefix");
        let sessions = temp.path().join("sessions");
        fs::create_dir(&sessions).unwrap();
        let path = session_path(&sessions, OWNER);
        let mut bytes = envelope(&repository, &output, "primary");
        // Self-contained MCP calls avoid an unrelated adapter pending-call ceiling;
        // every filler still has a distinct native invocation and terminal ID.
        bytes.extend(jsonl_bytes((0..4095).map(|index| {
            exact_mcp_result(&format!("unique-{index}"), "long session result")
        })));
        bytes.extend(jsonl_bytes([
            commit_call(&repository, "after-boundary"),
            exact_exec_result("after-boundary", &after_output),
        ]));
        fs::write(&path, bytes).unwrap();
        let data = data_root(temp.path());
        let index_root = data.join("search/lexical");
        let registry = register_tree(&[&sessions]);
        let prefix =
            refresh_source_backed_generation(&index_root, &registry, writer_options()).unwrap();
        assert!(prefix.failed_routes.is_empty());
        let index = VerifiedIndex::open_pinned(&index_root).unwrap();
        let before = records_for(&index, OWNER);
        assert_eq!(
            before
                .iter()
                .filter(|record| record.content.activity.as_ref().unwrap().result.is_some())
                .count(),
            4097
        );
        assert!(before.iter().all(|record| record
            .content
            .activity
            .as_ref()
            .unwrap()
            .provider_call_id
            .is_some()));
        assert_saturated_checkpoint(&index);
        assert_blame(&data, &prefix.commit.generation_id, &oid, &before, true);
        assert_call_blame(
            &data,
            &prefix.commit.generation_id,
            &after_oid,
            &before,
            Some("after-boundary"),
        );
        drop(index);
        append_event(&path, commit_call(&repository, "appended-unique"));
        append_event(
            &path,
            exact_exec_result("appended-unique", &appended_output),
        );
        let (unique_append, completed) = if direct_append {
            let (receipt, completed) =
                incremental_refresh(&index_root, &register_tree(&[&sessions]), &prefix);
            (receipt, Some(completed))
        } else {
            (
                refresh_source_backed_generation(&index_root, &registry, writer_options()).unwrap(),
                None,
            )
        };
        let index = VerifiedIndex::open_pinned(&index_root).unwrap();
        let unique_records = records_for(&index, OWNER);
        if let Some(completed) = completed {
            assert_eq!(
                completed,
                unique_records.len() as u64,
                "saturated prefix settles the complete source"
            );
        }
        assert_eq!(&unique_records[..before.len()], &before);
        assert_saturated_checkpoint(&index);
        assert_blame(
            &data,
            &unique_append.commit.generation_id,
            &oid,
            &unique_records,
            true,
        );
        assert_call_blame(
            &data,
            &unique_append.commit.generation_id,
            &after_oid,
            &unique_records,
            Some("after-boundary"),
        );
        assert_call_blame(
            &data,
            &unique_append.commit.generation_id,
            &appended_oid,
            &unique_records,
            Some("appended-unique"),
        );
        drop(index);
        append_event(&path, duplicate(&output, &output, "failed"));
        let (duplicate_append, completed) = if direct_append {
            let (receipt, completed) = incremental_refresh(&index_root, &registry, &unique_append);
            (receipt, Some(completed))
        } else {
            (
                refresh_source_backed_generation(&index_root, &registry, writer_options()).unwrap(),
                None,
            )
        };
        let index = VerifiedIndex::open_pinned(&index_root).unwrap();
        let records = records_for(&index, OWNER);
        if let Some(completed) = completed {
            assert_eq!(completed, records.len() as u64);
        }
        assert_unlinked(&records[1]);
        assert_unlinked(records.last().unwrap());
        assert_eq!(
            records[0]
                .content
                .activity
                .as_ref()
                .unwrap()
                .provider_call_id,
            None
        );
        assert_eq!(records[0].event_id, before[0].event_id);
        assert_eq!(&records[2..unique_records.len()], &unique_records[2..]);
        assert_eq!(records[1].event_id, before[1].event_id);
        assert_blame(
            &data,
            &duplicate_append.commit.generation_id,
            &oid,
            &records,
            false,
        );
        assert_call_blame(
            &data,
            &duplicate_append.commit.generation_id,
            &after_oid,
            &records,
            Some("after-boundary"),
        );
        assert_call_blame(
            &data,
            &duplicate_append.commit.generation_id,
            &appended_oid,
            &records,
            Some("appended-unique"),
        );
        drop(index);
        let cold_root = temp.path().join("cold");
        fs::create_dir(&cold_root).unwrap();
        let cold_data = data_root(&cold_root);
        let cold_index_root = cold_data.join("search/lexical");
        let cold = refresh_source_backed_generation(&cold_index_root, &registry, writer_options())
            .unwrap();
        let index = VerifiedIndex::open_pinned(&cold_index_root).unwrap();
        let cold_records = records_for(&index, OWNER);
        assert_eq!(cold_records, records);
        assert_blame(
            &cold_data,
            &cold.commit.generation_id,
            &oid,
            &cold_records,
            false,
        );
        assert_call_blame(
            &cold_data,
            &cold.commit.generation_id,
            &after_oid,
            &cold_records,
            Some("after-boundary"),
        );
        assert_call_blame(
            &cold_data,
            &cold.commit.generation_id,
            &appended_oid,
            &cold_records,
            Some("appended-unique"),
        );
        let (repeated, _) = incremental_refresh(&index_root, &registry, &duplicate_append);
        assert_eq!(
            repeated.commit.generation_id,
            duplicate_append.commit.generation_id
        );
        assert_attribution_noop(&data, &repeated.commit.generation_id);
    }
}
