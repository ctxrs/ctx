use super::*;
use ctx_history_core::{ActivityJsonCapture, ActivityTextCapture, LiteralFactKind};
use ctx_repository_evidence::{CoreRepositoryEvidenceAdapter, RepositoryVcsObservationKind};
use sha2::{Digest, Sha256};

fn git(repo: &Path, args: &[&str]) -> String {
    let output = std::process::Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(args)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", repo.join("no-global-config"))
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap()
}

fn committed_oids(records: &[CoreRecord]) -> Vec<String> {
    let mut adapter = CoreRepositoryEvidenceAdapter::default();
    adapter.begin_source();
    records
        .iter()
        .flat_map(|record| {
            let digest = format!("{:x}", Sha256::digest(record.encode_stored().unwrap()));
            adapter
                .evaluate_record(record, &digest)
                .repository_vcs_observations
                .into_iter()
                .filter_map(|observation| match observation.kind {
                    RepositoryVcsObservationKind::Outcome(outcome) => {
                        Some(outcome.produced_object_ids)
                    }
                    _ => None,
                })
                .flatten()
                .map(|object| object.hex)
                .collect::<Vec<_>>()
        })
        .collect()
}

#[test]
fn pi_nested_and_flat_calls_join_native_result_blocks_to_a_real_commit() {
    // Authored Pi envelopes carrying output from an isolated real Git commit.
    let temp = crate::test_support_paths::tempdir().unwrap();
    let repo = temp.path().join("repository");
    fs::create_dir_all(&repo).unwrap();
    git(&repo, &["init", "-q"]);
    git(&repo, &["config", "user.name", "Fixture"]);
    git(&repo, &["config", "user.email", "fixture@example.invalid"]);
    git(
        &repo,
        &[
            "remote",
            "add",
            "origin",
            "https://github.com/example/pi-fixture.git",
        ],
    );
    fs::write(repo.join("witness.txt"), "Pi commit witness\n").unwrap();
    git(&repo, &["add", "witness.txt"]);
    let receipt = git(&repo, &["commit", "-m", "fixture"]);
    let oid = git(&repo, &["rev-parse", "HEAD"]).trim().to_owned();
    let output = format!("{receipt}{oid}\n");
    let command = "git commit -m fixture && git rev-parse HEAD";

    for shape in ["nested", "parallel", "flat"] {
        let root = temp.path().join(shape);
        let path = root.join("session.jsonl");
        let index = temp.path().join(format!("{shape}-index"));
        let call = json!({"type": "toolCall", "id": "commit-call", "name": "bash", "arguments": {"command": command}});
        let message = match shape {
            "parallel" => json!({"role": "assistant", "content": [
                {"type": "text", "text": "Run independent commands."},
                {"type": "toolCall", "id": "status-call", "name": "bash", "arguments": {"command": "git status --short"}},
                call
            ]}),
            "flat" => {
                json!({"role": "assistant", "toolCallId": "commit-call", "toolName": "bash", "arguments": {"command": command}})
            }
            _ => json!({"role": "assistant", "content": [call]}),
        };
        let mut rows = vec![
            json!({"type": "session", "version": 3, "id": shape, "timestamp": "2026-11-03T00:00:00Z", "cwd": repo}),
            json!({"type": "message", "id": "calls", "timestamp": "2026-11-03T00:00:01Z", "message": message}),
            json!({"type": "message", "id": "result", "timestamp": "2026-11-03T00:00:02Z", "message": {
                "role": "toolResult", "toolCallId": "commit-call", "toolName": "bash",
                "content": [{"type": "text", "text": output}], "isError": false
            }}),
        ];
        write_openclaw_lifecycle_transcript(&path, &rows);
        let original = fs::read(&path).unwrap();
        let registry = pi_registry(&root);
        let first = refresh_source_backed_generation(&index, &registry, writer_options()).unwrap();
        assert!(first.failed_routes.is_empty());
        let records = all_indexed_records(&index);
        let invocations = records
            .iter()
            .filter_map(|record| {
                let activity = record.content.activity.as_ref()?;
                Some((record, activity, activity.invocation.as_ref()?))
            })
            .collect::<Vec<_>>();
        assert_eq!(invocations.len(), if shape == "parallel" { 2 } else { 1 });
        let (call_record, activity, invocation) = invocations
            .iter()
            .find(|(_, activity, _)| {
                activity.provider_call_id == Some(TypedKey::Utf8("commit-call".to_owned()))
            })
            .unwrap();
        assert_eq!(invocation.tool, "bash");
        assert_eq!(
            invocation.arguments,
            ActivityJsonCapture::Present {
                value: json!({"command": command})
            }
        );
        let commands = activity
            .facts
            .iter()
            .filter(|fact| fact.kind == LiteralFactKind::Command)
            .map(|fact| fact.value.as_str())
            .collect::<Vec<_>>();
        assert_eq!(commands, [command]);
        let result_record = records
            .iter()
            .find(|record| record.event_type == "tool_output")
            .unwrap();
        let result_activity = result_record.content.activity.as_ref().unwrap();
        assert_eq!(activity.provider_call_id, result_activity.provider_call_id);
        assert_eq!(
            result_activity.result.as_ref().unwrap().text,
            ActivityTextCapture::Present {
                value: output.clone()
            }
        );
        assert!(call_record.event_sequence < result_record.event_sequence);
        assert_eq!(
            result_record.content.structured_content.as_ref().unwrap()["entry"]["message"],
            rows[2]["message"]
        );
        // Repository outcome derivation can now reach the OID through exact
        // invocation/result linkage. Producer-authority policy is separate.
        assert_eq!(
            committed_oids(&records),
            std::slice::from_ref(&oid),
            "{shape}"
        );
        let mut reversed = records.clone();
        reversed.reverse();
        assert_eq!(committed_oids(&reversed), std::slice::from_ref(&oid));
        let repeated =
            refresh_source_backed_generation(&index, &registry, writer_options()).unwrap();
        assert_eq!(repeated.commit.generation_id, first.commit.generation_id);
        assert_eq!(all_indexed_records(&index), records);
        assert_eq!(fs::read(&path).unwrap(), original);

        if shape == "nested" {
            for revision in [
                "pi-shared-jsonl-v11-omp-parent-lineage",
                "pi-shared-jsonl-v12-nested-tool-activity",
            ] {
                seed_predecessor_projection(&index, &records, revision);
                let migrated =
                    refresh_source_backed_generation(&index, &registry, writer_options()).unwrap();
                assert!(migrated.failed_routes.is_empty());
                assert_eq!(all_indexed_records(&index), records);
                assert!(records.iter().all(|record| record.parser_revision
                    == "pi-shared-jsonl-v13-nested-tool-parent-evidence"));
                assert_eq!(
                    committed_oids(&all_indexed_records(&index)),
                    std::slice::from_ref(&oid)
                );
            }
        }
        append_openclaw_lifecycle_transcript(
            &path,
            &json!({
                "type": "message", "id": "append", "timestamp": "2026-11-03T00:00:03Z",
                "message": {"role": "assistant", "content": "Finished."}
            }),
        );
        let appended =
            refresh_source_backed_generation(&index, &registry, writer_options()).unwrap();
        assert!(appended.failed_routes.is_empty());
        let appended_records = all_indexed_records(&index);
        assert_eq!(&appended_records[..records.len()], &records);
        assert_eq!(
            committed_oids(&appended_records),
            std::slice::from_ref(&oid)
        );

        for (field, value) in [("isError", json!(true)), ("toolCallId", json!("unmatched"))] {
            let previous = rows[2]["message"][field].clone();
            rows[2]["message"][field] = value;
            write_openclaw_lifecycle_transcript(&path, &rows);
            let changed =
                refresh_source_backed_generation(&index, &registry, writer_options()).unwrap();
            assert!(changed.failed_routes.is_empty());
            assert!(
                committed_oids(&all_indexed_records(&index)).is_empty(),
                "{shape}: {field}"
            );
            rows[2]["message"][field] = previous;
        }
    }
}

// Both predecessors lack the retained header observation. v11 also lacks the
// nested invocation and puts an envelope in the result payload.
fn seed_predecessor_projection(index: &Path, records: &[CoreRecord], revision: &str) {
    let v11 = revision == "pi-shared-jsonl-v11-omp-parent-lineage";
    let verified = VerifiedIndex::open_pinned(index).unwrap();
    let current = &verified.manifest().sources[0];
    let source = current.observation().source().clone();
    let mut certificate = serde_json::to_value(current).unwrap();
    certificate["parser_revision"] = json!(revision);
    let routes = verified.manifest().source_routes().to_vec();
    drop(verified);
    let mut writer = ctx_history_index::GenerationWriter::open(index, writer_options())
        .unwrap()
        .into_writer()
        .unwrap();
    writer.set_present_source_routes(routes).unwrap();
    writer.begin_source(source).unwrap();
    for mut record in records.iter().cloned() {
        record.parser_revision = revision.to_owned();
        record.content.structured_content = record
            .content
            .structured_content
            .take()
            .map(|mut value| value["entry"].take());
        if v11 {
            record.event_sequence /= u64::from(u32::MAX) + 1;
        }
        if let Some(activity) = record.content.activity.as_mut().filter(|_| v11) {
            if activity.invocation.take().is_some() {
                activity.provider_call_id = None;
            }
            if let Some(result) = activity.result.as_mut() {
                result.structured_content = ActivityJsonCapture::Present {
                    value: record.content.structured_content.as_ref().unwrap()["message"].clone(),
                };
                result.status = None;
            }
        }
        writer.add_core_record(record).unwrap();
    }
    writer
        .certify_source(serde_json::from_value(certificate).unwrap())
        .unwrap();
    writer.commit(|_| true).unwrap();
    if v11 {
        assert!(committed_oids(&all_indexed_records(index)).is_empty());
    }
}
