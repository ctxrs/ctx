use std::{
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

use anyhow::{anyhow, bail, Context, Result};
use ctx_daemon_service::{
    daemon_semantic_intensity_lease_request, SemanticIntensityLeaseOperation,
};
use serde_json::Value;
use uuid::Uuid;

const LEASE_TTL: Duration = Duration::from_secs(30);
const LEASE_REQUEST_TIMEOUT: Duration = Duration::from_millis(500);
const LEASE_RESPONSE_MAX_BYTES: u64 = 16 * 1024;

trait SemanticIntensityLeaseClient: Send + Sync {
    fn request(
        &self,
        data_root: &Path,
        operation: SemanticIntensityLeaseOperation,
        request_id: &str,
        ttl: Option<Duration>,
        timeout: Duration,
        max_response_bytes: u64,
    ) -> Result<Option<Value>>;
}

struct DaemonSemanticIntensityLeaseClient;

impl SemanticIntensityLeaseClient for DaemonSemanticIntensityLeaseClient {
    fn request(
        &self,
        data_root: &Path,
        operation: SemanticIntensityLeaseOperation,
        request_id: &str,
        ttl: Option<Duration>,
        timeout: Duration,
        max_response_bytes: u64,
    ) -> Result<Option<Value>> {
        daemon_semantic_intensity_lease_request(
            data_root,
            operation,
            request_id,
            ttl,
            timeout,
            max_response_bytes,
        )
    }
}

/// A temporary daemon-owned request for full semantic document-indexing intensity.
///
/// The lease changes no persistent configuration. Call [`Self::renew`] before
/// each wait poll so a stalled or terminated client naturally loses authority.
/// Dropping the guard makes one best-effort release request; the daemon TTL is
/// the final cleanup boundary if that request cannot be delivered.
pub struct SemanticIndexingIntensityLease {
    data_root: PathBuf,
    request_id: String,
    client: Arc<dyn SemanticIntensityLeaseClient>,
    active: bool,
}

impl SemanticIndexingIntensityLease {
    pub fn acquire_full(data_root: &Path) -> Result<Self> {
        Self::acquire_full_with_client(data_root, Arc::new(DaemonSemanticIntensityLeaseClient))
    }

    fn acquire_full_with_client(
        data_root: &Path,
        client: Arc<dyn SemanticIntensityLeaseClient>,
    ) -> Result<Self> {
        let lease = Self {
            data_root: data_root.to_path_buf(),
            request_id: Uuid::now_v7().to_string(),
            client,
            active: true,
        };
        lease
            .request(SemanticIntensityLeaseOperation::Acquire, Some(LEASE_TTL))
            .context("acquire temporary full semantic indexing intensity")?;
        Ok(lease)
    }

    pub fn renew(&mut self) -> Result<()> {
        self.request(SemanticIntensityLeaseOperation::Renew, Some(LEASE_TTL))
            .context("renew temporary full semantic indexing intensity")
    }

    fn request(
        &self,
        operation: SemanticIntensityLeaseOperation,
        ttl: Option<Duration>,
    ) -> Result<()> {
        let response = self.client.request(
            &self.data_root,
            operation,
            &self.request_id,
            ttl,
            LEASE_REQUEST_TIMEOUT,
            LEASE_RESPONSE_MAX_BYTES,
        )?;
        validate_lease_response(response.as_ref(), operation, &self.request_id, ttl)
    }
}

impl Drop for SemanticIndexingIntensityLease {
    fn drop(&mut self) {
        if self.active {
            let _ = self.request(SemanticIntensityLeaseOperation::Release, None);
            self.active = false;
        }
    }
}

fn validate_lease_response(
    response: Option<&Value>,
    operation: SemanticIntensityLeaseOperation,
    request_id: &str,
    ttl: Option<Duration>,
) -> Result<()> {
    let response =
        response.ok_or_else(|| anyhow!("daemon returned no semantic intensity lease response"))?;
    if response.get("ok").and_then(Value::as_bool) != Some(true) {
        let error = response
            .get("error")
            .and_then(Value::as_str)
            .unwrap_or("daemon rejected semantic intensity lease request");
        bail!("{error}");
    }
    if response.get("schema_version").and_then(Value::as_u64) != Some(1) {
        bail!("daemon semantic intensity lease response schema mismatch");
    }
    if response.get("owner").and_then(Value::as_str) != Some("daemon")
        || response.get("service").and_then(Value::as_str) != Some("semantic_intensity")
    {
        bail!("daemon semantic intensity lease response authority mismatch");
    }
    if response.get("op").and_then(Value::as_str) != Some(operation.as_str()) {
        bail!("daemon semantic intensity lease response operation mismatch");
    }
    if response.get("request_id").and_then(Value::as_str) != Some(request_id) {
        bail!("daemon semantic intensity lease response request ID mismatch");
    }

    let expected_status = if operation == SemanticIntensityLeaseOperation::Release {
        "released"
    } else {
        "active"
    };
    if response.get("lease_status").and_then(Value::as_str) != Some(expected_status) {
        bail!("daemon semantic intensity lease response status mismatch");
    }
    for field in [
        "configured_indexing_intensity",
        "effective_indexing_intensity",
    ] {
        if !matches!(
            response.get(field).and_then(Value::as_str),
            Some("quiet" | "full")
        ) {
            bail!("daemon semantic intensity lease response has invalid {field}");
        }
    }
    if operation != SemanticIntensityLeaseOperation::Release
        && response
            .get("effective_indexing_intensity")
            .and_then(Value::as_str)
            != Some("full")
    {
        bail!("daemon did not activate full semantic indexing intensity");
    }

    let active_leases = response
        .get("active_full_intensity_leases")
        .and_then(Value::as_u64)
        .ok_or_else(|| {
            anyhow!("daemon semantic intensity lease response is missing its active lease count")
        })?;
    if operation != SemanticIntensityLeaseOperation::Release && active_leases == 0 {
        bail!("daemon semantic intensity lease response has no active full lease");
    }

    if let Some(ttl) = ttl {
        let ttl_ms = u64::try_from(ttl.as_millis())?;
        if response.get("ttl_ms").and_then(Value::as_u64) != Some(ttl_ms)
            || response
                .get("expires_at_ms")
                .and_then(Value::as_u64)
                .is_none()
        {
            bail!("daemon semantic intensity lease response TTL mismatch");
        }
    } else if response.get("ttl_ms").is_some() || response.get("expires_at_ms").is_some() {
        bail!("daemon semantic intensity release response retained lease timing");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use serde_json::json;

    use super::*;

    #[derive(Debug, Clone)]
    struct RecordedRequest {
        operation: SemanticIntensityLeaseOperation,
        request_id: String,
        ttl: Option<Duration>,
        timeout: Duration,
        max_response_bytes: u64,
    }

    #[derive(Default)]
    struct RecordingClient {
        requests: Mutex<Vec<RecordedRequest>>,
        response_override: Mutex<Option<Option<Value>>>,
    }

    impl RecordingClient {
        fn with_response(response: Option<Value>) -> Self {
            Self {
                response_override: Mutex::new(Some(response)),
                ..Self::default()
            }
        }

        fn requests(&self) -> Vec<RecordedRequest> {
            self.requests
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .clone()
        }
    }

    impl SemanticIntensityLeaseClient for RecordingClient {
        fn request(
            &self,
            _data_root: &Path,
            operation: SemanticIntensityLeaseOperation,
            request_id: &str,
            ttl: Option<Duration>,
            timeout: Duration,
            max_response_bytes: u64,
        ) -> Result<Option<Value>> {
            self.requests
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .push(RecordedRequest {
                    operation,
                    request_id: request_id.to_owned(),
                    ttl,
                    timeout,
                    max_response_bytes,
                });
            if let Some(mut response) = self
                .response_override
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .take()
            {
                if let Some(response) = response.as_mut() {
                    if response.get("request_id").and_then(Value::as_str)
                        == Some("replaced-by-test")
                    {
                        response["request_id"] = json!(request_id);
                    }
                }
                return Ok(response);
            }
            let release = operation == SemanticIntensityLeaseOperation::Release;
            Ok(Some(crate::compact_json(json!({
                "schema_version": 1,
                "ok": true,
                "owner": "daemon",
                "service": "semantic_intensity",
                "op": operation.as_str(),
                "request_id": request_id,
                "lease_status": if release { "released" } else { "active" },
                "ttl_ms": ttl.map(|ttl| ttl.as_millis() as u64),
                "expires_at_ms": ttl.map(|_| 123_456_u64),
                "active_full_intensity_leases": if release { 0 } else { 1 },
                "configured_indexing_intensity": "quiet",
                "effective_indexing_intensity": if release { "quiet" } else { "full" },
            }))))
        }
    }

    #[test]
    fn guard_acquires_renews_and_releases_one_uuid_keyed_lease_without_config_writes() -> Result<()>
    {
        let temp = tempfile::tempdir()?;
        let client = Arc::new(RecordingClient::default());
        {
            let mut lease = SemanticIndexingIntensityLease::acquire_full_with_client(
                temp.path(),
                client.clone(),
            )?;
            lease.renew()?;
            assert!(!temp.path().join(crate::config::CONFIG_FILE).exists());
        }

        let requests = client.requests();
        assert_eq!(requests.len(), 3);
        assert_eq!(
            requests[0].operation,
            SemanticIntensityLeaseOperation::Acquire
        );
        assert_eq!(
            requests[1].operation,
            SemanticIntensityLeaseOperation::Renew
        );
        assert_eq!(
            requests[2].operation,
            SemanticIntensityLeaseOperation::Release
        );
        let request_id = Uuid::parse_str(&requests[0].request_id)?;
        assert!(!request_id.is_nil());
        assert!(requests
            .iter()
            .all(|request| request.request_id == requests[0].request_id));
        assert_eq!(requests[0].ttl, Some(LEASE_TTL));
        assert_eq!(requests[1].ttl, Some(LEASE_TTL));
        assert_eq!(requests[2].ttl, None);
        assert!(requests
            .iter()
            .all(|request| request.timeout == LEASE_REQUEST_TIMEOUT));
        assert!(requests
            .iter()
            .all(|request| request.max_response_bytes == LEASE_RESPONSE_MAX_BYTES));
        Ok(())
    }

    #[test]
    fn acquire_rejects_missing_negative_or_unauthorized_daemon_responses() {
        let valid = json!({
            "schema_version": 1,
            "ok": true,
            "owner": "daemon",
            "service": "semantic_intensity",
            "op": "semantic_intensity_acquire",
            "request_id": "replaced-by-test",
            "lease_status": "active",
            "ttl_ms": LEASE_TTL.as_millis() as u64,
            "expires_at_ms": 123_456,
            "active_full_intensity_leases": 1,
            "configured_indexing_intensity": "quiet",
            "effective_indexing_intensity": "full",
        });
        let invalid = [
            None,
            Some(json!({"schema_version": 1, "ok": false, "error": "rejected"})),
            Some({
                let mut value = valid.clone();
                value["owner"] = json!("cli");
                value
            }),
            Some({
                let mut value = valid.clone();
                value["effective_indexing_intensity"] = json!("quiet");
                value
            }),
            Some({
                let mut value = valid.clone();
                value["ttl_ms"] = json!(1);
                value
            }),
        ];

        for response in invalid {
            let client = Arc::new(RecordingClient::with_response(response));
            assert!(SemanticIndexingIntensityLease::acquire_full_with_client(
                Path::new("/unused-test-root"),
                client.clone(),
            )
            .is_err());
            assert_eq!(
                client.requests().last().map(|request| request.operation),
                Some(SemanticIntensityLeaseOperation::Release),
                "a potentially acquired lease must be released after response validation fails",
            );
        }
    }

    #[test]
    fn renew_and_release_validation_require_their_exact_operation_shapes() {
        let request_id = "019fcaaa-0000-7000-8000-000000000501";
        let renew = json!({
            "schema_version": 1,
            "ok": true,
            "owner": "daemon",
            "service": "semantic_intensity",
            "op": "semantic_intensity_renew",
            "request_id": request_id,
            "lease_status": "active",
            "ttl_ms": LEASE_TTL.as_millis() as u64,
            "expires_at_ms": 123_456,
            "active_full_intensity_leases": 1,
            "configured_indexing_intensity": "quiet",
            "effective_indexing_intensity": "full",
        });
        validate_lease_response(
            Some(&renew),
            SemanticIntensityLeaseOperation::Renew,
            request_id,
            Some(LEASE_TTL),
        )
        .unwrap();
        let mut invalid_renew = renew;
        invalid_renew["lease_status"] = json!("released");
        assert!(validate_lease_response(
            Some(&invalid_renew),
            SemanticIntensityLeaseOperation::Renew,
            request_id,
            Some(LEASE_TTL),
        )
        .is_err());

        let release = json!({
            "schema_version": 1,
            "ok": true,
            "owner": "daemon",
            "service": "semantic_intensity",
            "op": "semantic_intensity_release",
            "request_id": request_id,
            "lease_status": "released",
            "active_full_intensity_leases": 0,
            "configured_indexing_intensity": "quiet",
            "effective_indexing_intensity": "quiet",
        });
        validate_lease_response(
            Some(&release),
            SemanticIntensityLeaseOperation::Release,
            request_id,
            None,
        )
        .unwrap();
        let mut invalid_release = release;
        invalid_release["ttl_ms"] = json!(LEASE_TTL.as_millis() as u64);
        assert!(validate_lease_response(
            Some(&invalid_release),
            SemanticIntensityLeaseOperation::Release,
            request_id,
            None,
        )
        .is_err());
    }
}
