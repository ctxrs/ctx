use super::*;
use ctx_history_index::MAX_PUBLICATION_METADATA_BYTES;

pub const SOURCE_REFRESH_PUBLICATION_METADATA_VERSION: u64 = 4;
const LEGACY_SOURCE_REFRESH_PUBLICATION_METADATA_VERSION: u64 = 1;
const PREVIOUS_SOURCE_REFRESH_PUBLICATION_METADATA_VERSION: u64 = 2;
const ROUTE_CONTROL_SOURCE_REFRESH_PUBLICATION_METADATA_VERSION: u64 = 3;
pub(crate) const COMMITTED_REJECTION_DIAGNOSTICS_FIELD: &str = "committed_rejection_diagnostics";

/// Provider-neutral query-readiness verdict for one physically verified Core
/// generation.
///
/// Public API boundaries can map an uncertified generation or a metadata
/// decoding failure into their own typed errors without reimplementing the
/// publication predicate.
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum GenerationQueryReadiness {
    Ready,
    Uncertified,
}

impl GenerationQueryReadiness {
    pub const fn is_ready(self) -> bool {
        matches!(self, Self::Ready)
    }
}

/// Refresh-owned authority carried by Core's opaque CommitPayload metadata.
/// Core deliberately knows nothing about this encoding.
#[derive(Debug, Clone)]
pub struct SourceBackedPublicationMetadata {
    #[doc(hidden)]
    pub version: u64,
    #[doc(hidden)]
    pub request_id: String,
    #[doc(hidden)]
    pub operation: SourceBackedRefreshOperation,
    #[doc(hidden)]
    pub refresh_scope: SourceBackedRefreshScope,
    #[doc(hidden)]
    pub receipt: Value,
    #[doc(hidden)]
    pub route_observations: BTreeMap<SourceRouteIdentity, String>,
    #[doc(hidden)]
    pub route_controls: BTreeMap<SourceRouteIdentity, Vec<u8>>,
}

impl SourceBackedPublicationMetadata {
    #[doc(hidden)]
    pub fn encode(&self) -> ctx_history_index::Result<Vec<u8>> {
        self.encode_with_committed_rejection_diagnostics(&json!({}))
    }

    pub(crate) fn encode_with_committed_rejection_diagnostics(
        &self,
        committed_rejection_diagnostics: &Value,
    ) -> ctx_history_index::Result<Vec<u8>> {
        if self.version != SOURCE_REFRESH_PUBLICATION_METADATA_VERSION {
            return Err(IndexError::PublicationMetadata(
                "new Core source-refresh publications must use metadata v4".to_owned(),
            ));
        }
        parse_committed_rejection_diagnostics(Some(committed_rejection_diagnostics))
            .map_err(|error| IndexError::PublicationMetadata(error.to_string()))?;
        validate_v2_receipt(&self.receipt, None, &self.refresh_scope)
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
        if self
            .route_controls
            .iter()
            .any(|(_, control)| control.len() > MAX_SOURCE_BACKED_ROUTE_CONTROL_BYTES)
        {
            return Err(IndexError::PublicationMetadata(
                "route control exceeds its bounded contract".to_owned(),
            ));
        }
        let encoded =
            self.encode_with_observations(route_observations, committed_rejection_diagnostics)?;
        if encoded.len() <= MAX_PUBLICATION_METADATA_BYTES {
            return Ok(encoded);
        }
        // Observations are a performance certificate, never publication
        // authority. Drop them as one deterministic unit before rejecting a
        // legitimate exact receipt; startup then performs the normal
        // fail-closed refresh for every route.
        let encoded = self.encode_with_observations(
            vec![Value::Null; route_ids.len()],
            committed_rejection_diagnostics,
        )?;
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
        committed_rejection_diagnostics: &Value,
    ) -> ctx_history_index::Result<Vec<u8>> {
        let route_controls = self
            .route_controls
            .iter()
            .map(|(route, control)| {
                (
                    route.as_str().to_owned(),
                    json!(encode_route_control(control)),
                )
            })
            .collect::<serde_json::Map<_, _>>();
        let value = compact_json(json!({
            "version": self.version,
            "request_id": self.request_id,
            "operation": self.operation.as_str(),
            "refresh_scope": refresh_scope_json(&self.refresh_scope),
            "receipt": self.receipt,
            "route_observations": route_observations,
            "route_controls": route_controls,
            (COMMITTED_REJECTION_DIAGNOSTICS_FIELD): committed_rejection_diagnostics,
        }));
        serde_json::to_vec(&value)
            .map_err(|error| IndexError::PublicationMetadata(error.to_string()))
    }

    pub fn decode(index: &VerifiedIndex) -> Result<Self> {
        Self::decode_with_committed_rejection_diagnostics(index).map(|(metadata, _)| metadata)
    }

    pub(crate) fn decode_with_committed_rejection_diagnostics(
        index: &VerifiedIndex,
    ) -> Result<(Self, Option<Vec<SourceBackedRefreshRecordRejection>>)> {
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
        let version = fields
            .get("version")
            .and_then(Value::as_u64)
            .ok_or_else(|| anyhow!("Core source-refresh publication metadata has no version"))?;
        if !matches!(
            version,
            LEGACY_SOURCE_REFRESH_PUBLICATION_METADATA_VERSION
                | PREVIOUS_SOURCE_REFRESH_PUBLICATION_METADATA_VERSION
                | ROUTE_CONTROL_SOURCE_REFRESH_PUBLICATION_METADATA_VERSION
                | SOURCE_REFRESH_PUBLICATION_METADATA_VERSION
        ) {
            bail!("unsupported Core source-refresh publication metadata version");
        }
        let mut expected = BTreeSet::from([
            "operation",
            "receipt",
            "refresh_scope",
            "request_id",
            "route_observations",
            "version",
        ]);
        if matches!(
            version,
            ROUTE_CONTROL_SOURCE_REFRESH_PUBLICATION_METADATA_VERSION
                | SOURCE_REFRESH_PUBLICATION_METADATA_VERSION
        ) {
            expected.insert("route_controls");
        }
        if version == SOURCE_REFRESH_PUBLICATION_METADATA_VERSION {
            expected.insert(COMMITTED_REJECTION_DIAGNOSTICS_FIELD);
        }
        if fields.keys().map(String::as_str).collect::<BTreeSet<_>>() != expected {
            bail!("Core source-refresh publication metadata has unknown or missing fields");
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
        let refresh_scope = refresh_scope_from_json(fields.get("refresh_scope"))?;
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
            PREVIOUS_SOURCE_REFRESH_PUBLICATION_METADATA_VERSION
            | ROUTE_CONTROL_SOURCE_REFRESH_PUBLICATION_METADATA_VERSION
            | SOURCE_REFRESH_PUBLICATION_METADATA_VERSION => {
                validate_v2_receipt(&receipt, Some(index), &refresh_scope)?;
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
        let live_routes = index
            .manifest()
            .source_routes()
            .iter()
            .map(|route| route.route_identity().clone())
            .collect::<BTreeSet<_>>();
        let route_controls = if matches!(
            version,
            ROUTE_CONTROL_SOURCE_REFRESH_PUBLICATION_METADATA_VERSION
                | SOURCE_REFRESH_PUBLICATION_METADATA_VERSION
        ) {
            fields
                .get("route_controls")
                .and_then(Value::as_object)
                .into_iter()
                .flatten()
                .filter_map(|(route, encoded)| {
                    let route = SourceRouteIdentity::from_sha256(route.clone()).ok()?;
                    if !live_routes.contains(&route) {
                        return None;
                    }
                    let control = encoded.as_str().and_then(decode_route_control)?;
                    (control.len() <= MAX_SOURCE_BACKED_ROUTE_CONTROL_BYTES)
                        .then_some((route, control))
                })
                .collect()
        } else {
            BTreeMap::new()
        };
        let committed_rejection_diagnostics = (version
            == SOURCE_REFRESH_PUBLICATION_METADATA_VERSION)
            .then(|| {
                parse_committed_rejection_diagnostics(
                    fields.get(COMMITTED_REJECTION_DIAGNOSTICS_FIELD),
                )
            })
            .transpose()?;
        Ok((
            Self {
                version,
                request_id,
                operation,
                refresh_scope,
                receipt,
                route_observations,
                route_controls,
            },
            committed_rejection_diagnostics,
        ))
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
                matches!(
                    self.version,
                    ROUTE_CONTROL_SOURCE_REFRESH_PUBLICATION_METADATA_VERSION
                        | SOURCE_REFRESH_PUBLICATION_METADATA_VERSION
                ) && required_route_results(self.receipt.get("route_results"))
                    .and_then(|route_results| {
                        crate::receipt_parse::parse_zero_source_authority(
                            self.receipt.get("zero_source_authority"),
                            &route_results,
                        )
                        .map(|authority| (route_results, authority))
                    })
                    .is_ok_and(|(route_results, authority)| {
                        if authority.is_empty() {
                            route_results.is_empty()
                                && self.refresh_scope == SourceBackedRefreshScope::All
                                && index.manifest().source_routes().is_empty()
                        } else {
                            authority
                                .iter()
                                .all(|entry| entry.generation_id == index.generation_id())
                        }
                    })
            }
            None => false,
        }
    }
}

fn parse_committed_rejection_diagnostics(
    value: Option<&Value>,
) -> Result<Vec<SourceBackedRefreshRecordRejection>> {
    let route_results = required_route_results(value)?;
    let diagnostic_total = route_results.iter().try_fold(0_usize, |total, result| {
        if result.outcome.changed() != Some(false)
            || result.source_failure_total != 0
            || !result.source_failures.is_empty()
            || result.rejected_record_total != result.rejection_diagnostics.len() as u64
        {
            bail!("committed rejection-diagnostic ledger is inconsistent");
        }
        total
            .checked_add(result.rejection_diagnostics.len())
            .ok_or_else(|| anyhow!("committed rejection-diagnostic ledger total overflow"))
    })?;
    if diagnostic_total > ctx_history_capture_runtime::MAX_RECORDED_SOURCE_BACKED_RECORD_REJECTIONS
    {
        bail!("committed rejection-diagnostic ledger exceeds its bounded contract");
    }
    let diagnostics = route_results
        .into_iter()
        .flat_map(|result| result.rejection_diagnostics)
        .collect::<Vec<_>>();
    let unique = diagnostics
        .iter()
        .map(|rejection| {
            (
                rejection.source_identity.as_str(),
                rejection.provider.as_str(),
                rejection.source_selector.as_str(),
                rejection.line,
                rejection.payload_type.as_str(),
                rejection.class.as_str(),
                rejection.detail.as_str(),
            )
        })
        .collect::<BTreeSet<_>>();
    if unique.len() != diagnostics.len() {
        bail!("committed rejection-diagnostic ledger contains a duplicate diagnostic");
    }
    Ok(diagnostics)
}

fn encode_route_control(control: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut encoded = String::with_capacity(control.len().saturating_mul(2));
    for byte in control {
        encoded.push(HEX[usize::from(byte >> 4)] as char);
        encoded.push(HEX[usize::from(byte & 0x0f)] as char);
    }
    encoded
}

fn decode_route_control(encoded: &str) -> Option<Vec<u8>> {
    if encoded.len() > MAX_SOURCE_BACKED_ROUTE_CONTROL_BYTES.saturating_mul(2)
        || !encoded.len().is_multiple_of(2)
    {
        return None;
    }
    fn nibble(value: u8) -> Option<u8> {
        match value {
            b'0'..=b'9' => Some(value - b'0'),
            b'a'..=b'f' => Some(value - b'a' + 10),
            b'A'..=b'F' => Some(value - b'A' + 10),
            _ => None,
        }
    }
    encoded
        .as_bytes()
        .chunks_exact(2)
        .map(|pair| Some((nibble(pair[0])? << 4) | nibble(pair[1])?))
        .collect()
}

/// Applies the single publication-authority predicate to a verified Core
/// generation. A legacy nonempty generation is query-ready without metadata;
/// metadata-bearing generations must decode and certify their exact pin.
pub fn verify_generation_query_readiness(
    index: &VerifiedIndex,
) -> Result<GenerationQueryReadiness> {
    let Some(_) = index.publication_metadata() else {
        return Ok(if index.manifest().sources.is_empty() {
            GenerationQueryReadiness::Uncertified
        } else {
            GenerationQueryReadiness::Ready
        });
    };
    let metadata = SourceBackedPublicationMetadata::decode(index)?;
    Ok(if metadata.certifies_generation(index) {
        GenerationQueryReadiness::Ready
    } else {
        GenerationQueryReadiness::Uncertified
    })
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

fn validate_v2_receipt(
    receipt: &Value,
    index: Option<&VerifiedIndex>,
    refresh_scope: &SourceBackedRefreshScope,
) -> Result<()> {
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
    let authority = crate::receipt_parse::parse_zero_source_authority(
        receipt.get("zero_source_authority"),
        &route_results,
    )?;
    let authoritative_empty_catalog = source_count == 0
        && route_results.is_empty()
        && authority.is_empty()
        && refresh_scope == &SourceBackedRefreshScope::All;
    if authoritative_empty_catalog
        && index.is_some_and(|index| !index.manifest().source_routes().is_empty())
    {
        bail!("empty-catalog Core source-refresh metadata retained source routes");
    }
    validate_zero_source_authority(
        generation_id,
        source_count,
        &route_results,
        &authority,
        !authoritative_empty_catalog,
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
        .map(|route| {
            SourceRouteIdentity::from_sha256(route).map_err(ctx_history_index::IndexError::from)
        })
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
            route_controls: BTreeMap::new(),
        }
    }

    fn rejection(
        route_identity: &str,
        source_identity: &str,
        line: u64,
    ) -> SourceBackedRefreshRecordRejection {
        SourceBackedRefreshRecordRejection {
            route_identity: route_identity.to_owned(),
            source_identity: source_identity.to_owned(),
            provider: "qwen_code".to_owned(),
            source_selector: "/tmp/qwen.jsonl".to_owned(),
            line,
            payload_type: "message".to_owned(),
            class: "malformed_record".to_owned(),
            detail: "invalid shape".to_owned(),
        }
    }

    fn ledger_json(
        entries: impl IntoIterator<Item = (String, Vec<SourceBackedRefreshRecordRejection>)>,
    ) -> Value {
        Value::Object(
            entries
                .into_iter()
                .map(|(route_identity, diagnostics)| {
                    let mut result =
                        SourceBackedRefreshRouteResult::succeeded(route_identity.clone(), false);
                    result.rejected_record_total = diagnostics.len() as u64;
                    result.rejection_diagnostics = diagnostics;
                    (route_identity, result.compact_json())
                })
                .collect(),
        )
    }

    #[test]
    fn metadata_v4_keeps_the_committed_ledger_outside_the_typed_receipt() {
        let value = metadata(json!({"route_results": {}}));
        let receipt = value.receipt.clone();

        let encoded = value.encode().unwrap();
        let encoded: Value = serde_json::from_slice(&encoded).unwrap();

        assert_eq!(
            encoded["version"],
            SOURCE_REFRESH_PUBLICATION_METADATA_VERSION
        );
        assert_eq!(encoded[COMMITTED_REJECTION_DIAGNOSTICS_FIELD], json!({}));
        assert_eq!(encoded["receipt"], receipt);
        assert!(encoded["receipt"]
            .get(COMMITTED_REJECTION_DIAGNOSTICS_FIELD)
            .is_none());
    }

    #[test]
    fn committed_ledger_rejects_malformed_duplicate_and_over_bound_evidence() {
        assert!(parse_committed_rejection_diagnostics(Some(&json!([]))).is_err());

        let source_identity = "33".repeat(32);
        let first_route = "11".repeat(32);
        let second_route = "22".repeat(32);
        let duplicate = ledger_json([
            (
                first_route.clone(),
                vec![rejection(&first_route, &source_identity, 3)],
            ),
            (
                second_route.clone(),
                vec![rejection(&second_route, &source_identity, 3)],
            ),
        ]);
        assert!(format!(
            "{:#}",
            parse_committed_rejection_diagnostics(Some(&duplicate)).unwrap_err()
        )
        .contains("duplicate diagnostic"));

        let over_bound = ledger_json([(
            first_route.clone(),
            (0..=ctx_history_capture_runtime::MAX_RECORDED_SOURCE_BACKED_RECORD_REJECTIONS)
                .map(|line| rejection(&first_route, &source_identity, line as u64 + 1))
                .collect(),
        )]);
        assert!(format!(
            "{:#}",
            parse_committed_rejection_diagnostics(Some(&over_bound)).unwrap_err()
        )
        .contains("bounded contract"));
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

    #[test]
    fn all_scope_accepts_a_truthful_empty_catalog_but_exact_scope_does_not() {
        let mut all = metadata(json!({
            "route_results": {},
        }));
        all.receipt["current"]["current_source_count"] = json!(0);
        all.encode()
            .expect("an all-scope refresh can certify a genuinely empty catalog");

        let mut exact = all;
        exact.refresh_scope = SourceBackedRefreshScope::exact(Vec::new());
        assert!(matches!(
            exact.encode(),
            Err(IndexError::PublicationMetadata(message))
                if message.contains("no publication authority")
        ));
    }
}
