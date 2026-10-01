//! Best-effort admission for foreground engines and aggregate snapshots.
//!
//! Immutable queued bytes keep their existing retry/identity semantics. A
//! summary is lower priority than an ordinary operation, never an eviction
//! reason, and is not a second accounting queue.
use super::*;

const MAX_SUMMARY_BODY: usize = 128 * 1024;
const MAX_QUEUED_SUMMARIES: usize = 4;

pub(super) fn is_summary(entry: &OutboxEntry) -> bool {
    if entry.kind != OutboxEntryKind::Ordinary {
        return false;
    }
    let Ok(value) = serde_json::from_str::<Value>(&entry.payload) else {
        return false;
    };
    let Some(events) = value.get("events").and_then(Value::as_array) else {
        return false;
    };
    !events.is_empty()
        && events.iter().all(|event| {
            event.get("event_name").and_then(Value::as_str) == Some("runtime_observation")
                && matches!(
                    event.get("operation").and_then(Value::as_str),
                    Some("sift_summary" | "server_summary" | "sharing_summary" | "graph_summary")
                )
        })
}

impl AnalyticsOutbox {
    /// Returns immediately on an occupied state lock. No orphan scan is needed
    /// for an optional operation; the ordinary owner still performs cleanup.
    pub(crate) fn try_open(path: PathBuf, data_root_id: &str) -> Result<Option<Self>> {
        validate_root_id(data_root_id)?;
        let parent = path
            .parent()
            .context("analytics outbox path has no parent")?;
        fs::create_dir_all(parent).context("create analytics outbox directory")?;
        let outbox = Self {
            path,
            data_root_id: data_root_id.to_owned(),
            nonblocking: true,
        };
        let Some(_lock) = OutboxLock::try_acquire(&outbox.state_lock_path())? else {
            return Ok(None);
        };
        Ok(Some(outbox))
    }

    pub(super) fn lock_state(&self) -> Result<OutboxLock> {
        if self.nonblocking {
            OutboxLock::try_acquire(&self.state_lock_path())?
                .context("optional analytics state is busy")
        } else {
            OutboxLock::acquire(&self.state_lock_path())
        }
    }

    pub(crate) fn append_summary(&self, endpoint: &str, body: &[u8]) -> Result<bool> {
        self.append_summary_at(endpoint, body, utc_now().timestamp())
    }

    fn append_summary_at(&self, endpoint: &str, body: &[u8], now: i64) -> Result<bool> {
        if body.len() > MAX_SUMMARY_BODY {
            return Ok(false);
        }
        let payload = validate_payload(body)?;
        let candidate = OutboxEntry {
            schema_version: OUTBOX_SCHEMA_VERSION,
            entry_id: uuid::Uuid::new_v4().to_string(),
            data_root_id: self.data_root_id.clone(),
            endpoint_fingerprint: endpoint_fingerprint(endpoint),
            queued_at_epoch_seconds: now,
            attempts: 0,
            next_attempt_at_epoch_seconds: 0,
            kind: OutboxEntryKind::Ordinary,
            payload,
        };
        if !is_summary(&candidate) {
            bail!("optional summary requires a closed summary-only batch");
        }
        let _lock = self.lock_state()?;
        let mut loaded = self.load_normalized(now)?;
        let summaries: Vec<_> = loaded
            .state
            .entries
            .iter()
            .filter(|entry| is_summary(entry))
            .collect();
        if loaded.state.entries.len() >= OUTBOX_MAX_ENTRIES
            || summaries.len() >= MAX_QUEUED_SUMMARIES
            || summaries.iter().any(|entry| {
                entry.data_root_id == self.data_root_id
                    && entry.endpoint_fingerprint == candidate.endpoint_fingerprint
            })
        {
            return Ok(false);
        }
        loaded.state.entries.push(candidate);
        loaded.state.trim_root_metadata();
        if serde_json::to_vec(&loaded.state)?.len() as u64 > OUTBOX_MAX_BYTES {
            return Ok(false);
        }
        self.persist(&loaded.state)?;
        Ok(true)
    }
}

#[cfg(test)]
mod tests;
