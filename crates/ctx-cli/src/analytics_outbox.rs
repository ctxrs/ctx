use std::{
    fs,
    io::{Read as _, Write as _},
    path::{Path, PathBuf},
    time::Duration,
};

use anyhow::{bail, Context as _, Result};
use ctx_client_observability::analytics::{
    AnalyticsDeliveryFailureClass, AnalyticsDeliveryObservationV1,
};
use ctx_history_core::utc_now;
use ctx_history_platform::platform_security::{restrict_private_file_handle, verify_private_file};
use fs2::FileExt as _;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest as _, Sha256};

const OUTBOX_SCHEMA_VERSION: u16 = 1;
const OUTBOX_MAX_BYTES: u64 = 2 * 1024 * 1024;
const OUTBOX_MAX_BODY_BYTES: usize = 512 * 1024;
const OUTBOX_MAX_ENTRIES: usize = 128;
const OUTBOX_MAX_FLUSH_PER_CALL: usize = 10;
const OUTBOX_MAX_AGE_SECONDS: i64 = 30 * 24 * 60 * 60;

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct OutboxEntry {
    schema_version: u16,
    endpoint_fingerprint: String,
    queued_at_epoch_seconds: i64,
    attempts: u16,
    payload: String,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct OutboxState {
    schema_version: u16,
    entries: Vec<OutboxEntry>,
    retry_attempts: u64,
    dropped: u64,
    failure_sequence: u64,
    last_failure_class: Option<AnalyticsDeliveryFailureClass>,
}

impl OutboxState {
    fn empty() -> Self {
        Self {
            schema_version: OUTBOX_SCHEMA_VERSION,
            entries: Vec::new(),
            retry_attempts: 0,
            dropped: 0,
            failure_sequence: 0,
            last_failure_class: None,
        }
    }

    fn recovered() -> Self {
        Self {
            dropped: 1,
            failure_sequence: 1,
            last_failure_class: Some(AnalyticsDeliveryFailureClass::LocalIo),
            ..Self::empty()
        }
    }
}

enum StoredOutbox {
    Missing,
    Corrupt,
    State(OutboxState),
}

pub(crate) struct OutboxObservation {
    pub(crate) event: AnalyticsDeliveryObservationV1,
    retry_attempts: u64,
    dropped: u64,
    failure_sequence: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FlushStatus {
    Available,
    Blocked(AnalyticsDeliveryFailureClass),
}

pub(crate) struct AnalyticsOutbox {
    path: PathBuf,
    _lock: OutboxLock,
    state: OutboxState,
}

impl AnalyticsOutbox {
    pub(crate) fn open(path: PathBuf) -> Result<Self> {
        let parent = path
            .parent()
            .context("analytics outbox path has no parent")?;
        fs::create_dir_all(parent).context("create analytics outbox directory")?;
        let lock = OutboxLock::acquire(&path.with_extension("lock"))?;
        let (state, recovered) = match read_state(&path)? {
            StoredOutbox::Missing => (OutboxState::empty(), false),
            StoredOutbox::Corrupt => (OutboxState::recovered(), true),
            StoredOutbox::State(state) => (state, false),
        };
        let mut outbox = Self {
            path,
            _lock: lock,
            state,
        };
        let recovered = recovered || outbox.recover_invalid_state();
        if recovered || outbox.prune_expired() {
            outbox.persist()?;
        }
        Ok(outbox)
    }

    pub(crate) fn purge(path: &Path) -> Result<()> {
        match fs::symlink_metadata(path) {
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(error).context("inspect analytics outbox"),
        }
        let Some(parent) = path.parent() else {
            bail!("analytics outbox path has no parent");
        };
        fs::create_dir_all(parent).context("create analytics outbox directory")?;
        let _lock = OutboxLock::acquire(&path.with_extension("lock"))?;
        match fs::remove_file(path) {
            Ok(()) => sync_parent(parent),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error).context("remove analytics outbox"),
        }
    }

    pub(crate) fn flush(
        &mut self,
        endpoint: &str,
        mut post: impl FnMut(&[u8]) -> std::result::Result<(), AnalyticsDeliveryFailureClass>,
    ) -> Result<FlushStatus> {
        let fingerprint = endpoint_fingerprint(endpoint);
        let mut index = 0;
        let mut attempted = 0;
        while index < self.state.entries.len() && attempted < OUTBOX_MAX_FLUSH_PER_CALL {
            if self.state.entries[index].endpoint_fingerprint != fingerprint {
                index += 1;
                continue;
            }
            attempted += 1;
            self.state.retry_attempts = self.state.retry_attempts.saturating_add(1);
            self.state.entries[index].attempts =
                self.state.entries[index].attempts.saturating_add(1);
            let body = self.state.entries[index].payload.as_bytes().to_vec();
            match post(&body) {
                Ok(()) => {
                    self.state.entries.remove(index);
                    self.persist()?;
                }
                Err(class) => {
                    self.record_failure(class);
                    self.persist()?;
                    return Ok(FlushStatus::Blocked(class));
                }
            }
        }
        Ok(FlushStatus::Available)
    }

    pub(crate) fn enqueue(
        &mut self,
        endpoint: &str,
        body: &[u8],
        failure_class: AnalyticsDeliveryFailureClass,
    ) -> Result<()> {
        if body.len() > OUTBOX_MAX_BODY_BYTES {
            self.state.dropped = self.state.dropped.saturating_add(1);
            self.record_failure(failure_class);
            self.persist()?;
            bail!("analytics payload exceeds the outbox body bound");
        }
        let parsed: Value =
            serde_json::from_slice(body).context("parse content-free analytics payload")?;
        if !parsed.is_object() {
            bail!("analytics outbox payload must be a JSON object");
        }
        let payload = std::str::from_utf8(body)
            .context("analytics outbox payload is not UTF-8")?
            .to_owned();
        self.state.entries.push(OutboxEntry {
            schema_version: OUTBOX_SCHEMA_VERSION,
            endpoint_fingerprint: endpoint_fingerprint(endpoint),
            queued_at_epoch_seconds: utc_now().timestamp(),
            attempts: 0,
            payload,
        });
        self.record_failure(failure_class);
        self.enforce_bounds()?;
        self.persist()
    }

    pub(crate) fn observation(&self) -> Option<OutboxObservation> {
        if self.state.entries.is_empty()
            && self.state.retry_attempts == 0
            && self.state.dropped == 0
            && self.state.last_failure_class.is_none()
        {
            return None;
        }
        let now = utc_now().timestamp();
        let oldest_age_seconds = self
            .state
            .entries
            .iter()
            .map(|entry| now.saturating_sub(entry.queued_at_epoch_seconds).max(0) as u64)
            .max()
            .unwrap_or(0);
        Some(OutboxObservation {
            event: AnalyticsDeliveryObservationV1::new(
                self.state.entries.len() as u64,
                self.state.retry_attempts,
                self.state.dropped,
                Duration::from_secs(oldest_age_seconds),
                self.state
                    .last_failure_class
                    .unwrap_or(AnalyticsDeliveryFailureClass::None),
            ),
            retry_attempts: self.state.retry_attempts,
            dropped: self.state.dropped,
            failure_sequence: self.state.failure_sequence,
        })
    }

    pub(crate) fn acknowledge(&mut self, observation: &OutboxObservation) -> Result<()> {
        self.state.retry_attempts = self
            .state
            .retry_attempts
            .saturating_sub(observation.retry_attempts);
        self.state.dropped = self.state.dropped.saturating_sub(observation.dropped);
        if self.state.failure_sequence == observation.failure_sequence {
            self.state.last_failure_class = None;
        }
        self.persist()
    }

    fn prune_expired(&mut self) -> bool {
        let cutoff = utc_now().timestamp().saturating_sub(OUTBOX_MAX_AGE_SECONDS);
        let before = self.state.entries.len();
        self.state
            .entries
            .retain(|entry| entry.queued_at_epoch_seconds >= cutoff);
        let removed = before.saturating_sub(self.state.entries.len()) as u64;
        self.state.dropped = self.state.dropped.saturating_add(removed);
        removed != 0
    }

    fn enforce_bounds(&mut self) -> Result<()> {
        while self.state.entries.len() > OUTBOX_MAX_ENTRIES {
            self.state.entries.remove(0);
            self.state.dropped = self.state.dropped.saturating_add(1);
        }
        loop {
            let body = serde_json::to_vec(&self.state).context("serialize analytics outbox")?;
            if body.len() as u64 <= OUTBOX_MAX_BYTES {
                return Ok(());
            }
            if self.state.entries.is_empty() {
                bail!("analytics outbox metadata exceeds its size bound");
            }
            self.state.entries.remove(0);
            self.state.dropped = self.state.dropped.saturating_add(1);
        }
    }

    fn record_failure(&mut self, class: AnalyticsDeliveryFailureClass) {
        self.state.failure_sequence = self.state.failure_sequence.saturating_add(1);
        self.state.last_failure_class = Some(class);
    }

    fn recover_invalid_state(&mut self) -> bool {
        if self.validate_state().is_ok() {
            return false;
        }
        self.state = OutboxState::recovered();
        true
    }

    fn validate_state(&self) -> Result<()> {
        if self.state.schema_version != OUTBOX_SCHEMA_VERSION {
            bail!("unsupported analytics outbox schema");
        }
        if self.state.entries.len() > OUTBOX_MAX_ENTRIES {
            bail!("analytics outbox exceeds its entry bound");
        }
        for entry in &self.state.entries {
            if entry.schema_version != OUTBOX_SCHEMA_VERSION
                || entry.endpoint_fingerprint.len() != 64
                || !entry
                    .endpoint_fingerprint
                    .bytes()
                    .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
                || entry.payload.len() > OUTBOX_MAX_BODY_BYTES
                || !serde_json::from_str::<Value>(&entry.payload)
                    .is_ok_and(|value| value.is_object())
            {
                bail!("analytics outbox entry is invalid");
            }
        }
        Ok(())
    }

    fn persist(&self) -> Result<()> {
        let body = serde_json::to_vec(&self.state).context("serialize analytics outbox")?;
        if body.len() as u64 > OUTBOX_MAX_BYTES {
            bail!("analytics outbox exceeds its size bound");
        }
        write_private_file_durably(&self.path, &body)
    }
}

fn endpoint_fingerprint(endpoint: &str) -> String {
    format!("{:x}", Sha256::digest(endpoint.as_bytes()))
}

fn read_state(path: &Path) -> Result<StoredOutbox> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(StoredOutbox::Missing);
        }
        Err(error) => return Err(error).context("inspect analytics outbox"),
    };
    if !metadata.is_file() || metadata.file_type().is_symlink() {
        bail!("analytics outbox is not a safe regular file");
    }
    verify_private_file(path).context("verify analytics outbox permissions")?;
    if metadata.len() > OUTBOX_MAX_BYTES {
        return Ok(StoredOutbox::Corrupt);
    }
    let file = fs::File::open(path).context("open analytics outbox")?;
    let mut body = Vec::with_capacity(metadata.len() as usize);
    file.take(OUTBOX_MAX_BYTES.saturating_add(1))
        .read_to_end(&mut body)
        .context("read analytics outbox")?;
    if body.len() as u64 > OUTBOX_MAX_BYTES {
        return Ok(StoredOutbox::Corrupt);
    }
    Ok(match serde_json::from_slice(&body) {
        Ok(state) => StoredOutbox::State(state),
        Err(_) => StoredOutbox::Corrupt,
    })
}

struct OutboxLock(fs::File);

impl OutboxLock {
    fn acquire(path: &Path) -> Result<Self> {
        let mut options = fs::OpenOptions::new();
        options.read(true).write(true).create(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt as _;
            options
                .mode(0o600)
                .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW);
        }
        #[cfg(windows)]
        {
            use std::os::windows::fs::OpenOptionsExt as _;
            const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
            options.custom_flags(FILE_FLAG_OPEN_REPARSE_POINT);
        }
        let file = options.open(path).context("open analytics outbox lock")?;
        restrict_private_file_handle(&file).context("protect analytics outbox lock")?;
        file.lock_exclusive()
            .context("acquire analytics outbox lock")?;
        Ok(Self(file))
    }
}

impl Drop for OutboxLock {
    fn drop(&mut self) {
        let _ = fs2::FileExt::unlock(&self.0);
    }
}

fn write_private_file_durably(path: &Path, body: &[u8]) -> Result<()> {
    let parent = path
        .parent()
        .context("analytics outbox path has no parent")?;
    let temp = parent.join(format!(".analytics-outbox-{}.tmp", uuid::Uuid::new_v4()));
    let result = (|| -> Result<()> {
        let mut options = fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt as _;
            options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
        }
        let mut file = options
            .open(&temp)
            .context("create analytics outbox temporary file")?;
        restrict_private_file_handle(&file).context("protect analytics outbox temporary file")?;
        file.write_all(body)
            .context("write analytics outbox temporary file")?;
        file.sync_all()
            .context("sync analytics outbox temporary file")?;
        drop(file);
        replace_file(&temp, path).context("publish analytics outbox")?;
        verify_private_file(path).context("verify analytics outbox permissions")?;
        sync_parent(parent)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temp);
    }
    result
}

#[cfg(not(windows))]
fn replace_file(source: &Path, target: &Path) -> std::io::Result<()> {
    fs::rename(source, target)
}

#[cfg(windows)]
fn replace_file(source: &Path, target: &Path) -> std::io::Result<()> {
    use std::os::windows::ffi::OsStrExt as _;
    use windows_sys::Win32::Storage::FileSystem::{
        MoveFileExW, MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH,
    };

    let source = source
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect::<Vec<_>>();
    let target = target
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect::<Vec<_>>();
    let moved = unsafe {
        MoveFileExW(
            source.as_ptr(),
            target.as_ptr(),
            MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
        )
    };
    if moved == 0 {
        Err(std::io::Error::last_os_error())
    } else {
        Ok(())
    }
}

#[cfg(unix)]
fn sync_parent(path: &Path) -> Result<()> {
    fs::File::open(path)
        .context("open analytics outbox directory")?
        .sync_all()
        .context("sync analytics outbox directory")
}

#[cfg(not(unix))]
fn sync_parent(_path: &Path) -> Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use ctx_client_observability::analytics::CountBucket;

    fn body(event_id: &str) -> Vec<u8> {
        serde_json::to_vec(&serde_json::json!({
            "client_profile_id": "00000000-0000-4000-8000-000000000001",
            "data_root_id": "00000000-0000-4000-8000-000000000002",
            "events": [{"event_id": event_id, "properties": {}}]
        }))
        .unwrap()
    }

    #[test]
    fn failed_body_retries_with_the_original_event_id() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("outbox.json");
        let mut outbox = AnalyticsOutbox::open(path.clone()).unwrap();
        let original = body("c220e8ef-eb0b-43d9-89c8-c64806e87d93");
        outbox
            .enqueue(
                "https://cli.ctx.rs/functions/v1/analytics",
                &original,
                AnalyticsDeliveryFailureClass::Transport,
            )
            .unwrap();
        drop(outbox);

        let mut observed = Vec::new();
        let mut reopened = AnalyticsOutbox::open(path).unwrap();
        let status = reopened
            .flush("https://cli.ctx.rs/functions/v1/analytics", |payload| {
                observed.push(payload.to_vec());
                Ok(())
            })
            .unwrap();

        assert_eq!(status, FlushStatus::Available);
        assert_eq!(observed, vec![original]);
        assert_eq!(
            reopened.observation().unwrap().event.queued,
            ctx_client_observability::analytics::CountBucket::Zero
        );
    }

    #[test]
    fn endpoint_fingerprint_prevents_cross_endpoint_replay() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("outbox.json");
        let mut outbox = AnalyticsOutbox::open(path).unwrap();
        outbox
            .enqueue(
                "https://one.example.test/events",
                &body("c220e8ef-eb0b-43d9-89c8-c64806e87d93"),
                AnalyticsDeliveryFailureClass::Transport,
            )
            .unwrap();
        let mut posts = 0;

        outbox
            .flush("https://two.example.test/events", |_| {
                posts += 1;
                Ok(())
            })
            .unwrap();

        assert_eq!(posts, 0);
        assert_eq!(outbox.state.entries.len(), 1);
    }

    #[test]
    fn purge_removes_payload_and_does_not_create_missing_state() {
        let root = tempfile::tempdir().unwrap();
        let missing = root.path().join("missing").join("outbox.json");
        AnalyticsOutbox::purge(&missing).unwrap();
        assert!(!missing.parent().unwrap().exists());

        let path = root.path().join("outbox.json");
        let mut outbox = AnalyticsOutbox::open(path.clone()).unwrap();
        outbox
            .enqueue(
                "https://cli.ctx.rs/functions/v1/analytics",
                &body("c220e8ef-eb0b-43d9-89c8-c64806e87d93"),
                AnalyticsDeliveryFailureClass::Transport,
            )
            .unwrap();
        drop(outbox);
        assert!(path.exists());

        AnalyticsOutbox::purge(&path).unwrap();
        assert!(!path.exists());
    }

    #[test]
    fn corrupt_private_state_recovers_and_reports_one_safe_drop() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("outbox.json");
        write_private_file_durably(&path, b"not-json").unwrap();

        let outbox = AnalyticsOutbox::open(path.clone()).unwrap();
        let observation = outbox.observation().unwrap().event;

        assert!(outbox.state.entries.is_empty());
        assert_eq!(observation.dropped, CountBucket::One);
        assert_eq!(
            observation.failure_class,
            AnalyticsDeliveryFailureClass::LocalIo
        );
        assert!(serde_json::from_slice::<OutboxState>(&fs::read(path).unwrap()).is_ok());
    }

    #[test]
    fn unsafe_state_path_still_fails_closed() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("outbox.json");
        fs::create_dir(&path).unwrap();

        assert!(AnalyticsOutbox::open(path).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn unsafe_state_permissions_still_fail_closed() {
        use std::os::unix::fs::PermissionsExt as _;

        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("outbox.json");
        write_private_file_durably(&path, b"not-json").unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();

        assert!(AnalyticsOutbox::open(path).is_err());
    }
}
