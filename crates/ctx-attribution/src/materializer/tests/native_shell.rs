use super::*;

use std::path::Path;
use std::process::Command;

use crate::graph::segment_graph::SegmentGraph;
use crate::materializer::CoreGenerationStart;
use crate::protocol::{
    ActivityInvocation, ActivityJsonCapture, ActivityResult, ActivityTextCapture, BlameTarget,
    CORE_ACTIVITY_REVISION, CoreActivity, LiteralFactKind, ProviderDeclaredFact,
};
use crate::query::{
    AttributionOutcome, BlameEntry, BlameGraph, BlameService, QueryBounds, QueryError,
    ResourceSelector,
};

pub(super) fn git(root: &Path, args: &[&str]) -> TestResult<String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(args)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", root.join("no-global-config"))
        .output()?;
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    Ok(String::from_utf8(output.stdout)?.trim().to_owned())
}

// Synthetic admitted Core consumer input carrying the actual Bash/Git result.
// This does not claim a newly captured native Codex result envelope.
pub(super) fn record(repository: &Path, command: &str, output: &str) -> TestResult<CoreRecord> {
    let source = SourceKey::derive(
        "codex",
        "codex_session_jsonl",
        "codex-nativepath-jsonl-v0",
        1,
        SourceAnchor::ProviderNative {
            namespace: "session".to_owned(),
            key: TypedKey::utf8("shell-evidence")?,
        },
    )?;
    let actor = stable_entity(&source, StableEntityKind::Session, 0x31)?;
    let parent = stable_entity(&source, StableEntityKind::Session, 0x30)?;
    let event = stable_entity(&source, StableEntityKind::Event, 0x41)?;
    let mut record = CoreRecord::new_selected(
        event,
        actor,
        source,
        1,
        "tool_output",
        "codex-nativepath-core-activity-v14-literal-patch-file-facts",
        "shell evidence".to_owned(),
    )?;
    record.parent_session_id = Some(parent);
    record.root_session_id = Some(parent);
    record.session_relationship = Some(ProviderNativeSessionRelationship::Forked);
    record.agent_scope = Some(AgentScope::Subagent);
    record.provider_session_id = Some("shell-evidence".to_owned());
    record.native_event_id = Some(TypedKey::utf8("shell-event")?);
    record.occurred_at_unix_ms = Some(1_700_000_000_000);
    record.content.activity = Some(CoreActivity {
        revision: CORE_ACTIVITY_REVISION,
        provider_call_id: Some(TypedKey::utf8("shell-call")?),
        invocation: Some(ActivityInvocation {
            protocol: None,
            server: None,
            tool: "exec_command".to_owned(),
            arguments: ActivityJsonCapture::Present {
                value: serde_json::Value::String(
                    serde_json::json!({"cmd": command, "workdir": repository}).to_string(),
                ),
            },
            started_at_unix_ms: None,
        }),
        result: Some(ActivityResult {
            status: Some("success".to_owned()),
            completed_at_unix_ms: None,
            duration_ns: None,
            text: ActivityTextCapture::Present {
                value: output.to_owned(),
            },
            structured_content: ActivityJsonCapture::Absent,
        }),
        facts: vec![
            ProviderDeclaredFact {
                kind: LiteralFactKind::ToolWorkdir,
                value: repository.to_string_lossy().into_owned(),
            },
            ProviderDeclaredFact {
                kind: LiteralFactKind::File,
                value: "witness.txt".to_owned(),
            },
        ],
    });
    record.validate_contract()?;
    Ok(record)
}

pub(super) fn materialize_and_query(
    root: &Path,
    record: CoreRecord,
    oid: &str,
) -> TestResult<Vec<AttributionOutcome>> {
    let source = source_state(&record, 0x63);
    let generation = head(0x63, std::slice::from_ref(&source))?;
    let mut materializer = SegmentMaterializer::open(root)?;
    let mut session = match materializer.start_core_generation(generation.clone())? {
        CoreGenerationStart::Started(session) => session,
        CoreGenerationStart::Current(_) => {
            return Err(io::Error::other("fresh graph unexpectedly current").into());
        }
    };
    let reconciliations = session.reconcile_source_page(protocol(CoreSourceDeltaPage::new(
        "0".repeat(64),
        generation.core_generation_id.clone(),
        0,
        true,
        vec![CoreSourceDelta::Present(source)],
    ))?)?;
    session.ingest_event_pages(vec![CoreEventDeltaPage {
        materialization_id: "0".repeat(64),
        core_generation_id: generation.core_generation_id,
        reconciliation: reconciliations[0].clone(),
        page_index: 0,
        terminal: true,
        deltas: vec![CoreEventDelta::Added(record.clone())],
    }])?;
    session.activate()?;
    drop(materializer);
    let graph = SegmentGraph::from_pinned(
        crate::graph::segment::FlatStore::new(root).open_active(SegmentGraph::flat_open_policy())?,
        None,
    );
    let files = (&graph).resolve(
        &ResourceSelector {
            kind: ResourceKind::File,
            value: "witness.txt".to_owned(),
            repository: None,
        },
        10,
    )?;
    assert_eq!(
        files.len(),
        1,
        "independent file evidence must survive shell abstention"
    );
    let repositories = (&graph).resolve(
        &ResourceSelector {
            kind: ResourceKind::Repository,
            value: "forge:github.com/example/shell-evidence".to_owned(),
            repository: None,
        },
        10,
    )?;
    assert_eq!(
        repositories.len(),
        1,
        "independent repository observation must survive"
    );
    let page = BlameService::new(&graph, QueryBounds::default())?.execute(
        &BlameTarget::Commit {
            oid: oid.to_owned(),
            repository: None,
        },
        None,
    );
    let page = match page {
        Ok(page) => page,
        Err(QueryError::TargetNotFound(ResourceKind::Commit)) => return Ok(Vec::new()),
        Err(error) => return Err(error.into()),
    };
    let mut outcomes = Vec::new();
    for entry in page.entries {
        let BlameEntry::Commit(entry) = entry else {
            return Err(io::Error::other("unexpected query entry").into());
        };
        assert_eq!(
            entry
                .direct_actor
                .as_ref()
                .map(|actor| actor.display.as_str()),
            Some(record.session_id.to_string().as_str())
        );
        assert_eq!(entry.fact.citations[0].0.event_id, record.event_id);
        assert_eq!(
            entry.fact.root_run.is_some(),
            record.root_session_id.is_some()
        );
        outcomes.push(entry.attribution);
    }
    Ok(outcomes)
}

#[cfg(unix)]
#[test]
fn shell_option_expansion_never_admits_possible_producer() -> TestResult {
    let temporary = tempfile::tempdir()?;
    for (index, (command, produces)) in [
        (
            r#"git commit -qm 'literal $MODE' && git rev-parse HEAD"#,
            true,
        ),
        (r#"git commit "$MODE" && git rev-parse HEAD"#, false),
        (
            r#"git commit "$(printf -- --dry-run)" && git rev-parse HEAD"#,
            false,
        ),
        (
            r#"git commit "`printf -- --dry-run`" && git rev-parse HEAD"#,
            false,
        ),
        (r#"git commit $'--dry\x2drun' && git rev-parse HEAD"#, false),
    ]
    .into_iter()
    .enumerate()
    {
        let repository = temporary.path().join(format!("repo-{index}"));
        std::fs::create_dir_all(&repository)?;
        git(&repository, &["init", "-q"])?;
        git(&repository, &["config", "user.name", "fixture"])?;
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
                "https://github.com/example/shell-evidence.git",
            ],
        )?;
        std::fs::write(repository.join("witness.txt"), "before\n")?;
        git(&repository, &["add", "witness.txt"])?;
        git(&repository, &["commit", "-qm", "before"])?;
        let prior = git(&repository, &["rev-parse", "HEAD"])?;
        std::fs::write(repository.join("witness.txt"), "after\n")?;
        git(&repository, &["add", "witness.txt"])?;
        let result = Command::new("bash")
            .args(["--noprofile", "--norc", "-c", command])
            .current_dir(&repository)
            .env("MODE", "--dry-run")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", repository.join("no-global-config"))
            .output()?;
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
        let current = git(&repository, &["rev-parse", "HEAD"])?;
        assert_eq!(
            current != prior,
            produces,
            "actual Git effect for {command}"
        );
        assert_eq!(
            git(&repository, &["diff", "--cached", "--name-only"])?.is_empty(),
            produces
        );
        let output = String::from_utf8(result.stdout)?;
        assert!(output.trim_end().ends_with(&current));
        let record = record(&repository, command, &output)?;
        let outcomes = materialize_and_query(
            &temporary.path().join(format!("graph-{index}")),
            record,
            &current,
        )?;
        if produces {
            assert_eq!(outcomes, vec![AttributionOutcome::Possible]);
        } else {
            assert_eq!(
                outcomes,
                Vec::<AttributionOutcome>::new(),
                "nonproducing shell command admitted attribution: {command}"
            );
        }
    }
    Ok(())
}
