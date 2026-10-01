//! Authored native envelopes pass through capture, retained Core, and public Blame.
//! Git output is produced in an isolated repository; no live provider history is used.
use super::*;
use ctx_attribution::protocol::{BlameAttribution, BlameDiagnosticReason, BlameMatch, BlameTarget};
use ctx_history_core::AgentScope;
use ctx_history_snapshot_reader::{CoreSnapshot, SnapshotContract};
use std::{os::unix::fs::PermissionsExt, process::Command};

const OWNER: &str = "019fb000-0000-7000-8000-0000000000b1";
const PARENT: &str = "019fb000-0000-7000-8000-0000000000b0";

#[test]
fn valid_mcp_pair_and_standalone_keep_native_fidelity_without_builtin_blame() {
    use serde_json::json;
    let temp = tempdir().unwrap();
    let repository = temp.path().join("repository");
    let (output, oid) = real_commit(&repository);
    for original in [
        "legacy-search",
        "namespaced-exec",
        "explicit-mcp",
        "standalone",
    ] {
        for direct in [false, true] {
            let root = temp.path().join(format!("{original}-{direct}"));
            let sessions = root.join("sessions");
            fs::create_dir_all(&sessions).unwrap();
            let path = session_path(&sessions, OWNER);
            let mut meta = session_meta(OWNER, ProviderNativeSessionRelationship::Root, None);
            meta["payload"]["cwd"] = json!(repository);
            let mut invocation =
                exec_call_with_command("mcp", "git commit -m 'native primary witness'");
            let arguments = if original == "legacy-search" {
                json!({"query":"terminal uniqueness"})
            } else {
                json!({"cmd":"git commit -m 'native primary witness'", "workdir":repository})
            };
            invocation["payload"]["arguments"] = json!(arguments.to_string());
            match original {
                "legacy-search" => invocation["payload"]["name"] = json!("mcp__ctx__search"),
                "namespaced-exec" => invocation["payload"]["namespace"] = json!("mcp__ctx"),
                "explicit-mcp" => {
                    invocation["payload"]["protocol"] = json!("mcp");
                    invocation["payload"]["server"] = json!("ctx");
                    invocation["payload"]["tool"] = json!("exec_command");
                }
                _ => {}
            }
            let mut mcp = exact_mcp_result("mcp", &output);
            mcp["payload"]["invocation"]["arguments"] = arguments;
            if original != "legacy-search" {
                mcp["payload"]["invocation"]["tool"] = json!("exec_command");
            }
            let model = json!({"type":"response_item", "payload":{
                "type":"function_call_output", "call_id":"mcp",
                "output":format!("Wall time: 0.0000 seconds\nOutput:\n{output}")
            }});
            let mut rows = vec![meta];
            if original != "standalone" {
                rows.push(invocation.clone());
            }
            fs::write(&path, jsonl_bytes(rows)).unwrap();
            let data = data_root(&root);
            let index_root = data.join("search/lexical");
            let registry = register_tree(&[&sessions]);
            let prefix =
                refresh_source_backed_generation(&index_root, &registry, writer_options()).unwrap();
            assert!(prefix.failed_routes.is_empty());
            append_event(&path, mcp.clone());
            let first = if direct {
                incremental_refresh(&index_root, &registry, &prefix).0
            } else {
                refresh_source_backed_generation(&index_root, &registry, writer_options()).unwrap()
            };
            assert!(first.failed_routes.is_empty());
            let index = VerifiedIndex::open_pinned(&index_root).unwrap();
            let before = records_for(&index, OWNER);
            let activity = before.last().unwrap().content.activity.as_ref().unwrap();
            assert_eq!(
                activity.provider_call_id,
                Some(TypedKey::Utf8("mcp".to_owned()))
            );
            assert_eq!(
                activity.invocation.as_ref().unwrap().protocol.as_deref(),
                Some("mcp")
            );
            assert!(activity.result.is_some());
            assert_call_blame(&data, &first.commit.generation_id, &oid, &before, None);
            drop(index);
            append_event(&path, model.clone());
            let paired = if direct {
                incremental_refresh(&index_root, &registry, &first).0
            } else {
                refresh_source_backed_generation(&index_root, &registry, writer_options()).unwrap()
            };
            assert!(paired.failed_routes.is_empty());
            let index = VerifiedIndex::open_pinned(&index_root).unwrap();
            let records = records_for(&index, OWNER);
            assert_eq!(records.len(), if original == "standalone" { 2 } else { 3 });
            assert_eq!(&records[..before.len()], &before);
            for record in &records {
                record.validate_contract().unwrap();
            }
            if original != "standalone" {
                let first = &records[0];
                assert_eq!(
                    first.content.structured_content.as_ref(),
                    Some(&invocation["payload"])
                );
                let activity = first.content.activity.as_ref().unwrap();
                if original == "namespaced-exec" {
                    assert_eq!(activity.provider_call_id, None);
                    assert_eq!(activity.invocation, None);
                } else {
                    assert!(activity.provider_call_id.is_some());
                    assert!(activity.invocation.is_some());
                }
            }
            for (record, raw) in records[records.len() - 2..].iter().zip([&mcp, &model]) {
                assert_eq!(
                    record.content.structured_content.as_ref(),
                    Some(&raw["payload"])
                );
                let activity = record.content.activity.as_ref().unwrap();
                assert_eq!(
                    activity.provider_call_id,
                    Some(TypedKey::Utf8("mcp".to_owned()))
                );
                assert!(activity.result.is_some());
            }
            assert_call_blame(&data, &paired.commit.generation_id, &oid, &records, None);
        }
    }
}

fn git(repository: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .arg("-C")
        .arg(repository)
        .args(args)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", repository.join("no-global-config"))
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap().trim().to_owned()
}

fn real_commit(repository: &Path) -> (String, String) {
    fs::create_dir(repository).unwrap();
    git(repository, &["init", "-q"]);
    git(repository, &["config", "user.name", "fixture"]);
    git(
        repository,
        &["config", "user.email", "fixture@example.invalid"],
    );
    git(
        repository,
        &[
            "remote",
            "add",
            "origin",
            "https://github.com/example/native-primary.git",
        ],
    );
    fs::write(repository.join("witness.txt"), "native primary witness\n").unwrap();
    git(repository, &["add", "witness.txt"]);
    let output = git(repository, &["commit", "-m", "native primary witness"]);
    (output, git(repository, &["rev-parse", "HEAD"]))
}

fn envelope(repository: &Path, output: &str, mode: &str) -> Vec<u8> {
    use serde_json::json;
    let mut meta = session_meta(OWNER, ProviderNativeSessionRelationship::Root, None);
    meta["payload"]["cwd"] = json!(repository);
    match mode {
        "missing-source" => {
            meta["payload"].as_object_mut().unwrap().remove("source");
        }
        "delegated" => {
            meta["payload"]["source"] =
                json!({"subagent":{"thread_spawn":{"parent_thread_id":PARENT}}})
        }
        "numeric-parent" => {
            meta["payload"]["source"] = json!({"subagent":{"thread_spawn":{"parent_thread_id":17}}})
        }
        "null-parent" => {
            meta["payload"]["source"] =
                json!({"subagent":{"thread_spawn":{"parent_thread_id":null}}})
        }
        "empty-parent" => {
            meta["payload"]["source"] = json!({"subagent":{"thread_spawn":{"parent_thread_id":""}}})
        }
        "missing-parent" => meta["payload"]["source"] = json!({"subagent":{"thread_spawn":{}}}),
        "conflicting-root" => meta["payload"]["session_id"] = json!(PARENT),
        "copied" | "fork-local" => meta["payload"]["forked_from_id"] = json!(PARENT),
        _ => {}
    }
    let mut call = exec_call_with_command("commit-call", "git commit -m 'native primary witness'");
    call["payload"]["arguments"] = json!(json!({
        "cmd":"git commit -m 'native primary witness'", "workdir":repository
    })
    .to_string());
    let mut result = exact_exec_result(
        if mode == "mismatched-call" {
            "other-call"
        } else {
            "commit-call"
        },
        output,
    );
    if mode == "failed" {
        result["payload"]["status"] = json!("failed");
        result["payload"]["output"] = json!(format!(
            "Process exited with code 1\nFinal output:\n{output}"
        ));
    }
    let mut rows = vec![meta];
    if mode == "fork-local" {
        let mut context = turn_context();
        context["payload"]["cwd"] = json!(repository);
        rows.push(context);
    }
    rows.push(call);
    let mut bytes = jsonl_bytes(rows);
    let terminal = serde_json::to_string(&result).unwrap();
    let terminal = if mode == "ambiguous-terminal" {
        terminal.replacen(
            "\"call_id\":\"commit-call\"",
            "\"call_id\":\"commit-call\",\"call_id\":\"other-call\"",
            1,
        )
    } else {
        terminal
    };
    bytes.extend_from_slice(terminal.as_bytes());
    bytes.push(b'\n');
    bytes
}

fn data_root(root: &Path) -> PathBuf {
    let data = root.join("data");
    for path in [&data, &data.join("search"), &data.join("search/lexical")] {
        fs::create_dir(path).unwrap();
        fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
    }
    data
}

fn assert_blame(data: &Path, generation: &str, oid: &str, records: &[CoreRecord], expected: bool) {
    assert_call_blame(
        data,
        generation,
        oid,
        records,
        expected.then_some("commit-call"),
    );
}

fn assert_call_blame(
    data: &Path,
    generation: &str,
    oid: &str,
    records: &[CoreRecord],
    call: Option<&str>,
) {
    let snapshot =
        CoreSnapshot::open(data, generation, &SnapshotContract::current().unwrap()).unwrap();
    ctx_attribution::catch_up(data, &snapshot, &|| false).unwrap();
    let result = ctx_attribution::query(
        data,
        &BlameTarget::Commit {
            oid: oid.to_owned(),
            repository: None,
        },
        10,
        None,
    );
    let Some(call) = call else {
        assert_eq!(
            result.unwrap_err().reason,
            BlameDiagnosticReason::TargetNotIndexed
        );
        return;
    };
    let result = result.unwrap().result;
    assert_eq!(result.outcome.attribution, BlameAttribution::Possible);
    let result_record = result_record_for_call(records, call);
    assert!(!result.matches.is_empty());
    for entry in result.matches {
        let BlameMatch::Commit(entry) = entry else {
            panic!("expected commit match")
        };
        assert_eq!(
            entry.direct_actor.as_ref().unwrap().display,
            result_record.session_id.to_string()
        );
    }
    // Stable-ID replay can encounter the result first. The event completing
    // the join owns the citation, in either direction.
    assert!(result.evidence.iter().any(|evidence| {
        evidence.citation.session_id == result_record.session_id
            && evidence.citation.core_generation_id == generation
            && records.iter().any(|record| {
                record.event_id == evidence.citation.event_id
                    && record.content.activity.as_ref().is_some_and(|activity| {
                        activity.provider_call_id == Some(TypedKey::Utf8(call.to_owned()))
                    })
            })
    }));
}

#[path = "primary_blame/terminal_multiplicity.rs"]
mod terminal_multiplicity;

#[test]
fn native_primary_and_child_commit_proof_survives_but_uncertain_or_copied_proof_does_not() {
    let temp = tempdir().unwrap();
    let repository = temp.path().join("repository");
    let (output, oid) = real_commit(&repository);
    for mode in [
        "primary",
        "missing-source",
        "delegated",
        "fork-local",
        "numeric-parent",
        "null-parent",
        "empty-parent",
        "missing-parent",
        "conflicting-root",
        "copied",
        "failed",
        "mismatched-call",
        "ambiguous-terminal",
    ] {
        let root = temp.path().join(mode);
        fs::create_dir(&root).unwrap();
        let sessions = root.join("sessions");
        fs::create_dir(&sessions).unwrap();
        let bytes = envelope(&repository, &output, mode);
        fs::write(session_path(&sessions, OWNER), &bytes).unwrap();
        let data = data_root(&root);
        let index_root = data.join("search/lexical");
        let registry = register_tree(&[&sessions]);
        let first =
            refresh_source_backed_generation(&index_root, &registry, writer_options()).unwrap();
        assert!(
            first.failed_routes.is_empty(),
            "{mode}: {:?}",
            first.failed_routes
        );
        assert!(first.logical_source_failures.is_empty(), "{mode}");
        let index = VerifiedIndex::open_pinned(&index_root).unwrap();
        let records = records_for(&index, OWNER);
        assert!(records.len() >= 2, "native evidence retained: {mode}");
        assert!(records
            .iter()
            .all(|record| record.parser_revision == CURRENT_PARSER_REVISION));
        if matches!(
            mode,
            "numeric-parent"
                | "null-parent"
                | "empty-parent"
                | "missing-parent"
                | "conflicting-root"
        ) {
            for record in &records {
                assert_eq!(record.agent_scope, None, "{mode}");
                assert_eq!(record.session_relationship, None, "{mode}");
                assert_eq!(record.parent_session_id, None, "{mode}");
                assert_eq!(record.root_session_id, None, "{mode}");
            }
        }
        if mode == "copied" {
            assert!(records.iter().any(|record| record.event_copy.is_some()));
        }
        let expected = matches!(
            mode,
            "primary" | "missing-source" | "delegated" | "fork-local"
        );
        assert_blame(&data, &first.commit.generation_id, &oid, &records, expected);
        drop(index);
        let (repeated, _) = incremental_refresh(&index_root, &registry, &first);
        assert_eq!(
            repeated.commit.generation_id, first.commit.generation_id,
            "{mode}"
        );
        assert_eq!(fs::read(session_path(&sessions, OWNER)).unwrap(), bytes);
        if matches!(mode, "primary" | "numeric-parent" | "conflicting-root") {
            append_event(
                &session_path(&sessions, OWNER),
                message("ordinary appended message"),
            );
            let (appended, _) = incremental_refresh(&index_root, &registry, &repeated);
            assert!(appended.failed_routes.is_empty());
            let index = VerifiedIndex::open_pinned(&index_root).unwrap();
            let appended_records = records_for(&index, OWNER);
            assert_eq!(&appended_records[..records.len()], &records);
            assert_blame(
                &data,
                &appended.commit.generation_id,
                &oid,
                &appended_records,
                expected,
            );
        }
    }
}

#[test]
fn old_primary_metadata_requires_source_reparse_and_cannot_recover_discarded_claims_from_core() {
    let temp = tempdir().unwrap();
    let repository = temp.path().join("repository");
    let (output, oid) = real_commit(&repository);
    for revision in [
        "codex-nativepath-core-activity-v11-item-call-identity",
        "codex-nativepath-core-activity-v14-literal-patch-file-facts",
        "codex-nativepath-core-activity-v15-revert-lineage",
    ] {
        for mode in ["primary", "numeric-parent", "conflicting-root"] {
            let root = temp.path().join(format!("{revision}-{mode}"));
            fs::create_dir(&root).unwrap();
            let sessions = root.join("sessions");
            fs::create_dir(&sessions).unwrap();
            let bytes = envelope(&repository, &output, mode);
            fs::write(session_path(&sessions, OWNER), &bytes).unwrap();
            let data = data_root(&root);
            let index_root = data.join("search/lexical");
            let registry = register_tree(&[&sessions]);
            refresh_source_backed_generation(&index_root, &registry, writer_options()).unwrap();
            let index = VerifiedIndex::open_pinned(&index_root).unwrap();
            let current = certificate_for(&index, OWNER);
            let expected_records = records_for(&index, OWNER);
            // Model precisely the information loss in the released parser: all
            // three headers became the same Primary/Root/self shape in Core.
            let old_records = expected_records
                .iter()
                .cloned()
                .map(|mut record| {
                    record.parser_revision = revision.to_owned();
                    record.agent_scope = Some(AgentScope::Primary);
                    record.session_relationship = Some(ProviderNativeSessionRelationship::Root);
                    record.root_session_id = Some(record.session_id);
                    record
                })
                .collect::<Vec<_>>();
            let old_certificate = CertifiedSource::certify_with_frontier(
                current.observation().clone(),
                current.observation().clone(),
                revision,
                *current.content_digest(),
                current.counts(),
                current.frontier().cloned(),
            )
            .unwrap();
            drop(index);
            let old_generation =
                install_single_source_records(&index_root, old_certificate, old_records.clone());
            assert_blame(&data, &old_generation, &oid, &old_records, false);
            let refreshed =
                refresh_source_backed_generation(&index_root, &registry, writer_options()).unwrap();
            assert!(refreshed.failed_routes.is_empty());
            assert!(refreshed.logical_source_failures.is_empty());
            assert_ne!(refreshed.commit.generation_id, old_generation);
            let index = VerifiedIndex::open_pinned(&index_root).unwrap();
            let records = records_for(&index, OWNER);
            assert_eq!(records, expected_records, "{revision} {mode}");
            assert_eq!(
                certificate_for(&index, OWNER).parser_revision(),
                CURRENT_PARSER_REVISION
            );
            assert_blame(
                &data,
                &refreshed.commit.generation_id,
                &oid,
                &records,
                mode == "primary",
            );
            drop(index);
            let (repeated, _) = incremental_refresh(&index_root, &registry, &refreshed);
            assert_eq!(
                repeated.commit.generation_id,
                refreshed.commit.generation_id
            );
            assert_eq!(fs::read(session_path(&sessions, OWNER)).unwrap(), bytes);
        }
    }
}
