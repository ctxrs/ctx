use super::super::*;
use super::receipts::SourcePredicate;

pub(crate) trait ProviderCaptureSink {
    fn base_source(&self, source: &SourceKey) -> Option<CertifiedSource>;
    fn begin(&mut self, source: SourceKey) -> SourceBackedRouteResult<()>;
    fn document(&mut self, document: LexicalDocument) -> SourceBackedRouteResult<()>;
    fn certify(&mut self, certificate: CertifiedSource) -> SourceBackedRouteResult<()>;
    fn certify_append(&mut self, append: CertifiedSourceAppend) -> SourceBackedRouteResult<()>;
}

struct EvidenceCaptureSink {
    bases: HashMap<[u8; 32], CertifiedSource>,
    active: Option<SourceKey>,
    certificates: Vec<CertifiedSource>,
    append_proofs: HashMap<[u8; 32], CertifiedSourceAppend>,
}

impl EvidenceCaptureSink {
    fn new(bases: &[CertifiedSource]) -> Self {
        Self {
            bases: bases
                .iter()
                .map(|base| {
                    (
                        base.observation().source().identity().digest(),
                        base.clone(),
                    )
                })
                .collect(),
            active: None,
            certificates: Vec::new(),
            append_proofs: HashMap::new(),
        }
    }
}

impl ProviderCaptureSink for EvidenceCaptureSink {
    fn base_source(&self, source: &SourceKey) -> Option<CertifiedSource> {
        self.bases.get(&source.identity().digest()).cloned()
    }

    fn begin(&mut self, source: SourceKey) -> SourceBackedRouteResult<()> {
        if self.active.replace(source).is_some() {
            return Err(SourceBackedRouteError::new(
                SourceBackedRouteErrorKind::Internal,
                "provider capture began a second source before certification",
            ));
        }
        Ok(())
    }

    fn document(&mut self, _document: LexicalDocument) -> SourceBackedRouteResult<()> {
        if self.active.is_none() {
            return Err(SourceBackedRouteError::new(
                SourceBackedRouteErrorKind::Internal,
                "provider capture emitted a document before beginning its source",
            ));
        }
        Ok(())
    }

    fn certify(&mut self, certificate: CertifiedSource) -> SourceBackedRouteResult<()> {
        let source = self.active.take().ok_or_else(|| {
            SourceBackedRouteError::new(
                SourceBackedRouteErrorKind::Internal,
                "provider capture certified a source that was not active",
            )
        })?;
        if !source.exact_descriptor_eq(certificate.observation().source()) {
            return Err(SourceBackedRouteError::new(
                SourceBackedRouteErrorKind::SourceChanged,
                "provider capture changed source descriptor before certification",
            ));
        }
        self.certificates
            .push(captured_replay_certificate(certificate)?);
        Ok(())
    }

    fn certify_append(&mut self, append: CertifiedSourceAppend) -> SourceBackedRouteResult<()> {
        let source = self.active.take().ok_or_else(|| {
            captured_route_internal("provider capture certified an append that was not active")
        })?;
        if !source.exact_descriptor_eq(append.current().observation().source())
            || !source.exact_descriptor_eq(append.base().observation().source())
        {
            return Err(captured_route_changed(
                "provider append proof changed source descriptor before certification",
            ));
        }
        if append.current().frontier().is_none() {
            return Err(captured_route_contract(
                "provider append proof has no current frontier",
            ));
        }
        let identity = source.identity().digest();
        if self.bases.get(&identity) != Some(append.base()) {
            return Err(captured_route_changed(
                "provider append proof did not extend the supplied route base",
            ));
        }
        if self
            .append_proofs
            .insert(identity, append.clone())
            .is_some()
        {
            return Err(captured_route_internal(
                "provider capture certified one source append more than once",
            ));
        }
        self.certificates.push(append.into_current());
        Ok(())
    }

}

pub(super) type ProviderCaptureCallback =
    dyn Fn(&mut dyn ProviderCaptureSink) -> SourceBackedRouteResult<()> + Send + Sync;

#[derive(Clone)]
pub(super) struct CapturedRouteInventoryAuthority {
    provider: String,
    route: ProviderSource,
}

impl CapturedRouteInventoryAuthority {
    pub(super) fn new(route: &ProviderSource) -> Self {
        Self {
            provider: route.provider.as_str().to_owned(),
            route: route.clone(),
        }
    }

    pub(super) fn certify(
        &self,
        certificates: &[CertifiedSource],
    ) -> SourceBackedRouteResult<CertifiedSourceInventory> {
        certify_source_inventory(&self.route, certificates)
    }
}

pub(super) struct CapturedRouteEvidence {
    pub(super) certificates: Vec<CertifiedSource>,
    pub(super) certificates_by_identity: HashMap<[u8; 32], CertifiedSource>,
    pub(super) append_proofs: HashMap<[u8; 32], CertifiedSourceAppend>,
    pub(super) inventory: CertifiedSourceInventory,
}

pub(super) fn capture_route_evidence(
    capture: &ProviderCaptureCallback,
    authority: &CapturedRouteInventoryAuthority,
    bases: &[CertifiedSource],
) -> SourceBackedRouteResult<CapturedRouteEvidence> {
    let mut evidence = EvidenceCaptureSink::new(bases);
    capture(&mut evidence)?;
    if evidence.active.is_some() {
        return Err(captured_route_internal(
            "provider capture ended with an uncertified active source",
        ));
    }
    finish_captured_route_evidence(
        evidence.certificates,
        evidence.append_proofs,
        None,
        authority,
    )
}

pub(super) fn finish_captured_route_evidence(
    mut certificates: Vec<CertifiedSource>,
    append_proofs: HashMap<[u8; 32], CertifiedSourceAppend>,
    inventory: Option<CertifiedSourceInventory>,
    authority: &CapturedRouteInventoryAuthority,
) -> SourceBackedRouteResult<CapturedRouteEvidence> {
    certificates
        .sort_by_key(|certificate| certificate.observation().source().exact_descriptor_digest());
    if certificates.windows(2).any(|pair| {
        pair[0].observation().source().identity() == pair[1].observation().source().identity()
    }) {
        return Err(captured_route_internal(
            "provider capture certified one source identity more than once",
        ));
    }
    if certificates
        .iter()
        .any(|certificate| certificate.observation().source().provider() != authority.provider)
    {
        return Err(captured_route_changed(
            "provider capture emitted a source outside its route provider",
        ));
    }
    let certificates_by_identity = certificates
        .iter()
        .map(|certificate| {
            (
                certificate.observation().source().identity().digest(),
                certificate.clone(),
            )
        })
        .collect::<HashMap<_, _>>();
    if append_proofs.values().any(|append| {
        certificates_by_identity.get(&append.current().observation().source().identity().digest())
            != Some(append.current())
    }) {
        return Err(captured_route_changed(
            "provider append proof did not match its captured certificate",
        ));
    }
    let inventory = match inventory {
        Some(inventory) => {
            inventory
                .validate_contract()
                .map_err(captured_route_contract)?;
            if inventory.observation().provider() != authority.provider
                || inventory.observed_sources() != certificates.len()
                || certificates
                    .iter()
                    .any(|certificate| !inventory.contains(certificate.observation().source()))
            {
                return Err(captured_route_changed(
                    "provider complete inventory did not match its captured source set",
                ));
            }
            inventory
        }
        None => authority.certify(&certificates)?,
    };
    Ok(CapturedRouteEvidence {
        certificates,
        certificates_by_identity,
        append_proofs,
        inventory,
    })
}

pub(super) fn validate_captured_route_ownership(
    evidence: &CapturedRouteEvidence,
    owns_source: &SourcePredicate,
) -> SourceBackedRouteResult<()> {
    if evidence
        .certificates
        .iter()
        .all(|certificate| owns_source(certificate.observation().source()))
    {
        Ok(())
    } else {
        Err(captured_route_changed(
            "provider capture emitted a source outside its registered ownership predicate",
        ))
    }
}

pub(super) fn captured_replay_certificate(
    certificate: CertifiedSource,
) -> SourceBackedRouteResult<CertifiedSource> {
    if certificate.frontier().is_some() {
        return Ok(certificate);
    }
    let counts = certificate.counts();
    let digest = *certificate.content_digest();
    let frontier = SourceFrontier::new(
        "ctx-captured-route-full-snapshot-v1",
        TypedKey::bytes(digest.to_vec()).map_err(captured_route_contract)?,
        counts.certified_bytes,
        digest,
    )
    .map_err(captured_route_contract)?;
    CertifiedSource::certify_with_frontier(
        certificate.observation().clone(),
        certificate.observation().clone(),
        certificate.parser_revision(),
        digest,
        counts,
        Some(frontier),
    )
    .map_err(captured_route_contract)
}

pub(super) fn captured_route_contract(error: impl fmt::Display) -> SourceBackedRouteError {
    SourceBackedRouteError::new(SourceBackedRouteErrorKind::Internal, error.to_string())
}

pub(super) fn captured_route_internal(detail: impl Into<String>) -> SourceBackedRouteError {
    SourceBackedRouteError::new(SourceBackedRouteErrorKind::Internal, detail)
}

pub(super) fn captured_route_changed(detail: impl Into<String>) -> SourceBackedRouteError {
    SourceBackedRouteError::new(SourceBackedRouteErrorKind::SourceChanged, detail)
}
