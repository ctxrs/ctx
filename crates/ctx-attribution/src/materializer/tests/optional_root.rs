use super::*;

use std::path::Path;
use std::process::Command;

use crate::graph::segment_graph::SegmentGraph;
use crate::materializer::CoreGenerationStart;
use crate::protocol::{
    ActivityInvocation, ActivityJsonCapture, ActivityResult, ActivityTextCapture,
    CORE_ACTIVITY_REVISION, CoreActivity, LiteralFactKind, ProviderDeclaredFact,
};
use crate::query::{BlameFactFamily, BlameGraph, ResourceSelector};

fn git(root: &Path, args: &[&str]) -> TestResult<String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(args)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", root.join("unused-global-config"))
        .output()?;
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    Ok(String::from_utf8(output.stdout)?.trim().to_owned())
}

// Synthetic consumer-contract witness, not a provider capture or origin-policy change.
pub(super) fn record(root: &Path, oid: &str) -> TestResult<CoreRecord> {
    let source = SourceKey::derive(
        "codex",
        "codex_session_jsonl",
        "codex-nativepath-jsonl-v0",
        1,
        SourceAnchor::ProviderNative {
            namespace: "session".to_owned(),
            key: TypedKey::utf8("optional-root")?,
        },
    )?;
    let session = stable_entity(&source, StableEntityKind::Session, 0x31)?;
    let event = stable_entity(&source, StableEntityKind::Event, 0x41)?;
    let parent = stable_entity(&source, StableEntityKind::Session, 0x30)?;
    let mut record = CoreRecord::new_selected(
        event,
        session,
        source,
        1,
        "tool_output",
        "codex-nativepath-core-activity-v14-literal-patch-file-facts",
        "commit fixture".to_owned(),
    )?;
    record.parent_session_id = Some(parent);
    record.session_relationship = Some(ProviderNativeSessionRelationship::Forked);
    record.agent_scope = Some(AgentScope::Subagent);
    record.provider_session_id = Some("optional-root".to_owned());
    record.native_event_id = Some(TypedKey::utf8("commit-event")?);
    record.occurred_at_unix_ms = Some(1_700_000_000_000);
    record.content.activity = Some(CoreActivity {
        revision: CORE_ACTIVITY_REVISION,
        provider_call_id: Some(TypedKey::utf8("commit-call")?),
        invocation: Some(ActivityInvocation {
            protocol: None,
            server: None,
            tool: "exec_command".to_owned(),
            arguments: ActivityJsonCapture::Present {
                value: serde_json::Value::String(serde_json::json!({"cmd": "git commit -m fixture && git rev-parse HEAD", "workdir": root}).to_string()),
            },
            started_at_unix_ms: None,
        }),
        result: Some(ActivityResult {
            status: Some("success".to_owned()),
            completed_at_unix_ms: None,
            duration_ns: None,
            text: ActivityTextCapture::Present {
                value: oid.to_owned(),
            },
            structured_content: ActivityJsonCapture::Absent,
        }),
        facts: vec![
            ProviderDeclaredFact {
                kind: LiteralFactKind::ToolWorkdir,
                value: root.to_string_lossy().into_owned(),
            },
            ProviderDeclaredFact {
                kind: LiteralFactKind::File,
                value: "src/witness.rs".to_owned(),
            },
        ],
    });
    record.validate_contract()?;
    Ok(record)
}

#[test]
fn optional_root_survives_real_preparation_publication_and_query() -> TestResult {
    let temporary = tempfile::tempdir()?;
    let repository = temporary.path().join("repository");
    std::fs::create_dir_all(repository.join("src"))?;
    git(&repository, &["init", "-q"])?;
    git(&repository, &["config", "user.name", "ctx fixture"])?;
    git(
        &repository,
        &["config", "user.email", "fixture@example.invalid"],
    )?;
    git(
        &repository,
        &[
            "remote",
            "add",
            "origin",
            "https://github.com/example/optional-root.git",
        ],
    )?;
    std::fs::write(repository.join("src/witness.rs"), "// witness\n")?;
    git(&repository, &["add", "."])?;
    git(&repository, &["commit", "-qm", "fixture"])?;
    let oid = git(&repository, &["rev-parse", "HEAD"])?;

    for mode in ["rootless", "explicit", "copied", "unknown"] {
        let explicit_root = mode == "explicit";
        let eligible = matches!(mode, "rootless" | "explicit");
        let mut record = record(&repository, &oid)?;
        if explicit_root {
            record.root_session_id = record.parent_session_id;
        }
        if mode == "copied" {
            record.event_copy = Some(crate::protocol::ProviderNativeEventCopy {
                ancestor_session_id: record
                    .parent_session_id
                    .ok_or_else(|| io::Error::other("fixture parent"))?,
                ancestor_event_id: stable_entity(&record.source, StableEntityKind::Event, 0x42)?,
                proof: crate::protocol::ProviderNativeCopyProof::NativeEventIdentity,
            });
        } else if mode == "unknown" {
            record.agent_scope = Some(AgentScope::Primary);
            record.parent_session_id = None;
            record.session_relationship = None;
        }
        record.validate_contract()?;
        assert_eq!(
            crate::core_materialization::producer_authority_disposition(&record)
                .permits_positive_authority(),
            eligible
        );
        let root = temporary.path().join(format!("graph-{mode}"));

        let source = source_state(&record, 0x57);
        let generation = head(0x57, std::slice::from_ref(&source))?;
        let mut materializer = SegmentMaterializer::open(&root)?;
        let mut session = match materializer.start_core_generation(generation.clone())? {
            CoreGenerationStart::Started(session) => session,
            CoreGenerationStart::Current(_) => {
                return Err(io::Error::other("unexpected current graph").into());
            }
        };
        let reconciliations =
            session.reconcile_source_page(protocol(CoreSourceDeltaPage::new(
                "0".repeat(64),
                generation.core_generation_id.clone(),
                0,
                true,
                vec![CoreSourceDelta::Present(source)],
            ))?)?;
        session.ingest_event_pages(vec![CoreEventDeltaPage {
            materialization_id: "0".repeat(64),
            core_generation_id: generation.core_generation_id.clone(),
            reconciliation: reconciliations[0].clone(),
            page_index: 0,
            terminal: true,
            deltas: vec![CoreEventDelta::Added(record.clone())],
        }])?;
        session.activate()?;
        drop(materializer);
        let pinned = crate::graph::segment::FlatStore::new(&root)
            .open_active(SegmentGraph::flat_open_policy())?;
        let graph = SegmentGraph::from_pinned(pinned, None);
        let files = (&graph).resolve(
            &ResourceSelector {
                kind: ResourceKind::File,
                value: "src/witness.rs".to_owned(),
                repository: None,
            },
            10,
        )?;
        assert_eq!(files.len(), 1, "rootless direct file evidence must survive");
        let commits = (&graph).resolve_commits(std::slice::from_ref(&oid), None, 10)?;
        if !eligible {
            assert!(commits.is_empty(), "{mode} must not gain producer evidence");
            continue;
        }
        assert_eq!(commits.len(), 1, "admitted commit observation must survive");
        let facts =
            (&graph).blame_facts_page(&commits[0].1.id, BlameFactFamily::Commit, None, 10)?;
        assert_eq!(facts.items.len(), 1);
        let fact = &facts.items[0].0;
        assert_eq!(fact.root_run.is_some(), explicit_root);
        if let Some(root_run) = &fact.root_run {
            let roots = (&graph).resources(std::slice::from_ref(root_run), 10)?;
            assert_eq!(
                roots[0].display,
                record
                    .root_session_id
                    .ok_or_else(|| io::Error::other("fixture root"))?
                    .to_string()
            );
        }
        let actors = (&graph).resources(
            &[fact
                .direct_actor
                .clone()
                .ok_or_else(|| io::Error::other("missing direct actor"))?],
            10,
        )?;
        assert_eq!(actors[0].display, record.session_id.to_string());
        assert_eq!(fact.citations[0].0.event_id, record.event_id);
        assert_eq!(
            fact.citations[0].0.core_generation_id,
            generation.core_generation_id
        );
        // Root context is never needed to withdraw event-owned facts.
        drop(graph);
        let mut replacement = record.clone();
        let activity = replacement
            .content
            .activity
            .as_mut()
            .ok_or_else(|| io::Error::other("fixture activity"))?;
        activity.invocation = None;
        activity.result = None;
        activity.facts[1].value = "src/replacement.rs".to_owned();
        update(&root, &replacement, false, 0x58)?;
        assert_file(&root, "src/witness.rs", 0)?;
        assert_file(&root, "src/replacement.rs", 1)?;
        update(&root, &replacement, true, 0x59)?;
        assert_file(&root, "src/replacement.rs", 0)?;
    }
    Ok(())
}

fn assert_file(root: &Path, path: &str, count: usize) -> TestResult {
    let graph = SegmentGraph::from_pinned(
        crate::graph::segment::FlatStore::new(root).open_active(SegmentGraph::flat_open_policy())?,
        None,
    );
    let files = (&graph).resolve(
        &ResourceSelector {
            kind: ResourceKind::File,
            value: path.to_owned(),
            repository: None,
        },
        10,
    )?;
    assert_eq!(files.len(), count, "{path}");
    Ok(())
}

fn update(root: &Path, record: &CoreRecord, remove: bool, revision: u8) -> TestResult {
    let mut materializer = SegmentMaterializer::open(root)?;
    let mut source = source_state(record, revision);
    source.event_count = u64::from(!remove);
    let generation = head(revision, std::slice::from_ref(&source))?;
    let mut session = match materializer.start_core_generation(generation.clone())? {
        CoreGenerationStart::Started(session) => session,
        CoreGenerationStart::Current(_) => {
            return Err(io::Error::other("update unexpectedly current").into());
        }
    };
    let reconciliations = session.reconcile_source_page(protocol(CoreSourceDeltaPage::new(
        "0".repeat(64),
        generation.core_generation_id.clone(),
        0,
        true,
        vec![CoreSourceDelta::Present(source)],
    ))?)?;
    let (states, terminal) = session.event_states(&reconciliations[0], None)?;
    assert!(terminal);
    assert_eq!(states.len(), 1);
    let prior = states[0].core_record_sha256.clone();
    let delta = if remove {
        CoreEventDelta::Tombstoned(crate::protocol::CoreEventTombstone {
            event_id: record.event_id,
            prior_core_record_sha256: prior,
        })
    } else {
        CoreEventDelta::Replaced(crate::protocol::CoreEventReplacement {
            prior_core_record_sha256: prior,
            record: record.clone(),
        })
    };
    session.ingest_event_pages(vec![CoreEventDeltaPage {
        materialization_id: "0".repeat(64),
        core_generation_id: generation.core_generation_id,
        reconciliation: reconciliations[0].clone(),
        page_index: 0,
        terminal: true,
        deltas: vec![delta],
    }])?;
    session.activate()?;
    Ok(())
}
