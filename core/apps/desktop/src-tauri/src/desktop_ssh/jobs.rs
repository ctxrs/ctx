use super::*;

const CONNECT_JOB_TTL_MS: u64 = 15 * 60 * 1000;

#[derive(Debug, Clone)]
struct ConnectJobRecord {
    snapshot: DesktopSshConnectJobStatus,
    terminal: bool,
}

fn connect_jobs() -> &'static std::sync::Mutex<HashMap<String, ConnectJobRecord>> {
    static SSH_CONNECT_JOBS: std::sync::OnceLock<
        std::sync::Mutex<HashMap<String, ConnectJobRecord>>,
    > = std::sync::OnceLock::new();
    SSH_CONNECT_JOBS.get_or_init(|| std::sync::Mutex::new(HashMap::new()))
}

fn next_connect_job_id() -> String {
    static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
    let seq = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    format!("ssh-connect-{seq}")
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

fn prune_stale_jobs(map: &mut HashMap<String, ConnectJobRecord>, now: u64) {
    map.retain(|_, record| {
        if !record.terminal {
            return true;
        }
        let updated = record.snapshot.updated_at_ms.unwrap_or(now);
        now.saturating_sub(updated) <= CONNECT_JOB_TTL_MS
    });
}

fn snapshot_for_phase(
    phase: ConnectJobPhase,
    info: Option<DesktopConnectionInfo>,
    error: Option<String>,
    created_at_ms: u64,
    updated_at_ms: u64,
) -> DesktopSshConnectJobStatus {
    DesktopSshConnectJobStatus {
        status: phase.status().to_string(),
        phase: Some(phase.as_str().to_string()),
        info,
        error,
        created_at_ms: Some(created_at_ms),
        updated_at_ms: Some(updated_at_ms),
    }
}

pub(super) fn begin_connect_job() -> Result<String, String> {
    let now = now_ms();
    let job_id = next_connect_job_id();
    let mut jobs = connect_jobs()
        .lock()
        .map_err(|err| format!("ssh connect jobs lock poisoned: {err}"))?;
    prune_stale_jobs(&mut jobs, now);
    jobs.insert(
        job_id.clone(),
        ConnectJobRecord {
            snapshot: snapshot_for_phase(ConnectJobPhase::Queued, None, None, now, now),
            terminal: false,
        },
    );
    Ok(job_id)
}

pub(super) fn record_connect_job_phase(job_id: &str, phase: ConnectJobPhase) {
    let now = now_ms();
    let Ok(mut jobs) = connect_jobs().lock() else {
        return;
    };
    prune_stale_jobs(&mut jobs, now);
    if let Some(record) = jobs.get_mut(job_id) {
        let created = record.snapshot.created_at_ms.unwrap_or(now);
        record.snapshot = snapshot_for_phase(
            phase,
            record.snapshot.info.clone(),
            record.snapshot.error.clone(),
            created,
            now,
        );
        record.terminal = matches!(phase, ConnectJobPhase::Succeeded | ConnectJobPhase::Failed);
    }
}

pub(super) fn complete_connect_job_success(job_id: &str, info: DesktopConnectionInfo) {
    let now = now_ms();
    let Ok(mut jobs) = connect_jobs().lock() else {
        return;
    };
    prune_stale_jobs(&mut jobs, now);
    if let Some(record) = jobs.get_mut(job_id) {
        let created = record.snapshot.created_at_ms.unwrap_or(now);
        record.snapshot =
            snapshot_for_phase(ConnectJobPhase::Succeeded, Some(info), None, created, now);
        record.terminal = true;
    }
}

pub(super) fn complete_connect_job_failure(job_id: &str, error: String) {
    let now = now_ms();
    let Ok(mut jobs) = connect_jobs().lock() else {
        return;
    };
    prune_stale_jobs(&mut jobs, now);
    if let Some(record) = jobs.get_mut(job_id) {
        let created = record.snapshot.created_at_ms.unwrap_or(now);
        record.snapshot =
            snapshot_for_phase(ConnectJobPhase::Failed, None, Some(error), created, now);
        record.terminal = true;
    }
}

#[tauri::command]
pub(crate) fn desktop_connect_ssh_poll(
    req: DesktopSshConnectPollReq,
) -> Result<DesktopSshConnectJobStatus, String> {
    let id = req.job_id.trim();
    if id.is_empty() {
        return Err("job_id is required".to_string());
    }
    let now = now_ms();
    let mut jobs = connect_jobs()
        .lock()
        .map_err(|err| format!("ssh connect jobs lock poisoned: {err}"))?;
    prune_stale_jobs(&mut jobs, now);
    let snapshot = jobs
        .get(id)
        .map(|record| record.snapshot.clone())
        .ok_or_else(|| format!("unknown ssh connect job id: {id}"))?;
    if req.consume && snapshot.status != "pending" {
        jobs.remove(id);
    }
    Ok(snapshot)
}
