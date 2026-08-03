use super::*;
use ctx_history_index::MAX_PUBLICATION_METADATA_BYTES;

const SOURCE_REFRESH_PUBLICATION_METADATA_VERSION: u64 = 1;

/// Refresh-owned authority carried by Core's opaque CommitPayload metadata.
/// Core deliberately knows nothing about this encoding.
#[derive(Debug, Clone)]
pub(super) struct SourceBackedPublicationMetadata {
    pub(super) request_id: String,
    pub(super) operation: SourceBackedRefreshOperation,
    pub(super) refresh_scope: SourceBackedRefreshScope,
    pub(super) receipt: Value,
    pub(super) route_observations: BTreeMap<SourceRouteIdentity, String>,
}

impl SourceBackedPublicationMetadata {
    pub(super) fn encode(&self) -> ctx_history_index::Result<Vec<u8>> {
        let route_ids = receipt_route_ids(&self.receipt)
            .map_err(|error| IndexError::PublicationMetadata(error.to_string()))?;
        if self
            .route_observations
            .keys()
            .any(|route| !route_ids.contains(route))
        {
            return Err(IndexError::PublicationMetadata(
                "route observation names a route outside the exact receipt".to_owned(),
            ));
        }
        let route_observations = route_ids
            .iter()
            .map(|route| {
                self.route_observations
                    .get(route)
                    .map_or(Value::Null, |observation| json!(observation))
            })
            .collect::<Vec<_>>();
        let value = compact_json(json!({
            "version": SOURCE_REFRESH_PUBLICATION_METADATA_VERSION,
            "request_id": self.request_id,
            "operation": self.operation.as_str(),
            "refresh_scope": coordinator_state::refresh_scope_json(&self.refresh_scope),
            "receipt": self.receipt,
            "route_observations": route_observations,
        }));
        let encoded = serde_json::to_vec(&value)
            .map_err(|error| IndexError::PublicationMetadata(error.to_string()))?;
        if encoded.len() > MAX_PUBLICATION_METADATA_BYTES {
            return Err(IndexError::PublicationMetadataTooLarge {
                actual: encoded.len(),
                maximum: MAX_PUBLICATION_METADATA_BYTES,
            });
        }
        Ok(encoded)
    }

    pub(super) fn decode(index: &VerifiedIndex) -> Result<Self> {
        let bytes = index
            .publication_metadata()
            .ok_or_else(|| anyhow!("active Core publication has no source-refresh metadata"))?;
        if bytes.len() > MAX_PUBLICATION_METADATA_BYTES {
            bail!("active Core source-refresh metadata exceeds its bounded contract");
        }
        let value: Value = serde_json::from_slice(bytes)
            .context("decode active Core source-refresh publication metadata")?;
        let fields = value
            .as_object()
            .ok_or_else(|| anyhow!("Core source-refresh publication metadata must be an object"))?;
        let expected = BTreeSet::from([
            "operation",
            "receipt",
            "refresh_scope",
            "request_id",
            "route_observations",
            "version",
        ]);
        if fields.keys().map(String::as_str).collect::<BTreeSet<_>>() != expected {
            bail!("Core source-refresh publication metadata has unknown or missing fields");
        }
        if fields.get("version").and_then(Value::as_u64)
            != Some(SOURCE_REFRESH_PUBLICATION_METADATA_VERSION)
        {
            bail!("unsupported Core source-refresh publication metadata version");
        }
        let request_id = fields
            .get("request_id")
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
            .map(str::to_owned)
            .ok_or_else(|| anyhow!("Core source-refresh publication metadata has no request ID"))?;
        let operation = SourceBackedRefreshOperation::from_request_json(&json!({
            "operation": fields.get("operation").cloned().unwrap_or(Value::Null),
        }))?;
        let refresh_scope =
            coordinator_state::refresh_scope_from_json(fields.get("refresh_scope"))?;
        let receipt = fields
            .get("receipt")
            .filter(|receipt| receipt.is_object())
            .cloned()
            .ok_or_else(|| anyhow!("Core source-refresh publication metadata has no receipt"))?;
        let observations = fields
            .get("route_observations")
            .and_then(Value::as_array)
            .ok_or_else(|| anyhow!("Core source-refresh route observations must be an array"))?;
        let route_ids = receipt_route_ids(&receipt)?;
        if observations.len() != route_ids.len() {
            bail!("Core source-refresh route observations do not align with its exact receipt");
        }
        let route_observations = route_ids
            .into_iter()
            .zip(observations)
            .filter_map(|(route, observation)| {
                if observation.is_null() {
                    return None;
                }
                Some(
                    observation
                        .as_str()
                        .filter(|value| is_sha256_identity(value))
                        .map(|observation| (route, observation.to_owned()))
                        .ok_or_else(|| anyhow!("Core source-refresh route observation is invalid")),
                )
            })
            .collect::<Result<BTreeMap<_, _>>>()?;
        Ok(Self {
            request_id,
            operation,
            refresh_scope,
            receipt,
            route_observations,
        })
    }

    pub(super) fn response_value(&self) -> Value {
        json!({
            "previous_generation": self.receipt.get("previous_generation"),
            "published_generation": self.receipt.get("published_generation"),
            "generation_changed": self.receipt.get("generation_changed"),
            "certified_source_count": self.receipt
                .get("current")
                .and_then(|current| current.get("current_source_count")),
            "certified_source_bytes": self.receipt
                .get("current")
                .and_then(|current| current.get("current_certified_source_bytes")),
            "receipt": self.receipt,
        })
    }
}

fn receipt_route_ids(receipt: &Value) -> Result<Vec<SourceRouteIdentity>> {
    let routes = receipt
        .get("route_results")
        .and_then(Value::as_object)
        .ok_or_else(|| anyhow!("Core source-refresh metadata receipt has no route results"))?;
    if routes.len() > SOURCE_REFRESH_TERMINAL_ROUTE_LIMIT {
        bail!("Core source-refresh metadata receipt has too many routes");
    }
    routes
        .keys()
        .cloned()
        .map(SourceRouteIdentity::from_sha256)
        .collect::<ctx_history_index::Result<Vec<_>>>()
        .map_err(Into::into)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn metadata(receipt: Value) -> SourceBackedPublicationMetadata {
        SourceBackedPublicationMetadata {
            request_id: "publication-metadata-test".to_owned(),
            operation: SourceBackedRefreshOperation::Refresh,
            refresh_scope: SourceBackedRefreshScope::All,
            receipt,
            route_observations: BTreeMap::new(),
        }
    }

    #[test]
    fn metadata_rejects_an_observation_outside_the_exact_receipt() {
        let receipt_route = SourceRouteIdentity::from_sha256("11".repeat(32)).unwrap();
        let outside_route = SourceRouteIdentity::from_sha256("22".repeat(32)).unwrap();
        let routes = BTreeMap::from([(
            receipt_route.as_str().to_owned(),
            json!(["s", true, [], 0, 0, []]),
        )]);
        let mut value = metadata(json!({
            "route_results": routes,
        }));
        value
            .route_observations
            .insert(outside_route, "33".repeat(32));
        assert!(matches!(
            value.encode(),
            Err(IndexError::PublicationMetadata(message))
                if message.contains("outside the exact receipt")
        ));
    }

    #[test]
    fn metadata_fails_closed_before_core_on_oversized_receipts() {
        let value = metadata(json!({
            "route_results": {},
            "diagnostic_padding": "x".repeat(MAX_PUBLICATION_METADATA_BYTES),
        }));
        assert!(matches!(
            value.encode(),
            Err(IndexError::PublicationMetadataTooLarge { maximum, .. })
                if maximum == MAX_PUBLICATION_METADATA_BYTES
        ));
    }

    #[test]
    fn invalid_route_identity_is_rejected_before_metadata_publication() {
        let value = metadata(json!({
            "route_results": {"not-a-route": ["s", true, [], 0, 0, []]},
        }));
        assert!(matches!(
            value.encode(),
            Err(IndexError::PublicationMetadata(_))
        ));
    }
}
