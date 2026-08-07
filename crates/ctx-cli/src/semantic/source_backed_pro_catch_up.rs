#[cfg(test)]
use std::path::Path;
use std::time::Duration;

use ctx_history_index::VerifiedIndex;
#[cfg(test)]
use ctx_pro_host_protocol::CoreMaterializationFinalizationPhase;
#[cfg(test)]
use ctx_pro_host_protocol::{
    CoreMaterializationFinalizationPending, CoreMaterializationFinalizationProgress,
    CoreMaterializationReceipt,
};
use serde_json::Value;

#[cfg(test)]
use crate::pro::{
    core_finalization_generation_lease, reconstruct_core_finalization_generation_lease,
};

#[cfg(test)]
use super::source_backed_refresh_coordinator::{open_verified_index, source_backed_index_root};
use super::{
    paths_status::{daemon_jobs_path, write_daemon_job_status},
    source_backed_refresh_coordinator::{PinnedCorePublication, PinnedSourceBackedGeneration},
};

mod finalization;
mod lease_reconciliation;
mod recheck;
mod status;

pub(super) use finalization::run_after_core_publication;
pub(crate) use finalization::wait_for_completed_generation;
#[cfg(test)]
use finalization::{
    run_with, wait_for_completed_generation_with, ProCatchUpAuthority, ProCatchUpSyncOutcome,
};
pub(crate) use lease_reconciliation::cancel_core_finalization_generation_lease;
pub(super) use lease_reconciliation::reconcile_core_finalization_generation_lease;
pub(super) use recheck::schedule as helper_recheck_schedule;
#[cfg(test)]
use recheck::{path as recheck_path, read as read_recheck_request};
pub(crate) use recheck::{
    publish as publish_helper_recheck_intent, targets as helper_recheck_targets,
    wake as wake_helper_recheck,
};
#[cfg(test)]
use status::{persist_status, status_path, SourceBackedProCatchUpStatus};
pub(super) use status::{
    persist_status_json, read_status_json, scheduled_target_generation, status_generation,
    status_has_finalization_pending,
};

const SOURCE_BACKED_PRO_CATCH_UP_WAKE_TIMEOUT: Duration = Duration::from_millis(500);
const SOURCE_BACKED_PRO_CATCH_UP_WAKE_RESPONSE_MAX_BYTES: u64 = 64 * 1024;

pub(super) struct SourceBackedProCatchUpRun {
    pub(super) status: Value,
    pub(super) did_work: bool,
    pub(super) continuation_pending: bool,
}

#[derive(Clone, Copy)]
pub(super) enum SourceBackedProCoreAuthority<'a> {
    Retained(&'a PinnedCorePublication),
    Durable(&'a PinnedSourceBackedGeneration),
}

impl<'a> SourceBackedProCoreAuthority<'a> {
    pub(super) fn generation_id(self) -> &'a str {
        match self {
            Self::Retained(authority) => authority.generation_id(),
            Self::Durable(authority) => authority.generation_id(),
        }
    }

    fn verified_index(self) -> &'a VerifiedIndex {
        match self {
            Self::Retained(authority) => authority.verified_index_ref(),
            Self::Durable(authority) => authority.verified_index(),
        }
    }

    fn surface(self) -> &'static str {
        match self {
            Self::Retained(_) => "retained Core generation pin",
            Self::Durable(_) => "durable active Core generation pin",
        }
    }
}

#[cfg(test)]
mod tests {
    use std::{cell::Cell, fs};

    use ctx_history_core::{
        CertifiedSource, ScannedSourceCounts, SourceAnchor, SourceKey, SourceObservation, TypedKey,
    };
    use ctx_history_index::{
        acquire_generation_retention_lease, load_generation_retention_lease,
        release_generation_retention_lease, GenerationWriter, WriterOptions,
    };

    use crate::semantic::source_backed_refresh_coordinator::{
        count_verified_index_opens, pin_retained_generation,
    };

    use super::*;

    fn empty_index(data_root: &Path) -> VerifiedIndex {
        GenerationWriter::open(
            source_backed_index_root(data_root),
            WriterOptions::default(),
        )
        .unwrap()
        .into_writer()
        .unwrap()
        .commit(|_| true)
        .unwrap();
        open_verified_index(&source_backed_index_root(data_root)).unwrap()
    }

    fn index_with_certified_source_at(
        data_root: &Path,
        source_path: &str,
        observation_byte: u8,
    ) -> VerifiedIndex {
        let source = SourceKey::derive(
            "codex",
            "codex_session_jsonl",
            "session",
            1,
            SourceAnchor::provider_native("session-file", TypedKey::utf8(source_path).unwrap())
                .unwrap(),
        )
        .unwrap();
        let observation =
            SourceObservation::new(source.clone(), "regular-file-v1", vec![observation_byte])
                .unwrap();
        let mut writer = GenerationWriter::open(
            source_backed_index_root(data_root),
            WriterOptions::default(),
        )
        .unwrap()
        .into_writer()
        .unwrap();
        writer.begin_source(source).unwrap();
        writer
            .certify_source(
                CertifiedSource::certify(
                    observation.clone(),
                    observation,
                    "continuation-test-parser-v1",
                    [observation_byte; 32],
                    ScannedSourceCounts::default(),
                )
                .unwrap(),
            )
            .unwrap();
        writer.commit(|_| true).unwrap();
        open_verified_index(&source_backed_index_root(data_root)).unwrap()
    }

    fn receipt_with_revision(
        index: &VerifiedIndex,
        materializer_revision: &str,
    ) -> CoreMaterializationReceipt {
        CoreMaterializationReceipt {
            core_generation_id: index.generation_id().to_owned(),
            core_record_contract_fingerprint: index
                .manifest()
                .core_record_contract_fingerprint
                .clone(),
            source_snapshot_sha256: "a".repeat(64),
            materializer_revision: materializer_revision.to_owned(),
            source_count: 0,
            event_count: 0,
        }
    }

    fn sync_outcome(
        index: &VerifiedIndex,
        materializer_revision: &str,
        did_work: bool,
    ) -> ProCatchUpSyncOutcome {
        ProCatchUpSyncOutcome::Finished {
            receipt: receipt_with_revision(index, materializer_revision),
            did_work,
            helper_artifact_sha256: "a".repeat(64),
        }
    }

    fn finalization_outcome(
        index: &VerifiedIndex,
        phase: CoreMaterializationFinalizationPhase,
        cursor: char,
        replayed: bool,
    ) -> ProCatchUpSyncOutcome {
        ProCatchUpSyncOutcome::FinalizationPending {
            pending: CoreMaterializationFinalizationPending {
                progress: finalization_progress(index, phase, cursor),
                replayed,
            },
        }
    }

    fn finalization_progress(
        index: &VerifiedIndex,
        phase: CoreMaterializationFinalizationPhase,
        cursor: char,
    ) -> CoreMaterializationFinalizationProgress {
        CoreMaterializationFinalizationProgress {
            materialization_id: "b".repeat(64),
            core_generation_id: index.generation_id().to_owned(),
            finish_request_digest: "d".repeat(64),
            materializer_revision: "test-core-materializer-v1".to_owned(),
            phase,
            cursor_sha256: cursor.to_string().repeat(64),
        }
    }

    #[test]
    fn durable_state_path_is_purpose_based() {
        assert_eq!(
            status_path(Path::new("ctx-data")),
            Path::new("ctx-data/daemon/jobs/pro-catch-up.json")
        );
        assert_eq!(
            recheck_path(Path::new("ctx-data")),
            Path::new("ctx-data/daemon/jobs/pro-catch-up-recheck.json")
        );
    }

    #[test]
    fn catch_up_reuses_pinned_core_and_persists_exact_receipt_generation() {
        let temp = tempfile::tempdir().unwrap();
        let index = empty_index(temp.path());
        let generation = index.generation_id().to_owned();
        let (run, opens) = count_verified_index_opens(|| {
            run_with(
                temp.path(),
                &generation,
                ProCatchUpAuthority {
                    generation_id: Some(&generation),
                    verified_index: Some(&index),
                },
                |_| Ok(()),
                |_, supplied| {
                    assert_eq!(supplied.generation_id(), generation);
                    Ok(sync_outcome(supplied, "test-core-materializer-v1", true))
                },
            )
            .unwrap()
        });
        assert_eq!(opens, 0);
        assert!(run.did_work);
        assert_eq!(run.status["status"], "completed");
        assert_eq!(run.status["receipt_core_generation_id"], generation);
    }

    #[test]
    fn finalization_pending_is_a_successful_non_backoff_yield() {
        let temp = tempfile::tempdir().unwrap();
        let index = empty_index(temp.path());
        let generation = index.generation_id().to_owned();
        let run = run_with(
            temp.path(),
            &generation,
            ProCatchUpAuthority {
                generation_id: Some(&generation),
                verified_index: Some(&index),
            },
            |_| Ok(()),
            |_, supplied| {
                Ok(finalization_outcome(
                    supplied,
                    CoreMaterializationFinalizationPhase::EmitReplay,
                    'c',
                    false,
                ))
            },
        )
        .unwrap();

        assert!(run.did_work);
        assert!(run.continuation_pending);
        assert_eq!(run.status["status"], "pending");
        assert_eq!(run.status["pending"], true);
        assert_eq!(run.status["retryable"], false);
        assert_eq!(run.status["reason"], "finalizing");
        assert!(run.status["error_code"].is_null());
        assert_eq!(
            run.status["finalization_progress"]["core_generation_id"],
            generation
        );
        assert!(status_has_finalization_pending(temp.path(), &generation));
    }

    #[test]
    fn leased_g1_survives_g2_through_g4_restart_then_reclaims_after_completion() {
        let temp = tempfile::tempdir().unwrap();
        let first = index_with_certified_source_at(temp.path(), "lease-g1.jsonl", 1);
        let generation = first.generation_id().to_owned();
        let pending = run_with(
            temp.path(),
            &generation,
            ProCatchUpAuthority {
                generation_id: Some(&generation),
                verified_index: Some(&first),
            },
            |_| Ok(()),
            |_, supplied| {
                Ok(finalization_outcome(
                    supplied,
                    CoreMaterializationFinalizationPhase::EmitReplay,
                    'c',
                    false,
                ))
            },
        )
        .unwrap();
        assert!(pending.continuation_pending);
        assert_eq!(
            core_finalization_generation_lease(temp.path())
                .unwrap()
                .unwrap()
                .generation_id(),
            generation
        );

        for (path, revision) in [
            ("lease-g2.jsonl", 2),
            ("lease-g3.jsonl", 3),
            ("lease-g4.jsonl", 4),
        ] {
            index_with_certified_source_at(temp.path(), path, revision);
        }

        // A fresh scheduler process performs this reconciliation before it
        // resolves the durable target.
        reconcile_core_finalization_generation_lease(temp.path()).unwrap();
        let retained = pin_retained_generation(temp.path(), &generation).unwrap();
        assert_eq!(retained.generation_id(), generation);
        let completed = run_with(
            temp.path(),
            &generation,
            ProCatchUpAuthority {
                generation_id: Some(retained.generation_id()),
                verified_index: Some(retained.verified_index()),
            },
            |_| Ok(()),
            |_, supplied| Ok(sync_outcome(supplied, "test-core-materializer-v1", true)),
        )
        .unwrap();
        assert_eq!(completed.status["status"], "completed");
        assert!(core_finalization_generation_lease(temp.path())
            .unwrap()
            .is_none());

        index_with_certified_source_at(temp.path(), "lease-g5.jsonl", 5);
        assert!(pin_retained_generation(temp.path(), &generation).is_err());
    }

    #[test]
    fn lost_pending_response_keeps_exact_target_after_core_advances() {
        let temp = tempfile::tempdir().unwrap();
        let index = index_with_certified_source_at(temp.path(), "continuation-old-core.jsonl", 1);
        let generation = index.generation_id().to_owned();
        let lost = run_with(
            temp.path(),
            &generation,
            ProCatchUpAuthority {
                generation_id: Some(&generation),
                verified_index: Some(&index),
            },
            |_| Ok(()),
            |data_root, supplied| {
                let progress = finalization_progress(
                    supplied,
                    CoreMaterializationFinalizationPhase::EmitReplay,
                    'c',
                );
                reconstruct_core_finalization_generation_lease(data_root, &progress).unwrap();
                anyhow::bail!("helper_crashed: committed Pending response was lost")
            },
        )
        .unwrap();
        assert_eq!(lost.status["status"], "error");
        assert_eq!(lost.status["retryable"], true);
        assert_eq!(
            scheduled_target_generation(temp.path()).unwrap().as_deref(),
            Some(generation.as_str())
        );

        for (path, revision) in [
            ("continuation-g2.jsonl", 2),
            ("continuation-g3.jsonl", 3),
            ("continuation-g4.jsonl", 4),
        ] {
            index_with_certified_source_at(temp.path(), path, revision);
        }
        reconcile_core_finalization_generation_lease(temp.path()).unwrap();
        let retained = pin_retained_generation(temp.path(), &generation).unwrap();
        assert_eq!(retained.generation_id(), generation);
        let resumed = run_with(
            temp.path(),
            &generation,
            ProCatchUpAuthority {
                generation_id: Some(retained.generation_id()),
                verified_index: Some(retained.verified_index()),
            },
            |_| Ok(()),
            |_, supplied| {
                Ok(finalization_outcome(
                    supplied,
                    CoreMaterializationFinalizationPhase::EmitReplay,
                    'c',
                    true,
                ))
            },
        )
        .unwrap();
        assert!(resumed.continuation_pending);
        assert_eq!(resumed.status["core_generation_id"], generation);
        assert_eq!(resumed.status["reason"], "finalizing");
    }

    #[test]
    fn lost_continue_response_preserves_finalizing_tuple_until_reconciliation() {
        let temp = tempfile::tempdir().unwrap();
        let index = empty_index(temp.path());
        let generation = index.generation_id().to_owned();
        let first = run_with(
            temp.path(),
            &generation,
            ProCatchUpAuthority {
                generation_id: Some(&generation),
                verified_index: Some(&index),
            },
            |_| Ok(()),
            |_, supplied| {
                Ok(finalization_outcome(
                    supplied,
                    CoreMaterializationFinalizationPhase::EmitReplay,
                    'c',
                    false,
                ))
            },
        )
        .unwrap();
        let expected = first.status["finalization_progress"].clone();

        let lost = run_with(
            temp.path(),
            &generation,
            ProCatchUpAuthority {
                generation_id: Some(&generation),
                verified_index: Some(&index),
            },
            |data_root| {
                let during_attempt = read_status_json(data_root).unwrap();
                assert_eq!(during_attempt["reason"], "finalizing");
                assert_eq!(during_attempt["finalization_progress"], expected);
                Ok(())
            },
            |_, _| anyhow::bail!("helper_crashed: committed Continue response was lost"),
        )
        .unwrap();
        assert_eq!(lost.status["status"], "error");
        assert_eq!(lost.status["retryable"], true);
        assert_eq!(lost.status["finalization_progress"], expected);

        let reconciled = run_with(
            temp.path(),
            &generation,
            ProCatchUpAuthority {
                generation_id: Some(&generation),
                verified_index: Some(&index),
            },
            |data_root| {
                let during_attempt = read_status_json(data_root).unwrap();
                assert_eq!(during_attempt["reason"], "finalizing");
                assert_eq!(during_attempt["finalization_progress"], expected);
                Ok(())
            },
            |_, supplied| {
                Ok(finalization_outcome(
                    supplied,
                    CoreMaterializationFinalizationPhase::EmitFlat,
                    'd',
                    true,
                ))
            },
        )
        .unwrap();
        assert_eq!(reconciled.status["attempts"], 3);
        assert_eq!(reconciled.status["reason"], "finalizing");
        assert_ne!(reconciled.status["finalization_progress"], expected);
    }

    #[test]
    fn finalization_digest_and_revision_mismatches_are_terminal() {
        for mismatch in ["digest", "revision"] {
            let temp = tempfile::tempdir().unwrap();
            let index = empty_index(temp.path());
            let generation = index.generation_id().to_owned();
            run_with(
                temp.path(),
                &generation,
                ProCatchUpAuthority {
                    generation_id: Some(&generation),
                    verified_index: Some(&index),
                },
                |_| Ok(()),
                |_, supplied| {
                    Ok(finalization_outcome(
                        supplied,
                        CoreMaterializationFinalizationPhase::EmitReplay,
                        'c',
                        false,
                    ))
                },
            )
            .unwrap();

            let failed = run_with(
                temp.path(),
                &generation,
                ProCatchUpAuthority {
                    generation_id: Some(&generation),
                    verified_index: Some(&index),
                },
                |_| Ok(()),
                |_, _| anyhow::bail!("invalid_response: finalization {mismatch} mismatch"),
            )
            .unwrap();
            assert_eq!(failed.status["status"], "error", "{mismatch}");
            assert_eq!(failed.status["retryable"], false, "{mismatch}");
            assert_eq!(failed.status["reason"], "invalid_response", "{mismatch}");
            assert!(
                scheduled_target_generation(temp.path()).unwrap().is_none(),
                "{mismatch}"
            );
            assert!(
                core_finalization_generation_lease(temp.path())
                    .unwrap()
                    .is_none(),
                "{mismatch}"
            );
        }
    }

    #[test]
    fn restart_releases_terminal_and_truly_missing_job_leases() {
        let temp = tempfile::tempdir().unwrap();
        let index = empty_index(temp.path());
        let generation = index.generation_id().to_owned();
        let progress = finalization_progress(
            &index,
            CoreMaterializationFinalizationPhase::EmitReplay,
            'c',
        );
        reconstruct_core_finalization_generation_lease(temp.path(), &progress).unwrap();

        let terminal =
            SourceBackedProCatchUpStatus::pending(&generation, 1).completed(generation.clone());
        persist_status(temp.path(), &terminal).unwrap();
        reconcile_core_finalization_generation_lease(temp.path()).unwrap();
        assert!(core_finalization_generation_lease(temp.path())
            .unwrap()
            .is_none());

        reconstruct_core_finalization_generation_lease(temp.path(), &progress).unwrap();
        let mismatched_progress = CoreMaterializationFinalizationProgress {
            materialization_id: "e".repeat(64),
            ..progress.clone()
        };
        let mismatched =
            SourceBackedProCatchUpStatus::pending(&generation, 2).finalizing(mismatched_progress);
        persist_status(temp.path(), &mismatched).unwrap();
        reconcile_core_finalization_generation_lease(temp.path()).unwrap();
        assert!(core_finalization_generation_lease(temp.path())
            .unwrap()
            .is_none());
        assert_eq!(
            read_status_json(temp.path()).unwrap()["error_code"],
            "cancelled"
        );

        reconstruct_core_finalization_generation_lease(temp.path(), &progress).unwrap();
        fs::remove_file(status_path(temp.path())).unwrap();
        reconcile_core_finalization_generation_lease(temp.path()).unwrap();
        assert!(core_finalization_generation_lease(temp.path())
            .unwrap()
            .is_none());
        assert_eq!(
            read_status_json(temp.path()).unwrap()["error_code"],
            "cancelled"
        );
    }

    #[test]
    fn malformed_and_unreadable_jobs_fail_closed_without_releasing_the_lease() {
        for corruption in ["truncated", "typed", "unreadable"] {
            let temp = tempfile::tempdir().unwrap();
            let index = empty_index(temp.path());
            let generation = index.generation_id().to_owned();
            let progress = finalization_progress(
                &index,
                CoreMaterializationFinalizationPhase::EmitReplay,
                'c',
            );
            reconstruct_core_finalization_generation_lease(temp.path(), &progress).unwrap();
            let observed = core_finalization_generation_lease(temp.path())
                .unwrap()
                .unwrap();
            let finalizing =
                SourceBackedProCatchUpStatus::pending(&generation, 1).finalizing(progress);
            persist_status(temp.path(), &finalizing).unwrap();
            match corruption {
                "truncated" => {
                    fs::write(status_path(temp.path()), b"{\"schema_version\":").unwrap();
                }
                "typed" => {
                    fs::write(
                        status_path(temp.path()),
                        b"{\"schema_version\":\"invalid\"}",
                    )
                    .unwrap();
                }
                "unreadable" => {
                    fs::remove_file(status_path(temp.path())).unwrap();
                    fs::create_dir(status_path(temp.path())).unwrap();
                }
                _ => unreachable!(),
            }

            let error = reconcile_core_finalization_generation_lease(temp.path()).unwrap_err();
            assert!(
                error.to_string().starts_with("invalid_response:"),
                "{corruption}: {error:#}"
            );
            assert_eq!(
                core_finalization_generation_lease(temp.path()).unwrap(),
                Some(observed.clone()),
                "{corruption}"
            );
            let cancel =
                cancel_core_finalization_generation_lease(temp.path(), "test cancel").unwrap_err();
            assert!(
                cancel.to_string().starts_with("invalid_response:"),
                "{corruption}: {cancel:#}"
            );
            assert_eq!(
                core_finalization_generation_lease(temp.path()).unwrap(),
                Some(observed),
                "{corruption}"
            );
        }
    }

    #[test]
    fn progress_and_job_generation_mismatch_cancels_and_releases_exact_lease() {
        let temp = tempfile::tempdir().unwrap();
        let index = empty_index(temp.path());
        let generation = index.generation_id().to_owned();
        let progress = finalization_progress(
            &index,
            CoreMaterializationFinalizationPhase::EmitReplay,
            'c',
        );
        reconstruct_core_finalization_generation_lease(temp.path(), &progress).unwrap();
        let mismatched = CoreMaterializationFinalizationProgress {
            core_generation_id: "f".repeat(64),
            ..progress
        };
        persist_status(
            temp.path(),
            &SourceBackedProCatchUpStatus::pending(&generation, 1).finalizing(mismatched),
        )
        .unwrap();

        reconcile_core_finalization_generation_lease(temp.path()).unwrap();

        assert!(core_finalization_generation_lease(temp.path())
            .unwrap()
            .is_none());
        assert_eq!(
            read_status_json(temp.path()).unwrap()["error_code"],
            "cancelled"
        );
    }

    #[test]
    fn lease_owner_mismatch_cancels_and_releases_exact_lease() {
        let temp = tempfile::tempdir().unwrap();
        let index = empty_index(temp.path());
        let generation = index.generation_id().to_owned();
        let progress = finalization_progress(
            &index,
            CoreMaterializationFinalizationPhase::EmitReplay,
            'c',
        );
        let index_root = source_backed_index_root(temp.path());
        let foreign = acquire_generation_retention_lease(
            &index_root,
            &generation,
            "foreign_consumer",
            &"f".repeat(64),
        )
        .unwrap();
        persist_status(
            temp.path(),
            &SourceBackedProCatchUpStatus::pending(&generation, 1).finalizing(progress),
        )
        .unwrap();

        reconcile_core_finalization_generation_lease(temp.path()).unwrap();

        assert!(load_generation_retention_lease(&index_root)
            .unwrap()
            .is_none());
        assert_ne!(foreign.owner_kind(), "pro_core_finalization");
        assert_eq!(
            read_status_json(temp.path()).unwrap()["error_code"],
            "cancelled"
        );
    }

    #[test]
    fn lease_generation_mismatch_cancels_and_releases_exact_lease() {
        let temp = tempfile::tempdir().unwrap();
        let first = index_with_certified_source_at(temp.path(), "lease-mismatch-g1.jsonl", 1);
        let generation = first.generation_id().to_owned();
        let progress = finalization_progress(
            &first,
            CoreMaterializationFinalizationPhase::EmitReplay,
            'c',
        );
        reconstruct_core_finalization_generation_lease(temp.path(), &progress).unwrap();
        let index_root = source_backed_index_root(temp.path());
        let expected = load_generation_retention_lease(&index_root)
            .unwrap()
            .unwrap();
        release_generation_retention_lease(&index_root, &expected).unwrap();
        let second = index_with_certified_source_at(temp.path(), "lease-mismatch-g2.jsonl", 2);
        acquire_generation_retention_lease(
            &index_root,
            second.generation_id(),
            expected.owner_kind(),
            expected.owner_id(),
        )
        .unwrap();
        persist_status(
            temp.path(),
            &SourceBackedProCatchUpStatus::pending(&generation, 1).finalizing(progress),
        )
        .unwrap();

        reconcile_core_finalization_generation_lease(temp.path()).unwrap();

        assert!(load_generation_retention_lease(&index_root)
            .unwrap()
            .is_none());
        assert_eq!(
            read_status_json(temp.path()).unwrap()["error_code"],
            "cancelled"
        );
    }

    #[test]
    fn same_generation_rechecks_helper_after_materializer_revision_change() {
        let temp = tempfile::tempdir().unwrap();
        let index = empty_index(temp.path());
        let generation = index.generation_id().to_owned();
        run_with(
            temp.path(),
            &generation,
            ProCatchUpAuthority {
                generation_id: Some(&generation),
                verified_index: Some(&index),
            },
            |_| Ok(()),
            |_, supplied| Ok(sync_outcome(supplied, "test-core-materializer-v1", true)),
        )
        .unwrap();

        let mut preflighted = false;
        let mut synced = false;
        let rerun = run_with(
            temp.path(),
            &generation,
            ProCatchUpAuthority {
                generation_id: Some(&generation),
                verified_index: Some(&index),
            },
            |_| {
                preflighted = true;
                Ok(())
            },
            |_, supplied| {
                synced = true;
                Ok(sync_outcome(supplied, "test-core-materializer-v2", true))
            },
        )
        .unwrap();

        assert!(preflighted);
        assert!(synced);
        assert!(rerun.did_work);
        assert_eq!(rerun.status["status"], "completed");
        assert_eq!(rerun.status["attempts"], 2);
    }

    #[test]
    fn same_generation_rechecks_helper_after_private_state_loss() {
        let temp = tempfile::tempdir().unwrap();
        let index = empty_index(temp.path());
        let generation = index.generation_id().to_owned();
        let helper_private_state_exists = Cell::new(false);
        run_with(
            temp.path(),
            &generation,
            ProCatchUpAuthority {
                generation_id: Some(&generation),
                verified_index: Some(&index),
            },
            |_| Ok(()),
            |_, supplied| {
                assert!(!helper_private_state_exists.get());
                helper_private_state_exists.set(true);
                Ok(sync_outcome(supplied, "test-core-materializer-v1", true))
            },
        )
        .unwrap();
        assert!(helper_private_state_exists.replace(false));

        let rerun = run_with(
            temp.path(),
            &generation,
            ProCatchUpAuthority {
                generation_id: Some(&generation),
                verified_index: Some(&index),
            },
            |_| Ok(()),
            |_, supplied| {
                assert!(!helper_private_state_exists.get());
                helper_private_state_exists.set(true);
                Ok(sync_outcome(supplied, "test-core-materializer-v1", true))
            },
        )
        .unwrap();

        assert!(helper_private_state_exists.get());
        assert!(rerun.did_work);
        assert_eq!(rerun.status["status"], "completed");
        assert_eq!(rerun.status["attempts"], 2);
    }

    #[test]
    fn same_generation_current_helper_is_revalidated_without_reporting_work() {
        let temp = tempfile::tempdir().unwrap();
        let index = empty_index(temp.path());
        let generation = index.generation_id().to_owned();
        run_with(
            temp.path(),
            &generation,
            ProCatchUpAuthority {
                generation_id: Some(&generation),
                verified_index: Some(&index),
            },
            |_| Ok(()),
            |_, supplied| Ok(sync_outcome(supplied, "test-core-materializer-v1", true)),
        )
        .unwrap();

        let preflighted = Cell::new(false);
        let synced = Cell::new(false);
        let replay = run_with(
            temp.path(),
            &generation,
            ProCatchUpAuthority {
                generation_id: Some(&generation),
                verified_index: Some(&index),
            },
            |_| {
                preflighted.set(true);
                Ok(())
            },
            |_, supplied| {
                synced.set(true);
                Ok(sync_outcome(supplied, "test-core-materializer-v1", false))
            },
        )
        .unwrap();

        assert!(preflighted.get());
        assert!(synced.get());
        assert!(!replay.did_work);
        assert_eq!(replay.status["status"], "completed");
        assert_eq!(replay.status["attempts"], 2);
    }

    #[test]
    fn helper_recheck_blocks_same_generation_completion_until_observed_success() {
        let temp = tempfile::tempdir().unwrap();
        let index = empty_index(temp.path());
        let generation = index.generation_id().to_owned();
        run_with(
            temp.path(),
            &generation,
            ProCatchUpAuthority {
                generation_id: Some(&generation),
                verified_index: Some(&index),
            },
            |_| Ok(()),
            |_, supplied| Ok(sync_outcome(supplied, "test-core-materializer-v1", true)),
        )
        .unwrap();
        wait_for_completed_generation_with(temp.path(), &generation, Duration::ZERO, || {})
            .unwrap();

        publish_helper_recheck_intent(temp.path(), &"a".repeat(64)).unwrap();
        let error =
            wait_for_completed_generation_with(temp.path(), &generation, Duration::ZERO, || {})
                .unwrap_err();
        assert!(error.to_string().contains("timed out"), "{error:#}");

        let rerun = run_with(
            temp.path(),
            &generation,
            ProCatchUpAuthority {
                generation_id: Some(&generation),
                verified_index: Some(&index),
            },
            |_| Ok(()),
            |_, supplied| Ok(sync_outcome(supplied, "test-core-materializer-v2", true)),
        )
        .unwrap();
        assert!(rerun.did_work);
        assert!(read_recheck_request(temp.path()).unwrap().is_none());
        wait_for_completed_generation_with(temp.path(), &generation, Duration::ZERO, || {})
            .unwrap();
    }

    #[test]
    fn older_run_cannot_clear_recheck_published_during_sync() {
        let temp = tempfile::tempdir().unwrap();
        let index = empty_index(temp.path());
        let generation = index.generation_id().to_owned();
        publish_helper_recheck_intent(temp.path(), &"a".repeat(64)).unwrap();
        let first_request = read_recheck_request(temp.path()).unwrap().unwrap();

        run_with(
            temp.path(),
            &generation,
            ProCatchUpAuthority {
                generation_id: Some(&generation),
                verified_index: Some(&index),
            },
            |_| Ok(()),
            |data_root, supplied| {
                publish_helper_recheck_intent(data_root, &"b".repeat(64)).unwrap();
                Ok(sync_outcome(supplied, "test-core-materializer-v1", true))
            },
        )
        .unwrap();

        let current_request = read_recheck_request(temp.path()).unwrap().unwrap();
        assert_ne!(current_request, first_request);
        assert_eq!(current_request.target_helper_sha256(), "b".repeat(64));
        let error =
            wait_for_completed_generation_with(temp.path(), &generation, Duration::ZERO, || {})
                .unwrap_err();
        assert!(error.to_string().contains("timed out"), "{error:#}");
    }

    #[test]
    fn old_helper_cannot_clear_pending_target_identity() {
        let temp = tempfile::tempdir().unwrap();
        let index = empty_index(temp.path());
        let generation = index.generation_id().to_owned();
        publish_helper_recheck_intent(temp.path(), &"b".repeat(64)).unwrap();

        run_with(
            temp.path(),
            &generation,
            ProCatchUpAuthority {
                generation_id: Some(&generation),
                verified_index: Some(&index),
            },
            |_| Ok(()),
            |_, supplied| Ok(sync_outcome(supplied, "test-core-materializer-v1", true)),
        )
        .unwrap();

        let pending = read_recheck_request(temp.path()).unwrap().unwrap();
        assert_eq!(pending.target_helper_sha256(), "b".repeat(64));
    }

    #[test]
    fn pinned_generation_mismatch_fails_before_sync() {
        let temp = tempfile::tempdir().unwrap();
        let index = empty_index(temp.path());
        let expected = "f".repeat(64);
        let run = run_with(
            temp.path(),
            &expected,
            ProCatchUpAuthority {
                generation_id: Some(index.generation_id()),
                verified_index: Some(&index),
            },
            |_| Ok(()),
            |_, _| panic!("mismatched pin must not sync"),
        )
        .unwrap();
        assert!(!run.did_work);
        assert_eq!(run.status["error_code"], "source_pro_generation_mismatch");
    }

    #[test]
    fn production_catch_up_has_no_manifest_resolver_or_provider_io() {
        let source = [
            include_str!("source_backed_pro_catch_up.rs"),
            include_str!("source_backed_pro_catch_up/finalization.rs"),
            include_str!("source_backed_pro_catch_up/lease_reconciliation.rs"),
            include_str!("source_backed_pro_catch_up/recheck.rs"),
            include_str!("source_backed_pro_catch_up/status.rs"),
        ]
        .join("\n");
        for forbidden in [
            ["Source", "Manifest"].concat(),
            ["source", "_manifest"].concat(),
            ["sync_source", "_manifest_materialization"].concat(),
        ] {
            assert!(!source.contains(&forbidden));
        }
        assert!(source.contains("sync_core_materialization"));
    }
}
