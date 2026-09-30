use std::{
    collections::BTreeMap,
    io::{Read, Seek, SeekFrom},
    path::{Path, PathBuf},
    sync::atomic::{AtomicBool, Ordering},
    time::{SystemTime, UNIX_EPOCH},
};

use ctx_history_server::{
    publish_fingerprint, CancelPublishRequest, PublishRequest, UploadSpec, UploadStatus,
};
use serde::{Deserialize, Serialize};

use crate::{
    capture, private_file,
    queue::{Enqueued, Pending},
    Error, RemoteClient, Result, SelectionDecision, SelectionObservation, SharingStore,
    UPLOAD_CHUNK_BYTES,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TickOutcome {
    Disabled,
    Paused,
    Idle,
    Progress,
    Failed(Error),
}

pub struct Collector {
    data_root: PathBuf,
    store: SharingStore,
}

#[derive(Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct CaptureStamp {
    generation: String,
    policy: u64,
    complete: bool,
    counts: Option<BTreeMap<SelectionDecision, u64>>,
}

impl CaptureStamp {
    pub(crate) fn invalidate(root: &Path) -> Result<()> {
        let path = root.join("capture.json");
        if let Some(mut stamp) = private_file::read_optional::<Self>(&path)? {
            // Identical pending bytes may have completed capture during
            // settlement backoff. Retiring that key must permit recapture.
            stamp.complete = false;
            private_file::write(&path, &stamp)?;
        }
        Ok(())
    }

    pub(crate) fn selection(self, policy: u64) -> Option<SelectionObservation> {
        if self.policy != policy {
            return None;
        }
        Some(SelectionObservation {
            generation: self.generation,
            policy_revision: self.policy,
            counts: self.counts?,
        })
    }
}

impl Collector {
    pub fn new(data_root: PathBuf, sharing_root: PathBuf) -> Self {
        Self {
            data_root,
            store: SharingStore::new(sharing_root),
        }
    }

    /// One request at most. Call again promptly after Progress, otherwise on
    /// the retry cadence or a committed-generation wake. All failures are typed
    /// sharing outcomes; local Core publication/search never depends on them.
    pub fn tick(&self) -> TickOutcome {
        self.tick_with_stop(&AtomicBool::new(false))
    }

    pub(crate) fn tick_with_stop(&self, stop: &AtomicBool) -> TickOutcome {
        match self.try_tick(stop) {
            Ok(outcome) => outcome,
            Err(error) => {
                // An error writing sharing diagnostics is optional too.
                if self.store.root().is_dir() {
                    let _ = private_file::write(&self.store.root().join("last-error.json"), &error);
                }
                TickOutcome::Failed(error)
            }
        }
    }

    fn try_tick(&self, stop: &AtomicBool) -> Result<TickOutcome> {
        let Some(settings) = self.store.settings()? else {
            return Ok(TickOutcome::Disabled);
        };
        let Some(policy) = &settings.policy else {
            return Ok(TickOutcome::Disabled);
        };
        if settings.paused {
            return Ok(TickOutcome::Paused);
        }
        if stop.load(Ordering::Acquire) {
            return Ok(TickOutcome::Idle);
        }
        let _owner = self.store.lock("uploader.lock", true)?;
        // Interrupted cleanup is outside the live namespace and must not hold
        // up either retries or capture of a later committed generation.
        let _ = self.store.cleanup_retired();
        let _ = self.store.cleanup_scratch();
        let now = unix_now()?;
        for path in self.store.pending_paths()? {
            let mut pending: Pending = private_file::read(&path.join("pending.json"))?;
            let settle = pending.settlement_started || !pending.allowed(policy);
            if settle && !pending.may_have_submitted() {
                continue;
            }
            if pending.retry_at > now {
                continue;
            }
            match self.step(&path, &mut pending, stop, settle) {
                Ok(()) => {
                    if pending.failures > 0 && path.try_exists().map_err(|_| Error::State)? {
                        // Preserve the failure count across staging recreation:
                        // repeated expiry must increase backoff, not hot-loop.
                        pending.retry_at = 0;
                        pending.last_error = None;
                        pending.save(&path)?;
                    }
                    let _ = std::fs::remove_file(self.store.root().join("last-error.json"));
                    return Ok(TickOutcome::Progress);
                }
                Err(Error::PolicyDenied) => return Ok(TickOutcome::Idle),
                Err(error) => {
                    pending.failed(&path, error, now)?;
                    return Err(error);
                }
            }
        }
        // Backoff is per publication. New sessions still need a durable copy
        // while older operations wait for the server to recover.
        self.capture(stop)
    }

    fn capture(&self, stop: &AtomicBool) -> Result<TickOutcome> {
        let settings = self.store.settings()?.ok_or(Error::NotConnected)?;
        let Some(policy) = settings.policy else {
            return Ok(TickOutcome::Disabled);
        };
        if settings.paused || stop.load(Ordering::Acquire) {
            return Ok(TickOutcome::Idle);
        }
        if !self
            .data_root
            .join("search/lexical")
            .try_exists()
            .map_err(|_| Error::Archive)?
        {
            return Ok(TickOutcome::Idle);
        }
        let index = capture::open_index(&self.data_root)?;
        let mut stamp = CaptureStamp {
            generation: index.generation_id().to_owned(),
            policy: policy.revision,
            complete: true,
            counts: Some(BTreeMap::new()),
        };
        let stamp_path = self.store.root().join("capture.json");
        if private_file::read_optional::<CaptureStamp>(&stamp_path)?.is_some_and(|previous| {
            previous.generation == stamp.generation
                && previous.policy == stamp.policy
                && previous.complete
                && previous.counts.is_some()
        }) {
            return Ok(TickOutcome::Idle);
        }
        let temporary = tempfile::Builder::new()
            .prefix(".capture-")
            .tempdir_in(self.store.root())
            .map_err(|_| Error::State)?;
        let archive = temporary.path().join("archive");
        let mut counts = BTreeMap::new();
        let mut queued = false;
        capture::export_index_control(
            &index,
            &archive,
            policy.archive_identity.clone(),
            &policy.sources,
            &mut |member, scope| {
                if stop.load(Ordering::Acquire) {
                    return Err(Error::PolicyDenied);
                }
                let current = self.store.settings()?.ok_or(Error::NotConnected)?;
                let Some(current_policy) = current.policy else {
                    return Err(Error::PolicyDenied);
                };
                if current.paused || current_policy.revision != policy.revision {
                    return Err(Error::PolicyDenied);
                }
                let decision = current_policy.select(scope);
                *counts.entry(decision).or_insert(0) += 1;
                if decision == SelectionDecision::Selected {
                    let enqueued =
                        self.store
                            .enqueue(&archive, member, scope, &current_policy, stop)?;
                    stamp.complete &= enqueued != Enqueued::Pending;
                    queued |= enqueued == Enqueued::New;
                }
                Ok(())
            },
            stop,
        )?;
        // Observation is useful even when an older unresolved operation blocks
        // a selected successor. Only a complete capture can skip a later scan.
        stamp.counts = Some(counts);
        private_file::write(&stamp_path, &stamp)?;
        Ok(if queued {
            TickOutcome::Progress
        } else {
            TickOutcome::Idle
        })
    }

    fn admit(&self, pending: &Pending, stop: &AtomicBool) -> Result<RemoteClient> {
        if stop.load(Ordering::Acquire) {
            return Err(Error::PolicyDenied);
        }
        let settings = self.store.settings()?.ok_or(Error::NotConnected)?;
        if settings.paused || settings.policy.as_ref().is_none_or(|p| !pending.allowed(p)) {
            return Err(Error::PolicyDenied);
        }
        // This final current-policy read is admission. A request already
        // admitted may finish while pause/narrowing commits; no later request
        // uploads or finalizes under the old policy.
        RemoteClient::new(settings.connection, settings.credentials)
    }

    fn admit_receipt(&self, stop: &AtomicBool) -> Result<RemoteClient> {
        let settings = self.store.settings()?.ok_or(Error::NotConnected)?;
        if stop.load(Ordering::Acquire) || settings.paused || settings.policy.is_none() {
            return Err(Error::PolicyDenied);
        }
        // Reconciliation sends only an operation key or its cancellation
        // fingerprint. Current credentials and server publish authority still
        // gate the response; this never authorizes old bytes or a publish retry.
        RemoteClient::new(settings.connection, settings.credentials)
    }

    fn step(
        &self,
        path: &Path,
        pending: &mut Pending,
        stop: &AtomicBool,
        settle: bool,
    ) -> Result<()> {
        if settle {
            let publisher = pending.publisher.clone().ok_or(Error::Protocol)?;
            let request = CancelPublishRequest {
                fingerprint: publish_fingerprint(
                    &publisher,
                    &pending.operation,
                    &pending.identity,
                    &pending.member,
                )
                .map_err(|_| Error::Protocol)?,
                publisher,
                operation: pending.operation.clone(),
            };
            let client = self.admit_receipt(stop)?;
            pending.settlement_started = true;
            pending.save(path)?;
            let response = client.cancel_publish(&request)?;
            return self.store.settled(path, pending, &request, &response);
        }
        if pending.lookup_receipt {
            match self
                .admit_receipt(stop)?
                .receipt(&pending.operation.idempotency_key)
            {
                Ok(receipt) => return self.store.accepted(path, pending, receipt),
                Err(Error::NotFound) => {
                    pending.lookup_receipt = false;
                    pending.save(path)?;
                    return Ok(());
                }
                Err(error) => return Err(error),
            }
        }
        let Some(upload) = &pending.upload else {
            let spec = UploadSpec {
                sha256: pending.member.sha256.clone(),
                bytes: pending.member.bytes,
            };
            let upload = self.admit(pending, stop)?.begin_upload(&spec)?;
            validate_upload(
                &upload,
                pending.member.bytes,
                None,
                pending.publisher.as_deref(),
            )?;
            if self
                .store
                .publication_checkpoint(&pending.operation.publication)?
                .is_some_and(|p| p.owner != upload.publisher)
            {
                return Err(Error::Forbidden);
            }
            pending.publisher = Some(upload.publisher.clone());
            pending.upload = Some(upload);
            return pending.save(path);
        };
        if pending.publisher.is_none() {
            return Err(Error::Protocol);
        }
        if pending.reconcile_offset {
            match self.admit(pending, stop)?.upload_status(&upload.id) {
                Ok(status) => {
                    validate_upload(
                        &status,
                        pending.member.bytes,
                        Some(&upload.id),
                        pending.publisher.as_deref(),
                    )?;
                    pending.upload = Some(status);
                    pending.reconcile_offset = false;
                }
                Err(Error::NotFound | Error::StagingExpired) => {
                    pending.upload = None;
                    pending.reconcile_offset = false;
                    pending.save(path)?;
                    return Err(Error::StagingExpired);
                }
                Err(error) => return Err(error),
            }
            return pending.save(path);
        }
        if upload.received_bytes < pending.member.bytes {
            let id = upload.id.clone();
            let offset = upload.received_bytes;
            let mut file = private_file::open(&path.join("payload"))?;
            file.seek(SeekFrom::Start(offset))
                .map_err(|_| Error::State)?;
            let size = (pending.member.bytes - offset).min(UPLOAD_CHUNK_BYTES as u64) as usize;
            let mut bytes = vec![0; size];
            file.read_exact(&mut bytes).map_err(|_| Error::State)?;
            pending.reconcile_offset = true;
            pending.save(path)?;
            match self.admit(pending, stop)?.upload_chunk(&id, offset, &bytes) {
                Ok(status) => {
                    validate_upload(
                        &status,
                        pending.member.bytes,
                        Some(&id),
                        pending.publisher.as_deref(),
                    )?;
                    if status.received_bytes != offset + size as u64 {
                        return Err(Error::Protocol);
                    }
                    pending.upload = Some(status);
                    pending.reconcile_offset = false;
                }
                Err(Error::NotFound | Error::StagingExpired) => {
                    pending.upload = None;
                    pending.reconcile_offset = false;
                    pending.save(path)?;
                    return Err(Error::StagingExpired);
                }
                Err(error) => return Err(error),
            }
            return pending.save(path);
        }
        let request = PublishRequest {
            operation: pending.operation.clone(),
            identity: pending.identity.clone(),
            member: pending.member.clone(),
            upload: upload.id.clone(),
        };
        // Admission may reject locally; only an admitted request can become
        // uncertain. An already admitted bounded request may finish on pause.
        let client = self.admit(pending, stop)?;
        pending.publish_attempted = true;
        pending.lookup_receipt = true;
        pending.save(path)?;
        match client.publish(&request) {
            Ok(receipt) => self.store.accepted(path, pending, receipt),
            Err(Error::NotFound | Error::StagingExpired) => {
                pending.upload = None;
                pending.lookup_receipt = false;
                pending.save(path)?;
                Err(Error::StagingExpired)
            }
            Err(error) => Err(error),
        }
    }
}

fn validate_upload(
    status: &UploadStatus,
    bytes: u64,
    id: Option<&str>,
    publisher: Option<&str>,
) -> Result<()> {
    if status.id.is_empty()
        || status.publisher.is_empty()
        || publisher.is_some_and(|publisher| publisher != status.publisher)
        || id.is_some_and(|id| status.id != id)
        || status.expected_bytes != bytes
        || status.received_bytes > bytes
    {
        return Err(Error::Protocol);
    }
    Ok(())
}

fn unix_now() -> Result<u64> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .map_err(|_| Error::State)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        tests::{commit_records, make_ready, publishing_mock, synthetic_record},
        PublicationMode,
    };

    #[test]
    fn local_denial_before_publish_does_not_create_uncertainty_and_replacement_can_capture() {
        let temp = tempfile::tempdir().unwrap();
        let server = publishing_mock(|_| panic!("denied publisher sent a request"));
        let (store, collector, mut policy) = make_ready(temp.path(), server.endpoint());
        let path = store.pending_paths().unwrap().pop().unwrap();
        let mut pending: Pending = private_file::read(&path.join("pending.json")).unwrap();
        pending.upload = Some(UploadStatus {
            publisher: "synthetic-publisher".into(),
            id: "fully-staged-before-policy-change".into(),
            received_bytes: pending.member.bytes,
            expected_bytes: pending.member.bytes,
            expires_at: u64::MAX,
        });
        pending.publisher = Some("synthetic-publisher".into());
        pending.save(&path).unwrap();
        let original = pending.operation.clone();
        policy.revision += 1;
        policy.mode = PublicationMode::Reviewed {
            revisions: Default::default(),
        };
        store.set_policy(policy.clone()).unwrap();
        // Simulate policy narrowing after queue selection but before final
        // admission, using the existing private step without a runtime switch.
        assert_eq!(
            collector.step(&path, &mut pending, &AtomicBool::new(false), false),
            Err(Error::PolicyDenied)
        );
        let saved: Pending = private_file::read(&path.join("pending.json")).unwrap();
        assert!(!saved.publish_attempted && !saved.lookup_receipt);
        assert_eq!(saved.operation, original);
        assert_eq!(server.requests().len(), 1); // Initial policy authentication only.

        let data = temp.path().join("data");
        commit_records(
            &data,
            &[synthetic_record(
                "session-one",
                "approved B after local denial",
            )],
            2,
        );
        let replacement = store
            .prepare_policy(
                &data,
                &temp.path().join("review-b"),
                policy.mode,
                policy.sources,
            )
            .unwrap();
        store.set_policy(replacement).unwrap();
        assert_eq!(collector.tick(), TickOutcome::Progress);
        let b: Pending = private_file::read(&path.join("pending.json")).unwrap();
        assert_ne!(b.operation.idempotency_key, original.idempotency_key);
        assert_eq!(b.operation.expected_sequence, None);
        assert_eq!(server.requests().len(), 1);
    }
}
