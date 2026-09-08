use super::*;

pub const SOURCE_BACKED_GENERATION_STATE_FORMAT: &str = "ctx.source-backed-generation-state.v1";

/// Source-refresh authority that participates in the immutable Core
/// generation identity. Request-local receipt fields deliberately do not
/// belong here.
#[derive(Debug, Clone)]
pub struct SourceBackedGenerationState {
    applied_explicit_source_catalog: Option<ExplicitSourceCatalogAuthority>,
    catalog_route_bindings: Vec<ExplicitSourceCatalogRouteBinding>,
    route_observations: BTreeMap<SourceRouteIdentity, String>,
    route_controls: BTreeMap<SourceRouteIdentity, Vec<u8>>,
    committed_rejection_diagnostics: Vec<SourceBackedRefreshRecordRejection>,
}

impl SourceBackedGenerationState {
    pub fn new(
        applied_explicit_source_catalog: Option<ExplicitSourceCatalogAuthority>,
        catalog_route_bindings: Vec<ExplicitSourceCatalogRouteBinding>,
        route_observations: BTreeMap<SourceRouteIdentity, String>,
        route_controls: BTreeMap<SourceRouteIdentity, Vec<u8>>,
        committed_rejection_diagnostics: Vec<SourceBackedRefreshRecordRejection>,
    ) -> ctx_history_index::Result<Self> {
        let applied_lineages = applied_explicit_source_catalog
            .as_ref()
            .map(ExplicitSourceCatalogAuthority::route_lineages)
            .unwrap_or_default();
        let mut bindings = BTreeMap::new();
        for binding in catalog_route_bindings {
            // Receipt-only failed-request bindings have no durable catalog
            // authority. Discard them before validating retained bindings.
            if !applied_lineages.contains(&binding.catalog_lineage) {
                continue;
            }
            if !is_canonical_sha256_identity(&binding.catalog_lineage)
                || SourceRouteIdentity::from_sha256(binding.route_identity.clone()).is_err()
            {
                return Err(ctx_history_index::IndexError::InvalidGenerationStateEnvelope);
            }
            if bindings
                .insert(binding.catalog_lineage.clone(), binding.route_identity)
                .is_some()
            {
                return Err(ctx_history_index::IndexError::InvalidGenerationStateEnvelope);
            }
        }
        if !applied_lineages.is_subset(&bindings.keys().cloned().collect()) {
            return Err(ctx_history_index::IndexError::InvalidGenerationStateEnvelope);
        }
        if route_observations
            .values()
            .any(|observation| !is_canonical_sha256_identity(observation))
            || route_controls.iter().any(|(_, control)| {
                control.len() > ctx_history_capture_runtime::MAX_SOURCE_BACKED_ROUTE_CONTROL_BYTES
            })
            || committed_rejection_diagnostics.len()
                > ctx_history_capture_runtime::MAX_RECORDED_SOURCE_BACKED_RECORD_REJECTIONS
        {
            return Err(ctx_history_index::IndexError::InvalidGenerationStateEnvelope);
        }
        let state = Self {
            applied_explicit_source_catalog,
            catalog_route_bindings: bindings
                .into_iter()
                .map(
                    |(catalog_lineage, route_identity)| ExplicitSourceCatalogRouteBinding {
                        catalog_lineage,
                        route_identity,
                    },
                )
                .collect(),
            route_observations,
            route_controls,
            committed_rejection_diagnostics,
        };
        state.validate_diagnostics()?;
        Ok(state)
    }

    pub fn envelope(
        &self,
    ) -> ctx_history_index::Result<ctx_history_index::GenerationStateEnvelope> {
        let mut fitted = self.clone();
        loop {
            let bytes = serde_json::to_vec(&fitted.json_value())
                .map_err(|_| ctx_history_index::IndexError::InvalidGenerationStateEnvelope)?;
            if bytes.len() <= ctx_history_index::MAX_GENERATION_STATE_BYTES {
                return ctx_history_index::GenerationStateEnvelope::new(
                    SOURCE_BACKED_GENERATION_STATE_FORMAT,
                    bytes,
                );
            }
            if !fitted.route_observations.is_empty() {
                // Observations only avoid redundant scans. Drop them as one
                // deterministic unit before reducing retained diagnostics.
                fitted.route_observations.clear();
            } else if fitted.committed_rejection_diagnostics.pop().is_none() {
                return ctx_history_index::GenerationStateEnvelope::new(
                    SOURCE_BACKED_GENERATION_STATE_FORMAT,
                    bytes,
                );
            }
        }
    }

    pub fn decode(
        envelope: &ctx_history_index::GenerationStateEnvelope,
    ) -> ctx_history_index::Result<Self> {
        if envelope.format() != SOURCE_BACKED_GENERATION_STATE_FORMAT {
            return Err(ctx_history_index::IndexError::InvalidGenerationStateEnvelope);
        }
        let value: Value = serde_json::from_slice(envelope.canonical_bytes())
            .map_err(|_| ctx_history_index::IndexError::InvalidGenerationStateEnvelope)?;
        let fields = value
            .as_object()
            .ok_or(ctx_history_index::IndexError::InvalidGenerationStateEnvelope)?;
        let expected = BTreeSet::from([
            "applied_explicit_source_catalog",
            "catalog_route_bindings",
            "committed_rejection_diagnostics",
            "route_controls",
            "route_observations",
        ]);
        if fields.keys().map(String::as_str).collect::<BTreeSet<_>>() != expected {
            return Err(ctx_history_index::IndexError::InvalidGenerationStateEnvelope);
        }
        let applied_explicit_source_catalog = fields
            .get("applied_explicit_source_catalog")
            .filter(|value| !value.is_null())
            .map(ExplicitSourceCatalogAuthority::from_json)
            .transpose()
            .map_err(|_| ctx_history_index::IndexError::InvalidGenerationStateEnvelope)?;
        let bindings = decode_bindings(fields.get("catalog_route_bindings"))?;
        let route_observations = decode_observations(fields.get("route_observations"))?;
        let route_controls = decode_route_controls(fields.get("route_controls"))?;
        let committed_rejection_diagnostics =
            parse_committed_rejection_diagnostics(fields.get("committed_rejection_diagnostics"))
                .map_err(|_| ctx_history_index::IndexError::InvalidGenerationStateEnvelope)?;
        let state = Self::new(
            applied_explicit_source_catalog,
            bindings,
            route_observations,
            route_controls,
            committed_rejection_diagnostics,
        )?;
        if state.json_bytes()? != envelope.canonical_bytes() {
            return Err(ctx_history_index::IndexError::InvalidGenerationStateEnvelope);
        }
        Ok(state)
    }

    /// Decodes generation-owned refresh state and binds every retained route
    /// and diagnostic back to the exact verified manifest that owns it.
    pub fn decode_from_verified_index(index: &VerifiedIndex) -> ctx_history_index::Result<Self> {
        Self::decode_from_manifest(index.manifest())
    }

    pub fn decode_from_manifest(manifest: &GenerationManifest) -> ctx_history_index::Result<Self> {
        let envelope = manifest
            .generation_state()
            .ok_or(ctx_history_index::IndexError::InvalidGenerationStateEnvelope)?;
        let state = Self::decode(envelope)?;
        state.validate_for_manifest(manifest)?;
        Ok(state)
    }

    pub fn applied_explicit_source_catalog(&self) -> Option<&ExplicitSourceCatalogAuthority> {
        self.applied_explicit_source_catalog.as_ref()
    }

    pub fn catalog_route_bindings(&self) -> &[ExplicitSourceCatalogRouteBinding] {
        &self.catalog_route_bindings
    }

    pub fn route_observations(&self) -> &BTreeMap<SourceRouteIdentity, String> {
        &self.route_observations
    }

    pub fn route_controls(&self) -> &BTreeMap<SourceRouteIdentity, Vec<u8>> {
        &self.route_controls
    }

    pub fn committed_rejection_diagnostics(&self) -> &[SourceBackedRefreshRecordRejection] {
        &self.committed_rejection_diagnostics
    }

    pub fn is_empty(&self) -> bool {
        self.applied_explicit_source_catalog.is_none()
            && self.catalog_route_bindings.is_empty()
            && self.route_observations.is_empty()
            && self.route_controls.is_empty()
            && self.committed_rejection_diagnostics.is_empty()
    }

    fn validate_diagnostics(&self) -> ctx_history_index::Result<()> {
        let value = crate::execution::publication::rejection_diagnostics_ledger_json(
            &self.committed_rejection_diagnostics,
        )
        .map_err(|_| ctx_history_index::IndexError::InvalidGenerationStateEnvelope)?;
        parse_committed_rejection_diagnostics(Some(&value))
            .map(|_| ())
            .map_err(|_| ctx_history_index::IndexError::InvalidGenerationStateEnvelope)
    }

    fn validate_for_manifest(
        &self,
        manifest: &GenerationManifest,
    ) -> ctx_history_index::Result<()> {
        let live_routes = manifest
            .source_routes()
            .iter()
            .map(|route| route.route_identity().clone())
            .collect::<BTreeSet<_>>();
        if self
            .route_observations
            .keys()
            .chain(self.route_controls.keys())
            .any(|route| !live_routes.contains(route))
            || self.catalog_route_bindings.iter().any(|binding| {
                SourceRouteIdentity::from_sha256(binding.route_identity.clone())
                    .map_or(true, |route| !live_routes.contains(&route))
            })
        {
            return Err(ctx_history_index::IndexError::InvalidGenerationStateEnvelope);
        }

        let certificates = manifest
            .sources
            .iter()
            .map(|certificate| {
                (
                    certificate.observation().source().identity().digest(),
                    certificate,
                )
            })
            .collect::<BTreeMap<_, _>>();
        let mut rejection_capacity = BTreeMap::<(String, String), u64>::new();
        for route in manifest.source_routes() {
            for source in route.sources() {
                let certificate = certificates
                    .get(&source.identity().digest())
                    .filter(|certificate| {
                        certificate
                            .observation()
                            .source()
                            .exact_descriptor_eq(source)
                    })
                    .ok_or(ctx_history_index::IndexError::InvalidGenerationStateEnvelope)?;
                rejection_capacity.insert(
                    (
                        route.route_identity().as_str().to_owned(),
                        source_identity(source),
                    ),
                    certificate.counts().rejected_records,
                );
            }
        }
        let mut observed = BTreeMap::<(String, String), u64>::new();
        for diagnostic in &self.committed_rejection_diagnostics {
            let key = (
                diagnostic.route_identity.clone(),
                diagnostic.source_identity.clone(),
            );
            let capacity = rejection_capacity
                .get(&key)
                .ok_or(ctx_history_index::IndexError::InvalidGenerationStateEnvelope)?;
            let count = observed.entry(key).or_default();
            *count = count
                .checked_add(1)
                .ok_or(ctx_history_index::IndexError::InvalidGenerationStateEnvelope)?;
            if *count > *capacity {
                return Err(ctx_history_index::IndexError::InvalidGenerationStateEnvelope);
            }
        }
        Ok(())
    }

    fn json_bytes(&self) -> ctx_history_index::Result<Vec<u8>> {
        serde_json::to_vec(&self.json_value())
            .map_err(|_| ctx_history_index::IndexError::InvalidGenerationStateEnvelope)
    }

    fn json_value(&self) -> Value {
        let bindings = self
            .catalog_route_bindings
            .iter()
            .map(|binding| {
                (
                    binding.catalog_lineage.clone(),
                    Value::String(binding.route_identity.clone()),
                )
            })
            .collect::<serde_json::Map<_, _>>();
        let observations = self
            .route_observations
            .iter()
            .map(|(route, observation)| {
                (
                    route.as_str().to_owned(),
                    Value::String(observation.clone()),
                )
            })
            .collect::<serde_json::Map<_, _>>();
        let controls = self
            .route_controls
            .iter()
            .map(|(route, control)| {
                (
                    route.as_str().to_owned(),
                    Value::String(encode_route_control(control)),
                )
            })
            .collect::<serde_json::Map<_, _>>();
        json!({
            "applied_explicit_source_catalog": self
                .applied_explicit_source_catalog
                .as_ref()
                .map(ExplicitSourceCatalogAuthority::to_json),
            "catalog_route_bindings": bindings,
            "route_observations": observations,
            "route_controls": controls,
            "committed_rejection_diagnostics": crate::execution::publication::rejection_diagnostics_ledger_json(
                &self.committed_rejection_diagnostics,
            ).expect("validated generation-state diagnostics"),
        })
    }
}

fn source_identity(source: &ctx_history_core::SourceKey) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let digest = source.identity().digest();
    let mut encoded = String::with_capacity(digest.len() * 2);
    for byte in digest {
        encoded.push(HEX[usize::from(byte >> 4)] as char);
        encoded.push(HEX[usize::from(byte & 0x0f)] as char);
    }
    encoded
}

fn decode_bindings(
    value: Option<&Value>,
) -> ctx_history_index::Result<Vec<ExplicitSourceCatalogRouteBinding>> {
    value
        .and_then(Value::as_object)
        .ok_or(ctx_history_index::IndexError::InvalidGenerationStateEnvelope)?
        .iter()
        .map(|(catalog_lineage, route_identity)| {
            let route_identity = route_identity
                .as_str()
                .ok_or(ctx_history_index::IndexError::InvalidGenerationStateEnvelope)?;
            Ok(ExplicitSourceCatalogRouteBinding {
                catalog_lineage: catalog_lineage.clone(),
                route_identity: route_identity.to_owned(),
            })
        })
        .collect()
}

fn decode_observations(
    value: Option<&Value>,
) -> ctx_history_index::Result<BTreeMap<SourceRouteIdentity, String>> {
    value
        .and_then(Value::as_object)
        .ok_or(ctx_history_index::IndexError::InvalidGenerationStateEnvelope)?
        .iter()
        .map(|(route, observation)| {
            let route = SourceRouteIdentity::from_sha256(route.clone())
                .map_err(|_| ctx_history_index::IndexError::InvalidGenerationStateEnvelope)?;
            let observation = observation
                .as_str()
                .filter(|value| is_canonical_sha256_identity(value))
                .ok_or(ctx_history_index::IndexError::InvalidGenerationStateEnvelope)?;
            Ok((route, observation.to_owned()))
        })
        .collect()
}

fn is_canonical_sha256_identity(value: &str) -> bool {
    value.len() == 64
        && value.bytes().all(|byte| {
            byte.is_ascii_digit() || (byte.is_ascii_lowercase() && byte.is_ascii_hexdigit())
        })
}

fn decode_route_controls(
    value: Option<&Value>,
) -> ctx_history_index::Result<BTreeMap<SourceRouteIdentity, Vec<u8>>> {
    value
        .and_then(Value::as_object)
        .ok_or(ctx_history_index::IndexError::InvalidGenerationStateEnvelope)?
        .iter()
        .map(|(route, control)| {
            let route = SourceRouteIdentity::from_sha256(route.clone())
                .map_err(|_| ctx_history_index::IndexError::InvalidGenerationStateEnvelope)?;
            let control = control
                .as_str()
                .and_then(decode_route_control)
                .ok_or(ctx_history_index::IndexError::InvalidGenerationStateEnvelope)?;
            Ok((route, control))
        })
        .collect()
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
            _ => None,
        }
    }
    encoded
        .as_bytes()
        .chunks_exact(2)
        .map(|pair| Some((nibble(pair[0])? << 4) | nibble(pair[1])?))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state() -> SourceBackedGenerationState {
        SourceBackedGenerationState::new(
            None,
            vec![ExplicitSourceCatalogRouteBinding {
                catalog_lineage: "11".repeat(32),
                route_identity: "22".repeat(32),
            }],
            BTreeMap::from([(
                SourceRouteIdentity::from_sha256("22".repeat(32)).unwrap(),
                "33".repeat(32),
            )]),
            BTreeMap::new(),
            Vec::new(),
        )
        .unwrap()
    }

    #[test]
    fn canonical_roundtrip_filters_unapplied_bindings() {
        let state = state();
        let envelope = state.envelope().unwrap();
        assert!(state.catalog_route_bindings().is_empty());
        assert_eq!(
            SourceBackedGenerationState::decode(&envelope)
                .unwrap()
                .catalog_route_bindings(),
            []
        );
    }

    #[test]
    fn malformed_noncanonical_and_oversize_state_are_rejected() {
        let envelope = state().envelope().unwrap();
        let mut malformed: Value = serde_json::from_slice(envelope.canonical_bytes()).unwrap();
        malformed["unknown"] = json!(true);
        let malformed = ctx_history_index::GenerationStateEnvelope::new(
            SOURCE_BACKED_GENERATION_STATE_FORMAT,
            serde_json::to_vec(&malformed).unwrap(),
        )
        .unwrap();
        assert!(SourceBackedGenerationState::decode(&malformed).is_err());

        let noncanonical = ctx_history_index::GenerationStateEnvelope::new(
            SOURCE_BACKED_GENERATION_STATE_FORMAT,
            br#"{"route_controls":{},"route_observations":{},"catalog_route_bindings":{},"committed_rejection_diagnostics":{},"applied_explicit_source_catalog":null}"#.to_vec(),
        )
        .unwrap();
        assert!(SourceBackedGenerationState::decode(&noncanonical).is_err());

        assert!(ctx_history_index::GenerationStateEnvelope::new(
            SOURCE_BACKED_GENERATION_STATE_FORMAT,
            vec![b'x'; 48 * 1024 + 1],
        )
        .is_err());
    }

    #[test]
    fn envelope_fitting_is_deterministic_and_does_not_revive_trimmed_evidence() {
        let route_identity = "11".repeat(32);
        let source_identity = "22".repeat(32);
        let diagnostics = (1..=64)
            .map(|line| SourceBackedRefreshRecordRejection {
                route_identity: route_identity.clone(),
                source_identity: source_identity.clone(),
                provider: "codex".to_owned(),
                source_selector: "s".repeat(512),
                line,
                payload_type: "p".repeat(128),
                class: "malformed_record".to_owned(),
                detail: "d".repeat(512),
            })
            .collect::<Vec<_>>();
        let observations = (0_u16..256)
            .map(|index| {
                (
                    SourceRouteIdentity::from_sha256(format!("{index:064x}")).unwrap(),
                    "33".repeat(32),
                )
            })
            .collect();
        let state = SourceBackedGenerationState::new(
            None,
            Vec::new(),
            observations,
            BTreeMap::new(),
            diagnostics.clone(),
        )
        .unwrap();

        let first = state.envelope().unwrap();
        assert_eq!(first, state.envelope().unwrap());
        let persisted = SourceBackedGenerationState::decode(&first).unwrap();
        assert!(persisted.route_observations().is_empty());
        assert!(!persisted.committed_rejection_diagnostics().is_empty());
        assert!(persisted.committed_rejection_diagnostics().len() < diagnostics.len());

        let carried = SourceBackedGenerationState::new(
            None,
            Vec::new(),
            BTreeMap::new(),
            BTreeMap::new(),
            persisted.committed_rejection_diagnostics().to_vec(),
        )
        .unwrap();
        assert_eq!(
            SourceBackedGenerationState::decode(&carried.envelope().unwrap())
                .unwrap()
                .committed_rejection_diagnostics(),
            persisted.committed_rejection_diagnostics()
        );
    }
}
