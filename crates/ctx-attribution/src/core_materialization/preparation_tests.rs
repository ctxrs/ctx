use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use ctx_history_core::{
    ActivityInvocation, ActivityJsonCapture, CORE_ACTIVITY_REVISION, CoreActivity,
    CoreDiscoveryExclusion, LiteralFactKind, ProviderDeclaredFact,
};

use super::{CoreProjectionPreparer, PreparedCoreProjectionBatch};

fn privacy_golden_record() -> crate::protocol::CoreRecord {
    crate::test_support::core_record()
}

struct RepositoryFixture {
    _directory: tempfile::TempDir,
    root: PathBuf,
}

fn run_git(repository: &Path, arguments: &[&str]) {
    let output = Command::new("git")
        .arg("-C")
        .arg(repository)
        .args(arguments)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env(
            "GIT_CONFIG_GLOBAL",
            repository.with_file_name("test-global-git-config"),
        )
        .output()
        .expect("run Git for repository fixture");
    assert!(
        output.status.success(),
        "git {arguments:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn repository_fixture() -> RepositoryFixture {
    let directory = tempfile::tempdir().expect("temporary repository fixture root");
    let root = directory.path().join("repository");
    fs::create_dir_all(root.join("src")).expect("create repository fixture");
    run_git(&root, &["init", "-q"]);
    run_git(&root, &["config", "user.name", "ctx test"]);
    run_git(&root, &["config", "user.email", "ctx@example.invalid"]);
    run_git(
        &root,
        &[
            "remote",
            "add",
            "origin",
            "https://github.com/ctxrs/ctx.git",
        ],
    );
    fs::write(root.join("src/mcp_attribution.rs"), "// fixture\n")
        .expect("write repository fixture");
    run_git(&root, &["add", "src/mcp_attribution.rs"]);
    run_git(&root, &["commit", "-qm", "fixture"]);
    RepositoryFixture {
        _directory: directory,
        root,
    }
}

fn projectable_repository_facts(repository: &Path) -> Vec<ProviderDeclaredFact> {
    vec![
        ProviderDeclaredFact {
            kind: LiteralFactKind::ToolWorkdir,
            value: repository.to_string_lossy().into_owned(),
        },
        ProviderDeclaredFact {
            kind: LiteralFactKind::File,
            value: "src/mcp_attribution.rs".to_owned(),
        },
    ]
}

fn add_projectable_repository_activity(
    record: &mut crate::protocol::CoreRecord,
    repository: &Path,
) {
    record.content.activity = Some(CoreActivity {
        revision: CORE_ACTIVITY_REVISION,
        provider_call_id: None,
        invocation: None,
        result: None,
        facts: projectable_repository_facts(repository),
    });
}

fn add_mcp_activity(
    record: &mut crate::protocol::CoreRecord,
    repository: &Path,
    server: Option<&str>,
    tool: &str,
) {
    record.content.activity = Some(CoreActivity {
        revision: CORE_ACTIVITY_REVISION,
        provider_call_id: Some(
            crate::protocol::TypedKey::utf8("mcp-call").expect("MCP provider call ID"),
        ),
        invocation: Some(ActivityInvocation {
            protocol: Some("mcp".to_owned()),
            server: server.map(str::to_owned),
            tool: tool.to_owned(),
            arguments: ActivityJsonCapture::Absent,
            started_at_unix_ms: None,
        }),
        result: None,
        facts: projectable_repository_facts(repository),
    });
}

#[test]
fn mcp_activity_strings_are_transport_only_across_preparation_and_materialization() {
    const SERVER_CANARY: &str = "mcp-secret-server-canary-8bc517";
    const TOOL_CANARY: &str = "mcp-secret-tool-canary-93a24e";

    let repository = repository_fixture();
    let mut baseline_record = privacy_golden_record();
    baseline_record.content.discovery_exclusion = None;
    add_mcp_activity(
        &mut baseline_record,
        &repository.root,
        None,
        "baseline-tool",
    );
    baseline_record
        .validate_contract()
        .expect("baseline Core record");
    let mut attributed_record = baseline_record.clone();
    let invocation = attributed_record
        .content
        .activity
        .as_mut()
        .and_then(|activity| activity.invocation.as_mut())
        .expect("MCP activity invocation");
    invocation.server = Some(SERVER_CANARY.to_owned());
    invocation.tool = TOOL_CANARY.to_owned();
    attributed_record
        .validate_contract()
        .expect("attributed Core record");
    let source = crate::protocol::CoreSourceState {
        source: baseline_record.source.clone(),
        core_record_accumulator: "b".repeat(64),
        event_count: 1,
    };
    let generation = "a".repeat(64);
    let baseline = CoreProjectionPreparer::with_parallelism(1)
        .expect("baseline single-worker preparer")
        .prepare_record(&generation, &source, &baseline_record)
        .expect("prepare baseline Core record");
    let attributed = CoreProjectionPreparer::with_parallelism(1)
        .expect("attributed single-worker preparer")
        .prepare_record(&generation, &source, &attributed_record)
        .expect("prepare attributed Core record");

    assert!(
        !attributed.facts.is_empty(),
        "fixture must exercise fact projection"
    );
    assert_eq!(attributed.facts, baseline.facts);
    assert_eq!(attributed.coverage, baseline.coverage);
    assert_eq!(attributed.stable_entities, baseline.stable_entities);
    assert_eq!(attributed.origin_event_id, baseline.origin_event_id);
    assert_ne!(
        attributed
            .evidence
            .as_ref()
            .and_then(|evidence| evidence.citation.evidence_sha256.as_ref()),
        baseline
            .evidence
            .as_ref()
            .and_then(|evidence| evidence.citation.evidence_sha256.as_ref()),
        "the whole-Core-record integrity digest must still cover attribution"
    );

    let materialized = crate::graph::segment::project_core_batch(&PreparedCoreProjectionBatch {
        core_generation_id: generation,
        source,
        units: vec![attributed.clone()],
    })
    .expect("materialize attributed Core record");
    for encoded in [
        serde_json::to_string(&attributed).expect("prepared Core unit JSON"),
        serde_json::to_string(&materialized.records).expect("materialized serving JSON"),
    ] {
        assert!(!encoded.contains(SERVER_CANARY));
        assert!(!encoded.contains(TOOL_CANARY));
    }
}

#[test]
fn discovery_exclusion_is_core_search_policy_only_across_pro_projection_and_replay() {
    const EXCLUSION_CANARY: &str = "ctx_retrieval_derived";

    let repository = repository_fixture();
    let mut baseline_record = privacy_golden_record();
    baseline_record.content.discovery_exclusion = None;
    add_projectable_repository_activity(&mut baseline_record, &repository.root);
    baseline_record
        .validate_contract()
        .expect("baseline Core record");
    let mut excluded_record = baseline_record.clone();
    excluded_record.content.discovery_exclusion = Some(CoreDiscoveryExclusion::CtxRetrievalDerived);
    excluded_record
        .validate_contract()
        .expect("discovery-excluded Core record");
    let source = crate::protocol::CoreSourceState {
        source: baseline_record.source.clone(),
        core_record_accumulator: "b".repeat(64),
        event_count: 1,
    };
    let generation = "a".repeat(64);
    let preparer = CoreProjectionPreparer::with_parallelism(1).expect("single-worker preparer");
    let baseline = preparer
        .prepare_record(&generation, &source, &baseline_record)
        .expect("prepare baseline Core record");
    let excluded = preparer
        .prepare_record(&generation, &source, &excluded_record)
        .expect("prepare discovery-excluded Core record");

    assert!(!excluded.facts.is_empty(), "fixture must exercise facts");
    assert_eq!(excluded.facts, baseline.facts);
    assert_eq!(excluded.coverage, baseline.coverage);
    assert_eq!(excluded.stable_entities, baseline.stable_entities);
    assert_eq!(excluded.origin_event_id, baseline.origin_event_id);
    assert_ne!(
        excluded
            .evidence
            .as_ref()
            .and_then(|evidence| evidence.citation.evidence_sha256.as_ref()),
        baseline
            .evidence
            .as_ref()
            .and_then(|evidence| evidence.citation.evidence_sha256.as_ref()),
        "whole-Core-record evidence must still commit to search policy metadata"
    );

    let prepared = PreparedCoreProjectionBatch {
        core_generation_id: generation,
        source,
        units: vec![excluded.clone()],
    };
    let first = crate::graph::segment::project_core_batch(&prepared).expect("first projection");
    let second = crate::graph::segment::project_core_batch(&prepared).expect("replayed projection");
    assert_eq!(second, first);
    for encoded in [
        serde_json::to_string(&excluded).expect("prepared Core unit JSON"),
        serde_json::to_string(&first.records).expect("materialized serving JSON"),
    ] {
        assert!(!encoded.contains("discovery_exclusion"));
        assert!(!encoded.contains(EXCLUSION_CANARY));
    }
}
