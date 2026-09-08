use std::{
    collections::{BTreeMap, HashSet},
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

const OUTBOX_SCHEMA_VERSION: u16 = 3;
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
    data_root_id: String,
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
    roots: BTreeMap<String, RootDeliveryState>,
}

#[derive(Debug, Clone, Copy, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct RootDeliveryState {
    retry_attempts: u64,
    dropped: u64,
    failure_sequence: u64,
    last_failure_class: Option<AnalyticsDeliveryFailureClass>,
    observation_due: bool,
}

impl RootDeliveryState {
    fn record_failure(&mut self, class: AnalyticsDeliveryFailureClass) {
        self.failure_sequence = self.failure_sequence.saturating_add(1);
        self.last_failure_class = Some(class);
        self.observation_due = false;
    }

    fn record_ordinary_drop(&mut self, class: AnalyticsDeliveryFailureClass) {
        self.dropped = self.dropped.saturating_add(1);
        self.record_failure(class);
    }

    fn has_counters(&self) -> bool {
        self.retry_attempts != 0 || self.dropped != 0 || self.last_failure_class.is_some()
    }
}

impl OutboxState {
    fn empty() -> Self {
        Self {
            schema_version: OUTBOX_SCHEMA_VERSION,
            entries: Vec::new(),
            roots: BTreeMap::new(),
        }
    }

    fn root(&self, data_root_id: &str) -> RootDeliveryState {
        self.roots.get(data_root_id).copied().unwrap_or_default()
    }

    fn root_mut(&mut self, data_root_id: &str) -> &mut RootDeliveryState {
        self.roots.entry(data_root_id.to_owned()).or_default()
    }

    fn recovered(data_root_id: &str) -> Self {
        let mut state = Self::empty();
        state
            .root_mut(data_root_id)
            .record_ordinary_drop(AnalyticsDeliveryFailureClass::LocalIo);
        state
    }

    fn prune_expired(&mut self, now_epoch_seconds: i64) -> bool {
        let cutoff = now_epoch_seconds.saturating_sub(OUTBOX_MAX_AGE_SECONDS);
        let before = self.entries.len();
        self.entries.retain(|entry| {
            let keep = entry.queued_at_epoch_seconds >= cutoff;
            if !keep && entry.kind == OutboxEntryKind::Ordinary {
                self.roots
                    .entry(entry.data_root_id.clone())
                    .or_default()
                    .record_ordinary_drop(AnalyticsDeliveryFailureClass::LocalIo);
            }
            keep
        });
        self.entries.len() != before
    }

    fn normalize_timestamps(&mut self, now_epoch_seconds: i64) -> bool {
        let latest_retry = now_epoch_seconds.saturating_add(RETRY_MAX_SECONDS as i64);
        let mut normalized = false;
        for entry in &mut self.entries {
            let changed = entry.queued_at_epoch_seconds > now_epoch_seconds
                || entry.next_attempt_at_epoch_seconds > latest_retry;
            entry.queued_at_epoch_seconds = entry.queued_at_epoch_seconds.min(now_epoch_seconds);
            entry.next_attempt_at_epoch_seconds =
                entry.next_attempt_at_epoch_seconds.min(latest_retry);
            if changed {
                self.roots
                    .entry(entry.data_root_id.clone())
                    .or_default()
                    .record_failure(AnalyticsDeliveryFailureClass::LocalIo);
                normalized = true;
            }
        }
        normalized
    }

    fn trim_root_metadata(&mut self) {
        let queued_roots: HashSet<_> = self
            .entries
            .iter()
            .map(|entry| entry.data_root_id.as_str())
            .collect();
        self.roots
            .retain(|id, state| queued_roots.contains(id.as_str()) || state.has_counters());
        // Every root with entries retains its counters. Counter-only records
        // are best effort and cannot turn root churn into unbounded storage.
        let excess = self.roots.len().saturating_sub(OUTBOX_MAX_ENTRIES);
        let evicted: Vec<_> = self
            .roots
            .keys()
            .filter(|id| !queued_roots.contains(id.as_str()))
            .take(excess)
            .cloned()
            .collect();
        for id in evicted {
            self.roots.remove(&id);
        }
    }

    fn enforce_bounds(&mut self) -> Result<()> {
        while self.entries.len() > OUTBOX_MAX_ENTRIES {
            self.drop_oldest_for_bound();
        }
        loop {
            self.trim_root_metadata();
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
            self.root_mut(&entry.data_root_id)
                .record_ordinary_drop(AnalyticsDeliveryFailureClass::LocalIo);
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
    data_root_id: String,
    pub(crate) event: AnalyticsDeliveryObservationV1,
    retry_attempts: u64,
    dropped: u64,
    failure_sequence: u64,
}

#[derive(Debug, Clone)]
pub(crate) struct SnapshotEntry {
    entry_id: String,
    data_root_id: String,
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
            && entry.data_root_id == self.data_root_id
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
    data_root_id: String,
}

pub(crate) struct UploaderLease {
    _lock: OutboxLock,
}

impl AnalyticsOutbox {
    pub(crate) fn open(path: PathBuf, data_root_id: &str) -> Result<Self> {
        Self::open_at(path, data_root_id, utc_now().timestamp())
    }

    fn open_at(path: PathBuf, data_root_id: &str, now_epoch_seconds: i64) -> Result<Self> {
        validate_root_id(data_root_id)?;
        let parent = path
            .parent()
            .context("analytics outbox path has no parent")?
            .to_path_buf();
        fs::create_dir_all(&parent).context("create analytics outbox directory")?;
        let outbox = Self {
            path,
            data_root_id: data_root_id.to_owned(),
        };
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

    pub(crate) fn purge(path: &Path, data_root_id: Option<&str>) -> Result<()> {
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
        if let StoredOutbox::State(mut state, _) = read_state(path)? {
            if validate_state(&state).is_ok() {
                state
                    .entries
                    .retain(|entry| Some(entry.data_root_id.as_str()) != data_root_id);
                if let Some(id) = data_root_id {
                    state.roots.remove(id);
                }
                if !state.entries.is_empty() || !state.roots.is_empty() {
                    let body = serde_json::to_vec(&state).context("serialize analytics outbox")?;
                    return write_private_file_durably(path, &body);
                }
            }
        }
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
                .root_mut(&self.data_root_id)
                .record_ordinary_drop(AnalyticsDeliveryFailureClass::LocalIo);
            loaded.state.enforce_bounds()?;
            self.persist(&loaded.state)?;
            bail!("analytics payload exceeds the outbox body bound");
        }
        loaded.state.entries.push(OutboxEntry {
            schema_version: OUTBOX_SCHEMA_VERSION,
            entry_id: uuid::Uuid::new_v4().to_string(),
            data_root_id: self.data_root_id.clone(),
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
                entry.data_root_id == self.data_root_id
                    && entry.endpoint_fingerprint == fingerprint
                    && entry.next_attempt_at_epoch_seconds <= now_epoch_seconds
            })
            .take(OUTBOX_MAX_FLUSH_PER_CALL)
            .map(|entry| SnapshotEntry {
                entry_id: entry.entry_id.clone(),
                data_root_id: entry.data_root_id.clone(),
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
        if snapshot.data_root_id != self.data_root_id {
            return Ok(false);
        }
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
            if snapshot.data_root_id != self.data_root_id {
                bail!("analytics snapshot belongs to another data root");
            }
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
                    let root = loaded.state.root_mut(&self.data_root_id);
                    if accepted.kind == OutboxEntryKind::Ordinary && root.has_counters() {
                        root.observation_due = true;
                    }
                }
                DeliveryDisposition::Retry { class, retry_after } => {
                    if snapshot.kind == OutboxEntryKind::Ordinary {
                        let root = loaded.state.root_mut(&self.data_root_id);
                        root.retry_attempts = root.retry_attempts.saturating_add(1);
                        root.record_failure(class);
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
                        loaded
                            .state
                            .root_mut(&self.data_root_id)
                            .record_ordinary_drop(class);
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
        let health_pending = loaded.state.entries.iter().any(|entry| {
            entry.data_root_id == self.data_root_id
                && entry.kind == OutboxEntryKind::DeliveryObservation
        });
        let root = loaded.state.root(&self.data_root_id);
        let has_counters = root.has_counters();
        if root.observation_due && !has_counters {
            loaded.state.root_mut(&self.data_root_id).observation_due = false;
            loaded.dirty = true;
        }
        if loaded.dirty {
            self.persist(&loaded.state)?;
        }
        if !root.observation_due || health_pending || !has_counters {
            return Ok(None);
        }
        let oldest_age_seconds = loaded
            .state
            .entries
            .iter()
            .filter(|entry| {
                entry.data_root_id == self.data_root_id && entry.kind == OutboxEntryKind::Ordinary
            })
            .map(|entry| {
                now_epoch_seconds
                    .saturating_sub(entry.queued_at_epoch_seconds)
                    .max(0) as u64
            })
            .max()
            .unwrap_or(0);
        Ok(Some(OutboxObservation {
            data_root_id: self.data_root_id.clone(),
            event: AnalyticsDeliveryObservationV1::new(
                loaded
                    .state
                    .entries
                    .iter()
                    .filter(|entry| {
                        entry.data_root_id == self.data_root_id
                            && entry.kind == OutboxEntryKind::Ordinary
                    })
                    .count() as u64,
                root.retry_attempts,
                root.dropped,
                Duration::from_secs(oldest_age_seconds),
                root.last_failure_class
                    .unwrap_or(AnalyticsDeliveryFailureClass::None),
            ),
            retry_attempts: root.retry_attempts,
            dropped: root.dropped,
            failure_sequence: root.failure_sequence,
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
        if observation.data_root_id != self.data_root_id {
            bail!("analytics observation belongs to another data root");
        }
        let payload = validate_payload(body)?;
        if body.len() > OUTBOX_MAX_BODY_BYTES {
            bail!("analytics delivery observation exceeds the outbox body bound");
        }
        let _lock = OutboxLock::acquire(&self.state_lock_path())?;
        let mut loaded = self.load_normalized(now_epoch_seconds)?;
        let root = loaded.state.root(&self.data_root_id);
        if !root.observation_due
            || loaded.state.entries.iter().any(|entry| {
                entry.data_root_id == self.data_root_id
                    && entry.kind == OutboxEntryKind::DeliveryObservation
            })
        {
            return Ok(());
        }
        if root.retry_attempts < observation.retry_attempts || root.dropped < observation.dropped {
            bail!("analytics delivery observation is stale");
        }
        loaded.state.entries.push(OutboxEntry {
            schema_version: OUTBOX_SCHEMA_VERSION,
            entry_id: uuid::Uuid::new_v4().to_string(),
            data_root_id: self.data_root_id.clone(),
            endpoint_fingerprint: endpoint_fingerprint(endpoint),
            queued_at_epoch_seconds: now_epoch_seconds,
            attempts: 0,
            next_attempt_at_epoch_seconds: 0,
            kind: OutboxEntryKind::DeliveryObservation,
            payload,
        });
        let root = loaded.state.root_mut(&self.data_root_id);
        root.retry_attempts = root
            .retry_attempts
            .saturating_sub(observation.retry_attempts);
        root.dropped = root.dropped.saturating_sub(observation.dropped);
        if root.failure_sequence == observation.failure_sequence {
            root.last_failure_class = None;
            root.observation_due = false;
        } else {
            root.observation_due = true;
        }
        loaded.state.enforce_bounds()?;
        if let Some(root) = loaded.state.roots.get_mut(&self.data_root_id) {
            if root.failure_sequence != observation.failure_sequence {
                root.observation_due = true;
            }
        }
        self.persist(&loaded.state)
    }

    fn state_lock_path(&self) -> PathBuf {
        self.path.with_extension("lock")
    }

    fn load_normalized(&self, now_epoch_seconds: i64) -> Result<LoadedState> {
        let (mut state, mut dirty, missing) = match read_state(&self.path)? {
            StoredOutbox::Missing => (OutboxState::empty(), false, true),
            StoredOutbox::Corrupt => (OutboxState::recovered(&self.data_root_id), true, false),
            StoredOutbox::State(state, migrated) => (state, migrated, false),
        };
        if validate_state(&state).is_err() {
            state = OutboxState::recovered(&self.data_root_id);
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
    if state.roots.len() > OUTBOX_MAX_ENTRIES {
        bail!("analytics outbox exceeds its root metadata bound");
    }
    for id in state.roots.keys() {
        validate_root_id(id)?;
    }
    let mut health_roots = HashSet::new();
    for entry in &state.entries {
        validate_root_id(&entry.data_root_id)?;
        if entry.kind == OutboxEntryKind::DeliveryObservation
            && !health_roots.insert(&entry.data_root_id)
        {
            bail!("analytics outbox contains multiple delivery observations for a root");
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
        // Older shared entries and counters have no consent owner. Do not
        // infer ownership from their payloads or replay them under this root.
        Some(1 | 2) => Ok(StoredOutbox::State(OutboxState::empty(), true)),
        _ => Ok(StoredOutbox::Corrupt),
    }
}

fn validate_root_id(id: &str) -> Result<()> {
    let parsed = uuid::Uuid::parse_str(id).context("parse analytics root identity")?;
    if parsed.is_nil() || parsed.to_string() != id {
        bail!("analytics root identity is invalid");
    }
    Ok(())
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
