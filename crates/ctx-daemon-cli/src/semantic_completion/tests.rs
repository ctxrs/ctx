use std::{
    cell::{Cell, RefCell},
    path::Path,
    time::{Duration, Instant},
};

use anyhow::{anyhow, Context as _};
use ctx_history_core::{
    derive_event_id, derive_session_id, CertifiedSource, CoreRecord, EventIdentityInput,
    NativeItemKey, NativeSessionKey, ScannedSourceCounts, SessionIdentityInput, SourceAnchor,
    SourceKey, SourceObservation, TypedKey,
};
use ctx_history_index::{CoreEventRecord, GenerationWriter, VerifiedIndex, WriterOptions};
use ctx_semantic_index::{
    source_backed_semantic_vector_path, SemanticBatchEmbedder, SemanticChunkDocument,
    SemanticDocumentBuilder, SemanticEventDocument, SemanticVectorStore,
    SourceBackedSemanticDocumentBuilder,
};
use ctx_semantic_model::{
    ExternalSemanticSpace, SemanticEmbeddingExecutorConfig, SemanticModelLoadDeferred,
};

use super::*;

fn semantic_index_revision_at(
    index_root: &Path,
    revision: u64,
    include_record: bool,
) -> Result<VerifiedIndex> {
    ctx_history_platform::platform_security::establish_private_data_root(index_root)
        .context("establish private semantic-completion fixture root")?;
    let source = SourceKey::derive(
        "codex",
        "codex_session_jsonl",
        "session",
        1,
        SourceAnchor::provider_native(
            "session-file",
            TypedKey::utf8("semantic-completion.jsonl")?,
        )?,
    )?;
    let native_session_key =
        NativeSessionKey::native_id("session", TypedKey::utf8("semantic-completion-session")?)?;
    let session_id = derive_session_id(SessionIdentityInput {
        source: &source,
        logical_session_kind: "thread",
        native_session_key: &native_session_key,
    })?;
    let event_id = derive_event_id(EventIdentityInput {
        source: &source,
        session_id,
        logical_item_kind: "message",
        native_item_key: &NativeItemKey::native_id("message", TypedKey::U64(revision))?,
        subrecord_selector: None,
    })?;
    let mut writer = GenerationWriter::open(index_root, WriterOptions::default())?
        .into_writer()
        .map_err(crate::committed_generation_recovery_error)?;
    writer.begin_source(source.clone())?;
    if include_record {
        let mut record = CoreRecord::new_selected(
            event_id,
            session_id,
            source.clone(),
            revision,
            "message",
            "semantic-completion-v1",
            format!("semantic completion fixture {revision}"),
        )?;
        record.provider_session_id = Some("semantic-completion-session".to_owned());
        record.native_event_id = Some(TypedKey::U64(revision));
        record.role = Some("user".to_owned());
        record.validate_contract()?;
        writer.add_core_record(record)?;
    }
    let record_count = u64::from(include_record);
    let observation =
        SourceObservation::new(source, "regular-file-v1", revision.to_le_bytes().to_vec())?;
    writer.certify_source(CertifiedSource::certify(
        observation.clone(),
        observation,
        "semantic-completion-parser-v1",
        [2; 32],
        ScannedSourceCounts {
            complete_records: record_count,
            retained_records: record_count,
            indexed_documents: record_count,
            certified_bytes: record_count,
            ..ScannedSourceCounts::default()
        },
    )?)?;
    writer.commit(|_| true)?;
    VerifiedIndex::open_pinned(index_root).map_err(Into::into)
}

fn test_pin() -> Result<(tempfile::TempDir, PinnedSourceBackedGeneration)> {
    let temp = tempfile::tempdir()?;
    let index = semantic_index_revision_at(&temp.path().join("index"), 1, false)?;
    Ok((temp, PinnedSourceBackedGeneration::from_index(index)))
}

fn foreground_empty_generation_fixture() -> Result<(tempfile::TempDir, PinnedSourceBackedGeneration)>
{
    let temp = tempfile::tempdir()?;
    let index = semantic_index_revision_at(
        &ctx_history_refresh::source_backed_index_root(temp.path()),
        1,
        false,
    )?;
    Ok((temp, PinnedSourceBackedGeneration::from_index(index)))
}

fn daemon_completion(
    pin: &PinnedSourceBackedGeneration,
    budgets: SemanticCompletionBudgets,
    now: Instant,
) -> Result<DaemonSemanticCompletion> {
    DaemonSemanticCompletion::new_at(
        pin,
        SemanticEmbeddingExecutorConfig::builtin(),
        SemanticCompletionDaemonConfig::new(true, "full", true),
        budgets,
        now,
    )
    .map_err(Into::into)
}

fn pending_progress(
    reload_attempt_at_ms: i64,
    reload_applied_at_ms: i64,
    run_at_ms: i64,
    indexed_chunks: u64,
) -> DaemonSemanticCompletionObservation {
    DaemonSemanticCompletionObservation::Pending(DaemonSemanticProgress {
        reload_status: Some("applied".to_owned()),
        reload_last_attempt_at_ms: Some(reload_attempt_at_ms),
        reload_last_applied_at_ms: Some(reload_applied_at_ms),
        requested_config_matches: true,
        applied_config_matches: true,
        job_target_matches: true,
        job_status: Some("budget_exhausted".to_owned()),
        job_last_run_at_ms: Some(run_at_ms),
        job_semantic_progress_sequence: Some(indexed_chunks),
        job_indexed_chunks: Some(indexed_chunks),
        job_source_generation_ready: Some(false),
        job_source_work_remaining: Some(true),
    })
}

fn resource_deferred_progress(
    reload_attempt_at_ms: i64,
    reload_applied_at_ms: i64,
    run_at_ms: i64,
) -> DaemonSemanticCompletionObservation {
    DaemonSemanticCompletionObservation::Pending(DaemonSemanticProgress {
        reload_status: Some("applied".to_owned()),
        reload_last_attempt_at_ms: Some(reload_attempt_at_ms),
        reload_last_applied_at_ms: Some(reload_applied_at_ms),
        requested_config_matches: true,
        applied_config_matches: true,
        job_target_matches: true,
        job_status: Some("resource_deferred".to_owned()),
        job_last_run_at_ms: Some(run_at_ms),
        job_semantic_progress_sequence: None,
        job_indexed_chunks: None,
        job_source_generation_ready: None,
        job_source_work_remaining: None,
    })
}

fn expect_completion_error<T>(
    result: std::result::Result<T, SemanticCompletionError>,
    message: &str,
) -> SemanticCompletionError {
    match result {
        Ok(_) => panic!("{message}"),
        Err(error) => error,
    }
}

#[test]
fn ready_preflight_returns_before_daemon_observation() -> Result<()> {
    let (_temp, pin) = test_pin()?;
    let generation = pin.generation_id().to_owned();
    let now = Instant::now();
    let mut completion = daemon_completion(&pin, SemanticCompletionBudgets::default(), now)?;
    let active_calls = Cell::new(0);

    let checkpoint = completion.checkpoint_with(
        now,
        &pin,
        || {
            active_calls.set(active_calls.get() + 1);
            Ok(generation.clone())
        },
        |_| Ok(true),
        || panic!("ready exact preflight must not observe daemon state"),
    )?;

    assert_eq!(checkpoint, SemanticCompletionCheckpoint::Ready);
    assert_eq!(active_calls.get(), 2);
    Ok(())
}

#[test]
fn exact_checkpoint_maps_matching_job_failure_without_preflight_authority() -> Result<()> {
    let (_temp, pin) = test_pin()?;
    let generation = pin.generation_id().to_owned();
    let now = Instant::now();
    let mut completion = daemon_completion(&pin, SemanticCompletionBudgets::default(), now)?;
    let error = expect_completion_error(
        completion.checkpoint_with(
            now,
            &pin,
            || Ok(generation.clone()),
            |_| Ok(false),
            || {
                Ok(DaemonSemanticCompletionObservation::JobFailed {
                    detail: "backend unavailable".to_owned(),
                    retryable: true,
                    failure_class: Some("retryable".to_owned()),
                })
            },
        ),
        "matching daemon failure must be typed",
    );
    assert!(matches!(
        error,
        SemanticCompletionError::DaemonJobFailed {
            retryable: true,
            failure_class: Some(failure_class),
            detail,
            ..
        } if failure_class == "retryable" && detail == "backend unavailable"
    ));
    Ok(())
}

#[test]
fn no_progress_budget_is_deterministic_and_progress_resets_it() -> Result<()> {
    let (_temp, pin) = test_pin()?;
    let generation = pin.generation_id().to_owned();
    let started = Instant::now();
    let budgets = SemanticCompletionBudgets::new(
        Duration::from_secs(1),
        Duration::from_secs(2),
        Duration::from_secs(10),
    );
    let mut completion = daemon_completion(&pin, budgets, started)?;
    for (elapsed, reload_attempt_at_ms, reload_applied_at_ms, run_at_ms, indexed_chunks) in
        [(0, 1, 1, 1, 8), (1, 2, 2, 2, 16), (2, 3, 3, 3, 16)]
    {
        assert_eq!(
            completion.checkpoint_with(
                started + Duration::from_secs(elapsed),
                &pin,
                || Ok(generation.clone()),
                |_| Ok(false),
                || {
                    Ok(pending_progress(
                        reload_attempt_at_ms,
                        reload_applied_at_ms,
                        run_at_ms,
                        indexed_chunks,
                    ))
                },
            )?,
            SemanticCompletionCheckpoint::Pending {
                poll_after: Duration::from_secs(1),
            }
        );
    }

    let error = expect_completion_error(
        completion.checkpoint_with(
            started + Duration::from_secs(3),
            &pin,
            || Ok(generation.clone()),
            |_| Ok(false),
            || Ok(pending_progress(4, 4, 4, 16)),
        ),
        "unchanged progress must exhaust the no-progress budget",
    );
    assert!(matches!(
        error,
        SemanticCompletionError::NoProgress {
            retryable: true,
            ..
        }
    ));
    Ok(())
}

#[test]
fn resource_deferred_receipt_churn_exhausts_the_no_progress_budget() -> Result<()> {
    let (_temp, pin) = test_pin()?;
    let generation = pin.generation_id().to_owned();
    let started = Instant::now();
    let budgets = SemanticCompletionBudgets::new(
        Duration::from_secs(1),
        Duration::from_secs(2),
        Duration::from_secs(10),
    );
    let mut completion = daemon_completion(&pin, budgets, started)?;
    for (elapsed, reload_attempt_at_ms, reload_applied_at_ms, run_at_ms) in
        [(0, 1, 1, 1), (1, 2, 2, 2)]
    {
        assert!(matches!(
            completion.checkpoint_with(
                started + Duration::from_secs(elapsed),
                &pin,
                || Ok(generation.clone()),
                |_| Ok(false),
                || {
                    Ok(resource_deferred_progress(
                        reload_attempt_at_ms,
                        reload_applied_at_ms,
                        run_at_ms,
                    ))
                },
            )?,
            SemanticCompletionCheckpoint::Pending { .. }
        ));
    }

    let error = expect_completion_error(
        completion.checkpoint_with(
            started + Duration::from_secs(2),
            &pin,
            || Ok(generation.clone()),
            |_| Ok(false),
            || Ok(resource_deferred_progress(3, 3, 3)),
        ),
        "resource-deferred receipt churn must not reset the no-progress budget",
    );
    assert!(matches!(
        error,
        SemanticCompletionError::NoProgress {
            retryable: true,
            ..
        }
    ));
    Ok(())
}

#[test]
fn readiness_without_a_sequence_does_not_reset_semantic_progress() {
    let pending = match pending_progress(9, 9, 9, 8) {
        DaemonSemanticCompletionObservation::Pending(progress) => {
            CompletionProgress::Pending(progress)
        }
        other => panic!("expected pending observation, got {other:?}"),
    };
    assert!(!CompletionProgress::ReadyAwaitingIndex.substantively_advances_from(Some(&pending)));
    assert!(!pending.substantively_advances_from(Some(&CompletionProgress::ReadyAwaitingIndex)));
}

#[test]
fn continuous_observation_outage_has_an_independent_budget() -> Result<()> {
    let (_temp, pin) = test_pin()?;
    let generation = pin.generation_id().to_owned();
    let started = Instant::now();
    let budgets = SemanticCompletionBudgets::new(
        Duration::from_secs(1),
        Duration::from_secs(20),
        Duration::from_secs(2),
    );
    let mut completion = daemon_completion(&pin, budgets, started)?;
    for elapsed in [0, 1] {
        assert_eq!(
            completion.checkpoint_with(
                started + Duration::from_secs(elapsed),
                &pin,
                || Ok(generation.clone()),
                |_| Ok(false),
                || {
                    Ok(DaemonSemanticCompletionObservation::Unavailable {
                        detail: "daemon status absent".to_owned(),
                    })
                },
            )?,
            SemanticCompletionCheckpoint::Pending {
                poll_after: Duration::from_secs(1),
            }
        );
    }

    let error = expect_completion_error(
        completion.checkpoint_with(
            started + Duration::from_secs(2),
            &pin,
            || Ok(generation.clone()),
            |_| Ok(false),
            || {
                Ok(DaemonSemanticCompletionObservation::Unavailable {
                    detail: "daemon status absent".to_owned(),
                })
            },
        ),
        "continuous outage must exhaust its own budget",
    );
    assert!(matches!(
        error,
        SemanticCompletionError::ObservationOutage {
            retryable: true,
            detail,
            ..
        } if detail == "daemon status absent"
    ));
    Ok(())
}

#[test]
fn observation_recovery_without_semantic_progress_does_not_reset_budget() -> Result<()> {
    let (_temp, pin) = test_pin()?;
    let generation = pin.generation_id().to_owned();
    let started = Instant::now();
    let budgets = SemanticCompletionBudgets::new(
        Duration::from_secs(1),
        Duration::from_secs(3),
        Duration::from_secs(10),
    );
    let mut completion = daemon_completion(&pin, budgets, started)?;
    assert!(matches!(
        completion.checkpoint_with(
            started,
            &pin,
            || Ok(generation.clone()),
            |_| Ok(false),
            || Ok(pending_progress(1, 1, 1, 8)),
        )?,
        SemanticCompletionCheckpoint::Pending { .. }
    ));
    assert!(matches!(
        completion.checkpoint_with(
            started + Duration::from_secs(1),
            &pin,
            || Ok(generation.clone()),
            |_| Ok(false),
            || {
                Ok(DaemonSemanticCompletionObservation::Unavailable {
                    detail: "transient outage".to_owned(),
                })
            },
        )?,
        SemanticCompletionCheckpoint::Pending { .. }
    ));

    let error = expect_completion_error(
        completion.checkpoint_with(
            started + Duration::from_secs(3),
            &pin,
            || Ok(generation.clone()),
            |_| Ok(false),
            || Ok(pending_progress(2, 2, 2, 8)),
        ),
        "observation recovery must not masquerade as semantic progress",
    );
    assert!(matches!(
        error,
        SemanticCompletionError::NoProgress {
            retryable: true,
            ..
        }
    ));
    Ok(())
}

#[test]
fn foreground_checkpoint_failure_precedes_active_generation_and_reconciliation() -> Result<()> {
    let (_temp, pin) = test_pin()?;
    let error = expect_completion_error(
        complete_semantic_generation_foreground_with_checkpoint(
            Path::new("must-not-be-observed"),
            pin,
            SemanticEmbeddingExecutorConfig::builtin(),
            &mut || Err(anyhow!("cancelled")),
        ),
        "checkpoint error must be preserved",
    );
    assert_eq!(error.code(), "semantic_completion_interrupted");
    assert!(format!("{error:#}").contains("cancelled"));
    Ok(())
}

#[test]
fn foreground_inner_source_projection_cancellation_preserves_checkpoint_identity() -> Result<()> {
    let (completed_temp, completed_pin) = foreground_empty_generation_fixture()?;
    let completed_generation_id = completed_pin.generation_id().to_owned();
    let completed_checkpoint_calls = Cell::new(0_u32);
    let completed = complete_semantic_generation_foreground_with_checkpoint(
        completed_temp.path(),
        completed_pin,
        SemanticEmbeddingExecutorConfig::builtin(),
        &mut || {
            completed_checkpoint_calls.set(completed_checkpoint_calls.get() + 1);
            Ok(())
        },
    )?;
    assert_eq!(completed.generation_id(), completed_generation_id);

    // The successful final callback is the outer post-reconciliation
    // authority check. Its predecessor is therefore the final checkpoint
    // inside source projection; derive it from the observed path so the test
    // remains valid if empty-generation checkpoint placement evolves.
    let inner_projection_checkpoint = completed_checkpoint_calls
        .get()
        .checked_sub(1)
        .expect("foreground reconciliation must run a source-projection checkpoint");
    assert!(
        inner_projection_checkpoint > 2,
        "the inner source-projection checkpoint must follow the pre-reconcile barriers"
    );

    let (temp, pin) = foreground_empty_generation_fixture()?;
    let generation_id = pin.generation_id().to_owned();
    let checkpoint_calls = Cell::new(0_u32);
    let inner_checkpoint_failed = Cell::new(false);

    let error = expect_completion_error(
        complete_semantic_generation_foreground_with_checkpoint(
            temp.path(),
            pin,
            SemanticEmbeddingExecutorConfig::builtin(),
            &mut || {
                let call = checkpoint_calls.get() + 1;
                checkpoint_calls.set(call);
                if call >= inner_projection_checkpoint {
                    if call == inner_projection_checkpoint {
                        inner_checkpoint_failed.set(true);
                    }
                    return Err(anyhow!("cancelled at inner source-projection checkpoint"));
                }
                Ok(())
            },
        ),
        "inner source-projection cancellation must preserve checkpoint identity",
    );

    assert!(inner_checkpoint_failed.get());
    assert_eq!(
        checkpoint_calls.get(),
        inner_projection_checkpoint.saturating_add(1),
        "the reconciliation error boundary must re-check sticky cancellation once"
    );
    assert_eq!(error.generation_id(), generation_id);
    assert_eq!(error.code(), "semantic_completion_interrupted");
    assert!(!error.retryable());
    assert!(format!("{error:#}").contains("cancelled at inner source-projection checkpoint"));
    Ok(())
}

#[test]
fn foreground_supersession_at_source_publication_checkpoint_has_no_exact_commit() -> Result<()> {
    let (completed_temp, completed_pin) = foreground_empty_generation_fixture()?;
    let completed_checkpoint_calls = Cell::new(0_u32);
    complete_semantic_generation_foreground_with_checkpoint(
        completed_temp.path(),
        completed_pin,
        SemanticEmbeddingExecutorConfig::builtin(),
        &mut || {
            completed_checkpoint_calls.set(completed_checkpoint_calls.get() + 1);
            Ok(())
        },
    )?;
    // The final callback is the outer post-reconcile authority check and its
    // predecessor is the exact acknowledgement commit. The callback before
    // both is the source-view publication boundary.
    let source_publication_checkpoint = completed_checkpoint_calls
        .get()
        .checked_sub(2)
        .expect("foreground reconciliation must expose a source publication checkpoint");

    let (temp, pin) = foreground_empty_generation_fixture()?;
    let index_root = ctx_history_refresh::source_backed_index_root(temp.path());
    let generation_id = pin.generation_id().to_owned();
    let replacement_generation = RefCell::new(None);
    let checkpoint_calls = Cell::new(0_u32);
    let error = expect_completion_error(
        complete_semantic_generation_foreground_with_checkpoint(
            temp.path(),
            pin,
            SemanticEmbeddingExecutorConfig::builtin(),
            &mut || {
                let call = checkpoint_calls.get() + 1;
                checkpoint_calls.set(call);
                if call == source_publication_checkpoint {
                    let replacement = semantic_index_revision_at(&index_root, 2, false)?;
                    *replacement_generation.borrow_mut() =
                        Some(replacement.generation_id().to_owned());
                }
                Ok(())
            },
        ),
        "supersession at source publication must remain typed",
    );

    assert!(matches!(
        error,
        SemanticCompletionError::CoreSuperseded {
            generation_id: error_generation,
            active_generation_id,
            retryable: true,
        } if error_generation == generation_id
            && Some(active_generation_id.clone()) == replacement_generation.into_inner()
    ));
    let retained = VerifiedIndex::open_pinned_generation(&index_root, &generation_id)?;
    let selected = SemanticEmbeddingExecutorConfig::builtin();
    let contract = crate::query_adapter::semantic_index_contract_for_selected(selected.contract())?;
    let not_ready = match SemanticQueryPin::preflight(&retained, temp.path(), &contract) {
        Ok(_) => panic!("a superseded source view unexpectedly became query ready"),
        Err(error) => error,
    };
    assert!(
        not_ready
            .downcast_ref::<SemanticNotReady>()
            .is_some_and(SemanticNotReady::retryable),
        "a superseded source view must not acquire an exact semantic acknowledgement"
    );
    Ok(())
}

#[test]
fn foreground_in_reconciliation_active_generation_read_failure_preserves_preflight_identity(
) -> Result<()> {
    let temp = tempfile::tempdir()?;
    let index_root = ctx_history_refresh::source_backed_index_root(temp.path());
    let index = semantic_index_revision_at(&index_root, 1, false)?;
    let generation_id = index.generation_id().to_owned();
    let checkpoint_calls = Cell::new(0_u32);

    let error = expect_completion_error(
        complete_semantic_generation_foreground_with_checkpoint(
            temp.path(),
            PinnedSourceBackedGeneration::from_index(index),
            SemanticEmbeddingExecutorConfig::builtin(),
            &mut || {
                let call = checkpoint_calls.get() + 1;
                checkpoint_calls.set(call);
                if call == 3 {
                    std::fs::write(index_root.join("active-generation.json"), b"{")?;
                }
                Ok(())
            },
        ),
        "in-reconciliation active-generation read failure must preserve preflight identity",
    );

    assert_eq!(checkpoint_calls.get(), 3);
    assert_eq!(error.generation_id(), generation_id);
    assert_eq!(error.code(), "semantic_completion_preflight_failed");
    assert!(error.retryable());
    Ok(())
}

#[test]
fn foreground_supersession_before_writable_open_preserves_semantic_state() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let index_root = ctx_history_refresh::source_backed_index_root(temp.path());
    let index = semantic_index_revision_at(&index_root, 1, false)?;
    let generation_id = index.generation_id().to_owned();
    let replacement_generation = RefCell::new(None);
    let checkpoint_calls = Cell::new(0_u32);

    let error = expect_completion_error(
        complete_semantic_generation_foreground_with_checkpoint(
            temp.path(),
            PinnedSourceBackedGeneration::from_index(index),
            SemanticEmbeddingExecutorConfig::builtin(),
            &mut || {
                let call = checkpoint_calls.get() + 1;
                checkpoint_calls.set(call);
                if call == 5 {
                    let replacement = semantic_index_revision_at(&index_root, 2, false)?;
                    *replacement_generation.borrow_mut() =
                        Some(replacement.generation_id().to_owned());
                }
                Ok(())
            },
        ),
        "supersession before writable open must be typed",
    );

    assert!(matches!(
        error,
        SemanticCompletionError::CoreSuperseded {
            generation_id: error_generation,
            active_generation_id,
            retryable: true,
        } if error_generation == generation_id
            && Some(active_generation_id.clone()) == replacement_generation.into_inner()
    ));
    assert!(
        !source_backed_semantic_vector_path(temp.path()).exists(),
        "a superseded run must not open or mutate the semantic vector state"
    );
    Ok(())
}

#[test]
fn foreground_supersession_after_final_preflight_is_typed() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let index_root = ctx_history_refresh::source_backed_index_root(temp.path());
    let index = semantic_index_revision_at(&index_root, 1, false)?;
    let generation_id = index.generation_id().to_owned();
    let replacement_generation = RefCell::new(None);

    let error = expect_completion_error(
        complete_semantic_generation_foreground_with_checkpoint_and_final_preflight(
            temp.path(),
            PinnedSourceBackedGeneration::from_index(index),
            SemanticEmbeddingExecutorConfig::builtin(),
            &mut || Ok(()),
            &mut |pin, data_root, contract| {
                SemanticQueryPin::preflight(pin.verified_index(), data_root, contract)?;
                let replacement = semantic_index_revision_at(&index_root, 2, false)?;
                *replacement_generation.borrow_mut() = Some(replacement.generation_id().to_owned());
                Ok(())
            },
        ),
        "supersession after final preflight must be typed",
    );

    assert!(matches!(
        error,
        SemanticCompletionError::CoreSuperseded {
            generation_id: error_generation,
            active_generation_id,
            retryable: true,
        } if error_generation == generation_id
            && Some(active_generation_id.clone()) == replacement_generation.into_inner()
    ));
    Ok(())
}

#[test]
fn permanent_vector_store_failure_is_not_retryable() {
    let error = anyhow::Error::new(ctx_semantic_index::test_support::storage_conflict_error(
        "semantic store identity changed",
    ));

    assert!(!reconciliation_failure_is_retryable(&error));
}

#[test]
fn resource_pressure_failure_is_retryable() {
    let error = anyhow::Error::new(SemanticModelLoadDeferred::for_test(1, 2));

    assert!(reconciliation_failure_is_retryable(&error));
}

struct RejectingEmptySemanticPorts;

impl SemanticDocumentBuilder for RejectingEmptySemanticPorts {
    fn build_document(
        &mut self,
        _record: &CoreEventRecord,
    ) -> Result<Option<SemanticEventDocument>> {
        panic!("empty generation must not build semantic documents")
    }
}

impl SemanticBatchEmbedder for RejectingEmptySemanticPorts {
    fn embed_chunks(&mut self, _chunks: &[SemanticChunkDocument]) -> Result<Vec<Vec<f32>>> {
        panic!("empty generation must not request embeddings")
    }
}

#[test]
fn ready_empty_external_v2_foreground_completion_is_executor_free() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let index_root = ctx_history_refresh::source_backed_index_root(temp.path());
    let index = semantic_index_revision_at(&index_root, 1, false)?;
    let generation = index.generation_id().to_owned();
    let config = SemanticEmbeddingExecutorConfig::http(
        "http://127.0.0.1:9",
        ExternalSemanticSpace::new("ready-empty-completion", 96)?,
    )?;
    let contract = semantic_index_contract_for_selected(config.contract())?;
    let mut store =
        SemanticVectorStore::open(&source_backed_semantic_vector_path(temp.path()), &contract)?;
    let mut builder = SourceBackedSemanticDocumentBuilder::new(&index);
    let mut embedder = RejectingEmptySemanticPorts;
    assert!(store
        .reconcile_source_backed_index(&index, &mut builder, &mut embedder)?
        .ready());
    drop(store);

    let checkpoints = RefCell::new(0_u32);
    let completed = complete_semantic_generation_foreground_with_checkpoint(
        temp.path(),
        PinnedSourceBackedGeneration::from_index(index),
        config,
        &mut || {
            *checkpoints.borrow_mut() += 1;
            Ok(())
        },
    )?;
    assert_eq!(completed.generation_id(), generation);
    assert_eq!(*checkpoints.borrow(), 1);
    Ok(())
}
