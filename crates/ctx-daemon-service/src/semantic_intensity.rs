use std::{
    collections::BTreeMap,
    path::Path,
    sync::{
        atomic::{AtomicBool, Ordering},
        Mutex,
    },
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use anyhow::{anyhow, bail, Result};
use serde_json::{json, Map, Value};
use uuid::Uuid;

use crate::{compact_json, daemon_source_refresh_request, SemanticIndexingIntensity};

pub const SEMANTIC_INTENSITY_LEASE_MIN_TTL_MS: u64 = 1_000;
pub const SEMANTIC_INTENSITY_LEASE_MAX_TTL_MS: u64 = 3_600_000;

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum SemanticIntensityLeaseOperation {
    Acquire,
    Renew,
    Release,
}

impl SemanticIntensityLeaseOperation {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Acquire => "semantic_intensity_acquire",
            Self::Renew => "semantic_intensity_renew",
            Self::Release => "semantic_intensity_release",
        }
    }

    fn parse(value: &str) -> Option<Self> {
        match value {
            "semantic_intensity_acquire" => Some(Self::Acquire),
            "semantic_intensity_renew" => Some(Self::Renew),
            "semantic_intensity_release" => Some(Self::Release),
            _ => None,
        }
    }

    fn requires_ttl(self) -> bool {
        matches!(self, Self::Acquire | Self::Renew)
    }
}

/// Sends a temporary semantic-indexing intensity lease request over the
/// authenticated daemon source-refresh service.
///
/// Acquire and renew require a bounded `ttl`; release requires `None`.
pub fn daemon_semantic_intensity_lease_request(
    data_root: &Path,
    operation: SemanticIntensityLeaseOperation,
    request_id: &str,
    ttl: Option<Duration>,
    timeout: Duration,
    max_response_bytes: u64,
) -> Result<Option<Value>> {
    let request_id = parse_request_id(request_id)?;
    let ttl_ms = match (operation.requires_ttl(), ttl) {
        (true, Some(ttl)) => Some(duration_to_ttl_ms(ttl)?),
        (true, None) => bail!("semantic intensity lease TTL is required"),
        (false, Some(_)) => bail!("semantic intensity release must not carry a TTL"),
        (false, None) => None,
    };
    daemon_source_refresh_request(
        data_root,
        compact_json(json!({
            "schema_version": 1,
            "op": operation.as_str(),
            "request_id": request_id.to_string(),
            "ttl_ms": ttl_ms,
        })),
        timeout,
        max_response_bytes,
    )
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub(crate) struct SemanticIntensitySnapshot {
    pub(crate) configured: SemanticIndexingIntensity,
    pub(crate) effective: SemanticIndexingIntensity,
    pub(crate) active_full_leases: usize,
}

#[derive(Debug, Clone, Copy)]
struct FullIntensityLease {
    deadline: Instant,
    expires_at_ms: u64,
}

#[derive(Debug, Clone, Copy)]
struct LeaseMutation {
    request_id: Uuid,
    ttl_ms: Option<u64>,
    expires_at_ms: Option<u64>,
}

#[derive(Debug, Default)]
pub(crate) struct SemanticIntensityLeaseRegistry {
    leases: Mutex<BTreeMap<Uuid, FullIntensityLease>>,
    configured_full: AtomicBool,
}

impl SemanticIntensityLeaseRegistry {
    pub(crate) fn snapshot(
        &self,
        configured: SemanticIndexingIntensity,
    ) -> SemanticIntensitySnapshot {
        self.set_configured(configured);
        self.current_snapshot()
    }

    pub(crate) fn set_configured(&self, configured: SemanticIndexingIntensity) {
        self.configured_full.store(
            configured == SemanticIndexingIntensity::Full,
            Ordering::Release,
        );
    }

    pub(crate) fn next_expiry_in(&self, now: Instant) -> Option<Duration> {
        let mut leases = self.lock();
        purge_expired(&mut leases, now);
        leases
            .values()
            .map(|lease| lease.deadline.saturating_duration_since(now))
            .min()
    }

    fn current_snapshot(&self) -> SemanticIntensitySnapshot {
        let configured = if self.configured_full.load(Ordering::Acquire) {
            SemanticIndexingIntensity::Full
        } else {
            SemanticIndexingIntensity::Quiet
        };
        self.snapshot_at(configured, Instant::now())
    }

    fn snapshot_at(
        &self,
        configured: SemanticIndexingIntensity,
        now: Instant,
    ) -> SemanticIntensitySnapshot {
        let mut leases = self.lock();
        purge_expired(&mut leases, now);
        let active_full_leases = leases.len();
        let effective = if configured == SemanticIndexingIntensity::Full || active_full_leases > 0 {
            SemanticIndexingIntensity::Full
        } else {
            SemanticIndexingIntensity::Quiet
        };
        SemanticIntensitySnapshot {
            configured,
            effective,
            active_full_leases,
        }
    }

    fn apply(&self, request: ValidatedLeaseRequest) -> Result<LeaseMutation> {
        self.apply_at(request, Instant::now(), unix_now_ms())
    }

    fn apply_at(
        &self,
        request: ValidatedLeaseRequest,
        now: Instant,
        now_ms: u64,
    ) -> Result<LeaseMutation> {
        let mut leases = self.lock();
        purge_expired(&mut leases, now);
        match request.operation {
            SemanticIntensityLeaseOperation::Acquire => {
                if leases.contains_key(&request.request_id) {
                    bail!(
                        "semantic intensity lease request {} is already active",
                        request.request_id
                    );
                }
                let ttl_ms = request
                    .ttl_ms
                    .expect("validated acquire request carries TTL");
                let lease = lease_from_ttl(now, now_ms, ttl_ms);
                leases.insert(request.request_id, lease);
                Ok(LeaseMutation {
                    request_id: request.request_id,
                    ttl_ms: Some(ttl_ms),
                    expires_at_ms: Some(lease.expires_at_ms),
                })
            }
            SemanticIntensityLeaseOperation::Renew => {
                let ttl_ms = request.ttl_ms.expect("validated renew request carries TTL");
                let Some(lease) = leases.get_mut(&request.request_id) else {
                    bail!(
                        "semantic intensity lease request {} is not active",
                        request.request_id
                    );
                };
                *lease = lease_from_ttl(now, now_ms, ttl_ms);
                Ok(LeaseMutation {
                    request_id: request.request_id,
                    ttl_ms: Some(ttl_ms),
                    expires_at_ms: Some(lease.expires_at_ms),
                })
            }
            SemanticIntensityLeaseOperation::Release => {
                if leases.remove(&request.request_id).is_none() {
                    bail!(
                        "semantic intensity lease request {} is not active",
                        request.request_id
                    );
                }
                Ok(LeaseMutation {
                    request_id: request.request_id,
                    ttl_ms: None,
                    expires_at_ms: None,
                })
            }
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, BTreeMap<Uuid, FullIntensityLease>> {
        self.leases
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

pub(crate) struct SemanticIntensityLeaseResponse {
    pub(crate) value: Value,
    pub(crate) wake_daemon: bool,
}

pub(crate) fn handle_semantic_intensity_lease_request(
    registry: &SemanticIntensityLeaseRegistry,
    request: &Value,
) -> Result<Option<SemanticIntensityLeaseResponse>> {
    let Some(operation) = request
        .get("op")
        .and_then(Value::as_str)
        .and_then(SemanticIntensityLeaseOperation::parse)
    else {
        return Ok(None);
    };
    let request = ValidatedLeaseRequest::from_json(operation, request)?;
    let mutation = registry.apply(request)?;
    let snapshot = registry.current_snapshot();
    let lease_status = if operation == SemanticIntensityLeaseOperation::Release {
        "released"
    } else {
        "active"
    };
    Ok(Some(SemanticIntensityLeaseResponse {
        value: compact_json(json!({
            "schema_version": 1,
            "ok": true,
            "owner": "daemon",
            "service": "semantic_intensity",
            "op": operation.as_str(),
            "request_id": mutation.request_id.to_string(),
            "lease_status": lease_status,
            "ttl_ms": mutation.ttl_ms,
            "expires_at_ms": mutation.expires_at_ms,
            "active_full_intensity_leases": snapshot.active_full_leases,
            "configured_indexing_intensity": snapshot.configured.as_str(),
            "effective_indexing_intensity": snapshot.effective.as_str(),
        })),
        wake_daemon: true,
    }))
}

#[derive(Debug, Clone, Copy)]
struct ValidatedLeaseRequest {
    operation: SemanticIntensityLeaseOperation,
    request_id: Uuid,
    ttl_ms: Option<u64>,
}

impl ValidatedLeaseRequest {
    fn from_json(operation: SemanticIntensityLeaseOperation, request: &Value) -> Result<Self> {
        let object = request
            .as_object()
            .ok_or_else(|| anyhow!("semantic intensity lease request must be an object"))?;
        validate_request_fields(operation, object)?;
        if object.get("schema_version").and_then(Value::as_u64) != Some(1) {
            bail!("semantic intensity lease request schema_version must be 1");
        }
        let request_id = object
            .get("request_id")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow!("semantic intensity lease request ID is missing"))?;
        let request_id = parse_request_id(request_id)?;
        let ttl_ms = if operation.requires_ttl() {
            let ttl_ms = object
                .get("ttl_ms")
                .and_then(Value::as_u64)
                .ok_or_else(|| anyhow!("semantic intensity lease TTL is missing or invalid"))?;
            validate_ttl_ms(ttl_ms)?;
            Some(ttl_ms)
        } else {
            None
        };
        Ok(Self {
            operation,
            request_id,
            ttl_ms,
        })
    }
}

fn validate_request_fields(
    operation: SemanticIntensityLeaseOperation,
    object: &Map<String, Value>,
) -> Result<()> {
    for field in object.keys() {
        if !matches!(
            field.as_str(),
            "schema_version" | "op" | "request_id" | "ttl_ms" | "token"
        ) {
            bail!("unknown semantic intensity lease request field `{field}`");
        }
    }
    if object
        .get("token")
        .is_some_and(|token| token.as_str().is_none_or(str::is_empty))
    {
        bail!("semantic intensity lease authentication token is invalid");
    }
    let expected_fields = if object.contains_key("token") { 1 } else { 0 };
    if operation.requires_ttl() {
        if object.len() != 4 + expected_fields || !object.contains_key("ttl_ms") {
            bail!("semantic intensity acquire or renew request must carry exactly one TTL");
        }
    } else if object.len() != 3 + expected_fields || object.contains_key("ttl_ms") {
        bail!("semantic intensity release request must not carry a TTL");
    }
    Ok(())
}

fn parse_request_id(value: &str) -> Result<Uuid> {
    let request_id = Uuid::parse_str(value)
        .map_err(|_| anyhow!("semantic intensity lease request ID must be a UUID"))?;
    if request_id.is_nil() {
        bail!("semantic intensity lease request ID must not be nil");
    }
    Ok(request_id)
}

fn duration_to_ttl_ms(ttl: Duration) -> Result<u64> {
    let ttl_ms = u64::try_from(ttl.as_millis())
        .map_err(|_| anyhow!("semantic intensity lease TTL is too large"))?;
    validate_ttl_ms(ttl_ms)?;
    Ok(ttl_ms)
}

fn validate_ttl_ms(ttl_ms: u64) -> Result<()> {
    if !(SEMANTIC_INTENSITY_LEASE_MIN_TTL_MS..=SEMANTIC_INTENSITY_LEASE_MAX_TTL_MS)
        .contains(&ttl_ms)
    {
        bail!(
            "semantic intensity lease TTL must be between {} and {} milliseconds",
            SEMANTIC_INTENSITY_LEASE_MIN_TTL_MS,
            SEMANTIC_INTENSITY_LEASE_MAX_TTL_MS
        );
    }
    Ok(())
}

fn lease_from_ttl(now: Instant, now_ms: u64, ttl_ms: u64) -> FullIntensityLease {
    FullIntensityLease {
        deadline: now + Duration::from_millis(ttl_ms),
        expires_at_ms: now_ms.saturating_add(ttl_ms),
    }
}

fn purge_expired(leases: &mut BTreeMap<Uuid, FullIntensityLease>, now: Instant) {
    leases.retain(|_, lease| lease.deadline > now);
}

fn unix_now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX))
        .unwrap_or_default()
}

#[cfg(test)]
pub(crate) fn semantic_indexing_intensity_from_json(
    value: Option<&Value>,
) -> SemanticIndexingIntensity {
    match value.and_then(Value::as_str) {
        Some("full") => SemanticIndexingIntensity::Full,
        _ => SemanticIndexingIntensity::Quiet,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIRST_ID: &str = "019fcaaa-0000-7000-8000-000000000401";
    const SECOND_ID: &str = "019fcaaa-0000-7000-8000-000000000402";

    fn request(
        operation: SemanticIntensityLeaseOperation,
        request_id: &str,
        ttl_ms: Option<u64>,
    ) -> ValidatedLeaseRequest {
        ValidatedLeaseRequest {
            operation,
            request_id: Uuid::parse_str(request_id).unwrap(),
            ttl_ms,
        }
    }

    #[test]
    fn configured_full_and_active_lease_both_take_precedence_over_quiet() {
        let registry = SemanticIntensityLeaseRegistry::default();
        let now = Instant::now();
        assert_eq!(
            registry.snapshot_at(SemanticIndexingIntensity::Quiet, now),
            SemanticIntensitySnapshot {
                configured: SemanticIndexingIntensity::Quiet,
                effective: SemanticIndexingIntensity::Quiet,
                active_full_leases: 0,
            }
        );
        registry
            .apply_at(
                request(
                    SemanticIntensityLeaseOperation::Acquire,
                    FIRST_ID,
                    Some(5_000),
                ),
                now,
                10_000,
            )
            .unwrap();
        assert_eq!(
            registry
                .snapshot_at(SemanticIndexingIntensity::Quiet, now)
                .active_full_leases,
            1
        );
        assert_eq!(
            registry
                .snapshot_at(SemanticIndexingIntensity::Quiet, now)
                .effective,
            SemanticIndexingIntensity::Full
        );
        assert_eq!(
            SemanticIntensityLeaseRegistry::default()
                .snapshot_at(SemanticIndexingIntensity::Full, now)
                .effective,
            SemanticIndexingIntensity::Full
        );
    }

    #[test]
    fn overlapping_leases_renew_release_and_expiry_are_independent() {
        let registry = SemanticIntensityLeaseRegistry::default();
        let now = Instant::now();
        registry
            .apply_at(
                request(
                    SemanticIntensityLeaseOperation::Acquire,
                    FIRST_ID,
                    Some(2_000),
                ),
                now,
                20_000,
            )
            .unwrap();
        assert_eq!(
            registry.next_expiry_in(now + Duration::from_millis(1_000)),
            Some(Duration::from_millis(1_000))
        );
        registry
            .apply_at(
                request(
                    SemanticIntensityLeaseOperation::Acquire,
                    SECOND_ID,
                    Some(5_000),
                ),
                now,
                20_000,
            )
            .unwrap();
        registry
            .apply_at(
                request(
                    SemanticIntensityLeaseOperation::Renew,
                    FIRST_ID,
                    Some(5_000),
                ),
                now + Duration::from_millis(1_000),
                21_000,
            )
            .unwrap();
        registry
            .apply_at(
                request(SemanticIntensityLeaseOperation::Release, SECOND_ID, None),
                now + Duration::from_millis(1_500),
                21_500,
            )
            .unwrap();
        assert_eq!(
            registry
                .snapshot_at(
                    SemanticIndexingIntensity::Quiet,
                    now + Duration::from_millis(5_500),
                )
                .active_full_leases,
            1
        );
        assert_eq!(
            registry
                .snapshot_at(
                    SemanticIndexingIntensity::Quiet,
                    now + Duration::from_millis(6_001),
                )
                .effective,
            SemanticIndexingIntensity::Quiet
        );
    }

    #[test]
    fn a_new_registry_models_daemon_restart_without_recovering_leases() {
        let first = SemanticIntensityLeaseRegistry::default();
        first
            .apply(request(
                SemanticIntensityLeaseOperation::Acquire,
                FIRST_ID,
                Some(5_000),
            ))
            .unwrap();
        assert_eq!(
            first.snapshot(SemanticIndexingIntensity::Quiet).effective,
            SemanticIndexingIntensity::Full
        );
        assert_eq!(
            SemanticIntensityLeaseRegistry::default()
                .snapshot(SemanticIndexingIntensity::Quiet)
                .effective,
            SemanticIndexingIntensity::Quiet
        );
    }

    #[test]
    fn wire_requests_reject_unknown_fields_invalid_ttls_and_missing_leases() {
        let registry = SemanticIntensityLeaseRegistry::default();
        for invalid in [
            json!({
                "schema_version": 1,
                "op": "semantic_intensity_acquire",
                "request_id": FIRST_ID,
                "ttl_ms": 999,
            }),
            json!({
                "schema_version": 1,
                "op": "semantic_intensity_acquire",
                "request_id": FIRST_ID,
                "ttl_ms": 5_000,
                "unexpected": true,
            }),
            json!({
                "schema_version": 1,
                "op": "semantic_intensity_release",
                "request_id": FIRST_ID,
                "ttl_ms": 5_000,
            }),
        ] {
            assert!(handle_semantic_intensity_lease_request(&registry, &invalid,).is_err());
        }
        assert!(handle_semantic_intensity_lease_request(
            &registry,
            &json!({
                "schema_version": 1,
                "op": "semantic_intensity_renew",
                "request_id": FIRST_ID,
                "ttl_ms": 5_000,
            }),
        )
        .is_err());
    }

    #[test]
    fn missing_persisted_intensity_decodes_as_quiet() {
        assert_eq!(
            semantic_indexing_intensity_from_json(None),
            SemanticIndexingIntensity::Quiet
        );
        assert_eq!(
            semantic_indexing_intensity_from_json(Some(&Value::String("full".to_owned()))),
            SemanticIndexingIntensity::Full
        );
    }
}
