use anyhow::{anyhow, Result};
use ctx_history_core::{
    derive_event_id, derive_session_id, AgentScope, CertifiedSource, CoreDiscoveryExclusion,
    CoreRecord, EventIdentityInput, EventRole, EventType, NativeItemKey, NativeSessionKey,
    ScannedSourceCounts, SessionIdentityInput, SourceAnchor, SourceKey, SourceObservation,
    TypedKey,
};
use ctx_history_index::{GenerationWriter, WriterOptions};
use ctx_semantic_model::{ExternalSemanticSpace, SemanticEmbeddingExecutorConfig};

use super::*;
use crate::{
    daemon_scheduler::{DaemonSchedulerCycleContext, DaemonSchedulerPorts, DaemonSemanticJobPorts},
    paths_status::{daemon_semantic_job_path, read_daemon_job_status},
    test_support::{ARTIFACT, GENERATION_PUBLISHED, OBSERVATION},
    DaemonConfigSnapshot,
};

struct RejectingSemanticAuth;

impl DaemonConfigPort for RejectingSemanticAuth {
    fn load(&self, data_root: &Path) -> Result<DaemonConfigSnapshot> {
        crate::test_support::CONFIG.load(data_root)
    }

    fn semantic_model_config(&self, data_root: &Path) -> ctx_semantic_model::SemanticModelConfig {
        crate::test_support::CONFIG.semantic_model_config(data_root)
    }

    fn semantic_executor_auth(&self) -> Result<ctx_semantic_model::SemanticEmbeddingExecutorAuth> {
        Err(anyhow!("semantic query activation has no credentials"))
    }

    fn discovery_context(&self, data_root: &Path) -> Result<ctx_history_capture::DiscoveryContext> {
        crate::test_support::CONFIG.discovery_context(data_root)
    }
}

static REJECTING_SEMANTIC_AUTH: RejectingSemanticAuth = RejectingSemanticAuth;

fn publish_zero_eligible_generation(data_root: &Path) -> Result<String> {
    let source = SourceKey::derive(
        "gemini",
        "gemini_cli_chat_recording_jsonl",
        "session",
        1,
        SourceAnchor::CatalogLineage([42; 32]),
    )?;
    let session_key = NativeSessionKey::native_id("session", TypedKey::utf8("zero-eligible")?)?;
    let session_id = derive_session_id(SessionIdentityInput {
        source: &source,
        logical_session_kind: "thread",
        native_session_key: &session_key,
    })?;
    let item = NativeItemKey::native_id("message", TypedKey::U64(1))?;
    let event_id = derive_event_id(EventIdentityInput {
        source: &source,
        session_id,
        logical_item_kind: "message",
        native_item_key: &item,
        subrecord_selector: None,
    })?;
    let mut record = CoreRecord::new_selected(
        event_id,
        session_id,
        source.clone(),
        1,
        EventType::Message.as_str(),
        "semantic-maintenance-test-v1",
        "excluded retrieval result",
    )?;
    record.role = Some(EventRole::User.as_str().to_owned());
    record.agent_scope = Some(AgentScope::Primary);
    record.content.discovery_exclusion = Some(CoreDiscoveryExclusion::CtxRetrievalDerived);
    record.validate_contract()?;
    let mut writer = GenerationWriter::open(
        crate::source_backed_refresh_coordinator::source_backed_index_root(data_root),
        WriterOptions::default(),
    )?
    .into_writer()
    .map_err(crate::committed_generation_recovery_error)?;
    writer.begin_source(source.clone())?;
    writer.add_core_record(record)?;
    let observation = SourceObservation::new(source, "fixture-v1", vec![1])?;
    writer.certify_source(CertifiedSource::certify(
        observation.clone(),
        observation,
        "fixture-parser-v1",
        [1; 32],
        ScannedSourceCounts {
            complete_records: 1,
            retained_records: 1,
            indexed_documents: 1,
            certified_bytes: 80,
            ..ScannedSourceCounts::default()
        },
    )?)?;
    Ok(writer.commit(|_| true)?.generation_id)
}

#[test]
fn zero_eligible_maintenance_runs_when_query_activation_has_no_auth() -> Result<()> {
    let temporary = tempfile::tempdir()?;
    let generation = publish_zero_eligible_generation(temporary.path())?;
    let mut runtime = DaemonRuntime::default();
    runtime.config.daemon.enabled = true;
    runtime.config.semantic_enabled = true;
    runtime.config.semantic_executor = SemanticEmbeddingExecutorConfig::http(
        "http://127.0.0.1:9",
        ExternalSemanticSpace::new("zero-eligible-maintenance", 96)?,
    )?;
    runtime.sidecar_drain.generation = Some(generation.clone());
    runtime.sidecar_drain.semantic_turn_pending = true;
    let mut reload = DaemonConfigReloadState::pending(&runtime.config);
    reload.status = "activation_failed";

    assert!(!daemon_semantic_runtime_active(&runtime, None));
    assert!(daemon_semantic_maintenance_requested(&runtime, &reload));
    let semantic_enabled = daemon_semantic_maintenance_requested(&runtime, &reload);
    let iteration = crate::daemon_scheduler::run_daemon_scheduler_cycle_with_activity(
        &test_daemon_run_args(),
        temporary.path(),
        &mut runtime,
        DaemonSchedulerCycleContext {
            deadline: None,
            semantic_enabled,
            query_activity: None,
            source_refresh: None,
        },
        DaemonSchedulerPorts {
            generation_published: &GENERATION_PUBLISHED,
            semantic: DaemonSemanticJobPorts {
                artifact_fetcher: &ARTIFACT,
                config: &REJECTING_SEMANTIC_AUTH,
            },
            observation: &OBSERVATION,
        },
    )?;

    assert!(
        iteration.did_work,
        "scheduler skipped maintenance: {iteration:?}; sidecar generation={:?}; attempted={:?}; pending={}",
        runtime.sidecar_drain.generation,
        runtime.sidecar_drain.semantic_attempted_generation,
        runtime.sidecar_drain.semantic_turn_pending,
    );
    let job = read_daemon_job_status(&daemon_semantic_job_path(temporary.path()))
        .ok_or_else(|| anyhow!("semantic maintenance did not persist a job"))?;
    assert!(!iteration.failed, "{job:#}");
    assert_eq!(job["status"], "ready", "{job:#}");
    assert_eq!(job["core_generation_id"], generation, "{job:#}");
    assert!(runtime.semantic_executor.is_none());
    reload.status = "failed";
    assert!(!daemon_semantic_maintenance_requested(&runtime, &reload));
    Ok(())
}
