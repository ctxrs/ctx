//! Authored retained-Core witnesses with real isolated Git output, not native captures.
//! The primary call and result are independent events, with no invented parent.
use super::native_shell::git;
use super::*;
use crate::graph::segment::{FlatStore, SegmentStore};
use crate::graph::segment_graph::SegmentGraph;
use crate::materializer::CoreGenerationStart;
use crate::protocol::{
    ActivityInvocation, ActivityJsonCapture, ActivityResult, ActivityTextCapture, BlameTarget,
    CORE_ACTIVITY_REVISION, CoreActivity, CoreEventReplacement, LiteralFactKind,
    ProviderDeclaredFact, ProviderNativeCopyProof, ProviderNativeEventCopy,
};
use crate::query::{
    AttributionOutcome, BlameEntry, BlameGraph, BlameService, QueryBounds, QueryError,
    ResourceSelector,
};
use std::path::Path;

const PREDECESSOR: &str = "2026.09.24.1+bounded-omissions";

fn primary_records(repository: &Path, output: &str, mode: &str) -> TestResult<Vec<CoreRecord>> {
    let provider = match mode {
        "opencode-v13" | "opencode-v14" | "opencode-v13-child" | "opencode-v14-child" => "opencode",
        other => other,
    };
    let mut provider_contract = match provider {
        "mux" | "openclaw" | "crush" | "opencode" | "zed" | "gemini" => Some(
            crate::core_materialization::provider_contract_test_support::provider_record(
                provider, false,
            )?,
        ),
        _ => None,
    };
    if mode.starts_with("opencode-v13") {
        provider_contract.as_mut().unwrap().parser_revision =
            "opencode-family-source-backed-v13-bounded-oversized-content".to_owned();
    }
    let source = if let Some(contract) = &provider_contract {
        contract.source.clone()
    } else {
        SourceKey::derive(
            if mode == "provider" { "fx" } else { "codex" },
            if mode == "format" {
                "unknown"
            } else {
                "codex_session_jsonl"
            },
            if mode == "schema" {
                "unknown"
            } else {
                "codex-nativepath-jsonl-v0"
            },
            if mode == "identity" { 2 } else { 1 },
            SourceAnchor::provider_native("session", TypedKey::utf8("primary-commit")?)?,
        )?
    };
    let session_id = stable_entity(&source, StableEntityKind::Session, 0x31)?;
    let mut records = Vec::new();
    for sequence in 1..=2 {
        let is_call = sequence == 1;
        let mut record = CoreRecord::new_selected(
            stable_entity(&source, StableEntityKind::Event, 0x40 + sequence as u8)?,
            session_id,
            source.clone(),
            sequence,
            if is_call { "tool_call" } else { "tool_output" },
            if let Some(contract) = &provider_contract {
                &contract.parser_revision
            } else {
                match mode {
                    "v11" | "v11-child" => "codex-nativepath-core-activity-v11-item-call-identity",
                    "v14" | "v14-child" => {
                        "codex-nativepath-core-activity-v14-literal-patch-file-facts"
                    }
                    "revision" => "codex-nativepath-core-activity-v15-revert-lineage-unknown",
                    "v15" | "v15-child" => "codex-nativepath-core-activity-v15-revert-lineage",
                    _ => "codex-nativepath-core-activity-v16-audited-primary-lineage",
                }
            },
            if is_call {
                "git commit -m 'primary witness'"
            } else {
                output
            },
        )?;
        record.provider_session_id = Some("primary-commit".to_owned());
        record.native_event_id = Some(TypedKey::U64(sequence));
        record.agent_scope = Some(AgentScope::Primary);
        record.session_relationship = Some(ProviderNativeSessionRelationship::Root);
        record.root_session_id = Some(session_id);
        if provider_contract.is_some() {
            record.session_relationship = None;
            record.root_session_id = None;
        }
        if mode.ends_with("-child") {
            record.agent_scope = Some(AgentScope::Subagent);
            record.parent_session_id =
                Some(stable_entity(&source, StableEntityKind::Session, 0x30)?);
            record.session_relationship = Some(ProviderNativeSessionRelationship::Delegated);
            record.root_session_id = None;
        }
        record.role = Some(if is_call { "assistant" } else { "tool" }.to_owned());
        record.occurred_at_unix_ms = Some(1_700_000_000_000 + sequence as i64);
        record.content.activity = Some(CoreActivity {
            revision: CORE_ACTIVITY_REVISION,
            provider_call_id: Some(TypedKey::utf8(if mode == "mismatched-call" && !is_call {
                "other-call"
            } else {
                "commit-call"
            })?),
            invocation: is_call.then(|| ActivityInvocation {
                protocol: None,
                server: None,
                tool: "exec_command".to_owned(),
                arguments: ActivityJsonCapture::Present {
                    value: serde_json::Value::String(
                        serde_json::json!({
                            "cmd": "git commit -m 'primary witness'", "workdir": repository,
                        })
                        .to_string(),
                    ),
                },
                started_at_unix_ms: None,
            }),
            result: (!is_call).then(|| ActivityResult {
                status: Some(
                    if mode == "failed" {
                        "failed"
                    } else {
                        "success"
                    }
                    .to_owned(),
                ),
                completed_at_unix_ms: None,
                duration_ns: None,
                text: ActivityTextCapture::Present {
                    value: output.to_owned(),
                },
                structured_content: ActivityJsonCapture::Absent,
            }),
            // Literal command/workdir keeps the unknown-provider controls from
            // merely failing the Codex argument decoder before the origin gate.
            facts: if is_call {
                vec![
                    ProviderDeclaredFact {
                        kind: LiteralFactKind::Command,
                        value: "git commit -m 'primary witness'".to_owned(),
                    },
                    ProviderDeclaredFact {
                        kind: LiteralFactKind::ToolWorkdir,
                        value: repository.to_string_lossy().into_owned(),
                    },
                    ProviderDeclaredFact {
                        kind: LiteralFactKind::File,
                        value: "witness.txt".to_owned(),
                    },
                ]
            } else {
                Vec::new()
            },
        });
        match mode {
            "copied" => {
                record.event_copy = Some(ProviderNativeEventCopy {
                    ancestor_session_id: stable_entity(&source, StableEntityKind::Session, 0x30)?,
                    ancestor_event_id: stable_entity(
                        &source,
                        StableEntityKind::Event,
                        0x50 + sequence as u8,
                    )?,
                    proof: ProviderNativeCopyProof::NativeCallResultIdentity,
                })
            }
            "unknown-lineage" => {
                record.agent_scope = None;
                record.session_relationship = None;
                record.root_session_id = None;
            }
            "optional-root" => record.root_session_id = None,
            "missing-native-event" => record.native_event_id = None,
            "null-native-event" => record.native_event_id = Some(TypedKey::Null),
            "mismatched-output" if !is_call => {
                record
                    .content
                    .activity
                    .as_mut()
                    .unwrap()
                    .result
                    .as_mut()
                    .unwrap()
                    .text = ActivityTextCapture::Present {
                    value: "[main 0000000000000000000000000000000000000000] primary witness"
                        .to_owned(),
                };
            }
            _ => {}
        }
        record.validate_contract()?;
        assert_eq!(record.parent_session_id.is_some(), mode.ends_with("-child"));
        records.push(record);
    }
    if mode == "duplicate-call" {
        let mut duplicate = records[0].clone();
        duplicate.event_id = records[1].event_id;
        duplicate.event_sequence = 2;
        duplicate.native_event_id = Some(TypedKey::U64(2));
        records[1].event_id = stable_entity(&source, StableEntityKind::Event, 0x43)?;
        records[1].event_sequence = 3;
        records[1].native_event_id = Some(TypedKey::U64(3));
        records.insert(1, duplicate);
    }
    Ok(records)
}

fn source_and_head(records: &[CoreRecord]) -> TestResult<(CoreSourceState, CoreGenerationHead)> {
    let mut source = source_state(&records[0], 0x69);
    source.event_count = records.len() as u64;
    let generation = head(0x69, std::slice::from_ref(&source))?;
    Ok((source, generation))
}

fn materialize(
    root: &Path,
    records: &[CoreRecord],
) -> TestResult<crate::protocol::CoreMaterializationReceipt> {
    let (source, head) = source_and_head(records)?;
    let mut materializer =
        SegmentMaterializer::open_for_revision(root, CORE_MATERIALIZER_REVISION)?;
    let mut session = match materializer.start_core_generation(head.clone())? {
        CoreGenerationStart::Started(session) => session,
        CoreGenerationStart::Current(_) => {
            return Err(io::Error::other("must rederive old policy").into());
        }
    };
    let reconciliations = session.reconcile_source_page(protocol(CoreSourceDeltaPage::new(
        "0".repeat(64),
        head.core_generation_id.clone(),
        0,
        true,
        vec![CoreSourceDelta::Present(source)],
    ))?)?;
    assert_eq!(reconciliations.len(), 1);
    let (prior, terminal) = session.event_states(&reconciliations[0], None)?;
    assert!(terminal);
    for (index, record) in records.iter().enumerate() {
        let delta = match prior.iter().find(|state| state.event_id == record.event_id) {
            Some(state) => {
                assert!(state.requires_replacement, "same bytes must be rederived");
                CoreEventDelta::Replaced(CoreEventReplacement {
                    prior_core_record_sha256: state.core_record_sha256.clone(),
                    record: record.clone(),
                })
            }
            None => CoreEventDelta::Added(record.clone()),
        };
        // Separate pages exercise the retained call/result join across a page boundary.
        session.ingest_event_pages(vec![CoreEventDeltaPage {
            materialization_id: "0".repeat(64),
            core_generation_id: head.core_generation_id.clone(),
            reconciliation: reconciliations[0].clone(),
            page_index: index as u32,
            terminal: index + 1 == records.len(),
            deltas: vec![delta],
        }])?;
    }
    Ok(session.activate()?)
}

fn query(root: &Path, records: &[CoreRecord], oid: &str) -> TestResult<Vec<AttributionOutcome>> {
    let graph = SegmentGraph::from_pinned(
        FlatStore::new(root).open_active(SegmentGraph::flat_open_policy())?,
        None,
    );
    assert_eq!(
        (&graph)
            .resolve(
                &ResourceSelector {
                    kind: ResourceKind::File,
                    value: "witness.txt".to_owned(),
                    repository: None,
                },
                10
            )?
            .len(),
        1,
        "neutral file evidence survives producer abstention"
    );
    let page = match BlameService::new(&graph, QueryBounds::default())?.execute(
        &BlameTarget::Commit {
            oid: oid.to_owned(),
            repository: None,
        },
        None,
    ) {
        Ok(page) => page,
        Err(QueryError::TargetNotFound(ResourceKind::Commit)) => return Ok(Vec::new()),
        Err(error) => return Err(error.into()),
    };
    let mut outcomes = Vec::new();
    for entry in page.entries {
        let BlameEntry::Commit(entry) = entry else {
            return Err(io::Error::other("commit entry").into());
        };
        assert_eq!(
            entry
                .direct_actor
                .as_ref()
                .map(|actor| actor.display.as_str()),
            Some(records[1].session_id.to_string().as_str())
        );
        assert_eq!(entry.fact.citations[0].0.event_id, records[1].event_id);
        assert_eq!(
            entry.fact.root_run.is_some(),
            records[1].root_session_id.is_some()
        );
        outcomes.push(entry.attribution);
    }
    Ok(outcomes)
}

fn real_commit(repository: &Path) -> TestResult<(String, String)> {
    std::fs::create_dir(repository)?;
    git(repository, &["init", "-q"])?;
    git(repository, &["config", "user.name", "fixture"])?;
    git(
        repository,
        &["config", "user.email", "fixture@example.invalid"],
    )?;
    git(
        repository,
        &[
            "remote",
            "add",
            "origin",
            "https://github.com/example/primary-witness.git",
        ],
    )?;
    std::fs::write(repository.join("witness.txt"), "primary witness\n")?;
    git(repository, &["add", "witness.txt"])?;
    let output = git(repository, &["commit", "-m", "primary witness"])?;
    let oid = git(repository, &["rev-parse", "HEAD"])?;
    Ok((output, oid))
}

#[test]
fn primary_native_commit_call_and_output_reach_blame_with_exact_controls() -> TestResult {
    let temp = tempfile::tempdir()?;
    let repository = temp.path().join("repository");
    let (output, oid) = real_commit(&repository)?;
    for mode in [
        "primary",
        "optional-root",
        "v11",
        "v14",
        "v15",
        "v11-child",
        "v14-child",
        "v15-child",
        "copied",
        "failed",
        "mismatched-call",
        "mismatched-output",
        "duplicate-call",
        "unknown-lineage",
        "missing-native-event",
        "null-native-event",
        "provider",
        "format",
        "schema",
        "identity",
        "revision",
        "mux",
        "openclaw",
        "crush",
        "opencode-v13",
        "opencode-v14",
        "opencode-v13-child",
        "opencode-v14-child",
        "zed",
        "gemini",
    ] {
        let records = primary_records(&repository, &output, mode)?;
        let original = records
            .iter()
            .map(CoreRecord::encode_stored)
            .collect::<Result<Vec<_>, _>>()?;
        let root = temp.path().join(mode);
        materialize(&root, &records)?;
        assert_eq!(
            query(&root, &records, &oid)?,
            if matches!(
                mode,
                "primary"
                    | "optional-root"
                    | "v11-child"
                    | "v14-child"
                    | "v15-child"
                    | "mux"
                    | "openclaw"
                    | "crush"
                    | "opencode-v13"
                    | "opencode-v14"
                    | "opencode-v13-child"
                    | "opencode-v14-child"
                    | "zed"
            ) {
                vec![AttributionOutcome::Possible]
            } else {
                Vec::new()
            },
            "{mode}"
        );
        assert_eq!(
            original,
            records
                .iter()
                .map(CoreRecord::encode_stored)
                .collect::<Result<Vec<_>, _>>()?
        );
    }
    Ok(())
}

#[test]
fn primary_commit_same_core_upgrade_repairs_abstention_then_is_current() -> TestResult {
    let temp = tempfile::tempdir()?;
    let repository = temp.path().join("repository");
    let (output, oid) = real_commit(&repository)?;
    for mode in [
        "primary",
        "v11",
        "v14",
        "v15",
        "mux",
        "openclaw",
        "crush",
        "opencode-v13",
        "opencode-v14",
        "opencode-v13-child",
        "opencode-v14-child",
        "zed",
        "gemini",
        "copied",
        "revision",
        "failed",
        "mismatched-call",
        "mismatched-output",
        "duplicate-call",
    ] {
        let records = primary_records(&repository, &output, mode)?;
        let original = records
            .iter()
            .map(CoreRecord::encode_stored)
            .collect::<Result<Vec<_>, _>>()?;
        let (source, head) = source_and_head(&records)?;
        let warm = temp.path().join(format!("warm-{mode}"));
        let cold = temp.path().join(format!("cold-{mode}"));
        super::codex_policy_revision::seed_predecessor_records(
            &warm,
            &records,
            &source,
            &head,
            PREDECESSOR,
            if mode == "copied" {
                IndexedCoreEventOriginKind::CopiedFromAncestor
            } else {
                IndexedCoreEventOriginKind::Unknown
            },
        )?;
        let old = SegmentStore::new(&warm).load_active()?.unwrap();
        assert_eq!(old.core_receipt.materializer_revision, PREDECESSOR);
        let repaired = materialize(&warm, &records)?;
        assert_eq!(repaired, materialize(&cold, &records)?);
        assert_eq!(repaired.core_generation_id, head.core_generation_id);
        assert_eq!(repaired.materializer_revision, CORE_MATERIALIZER_REVISION);
        let expected = if matches!(
            mode,
            "primary"
                | "mux"
                | "openclaw"
                | "crush"
                | "opencode-v13"
                | "opencode-v14"
                | "opencode-v13-child"
                | "opencode-v14-child"
                | "zed"
        ) {
            vec![AttributionOutcome::Possible]
        } else {
            Vec::new()
        };
        assert_eq!(query(&warm, &records, &oid)?, expected, "{mode}");
        assert_eq!(query(&cold, &records, &oid)?, expected, "{mode}");
        let before = SegmentStore::new(&warm).load_active()?.unwrap();
        assert_ne!(before.generation_id, old.generation_id);
        let mut reopened =
            SegmentMaterializer::open_for_revision(&warm, CORE_MATERIALIZER_REVISION)?;
        assert!(
            matches!(reopened.start_core_generation(head)?, CoreGenerationStart::Current(receipt) if receipt == repaired)
        );
        drop(reopened);
        assert_eq!(
            before.generation_id,
            SegmentStore::new(&warm)
                .load_active()?
                .unwrap()
                .generation_id
        );
        assert_eq!(
            original,
            records
                .iter()
                .map(CoreRecord::encode_stored)
                .collect::<Result<Vec<_>, _>>()?
        );
    }
    Ok(())
}
