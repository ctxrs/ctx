use std::{
    collections::HashSet,
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

const RELEASED_OUTBOX_SCHEMA_VERSION: u16 = 1;
const OUTBOX_SCHEMA_VERSION: u16 = 2;
const OUTBOX_MAX_BYTES: u64 = 2 * 1024 * 1024;
const OUTBOX_MAX_BODY_BYTES: usize = 512 * 1024;
const OUTBOX_MAX_ENTRIES: usize = 128;
const OUTBOX_MAX_FLUSH_PER_CALL: usize = 10;
const OUTBOX_MAX_AGE_SECONDS: i64 = 30 * 24 * 60 * 60;
const RETRY_BASE_SECONDS: u64 = 5;
const RETRY_MAX_SECONDS: u64 = 60 * 60;
const OUTBOX_TEMP_PREFIX: &str = ".analytics-outbox-";
const OUTBOX_TEMP_SUFFIX: &str = ".tmp";

#[derive(Debug, Clone, Copy, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
enum OutboxEntryKind {
    Ordinary,
    DeliveryObservation,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct OutboxEntry {
    schema_version: u16,
    entry_id: String,
    endpoint_fingerprint: String,
    queued_at_epoch_seconds: i64,
    attempts: u16,
    next_attempt_at_epoch_seconds: i64,
    kind: OutboxEntryKind,
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
    observation_due: bool,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ReleasedOutboxEntryV1 {
    schema_version: u16,
    endpoint_fingerprint: String,
    queued_at_epoch_seconds: i64,
    attempts: u16,
    payload: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ReleasedOutboxStateV1 {
    schema_version: u16,
    entries: Vec<ReleasedOutboxEntryV1>,
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
            observation_due: false,
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

    fn record_failure(&mut self, class: AnalyticsDeliveryFailureClass) {
        self.failure_sequence = self.failure_sequence.saturating_add(1);
        self.last_failure_class = Some(class);
        // A later success must precede any observation that includes this
        // failure. This also coalesces an interrupted recovery with the next
        // one instead of reporting a failure as though it had recovered.
        self.observation_due = false;
    }

    fn record_ordinary_drop(&mut self, class: AnalyticsDeliveryFailureClass) {
        self.dropped = self.dropped.saturating_add(1);
        self.record_failure(class);
    }

    fn prune_expired(&mut self, now_epoch_seconds: i64) -> bool {
        let cutoff = now_epoch_seconds.saturating_sub(OUTBOX_MAX_AGE_SECONDS);
        let mut ordinary_dropped = 0_u64;
        let before = self.entries.len();
        self.entries.retain(|entry| {
            let keep = entry.queued_at_epoch_seconds >= cutoff;
            if !keep && entry.kind == OutboxEntryKind::Ordinary {
                ordinary_dropped = ordinary_dropped.saturating_add(1);
            }
            keep
        });
        if ordinary_dropped != 0 {
            self.dropped = self.dropped.saturating_add(ordinary_dropped);
            self.record_failure(AnalyticsDeliveryFailureClass::LocalIo);
        }
        self.entries.len() != before
    }

    fn normalize_timestamps(&mut self, now_epoch_seconds: i64) -> bool {
        let latest_retry = now_epoch_seconds.saturating_add(RETRY_MAX_SECONDS as i64);
        let mut normalized = false;
        for entry in &mut self.entries {
            if entry.queued_at_epoch_seconds > now_epoch_seconds {
                entry.queued_at_epoch_seconds = now_epoch_seconds;
                normalized = true;
            }
            if entry.next_attempt_at_epoch_seconds > latest_retry {
                entry.next_attempt_at_epoch_seconds = latest_retry;
                normalized = true;
            }
        }
        if normalized {
            self.record_failure(AnalyticsDeliveryFailureClass::LocalIo);
        }
        normalized
    }

    fn enforce_bounds(&mut self) -> Result<()> {
        while self.entries.len() > OUTBOX_MAX_ENTRIES {
            self.drop_oldest_for_bound();
        }
        loop {
            let body = serde_json::to_vec(self).context("serialize analytics outbox")?;
            if body.len() as u64 <= OUTBOX_MAX_BYTES {
                return Ok(());
            }
            if self.entries.is_empty() {
                bail!("analytics outbox metadata exceeds its size bound");
            }
            self.drop_oldest_for_bound();
        }
    }

    fn drop_oldest_for_bound(&mut self) {
        let entry = self.entries.remove(0);
        if entry.kind == OutboxEntryKind::Ordinary {
            self.record_ordinary_drop(AnalyticsDeliveryFailureClass::LocalIo);
        }
    }
}

struct LoadedState {
    state: OutboxState,
    dirty: bool,
    missing: bool,
}

enum StoredOutbox {
    Missing,
    Corrupt,
    State(OutboxState, bool),
}

pub(crate) struct OutboxObservation {
    pub(crate) event: AnalyticsDeliveryObservationV1,
    retry_attempts: u64,
    dropped: u64,
    failure_sequence: u64,
}

#[derive(Debug, Clone)]
pub(crate) struct SnapshotEntry {
    entry_id: String,
    endpoint_fingerprint: String,
    payload: String,
    attempts: u16,
    kind: OutboxEntryKind,
}

impl SnapshotEntry {
    pub(crate) fn payload(&self) -> &[u8] {
        self.payload.as_bytes()
    }

    fn matches(&self, entry: &OutboxEntry) -> bool {
        entry.entry_id == self.entry_id
            && entry.endpoint_fingerprint == self.endpoint_fingerprint
            && entry.payload == self.payload
            && entry.attempts == self.attempts
            && entry.kind == self.kind
    }
}

#[derive(Debug, Clone, Copy)]
pub(crate) enum DeliveryDisposition {
    Accepted,
    Retry {
        class: AnalyticsDeliveryFailureClass,
        retry_after: Option<Duration>,
    },
    Permanent {
        class: AnalyticsDeliveryFailureClass,
    },
}

pub(crate) struct AnalyticsOutbox {
    path: PathBuf,
}

pub(crate) struct UploaderLease {
    _lock: OutboxLock,
}

impl AnalyticsOutbox {
    pub(crate) fn open(path: PathBuf) -> Result<Self> {
        Self::open_at(path, utc_now().timestamp())
    }

    fn open_at(path: PathBuf, now_epoch_seconds: i64) -> Result<Self> {
        let parent = path
            .parent()
            .context("analytics outbox path has no parent")?
            .to_path_buf();
        fs::create_dir_all(&parent).context("create analytics outbox directory")?;
        let outbox = Self { path };
        let _lock = OutboxLock::acquire(&outbox.state_lock_path())?;
        if cleanup_orphan_temps(&parent)? {
            sync_parent(&parent)?;
        }
        let loaded = outbox.load_normalized(now_epoch_seconds)?;
        if loaded.dirty {
            outbox.persist(&loaded.state)?;
        }
        Ok(outbox)
    }

    pub(crate) fn purge(path: &Path) -> Result<()> {
        let Some(parent) = path.parent() else {
            bail!("analytics outbox path has no parent");
        };
        match fs::symlink_metadata(parent) {
            Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => {}
            Ok(_) => bail!("analytics outbox parent is not a safe directory"),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(error).context("inspect analytics outbox directory"),
        }
        let _lock = OutboxLock::acquire(&path.with_extension("lock"))?;
        let mut changed = cleanup_orphan_temps(parent)?;
        match fs::remove_file(path) {
            Ok(()) => changed = true,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error).context("remove analytics outbox"),
        }
        if changed {
            sync_parent(parent)
        } else {
            Ok(())
        }
    }

    pub(crate) fn try_begin_upload(&self) -> Result<Option<UploaderLease>> {
        Ok(
            OutboxLock::try_acquire(&self.path.with_extension("uploader.lock"))?
                .map(|lock| UploaderLease { _lock: lock }),
        )
    }

    pub(crate) fn append(&self, endpoint: &str, body: &[u8]) -> Result<()> {
        self.append_at(endpoint, body, utc_now().timestamp())
    }

    fn append_at(&self, endpoint: &str, body: &[u8], now_epoch_seconds: i64) -> Result<()> {
        let payload = validate_payload(body)?;
        let _lock = OutboxLock::acquire(&self.state_lock_path())?;
        let mut loaded = self.load_normalized(now_epoch_seconds)?;
        if body.len() > OUTBOX_MAX_BODY_BYTES {
            loaded
                .state
                .record_ordinary_drop(AnalyticsDeliveryFailureClass::LocalIo);
            self.persist(&loaded.state)?;
            bail!("analytics payload exceeds the outbox body bound");
        }
        loaded.state.entries.push(OutboxEntry {
            schema_version: OUTBOX_SCHEMA_VERSION,
            entry_id: uuid::Uuid::new_v4().to_string(),
            endpoint_fingerprint: endpoint_fingerprint(endpoint),
            queued_at_epoch_seconds: now_epoch_seconds,
            attempts: 0,
            next_attempt_at_epoch_seconds: 0,
            kind: OutboxEntryKind::Ordinary,
            payload,
        });
        loaded.state.enforce_bounds()?;
        self.persist(&loaded.state)
    }

    pub(crate) fn snapshot(&self, endpoint: &str) -> Result<Vec<SnapshotEntry>> {
        self.snapshot_at(endpoint, utc_now().timestamp())
    }

    fn snapshot_at(&self, endpoint: &str, now_epoch_seconds: i64) -> Result<Vec<SnapshotEntry>> {
        let _lock = OutboxLock::acquire(&self.state_lock_path())?;
        let loaded = self.load_normalized(now_epoch_seconds)?;
        if loaded.dirty {
            self.persist(&loaded.state)?;
        }
        let fingerprint = endpoint_fingerprint(endpoint);
        Ok(loaded
            .state
            .entries
            .iter()
            .filter(|entry| {
                entry.endpoint_fingerprint == fingerprint
                    && entry.next_attempt_at_epoch_seconds <= now_epoch_seconds
            })
            .take(OUTBOX_MAX_FLUSH_PER_CALL)
            .map(|entry| SnapshotEntry {
                entry_id: entry.entry_id.clone(),
                endpoint_fingerprint: entry.endpoint_fingerprint.clone(),
                payload: entry.payload.clone(),
                attempts: entry.attempts,
                kind: entry.kind,
            })
            .collect())
    }

    pub(crate) fn reconcile(
        &self,
        attempts: &[(SnapshotEntry, DeliveryDisposition)],
    ) -> Result<()> {
        self.reconcile_at(attempts, utc_now().timestamp())
    }

    pub(crate) fn contains_snapshot(&self, snapshot: &SnapshotEntry) -> Result<bool> {
        let _lock = OutboxLock::acquire(&self.state_lock_path())?;
        let loaded = self.load_normalized(utc_now().timestamp())?;
        if loaded.dirty {
            self.persist(&loaded.state)?;
        }
        Ok(!loaded.missing
            && loaded
                .state
                .entries
                .iter()
                .any(|entry| snapshot.matches(entry)))
    }

    fn reconcile_at(
        &self,
        attempts: &[(SnapshotEntry, DeliveryDisposition)],
        now_epoch_seconds: i64,
    ) -> Result<()> {
        if attempts.is_empty() {
            return Ok(());
        }
        let _lock = OutboxLock::acquire(&self.state_lock_path())?;
        let mut loaded = self.load_normalized(now_epoch_seconds)?;
        if loaded.missing {
            return Ok(());
        }
        for (snapshot, disposition) in attempts {
            let Some(index) = loaded
                .state
                .entries
                .iter()
                .position(|entry| entry.entry_id == snapshot.entry_id)
            else {
                continue;
            };
            let entry = &loaded.state.entries[index];
            if !snapshot.matches(entry) {
                bail!("analytics outbox entry changed while delivery was in flight");
            }
            match *disposition {
                DeliveryDisposition::Accepted => {
                    let accepted = loaded.state.entries.remove(index);
                    if accepted.kind == OutboxEntryKind::Ordinary
                        && (loaded.state.retry_attempts != 0
                            || loaded.state.dropped != 0
                            || loaded.state.last_failure_class.is_some())
                    {
                        loaded.state.observation_due = true;
                    }
                }
                DeliveryDisposition::Retry { class, retry_after } => {
                    if snapshot.kind == OutboxEntryKind::Ordinary {
                        loaded.state.retry_attempts = loaded.state.retry_attempts.saturating_add(1);
                        loaded.state.record_failure(class);
                    }
                    let entry = &mut loaded.state.entries[index];
                    entry.attempts = entry.attempts.saturating_add(1);
                    let delay = retry_delay(&entry.entry_id, entry.attempts, retry_after);
                    entry.next_attempt_at_epoch_seconds = now_epoch_seconds
                        .saturating_add(i64::try_from(delay.as_secs()).unwrap_or(i64::MAX));
                }
                DeliveryDisposition::Permanent { class } => {
                    let rejected = loaded.state.entries.remove(index);
                    if rejected.kind == OutboxEntryKind::Ordinary {
                        loaded.state.record_ordinary_drop(class);
                    }
                }
            }
        }
        loaded.state.enforce_bounds()?;
        self.persist(&loaded.state)
    }

    pub(crate) fn pending_observation(&self) -> Result<Option<OutboxObservation>> {
        self.pending_observation_at(utc_now().timestamp())
    }

    fn pending_observation_at(&self, now_epoch_seconds: i64) -> Result<Option<OutboxObservation>> {
        let _lock = OutboxLock::acquire(&self.state_lock_path())?;
        let mut loaded = self.load_normalized(now_epoch_seconds)?;
        let health_pending = loaded
            .state
            .entries
            .iter()
            .any(|entry| entry.kind == OutboxEntryKind::DeliveryObservation);
        let has_counters = loaded.state.retry_attempts != 0
            || loaded.state.dropped != 0
            || loaded.state.last_failure_class.is_some();
        if loaded.state.observation_due && !has_counters {
            loaded.state.observation_due = false;
            loaded.dirty = true;
        }
        if loaded.dirty {
            self.persist(&loaded.state)?;
        }
        if !loaded.state.observation_due || health_pending || !has_counters {
            return Ok(None);
        }
        let oldest_age_seconds = loaded
            .state
            .entries
            .iter()
            .filter(|entry| entry.kind == OutboxEntryKind::Ordinary)
            .map(|entry| {
                now_epoch_seconds
                    .saturating_sub(entry.queued_at_epoch_seconds)
                    .max(0) as u64
            })
            .max()
            .unwrap_or(0);
        Ok(Some(OutboxObservation {
            event: AnalyticsDeliveryObservationV1::new(
                loaded
                    .state
                    .entries
                    .iter()
                    .filter(|entry| entry.kind == OutboxEntryKind::Ordinary)
                    .count() as u64,
                loaded.state.retry_attempts,
                loaded.state.dropped,
                Duration::from_secs(oldest_age_seconds),
                loaded
                    .state
                    .last_failure_class
                    .unwrap_or(AnalyticsDeliveryFailureClass::None),
            ),
            retry_attempts: loaded.state.retry_attempts,
            dropped: loaded.state.dropped,
            failure_sequence: loaded.state.failure_sequence,
        }))
    }

    pub(crate) fn queue_observation(
        &self,
        endpoint: &str,
        body: &[u8],
        observation: &OutboxObservation,
    ) -> Result<()> {
        self.queue_observation_at(endpoint, body, observation, utc_now().timestamp())
    }

    fn queue_observation_at(
        &self,
        endpoint: &str,
        body: &[u8],
        observation: &OutboxObservation,
        now_epoch_seconds: i64,
    ) -> Result<()> {
        let payload = validate_payload(body)?;
        if body.len() > OUTBOX_MAX_BODY_BYTES {
            bail!("analytics delivery observation exceeds the outbox body bound");
        }
        let _lock = OutboxLock::acquire(&self.state_lock_path())?;
        let mut loaded = self.load_normalized(now_epoch_seconds)?;
        if !loaded.state.observation_due
            || loaded
                .state
                .entries
                .iter()
                .any(|entry| entry.kind == OutboxEntryKind::DeliveryObservation)
        {
            return Ok(());
        }
        if loaded.state.retry_attempts < observation.retry_attempts
            || loaded.state.dropped < observation.dropped
        {
            bail!("analytics delivery observation is stale");
        }
        loaded.state.entries.push(OutboxEntry {
            schema_version: OUTBOX_SCHEMA_VERSION,
            entry_id: uuid::Uuid::new_v4().to_string(),
            endpoint_fingerprint: endpoint_fingerprint(endpoint),
            queued_at_epoch_seconds: now_epoch_seconds,
            attempts: 0,
            next_attempt_at_epoch_seconds: 0,
            kind: OutboxEntryKind::DeliveryObservation,
            payload,
        });
        loaded.state.retry_attempts = loaded
            .state
            .retry_attempts
            .saturating_sub(observation.retry_attempts);
        loaded.state.dropped = loaded.state.dropped.saturating_sub(observation.dropped);
        if loaded.state.failure_sequence == observation.failure_sequence {
            loaded.state.last_failure_class = None;
            loaded.state.observation_due = false;
        } else {
            loaded.state.observation_due = true;
        }
        loaded.state.enforce_bounds()?;
        if loaded.state.failure_sequence != observation.failure_sequence {
            loaded.state.observation_due = true;
        }
        self.persist(&loaded.state)
    }

    fn state_lock_path(&self) -> PathBuf {
        self.path.with_extension("lock")
    }

    fn load_normalized(&self, now_epoch_seconds: i64) -> Result<LoadedState> {
        let (mut state, mut dirty, missing) = match read_state(&self.path)? {
            StoredOutbox::Missing => (OutboxState::empty(), false, true),
            StoredOutbox::Corrupt => (OutboxState::recovered(), true, false),
            StoredOutbox::State(state, migrated) => (state, migrated, false),
        };
        if validate_state(&state).is_err() {
            state = OutboxState::recovered();
            dirty = true;
        }
        if state.normalize_timestamps(now_epoch_seconds) {
            dirty = true;
        }
        if state.prune_expired(now_epoch_seconds) {
            dirty = true;
        }
        state.enforce_bounds()?;
        Ok(LoadedState {
            state,
            dirty,
            missing,
        })
    }

    fn persist(&self, state: &OutboxState) -> Result<()> {
        let body = serde_json::to_vec(state).context("serialize analytics outbox")?;
        if body.len() as u64 > OUTBOX_MAX_BYTES {
            bail!("analytics outbox exceeds its size bound");
        }
        write_private_file_durably(&self.path, &body)
    }
}

fn validate_payload(body: &[u8]) -> Result<String> {
    let parsed: Value =
        serde_json::from_slice(body).context("parse content-free analytics payload")?;
    if !parsed.is_object() {
        bail!("analytics outbox payload must be a JSON object");
    }
    Ok(std::str::from_utf8(body)
        .context("analytics outbox payload is not UTF-8")?
        .to_owned())
}

fn retry_delay(entry_id: &str, attempts: u16, retry_after: Option<Duration>) -> Duration {
    let exponent = u32::from(attempts.saturating_sub(1)).min(31);
    let exponential = RETRY_BASE_SECONDS
        .saturating_mul(1_u64 << exponent)
        .min(RETRY_MAX_SECONDS);
    let jitter_window = exponential / 2;
    let mut hasher = Sha256::new();
    hasher.update(entry_id.as_bytes());
    hasher.update(attempts.to_be_bytes());
    let digest = hasher.finalize();
    let jitter_seed = u64::from_be_bytes(digest[..8].try_into().unwrap_or([0; 8]));
    let jitter = jitter_seed % jitter_window.saturating_add(1);
    let backoff = exponential.saturating_add(jitter).min(RETRY_MAX_SECONDS);
    let retry_after = retry_after
        .map(|value| value.as_secs().min(RETRY_MAX_SECONDS))
        .unwrap_or(0);
    Duration::from_secs(backoff.max(retry_after))
}

fn endpoint_fingerprint(endpoint: &str) -> String {
    format!("{:x}", Sha256::digest(endpoint.as_bytes()))
}

fn validate_state(state: &OutboxState) -> Result<()> {
    if state.schema_version != OUTBOX_SCHEMA_VERSION {
        bail!("unsupported analytics outbox schema");
    }
    if state.entries.len() > OUTBOX_MAX_ENTRIES {
        bail!("analytics outbox exceeds its entry bound");
    }
    let mut ids = HashSet::with_capacity(state.entries.len());
    let mut health_entries = 0_usize;
    for entry in &state.entries {
        if entry.kind == OutboxEntryKind::DeliveryObservation {
            health_entries = health_entries.saturating_add(1);
        }
        if entry.schema_version != OUTBOX_SCHEMA_VERSION
            || uuid::Uuid::parse_str(&entry.entry_id).is_err()
            || !ids.insert(entry.entry_id.as_str())
            || entry.endpoint_fingerprint.len() != 64
            || !entry
                .endpoint_fingerprint
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
            || entry.payload.len() > OUTBOX_MAX_BODY_BYTES
            || !serde_json::from_str::<Value>(&entry.payload).is_ok_and(|value| value.is_object())
        {
            bail!("analytics outbox entry is invalid");
        }
    }
    if health_entries > 1 {
        bail!("analytics outbox contains multiple delivery observations");
    }
    Ok(())
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
    let Ok(value) = serde_json::from_slice::<Value>(&body) else {
        return Ok(StoredOutbox::Corrupt);
    };
    let schema_version = value
        .get("schema_version")
        .and_then(Value::as_u64)
        .and_then(|value| u16::try_from(value).ok());
    match schema_version {
        Some(OUTBOX_SCHEMA_VERSION) => Ok(serde_json::from_value(value)
            .map(|state| StoredOutbox::State(state, false))
            .unwrap_or(StoredOutbox::Corrupt)),
        Some(RELEASED_OUTBOX_SCHEMA_VERSION) => Ok(serde_json::from_value(value)
            .ok()
            .and_then(migrate_released_v1)
            .map(|state| StoredOutbox::State(state, true))
            .unwrap_or(StoredOutbox::Corrupt)),
        _ => Ok(StoredOutbox::Corrupt),
    }
}

fn migrate_released_v1(released: ReleasedOutboxStateV1) -> Option<OutboxState> {
    if released.schema_version != RELEASED_OUTBOX_SCHEMA_VERSION
        || released
            .entries
            .iter()
            .any(|entry| entry.schema_version != RELEASED_OUTBOX_SCHEMA_VERSION)
    {
        return None;
    }
    Some(OutboxState {
        schema_version: OUTBOX_SCHEMA_VERSION,
        entries: released
            .entries
            .into_iter()
            .map(|entry| OutboxEntry {
                schema_version: OUTBOX_SCHEMA_VERSION,
                entry_id: uuid::Uuid::new_v4().to_string(),
                endpoint_fingerprint: entry.endpoint_fingerprint,
                queued_at_epoch_seconds: entry.queued_at_epoch_seconds,
                attempts: entry.attempts,
                next_attempt_at_epoch_seconds: 0,
                kind: OutboxEntryKind::Ordinary,
                payload: entry.payload,
            })
            .collect(),
        retry_attempts: released.retry_attempts,
        dropped: released.dropped,
        failure_sequence: released.failure_sequence,
        last_failure_class: released.last_failure_class,
        observation_due: false,
    })
}

struct OutboxLock(fs::File);

impl OutboxLock {
    fn acquire(path: &Path) -> Result<Self> {
        let file = open_lock_file(path)?;
        file.lock_exclusive()
            .context("acquire analytics outbox lock")?;
        Ok(Self(file))
    }

    fn try_acquire(path: &Path) -> Result<Option<Self>> {
        let file = open_lock_file(path)?;
        match file.try_lock_exclusive() {
            Ok(()) => Ok(Some(Self(file))),
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => Ok(None),
            Err(error) => Err(error).context("acquire analytics uploader lock"),
        }
    }
}

impl Drop for OutboxLock {
    fn drop(&mut self) {
        let _ = fs2::FileExt::unlock(&self.0);
    }
}

fn open_lock_file(path: &Path) -> Result<fs::File> {
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
        use windows_sys::Win32::{
            Foundation::{GENERIC_READ, GENERIC_WRITE},
            Storage::FileSystem::{
                FILE_FLAG_OPEN_REPARSE_POINT, FILE_SHARE_READ, FILE_SHARE_WRITE, READ_CONTROL,
                WRITE_DAC,
            },
        };

        options
            .access_mode(GENERIC_READ | GENERIC_WRITE | READ_CONTROL | WRITE_DAC)
            .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
            .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT);
    }
    let file = options.open(path).context("open analytics outbox lock")?;
    restrict_private_file_handle(&file).context("protect analytics outbox lock")?;
    Ok(file)
}

fn cleanup_orphan_temps(parent: &Path) -> Result<bool> {
    let mut removed = false;
    for entry in fs::read_dir(parent).context("inspect analytics outbox temporary files")? {
        let entry = entry.context("inspect analytics outbox temporary entry")?;
        let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
            continue;
        };
        let Some(id) = name
            .strip_prefix(OUTBOX_TEMP_PREFIX)
            .and_then(|name| name.strip_suffix(OUTBOX_TEMP_SUFFIX))
        else {
            continue;
        };
        if uuid::Uuid::parse_str(id).is_err() {
            continue;
        }
        let metadata = fs::symlink_metadata(entry.path())
            .context("inspect analytics outbox temporary file")?;
        if metadata.is_file() || metadata.file_type().is_symlink() {
            fs::remove_file(entry.path()).context("remove analytics outbox temporary file")?;
            removed = true;
        }
    }
    Ok(removed)
}

fn write_private_file_durably(path: &Path, body: &[u8]) -> Result<()> {
    let parent = path
        .parent()
        .context("analytics outbox path has no parent")?;
    let temp = parent.join(format!(
        "{OUTBOX_TEMP_PREFIX}{}{OUTBOX_TEMP_SUFFIX}",
        uuid::Uuid::new_v4()
    ));
    let result = (|| -> Result<()> {
        let mut options = fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt as _;
            options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
        }
        #[cfg(windows)]
        {
            use std::os::windows::fs::OpenOptionsExt as _;
            use windows_sys::Win32::{
                Foundation::{GENERIC_READ, GENERIC_WRITE},
                Storage::FileSystem::{
                    FILE_FLAG_OPEN_REPARSE_POINT, FILE_SHARE_READ, READ_CONTROL, WRITE_DAC,
                },
            };

            options
                .access_mode(GENERIC_READ | GENERIC_WRITE | READ_CONTROL | WRITE_DAC)
                .share_mode(FILE_SHARE_READ)
                .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT);
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
#[path = "analytics_outbox/tests.rs"]
mod tests;
