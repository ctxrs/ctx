use super::*;
use ctx_history_index::MAX_PUBLICATION_METADATA_BYTES;

pub(crate) const SOURCE_REFRESH_PUBLICATION_METADATA_VERSION: u64 = 2;
const LEGACY_SOURCE_REFRESH_PUBLICATION_METADATA_VERSION: u64 = 1;

/// Refresh-owned authority carried by Core's opaque CommitPayload metadata.
/// Core deliberately knows nothing about this encoding.
#[derive(Debug, Clone)]
pub struct SourceBackedPublicationMetadata {
    pub(crate) version: u64,
    pub(crate) request_id: String,
    pub(crate) operation: SourceBackedRefreshOperation,
    pub(crate) refresh_scope: SourceBackedRefreshScope,
    pub(crate) receipt: Value,
    pub(crate) route_observations: BTreeMap<SourceRouteIdentity, String>,
}

impl SourceBackedPublicationMetadata {
    pub(crate) fn encode(&self) -> ctx_history_index::Result<Vec<u8>> {
        if self.version != SOURCE_REFRESH_PUBLICATION_METADATA_VERSION {
            return Err(IndexError::PublicationMetadata(
                "new Core source-refresh publications must use metadata v2".to_owned(),
            ));
        }
        validate_v2_receipt(&self.receipt, None)
            .map_err(|error| IndexError::PublicationMetadata(error.to_string()))?;
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
        let encoded = self.encode_with_observations(route_observations)?;
        if encoded.len() <= MAX_PUBLICATION_METADATA_BYTES {
            return Ok(encoded);
        }
        // Observations are a performance certificate, never publication
        // authority. Drop them as one deterministic unit before rejecting a
        // legitimate exact receipt; startup then performs the normal
        // fail-closed refresh for every route.
        let encoded = self.encode_with_observations(vec![Value::Null; route_ids.len()])?;
        if encoded.len() > MAX_PUBLICATION_METADATA_BYTES {
            return Err(IndexError::PublicationMetadataTooLarge {
                actual: encoded.len(),
                maximum: MAX_PUBLICATION_METADATA_BYTES,
            });
        }
        Ok(encoded)
    }

    fn encode_with_observations(
        &self,
        route_observations: Vec<Value>,
    ) -> ctx_history_index::Result<Vec<u8>> {
        let value = compact_json(json!({
            "version": self.version,
            "request_id": self.request_id,
            "operation": self.operation.as_str(),
            "refresh_scope": engine::refresh_scope_json(&self.refresh_scope),
            "receipt": self.receipt,
            "route_observations": route_observations,
        }));
        serde_json::to_vec(&value)
            .map_err(|error| IndexError::PublicationMetadata(error.to_string()))
    }

    pub fn decode(index: &VerifiedIndex) -> Result<Self> {
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
        let version = fields
            .get("version")
            .and_then(Value::as_u64)
            .ok_or_else(|| anyhow!("Core source-refresh publication metadata has no version"))?;
        if !matches!(
            version,
            LEGACY_SOURCE_REFRESH_PUBLICATION_METADATA_VERSION
                | SOURCE_REFRESH_PUBLICATION_METADATA_VERSION
        ) {
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
        let refresh_scope = engine::refresh_scope_from_json(fields.get("refresh_scope"))?;
        let receipt = fields
            .get("receipt")
            .filter(|receipt| receipt.is_object())
            .cloned()
            .ok_or_else(|| anyhow!("Core source-refresh publication metadata has no receipt"))?;
        match version {
            LEGACY_SOURCE_REFRESH_PUBLICATION_METADATA_VERSION => {
                if receipt.get("zero_source_authority").is_some() {
                    bail!("Core source-refresh metadata v1 carries v2-only authority");
                }
                validate_receipt_generation(&receipt, index)?;
            }
            SOURCE_REFRESH_PUBLICATION_METADATA_VERSION => {
                validate_v2_receipt(&receipt, Some(index))?;
            }
            _ => unreachable!("metadata version checked above"),
        }
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
            version,
            request_id,
            operation,
            refresh_scope,
            receipt,
            route_observations,
        })
    }

    pub fn response_value(&self) -> Value {
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

    /// Whether this metadata proves that its exact verified generation is
    /// query-ready. Legacy nonempty generations remain valid, while legacy
    /// zero-source generations require a successful v2 recertification.
    pub fn certifies_generation(&self, index: &VerifiedIndex) -> bool {
        let source_count = self
            .receipt
            .get("current")
            .and_then(|current| current.get("current_source_count"))
            .and_then(Value::as_u64)
            .and_then(|count| usize::try_from(count).ok());
        match source_count {
            Some(1..) => true,
            Some(0) => {
                self.version == SOURCE_REFRESH_PUBLICATION_METADATA_VERSION
                    && required_route_results(self.receipt.get("route_results"))
                        .and_then(|route_results| {
                            parse_zero_source_authority(
                                self.receipt.get("zero_source_authority"),
                                &route_results,
                            )
                        })
                        .is_ok_and(|authority| {
                            !authority.is_empty()
                                && authority
                                    .iter()
                                    .all(|entry| entry.generation_id == index.generation_id())
                        })
            }
            None => false,
        }
    }
}

fn validate_receipt_generation(receipt: &Value, index: &VerifiedIndex) -> Result<()> {
    let generation_id = receipt
        .get("published_generation")
        .and_then(Value::as_str)
        .filter(|generation| !generation.is_empty())
        .ok_or_else(|| anyhow!("Core source-refresh receipt has no published generation"))?;
    let source_count = receipt
        .get("current")
        .and_then(|current| current.get("current_source_count"))
        .and_then(Value::as_u64)
        .and_then(|count| usize::try_from(count).ok())
        .ok_or_else(|| anyhow!("Core source-refresh receipt has no current source count"))?;
    if generation_id != index.generation_id() || source_count != index.manifest().sources.len() {
        bail!("Core source-refresh metadata does not match its exact generation");
    }
    Ok(())
}

fn validate_v2_receipt(receipt: &Value, index: Option<&VerifiedIndex>) -> Result<()> {
    let generation_id = receipt
        .get("published_generation")
        .and_then(Value::as_str)
        .filter(|generation| !generation.is_empty())
        .ok_or_else(|| anyhow!("Core source-refresh receipt has no published generation"))?;
    let source_count = receipt
        .get("current")
        .and_then(|current| current.get("current_source_count"))
        .and_then(Value::as_u64)
        .and_then(|count| usize::try_from(count).ok())
        .ok_or_else(|| anyhow!("Core source-refresh receipt has no current source count"))?;
    if let Some(index) = index {
        validate_receipt_generation(receipt, index)?;
    }
    let route_results = required_route_results(receipt.get("route_results"))?;
    let authority =
        parse_zero_source_authority(receipt.get("zero_source_authority"), &route_results)?;
    validate_zero_source_authority(
        generation_id,
        source_count,
        &route_results,
        &authority,
        true,
    )
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

    fn metadata(mut receipt: Value) -> SourceBackedPublicationMetadata {
        let receipt = receipt.as_object_mut().expect("test receipt object");
        receipt.insert("published_generation".to_owned(), json!("44".repeat(32)));
        receipt.insert("current".to_owned(), json!({"current_source_count": 1}));
        SourceBackedPublicationMetadata {
            version: SOURCE_REFRESH_PUBLICATION_METADATA_VERSION,
            request_id: "publication-metadata-test".to_owned(),
            operation: SourceBackedRefreshOperation::Refresh,
            refresh_scope: SourceBackedRefreshScope::All,
            receipt: Value::Object(receipt.clone()),
            route_observations: BTreeMap::new(),
        }
    }

    #[test]
    fn metadata_rejects_an_observation_outside_the_exact_receipt() {
        let receipt_route = SourceRouteIdentity::from_sha256("11".repeat(32)).unwrap();
        let outside_route = SourceRouteIdentity::from_sha256("22".repeat(32)).unwrap();
        let routes = BTreeMap::from([(receipt_route.as_str().to_owned(), json!(["s", true]))]);
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
    fn exact_scope_reserves_envelope_capacity_by_dropping_optional_observations() {
        let route_ids = (0_u16..=255)
            .map(|index| {
                SourceRouteIdentity::from_sha256(format!("{index:064x}"))
                    .expect("bounded route identity")
            })
            .collect::<Vec<_>>();
        let routes = route_ids
            .iter()
            .map(|route| (route.as_str().to_owned(), json!(["s", false])))
            .collect::<serde_json::Map<_, _>>();
        let mut value = metadata(json!({
            "route_results": routes,
        }));
        value.refresh_scope = SourceBackedRefreshScope::exact(route_ids.clone());
        value.route_observations = route_ids
            .into_iter()
            .map(|route| (route, "55".repeat(32)))
            .collect();

        let encoded = value.encode().expect("required metadata envelope fits");
        assert!(encoded.len() <= MAX_PUBLICATION_METADATA_BYTES);
        let decoded: Value = serde_json::from_slice(&encoded).unwrap();
        let observations = decoded["route_observations"].as_array().unwrap();
        assert_eq!(observations.len(), SOURCE_REFRESH_TERMINAL_ROUTE_LIMIT);
        assert!(
            observations.iter().all(Value::is_null),
            "optional observation certificates must yield to durable authority"
        );
    }

    #[test]
    fn invalid_route_identity_is_rejected_before_metadata_publication() {
        let value = metadata(json!({
            "route_results": {"not-a-route": ["s", true]},
        }));
        assert!(matches!(
            value.encode(),
            Err(IndexError::PublicationMetadata(_))
        ));
    }
}
