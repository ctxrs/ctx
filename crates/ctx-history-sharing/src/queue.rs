use std::{
    fs,
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::atomic::{AtomicBool, Ordering},
};

use ctx_history_archive::{ArchiveIdentity, SessionMember};
use ctx_history_platform::platform_security::{
    create_private_directory_all, create_private_file_new, ensure_private_directory,
};
use ctx_history_server::{
    CancelPublishOutcome, CancelPublishRequest, CancelPublishResponse, Operation, PublicationState,
    Receipt, UploadStatus,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::{
    capture::hex, collector::CaptureStamp, private_file, Error, Result, SelectionObservation,
    SessionScope, SharingPolicy, SharingStore,
};

#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct Pending {
    pub identity: ArchiveIdentity,
    pub member: SessionMember,
    pub scope: SessionScope,
    pub operation: Operation,
    pub upload: Option<UploadStatus>,
    pub publisher: Option<String>,
    pub reconcile_offset: bool,
    pub lookup_receipt: bool,
    /// Persisted before any final publish request; never reset by a missing
    /// receipt or expired staging.
    pub publish_attempted: bool,
    /// Finish terminal settlement even if later policy approves these bytes.
    pub settlement_started: bool,
    pub retry_at: u64,
    pub failures: u32,
    pub last_error: Option<Error>,
}

#[derive(PartialEq, Eq)]
pub(crate) enum Enqueued {
    Captured,
    Pending,
    New,
}

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct SharingStatus {
    pub connected: bool,
    pub enabled: bool,
    pub paused: bool,
    pub pending: u64,
    pub held: u64,
    /// Current-policy selection observation; `held` above counts queue entries.
    #[serde(default)]
    pub selection: Option<SelectionObservation>,
    pub stored_sessions: u64,
    pub last_accepted_sequence: Option<u64>,
    pub last_error: Option<Error>,
}

impl SharingStore {
    pub fn status(&self) -> Result<SharingStatus> {
        let Some(settings) = self.settings()? else {
            return Ok(SharingStatus::default());
        };
        let mut status = SharingStatus {
            connected: true,
            enabled: settings.policy.is_some(),
            paused: settings.paused,
            last_error: private_file::read_optional(&self.root().join("last-error.json"))?,
            ..SharingStatus::default()
        };
        if let Some(policy) = &settings.policy {
            status.selection =
                private_file::read_optional::<CaptureStamp>(&self.root().join("capture.json"))?
                    .and_then(|stamp| stamp.selection(policy.revision));
        }
        for path in self.pending_paths()? {
            let pending: Pending = private_file::read(&path.join("pending.json"))?;
            status.pending += 1;
            if settings.paused || settings.policy.as_ref().is_none_or(|p| !pending.allowed(p)) {
                status.held += 1;
            }
            if status.last_error.is_none() {
                status.last_error = pending.last_error;
            }
        }
        for path in json_paths(&self.root().join("receipts"))? {
            let Some(publication): Option<PublicationState> = private_file::read(&path)? else {
                continue;
            };
            status.stored_sessions += 1;
            status.last_accepted_sequence = Some(
                status
                    .last_accepted_sequence
                    .unwrap_or(0)
                    .max(publication.sequence),
            );
        }
        Ok(status)
    }

    pub(crate) fn pending_paths(&self) -> Result<Vec<PathBuf>> {
        let root = self.root().join("queue");
        if !root.try_exists().map_err(|_| Error::State)? {
            return Ok(Vec::new());
        }
        let mut paths = Vec::new();
        for entry in fs::read_dir(root).map_err(|_| Error::State)? {
            let entry = entry.map_err(|_| Error::State)?;
            let name = entry.file_name();
            if name.to_str().is_some_and(is_digest) {
                if !entry.file_type().map_err(|_| Error::State)?.is_dir() {
                    return Err(Error::State);
                }
                paths.push(entry.path());
            }
        }
        paths.sort();
        Ok(paths)
    }

    pub(crate) fn enqueue(
        &self,
        archive: &Path,
        member: &SessionMember,
        scope: &SessionScope,
        policy: &SharingPolicy,
        stop: &AtomicBool,
    ) -> Result<Enqueued> {
        member.validate().map_err(|_| Error::Archive)?;
        let publication = publication_id(&policy.archive_identity, member)?;
        let queue_root = self.root().join("queue");
        create_private_directory_all(&queue_root).map_err(|_| Error::State)?;
        create_private_directory_all(&self.root().join("receipts")).map_err(|_| Error::State)?;
        let pending_path = queue_root.join(&publication);
        if pending_path.try_exists().map_err(|_| Error::State)? {
            let pending: Pending = private_file::read(&pending_path.join("pending.json"))?;
            // The same revision is already durable. Unresolved submissions
            // keep their exact bytes and identity until receipt reconciliation.
            if pending.member.sha256 == member.sha256 {
                return Ok(Enqueued::Captured);
            }
            if pending.allowed(policy) || pending.may_have_submitted() {
                return Ok(Enqueued::Pending);
            }
            // The caller selected the replacement under current policy. No
            // publish request ever used this denied predecessor's key, so its
            // staging (if any) may expire without a receipt or consent widening.
            let retired = self.retire(&pending_path, ".discarded-")?;
            let _ = fs::remove_dir_all(retired);
        }
        let previous = self.publication_checkpoint(&publication)?;
        if previous
            .as_ref()
            .is_some_and(|p| p.withdrawn || p.writer_epoch != policy.writer_epoch)
        {
            return Err(Error::Conflict);
        }
        if previous
            .as_ref()
            .is_some_and(|r| r.revision == member.sha256)
        {
            return Ok(Enqueued::Captured);
        }
        let pending = Pending {
            identity: policy.archive_identity.clone(),
            member: member.clone(),
            scope: scope.clone(),
            operation: Operation {
                idempotency_key: uuid::Uuid::new_v4().to_string(),
                publication: publication.clone(),
                writer_epoch: policy.writer_epoch,
                policy_revision: policy.revision,
                expected_revision: previous.as_ref().map(|r| r.revision.clone()),
                expected_sequence: previous.map(|r| r.sequence),
                revision: member.sha256.clone(),
            },
            upload: None,
            publisher: None,
            reconcile_offset: false,
            lookup_receipt: false,
            publish_attempted: false,
            settlement_started: false,
            retry_at: 0,
            failures: 0,
            last_error: None,
        };
        let temporary = tempfile::Builder::new()
            .prefix(".pending-")
            .tempdir_in(&queue_root)
            .map_err(|_| Error::State)?;
        ensure_private_directory(temporary.path()).map_err(|_| Error::State)?;
        // Generated, validated archive paths only. Copy instead of linking: a
        // caller editing its preview must never edit queued publication bytes.
        let mut input = private_file::open(&archive.join(&member.path))?;
        let mut output =
            create_private_file_new(&temporary.path().join("payload")).map_err(|_| Error::State)?;
        let mut hash = Sha256::new();
        let mut copied = 0_u64;
        let mut buffer = [0_u8; 64 * 1024];
        loop {
            if stop.load(Ordering::Acquire) {
                return Err(Error::PolicyDenied);
            }
            let n = input.read(&mut buffer).map_err(|_| Error::Archive)?;
            if n == 0 {
                break;
            }
            hash.update(&buffer[..n]);
            output.write_all(&buffer[..n]).map_err(|_| Error::State)?;
            copied = copied.checked_add(n as u64).ok_or(Error::Archive)?;
        }
        if copied != member.bytes || hex(&hash.finalize()) != member.sha256 {
            return Err(Error::Archive);
        }
        output.sync_all().map_err(|_| Error::State)?;
        pending.save(temporary.path())?;
        fs::rename(temporary.path(), pending_path).map_err(|_| Error::State)?;
        private_file::sync_directory(&queue_root)?;
        Ok(Enqueued::New)
    }

    pub(crate) fn checkpoint_path(&self, publication: &str) -> PathBuf {
        self.root()
            .join("receipts")
            .join(format!("{publication}.json"))
    }

    pub(crate) fn publication_checkpoint(
        &self,
        publication: &str,
    ) -> Result<Option<PublicationState>> {
        Ok(private_file::read_optional::<Option<PublicationState>>(
            &self.checkpoint_path(publication),
        )?
        .flatten())
    }

    pub(crate) fn accepted(&self, path: &Path, pending: &Pending, receipt: Receipt) -> Result<()> {
        self.record_receipt(pending, &receipt)?;
        let retired = self.retire_accepted(path)?;
        // Cleanup failure cannot undo acceptance. The next tick retries only
        // cleanup; this directory can no longer be treated as pending work.
        let _ = fs::remove_dir_all(retired);
        Ok(())
    }

    pub(crate) fn record_receipt(&self, pending: &Pending, receipt: &Receipt) -> Result<()> {
        self.validate_receipt(pending, receipt)?;
        let publication = PublicationState {
            publication: receipt.operation.publication.clone(),
            owner: receipt.publisher.clone(),
            revision: receipt.operation.revision.clone(),
            sequence: receipt.sequence,
            writer_epoch: receipt.operation.writer_epoch,
            policy_revision: receipt.operation.policy_revision,
            withdrawn: false,
        };
        private_file::write(
            &self.checkpoint_path(&pending.operation.publication),
            &Some(publication),
        )
    }

    fn validate_receipt(&self, pending: &Pending, receipt: &Receipt) -> Result<()> {
        let settings = self.settings()?.ok_or(Error::NotConnected)?;
        if receipt.collection != settings.connection.collection
            || receipt.operation != pending.operation
            || pending.publisher.as_deref() != Some(receipt.publisher.as_str())
            || receipt.kind != "publish"
            || receipt.sequence == 0
            || receipt.payload.as_ref().is_none_or(|p| {
                p.sha256 != pending.member.sha256 || p.bytes != pending.member.bytes
            })
        {
            return Err(Error::Protocol);
        }
        Ok(())
    }

    pub(crate) fn settled(
        &self,
        path: &Path,
        pending: &Pending,
        request: &CancelPublishRequest,
        response: &CancelPublishResponse,
    ) -> Result<()> {
        self.record_settlement(pending, request, response)?;
        CaptureStamp::invalidate(self.root())?;
        let retired = self.retire(path, ".settled-")?;
        let _ = fs::remove_dir_all(retired);
        Ok(())
    }

    pub(crate) fn record_settlement(
        &self,
        pending: &Pending,
        request: &CancelPublishRequest,
        response: &CancelPublishResponse,
    ) -> Result<()> {
        if request.operation != pending.operation
            || pending.publisher.as_deref() != Some(request.publisher.as_str())
        {
            return Err(Error::Protocol);
        }
        match &response.outcome {
            CancelPublishOutcome::Accepted { receipt } => {
                self.validate_receipt(pending, receipt)?
            }
            CancelPublishOutcome::Cancelled {
                publisher,
                operation,
                fingerprint,
            } => {
                if publisher != &request.publisher
                    || operation != &request.operation
                    || fingerprint != &request.fingerprint
                {
                    return Err(Error::Protocol);
                }
            }
        }
        if response.publication.as_ref().is_some_and(|p| {
            p.publication != pending.operation.publication
                || p.owner.is_empty()
                || p.revision.is_empty()
                || p.sequence == 0
                || p.writer_epoch == 0
                || p.policy_revision == 0
        }) {
            return Err(Error::Protocol);
        }
        private_file::write(
            &self.checkpoint_path(&pending.operation.publication),
            &response.publication,
        )?;
        if response.publication.as_ref().is_some_and(|p| {
            p.owner != request.publisher
                || p.writer_epoch != pending.operation.writer_epoch
                || p.withdrawn
        }) {
            return Err(Error::Conflict);
        }
        Ok(())
    }

    /// Only after recording the matching durable receipt. Before this rename,
    /// the live entry remains intact and retryable; after it, only cleanup may
    /// touch the directory, even if recursive deletion is interrupted.
    pub(crate) fn retire_accepted(&self, path: &Path) -> Result<PathBuf> {
        self.retire(path, ".accepted-")
    }

    fn retire(&self, path: &Path, prefix: &str) -> Result<PathBuf> {
        let queue = self.root().join("queue");
        let retired = queue.join(format!("{prefix}{}", uuid::Uuid::new_v4()));
        fs::rename(path, &retired).map_err(|_| Error::State)?;
        private_file::sync_directory(&queue)?;
        Ok(retired)
    }

    pub(crate) fn cleanup_retired(&self) -> Result<()> {
        let queue = self.root().join("queue");
        if !queue.try_exists().map_err(|_| Error::State)? {
            return Ok(());
        }
        let mut retired = Vec::new();
        for entry in fs::read_dir(&queue).map_err(|_| Error::State)? {
            let entry = entry.map_err(|_| Error::State)?;
            if entry
                .file_name()
                .to_str()
                .and_then(|name| {
                    name.strip_prefix(".accepted-")
                        .or_else(|| name.strip_prefix(".discarded-"))
                        .or_else(|| name.strip_prefix(".settled-"))
                })
                .is_some_and(|id| uuid::Uuid::parse_str(id).is_ok())
                && entry.file_type().map_err(|_| Error::State)?.is_dir()
            {
                retired.push(entry.path());
            }
        }
        if !retired.is_empty() {
            // Also covers a process exit between rename and its directory
            // fsync: make retirement durable before removing any contents.
            private_file::sync_directory(&queue)?;
            for path in retired {
                let _ = fs::remove_dir_all(path);
            }
        }
        Ok(())
    }

    /// Called only under uploader ownership. These names never denote admitted
    /// work or user-retained previews; a crashed exporter/copier is their sole
    /// remaining owner once the lock has been acquired.
    pub(crate) fn cleanup_scratch(&self) -> Result<()> {
        for (root, prefix) in [
            (self.root().to_owned(), ".capture-"),
            (self.root().join("queue"), ".pending-"),
        ] {
            let entries = match fs::read_dir(root) {
                Ok(entries) => entries,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                Err(_) => return Err(Error::State),
            };
            for entry in entries {
                let entry = entry.map_err(|_| Error::State)?;
                if entry
                    .file_name()
                    .to_str()
                    .is_some_and(|name| name.starts_with(prefix) && name.len() > prefix.len())
                    && entry.file_type().map_err(|_| Error::State)?.is_dir()
                {
                    fs::remove_dir_all(entry.path()).map_err(|_| Error::State)?;
                }
            }
        }
        Ok(())
    }
}

impl Pending {
    pub(crate) fn may_have_submitted(&self) -> bool {
        self.publish_attempted || self.lookup_receipt || self.settlement_started
    }

    pub(crate) fn allowed(&self, policy: &SharingPolicy) -> bool {
        self.identity == policy.archive_identity
            && self.operation.writer_epoch == policy.writer_epoch
            && policy.select(&self.scope) == crate::SelectionDecision::Selected
    }

    pub(crate) fn save(&self, path: &Path) -> Result<()> {
        private_file::write(&path.join("pending.json"), self)
    }

    pub(crate) fn failed(&mut self, path: &Path, error: Error, now: u64) -> Result<()> {
        self.failures = self.failures.saturating_add(1);
        self.retry_at = now.saturating_add(30_u64.saturating_mul(1_u64 << self.failures.min(4)));
        self.last_error = Some(error);
        self.save(path)
    }
}

fn publication_id(identity: &ArchiveIdentity, member: &SessionMember) -> Result<String> {
    let bytes = serde_json::to_vec(&(identity, &member.source, member.session_id))
        .map_err(|_| Error::Archive)?;
    Ok(hex(&Sha256::digest(bytes)))
}

fn is_digest(text: &str) -> bool {
    text.len() == 64
        && text
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

fn json_paths(root: &Path) -> Result<Vec<PathBuf>> {
    if !root.try_exists().map_err(|_| Error::State)? {
        return Ok(Vec::new());
    }
    fs::read_dir(root)
        .map_err(|_| Error::State)?
        .map(|entry| entry.map(|e| e.path()).map_err(|_| Error::State))
        .filter(|entry| match entry {
            Ok(p) => p.extension().is_some_and(|x| x == "json"),
            Err(_) => true,
        })
        .collect()
}
