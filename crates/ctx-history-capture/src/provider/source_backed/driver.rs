use super::*;
use ctx_history_core::SourceInventoryObservation;
use sha2::{Digest, Sha256};
use std::sync::Mutex;

pub type SourceBackedCoordinatorResult<T> = Result<T, SourceBackedCoordinatorError>;
pub type SourceBackedRouteResult<T> = Result<T, SourceBackedRouteError>;

/// Runtime metadata for one selected source route.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceBackedRouteMetadata {
    pub source: ProviderSource,
    pub certified_source_format: &'static str,
    pub selection: Option<SourceBackedRouteSelection>,
    pub selector_authority: SourceBackedSelectorAuthority,
    pub unsupported_reason: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceBackedRouteErrorKind {
    Unavailable,
    SourceChanged,
    InvalidSource,
    Unsupported,
    Internal,
}

#[derive(Debug, Clone, Error, PartialEq, Eq)]
#[error("{kind:?}: {detail}")]
pub struct SourceBackedRouteError {
    pub kind: SourceBackedRouteErrorKind,
    pub detail: String,
}

impl SourceBackedRouteError {
    pub fn new(kind: SourceBackedRouteErrorKind, detail: impl Into<String>) -> Self {
        Self {
            kind,
            detail: detail.into(),
        }
    }
}

#[derive(Debug, Error)]
pub enum SourceBackedCoordinatorError {
    #[error(transparent)]
    Index(#[from] IndexError),
    #[error("invalid source-backed route for {provider}: {detail}")]
    InvalidRoute {
        provider: CaptureProvider,
        detail: String,
    },
    #[error("source-backed scan failed for {provider}: {source}")]
    RouteScan {
        provider: CaptureProvider,
        #[source]
        source: SourceBackedRouteError,
    },
    #[error("source {source_id} was staged by more than one provider route")]
    DuplicateSourceOwner { source_id: String },
    #[error("no executable source-backed routes were registered")]
    NoExecutableRoutes,
    #[error("source deletion was not certified by its supplied authoritative inventory")]
    InvalidDeletionWitness,
    #[error("source-backed refresh progress callback failed: {0}")]
    Progress(SourceBackedRouteError),
}

/// The only write surface provider drivers receive. It exposes staging and
/// certification, but never generation commit.
pub struct SourceBackedGenerationSink<'writer> {
    pub(super) writer: &'writer mut GenerationWriter,
    pub(super) owners: &'writer mut HashMap<[u8; 32], SourceOwner>,
    pub(super) complete_inventories: &'writer mut Vec<CompleteInventoryOwner>,
    pub(super) route_index: usize,
}

#[derive(Clone)]
pub(super) struct SourceOwner {
    pub(super) route_index: usize,
    pub(super) source: SourceKey,
}

#[derive(Clone)]
pub(super) struct CompleteInventoryOwner {
    pub(super) route_index: usize,
    pub(super) inventory: CertifiedSourceInventory,
}

impl SourceBackedGenerationSink<'_> {
    pub fn base_source(&self, source: &SourceKey) -> Option<&CertifiedSource> {
        self.writer.base_manifest().and_then(|manifest| {
            manifest
                .sources
                .iter()
                .find(|candidate| candidate.observation().source().exact_descriptor_eq(source))
        })
    }

    pub fn begin_source(&mut self, source: SourceKey) -> SourceBackedCoordinatorResult<()> {
        self.claim(&source)?;
        self.writer.begin_source(source)?;
        Ok(())
    }

    pub fn begin_source_append(
        &mut self,
        source: SourceKey,
    ) -> SourceBackedCoordinatorResult<&CertifiedSource> {
        self.claim(&source)?;
        Ok(self.writer.begin_source_append(source)?)
    }

    pub fn add_document(&mut self, document: LexicalDocument) -> SourceBackedCoordinatorResult<()> {
        self.writer.add_document(document)?;
        Ok(())
    }

    pub fn certify_source(
        &mut self,
        certificate: CertifiedSource,
    ) -> SourceBackedCoordinatorResult<()> {
        self.writer.certify_source(certificate)?;
        Ok(())
    }

    pub fn certify_source_append(
        &mut self,
        append: CertifiedSourceAppend,
    ) -> SourceBackedCoordinatorResult<()> {
        self.writer.certify_source_append(append)?;
        Ok(())
    }

    pub fn certify_complete_inventory(
        &mut self,
        inventory: CertifiedSourceInventory,
    ) -> SourceBackedCoordinatorResult<()> {
        self.writer.certify_complete_inventory(inventory.clone())?;
        self.complete_inventories.push(CompleteInventoryOwner {
            route_index: self.route_index,
            inventory,
        });
        Ok(())
    }

    pub fn delete_source(
        &mut self,
        deletion: CertifiedSourceDeletion,
        inventory: CertifiedSourceInventory,
    ) -> SourceBackedCoordinatorResult<()> {
        if !deletion.verifies(&inventory) {
            return Err(SourceBackedCoordinatorError::InvalidDeletionWitness);
        }
        self.claim(deletion.source())?;
        self.writer.delete_source(deletion, inventory)?;
        Ok(())
    }

    pub fn replace_source(
        &mut self,
        certificate: CertifiedSource,
        documents: impl IntoIterator<Item = LexicalDocument>,
    ) -> SourceBackedCoordinatorResult<()> {
        self.begin_source(certificate.observation().source().clone())?;
        for document in documents {
            self.add_document(document)?;
        }
        self.certify_source(certificate)
    }

    pub(super) fn claim(&mut self, source: &SourceKey) -> SourceBackedCoordinatorResult<()> {
        let digest = source.identity().digest();
        match self.owners.get(&digest) {
            Some(owner)
                if owner.route_index != self.route_index
                    || !owner.source.exact_descriptor_eq(source) =>
            {
                return Err(SourceBackedCoordinatorError::DuplicateSourceOwner {
                    source_id: source.identity().to_string(),
                });
            }
            Some(_) => {}
            None => {
                self.owners.insert(
                    digest,
                    SourceOwner {
                        route_index: self.route_index,
                        source: source.clone(),
                    },
                );
            }
        }
        Ok(())
    }
}

pub enum SourceBackedRevalidationTarget<'a> {
    Source(&'a CertifiedSource),
    Deletion(&'a CertifiedSourceDeletion),
}

pub(crate) trait ProviderCaptureSink {
    fn base_source(&self, source: &SourceKey) -> Option<CertifiedSource>;
    fn begin(&mut self, source: SourceKey) -> SourceBackedRouteResult<()>;
    fn document(&mut self, document: LexicalDocument) -> SourceBackedRouteResult<()>;
    fn certify(&mut self, certificate: CertifiedSource) -> SourceBackedRouteResult<()>;
    fn certify_append(&mut self, append: CertifiedSourceAppend) -> SourceBackedRouteResult<()>;
    fn certify_complete_inventory(
        &mut self,
        inventory: CertifiedSourceInventory,
    ) -> SourceBackedRouteResult<()>;
}

struct WriterCaptureSink<'sink, 'writer> {
    sink: &'sink mut SourceBackedGenerationSink<'writer>,
    plans: &'sink HashMap<[u8; 32], CapturedSourcePlan>,
    active: Option<CapturedSourceMode>,
    certificates: Vec<CertifiedSource>,
    append_proofs: HashMap<[u8; 32], CertifiedSourceAppend>,
    inventory: Option<CertifiedSourceInventory>,
}

enum CapturedSourcePlan {
    Exact {
        base: CertifiedSource,
        expected: CertifiedSource,
    },
    Append {
        proof: CertifiedSourceAppend,
    },
    Replace {
        base: Option<CertifiedSource>,
        expected: CertifiedSource,
    },
}

enum CapturedSourceMode {
    Exact {
        base: CertifiedSource,
        expected: CertifiedSource,
    },
    Append {
        expected: CertifiedSourceAppend,
    },
    Replace {
        expected: CertifiedSource,
    },
}

impl ProviderCaptureSink for WriterCaptureSink<'_, '_> {
    fn base_source(&self, source: &SourceKey) -> Option<CertifiedSource> {
        match self.plans.get(&source.identity().digest())? {
            CapturedSourcePlan::Exact { base, .. } => Some(base.clone()),
            CapturedSourcePlan::Append { proof } => Some(proof.base().clone()),
            CapturedSourcePlan::Replace { base, .. } => base.clone(),
        }
    }

    fn begin(&mut self, source: SourceKey) -> SourceBackedRouteResult<()> {
        if self.active.is_some() {
            return Err(captured_route_internal(
                "provider capture began a second source before certification",
            ));
        }
        let plan = self.plans.get(&source.identity().digest()).ok_or_else(|| {
            captured_route_changed("provider capture introduced an unplanned source")
        })?;
        self.active = Some(match plan {
            CapturedSourcePlan::Exact { base, expected } => {
                if !source.exact_descriptor_eq(expected.observation().source()) {
                    return Err(captured_route_changed(
                        "provider capture changed an exact-replay source descriptor",
                    ));
                }
                CapturedSourceMode::Exact {
                    base: base.clone(),
                    expected: expected.clone(),
                }
            }
            CapturedSourcePlan::Append { proof } => {
                if !source.exact_descriptor_eq(proof.current().observation().source()) {
                    return Err(captured_route_changed(
                        "provider capture changed an append source descriptor",
                    ));
                }
                self.sink
                    .begin_source_append(source)
                    .map_err(route_coordinator_error)?;
                CapturedSourceMode::Append {
                    expected: proof.clone(),
                }
            }
            CapturedSourcePlan::Replace { expected, .. } => {
                if !source.exact_descriptor_eq(expected.observation().source()) {
                    return Err(captured_route_changed(
                        "provider capture changed a replacement source descriptor",
                    ));
                }
                self.sink
                    .begin_source(source)
                    .map_err(route_coordinator_error)?;
                CapturedSourceMode::Replace {
                    expected: expected.clone(),
                }
            }
        });
        Ok(())
    }

    fn document(&mut self, document: LexicalDocument) -> SourceBackedRouteResult<()> {
        match self.active.as_ref() {
            Some(CapturedSourceMode::Exact { .. }) => Ok(()),
            Some(CapturedSourceMode::Append { .. } | CapturedSourceMode::Replace { .. }) => self
                .sink
                .add_document(document)
                .map_err(route_coordinator_error),
            None => Err(captured_route_internal(
                "provider capture emitted a document before beginning its source",
            )),
        }
    }

    fn certify(&mut self, certificate: CertifiedSource) -> SourceBackedRouteResult<()> {
        let certificate = captured_replay_certificate(certificate)?;
        let active = self.active.take().ok_or_else(|| {
            captured_route_internal("provider capture certified a source that was not active")
        })?;
        match active {
            CapturedSourceMode::Exact { base, expected } => {
                if certificate != expected {
                    return Err(captured_route_changed(
                        "provider capture changed an exact-replay certificate",
                    ));
                }
                stage_captured_exact_replay(self.sink, &base, certificate.clone())?;
            }
            CapturedSourceMode::Append { .. } => {
                return Err(captured_route_changed(
                    "provider capture replaced a planned append certificate",
                ));
            }
            CapturedSourceMode::Replace { expected } => {
                if certificate != expected {
                    return Err(captured_route_changed(
                        "provider capture changed a replacement certificate",
                    ));
                }
                self.sink
                    .certify_source(certificate.clone())
                    .map_err(route_coordinator_error)?;
            }
        }
        self.certificates.push(certificate);
        Ok(())
    }

    fn certify_append(&mut self, append: CertifiedSourceAppend) -> SourceBackedRouteResult<()> {
        let active = self.active.take().ok_or_else(|| {
            captured_route_internal("provider capture certified an append that was not active")
        })?;
        let CapturedSourceMode::Append { expected } = active else {
            return Err(captured_route_changed(
                "provider capture introduced an unplanned append",
            ));
        };
        if append != expected {
            return Err(captured_route_changed(
                "provider append proof changed between planning and staging",
            ));
        }
        self.sink
            .certify_source_append(append.clone())
            .map_err(route_coordinator_error)?;
        self.append_proofs.insert(
            append.current().observation().source().identity().digest(),
            append.clone(),
        );
        self.certificates.push(append.into_current());
        Ok(())
    }

    fn certify_complete_inventory(
        &mut self,
        inventory: CertifiedSourceInventory,
    ) -> SourceBackedRouteResult<()> {
        if self.active.is_some() {
            return Err(captured_route_internal(
                "provider capture certified its inventory with an active source",
            ));
        }
        if self.inventory.replace(inventory).is_some() {
            return Err(captured_route_internal(
                "provider capture certified more than one complete inventory",
            ));
        }
        Ok(())
    }
}

struct EvidenceCaptureSink {
    bases: HashMap<[u8; 32], CertifiedSource>,
    active: Option<SourceKey>,
    certificates: Vec<CertifiedSource>,
    append_proofs: HashMap<[u8; 32], CertifiedSourceAppend>,
    inventory: Option<CertifiedSourceInventory>,
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
            inventory: None,
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

    fn certify_complete_inventory(
        &mut self,
        inventory: CertifiedSourceInventory,
    ) -> SourceBackedRouteResult<()> {
        if self.active.is_some() {
            return Err(captured_route_internal(
                "provider capture certified its inventory with an active source",
            ));
        }
        if self.inventory.replace(inventory).is_some() {
            return Err(captured_route_internal(
                "provider capture certified more than one complete inventory",
            ));
        }
        Ok(())
    }
}

type ProviderCaptureCallback =
    dyn Fn(&mut dyn ProviderCaptureSink) -> SourceBackedRouteResult<()> + Send + Sync;

#[derive(Clone)]
struct CapturedRouteInventoryAuthority {
    provider: String,
    route_key: [u8; 32],
}

impl CapturedRouteInventoryAuthority {
    fn new(route: &ProviderSource) -> Self {
        let path = route.path.as_os_str().as_encoded_bytes();
        let mut digest = Sha256::new();
        digest.update(b"ctx.captured-route-authority\0");
        digest.update((route.provider.as_str().len() as u64).to_be_bytes());
        digest.update(route.provider.as_str().as_bytes());
        digest.update((route.source_format.len() as u64).to_be_bytes());
        digest.update(route.source_format.as_bytes());
        digest.update((path.len() as u64).to_be_bytes());
        digest.update(path);
        Self {
            provider: route.provider.as_str().to_owned(),
            route_key: digest.finalize().into(),
        }
    }

    fn certify(
        &self,
        certificates: &[CertifiedSource],
    ) -> SourceBackedRouteResult<CertifiedSourceInventory> {
        let mut sources = certificates
            .iter()
            .map(|certificate| certificate.observation().source().clone())
            .collect::<Vec<_>>();
        sources.sort_by_key(SourceKey::exact_descriptor_digest);
        let mut revision = Sha256::new();
        revision.update(b"ctx.captured-route-inventory\0");
        revision.update((sources.len() as u64).to_be_bytes());
        for source in &sources {
            revision.update(source.exact_descriptor_digest());
        }
        let observation = SourceInventoryObservation::new(
            self.provider.clone(),
            "ctx.captured-route",
            TypedKey::bytes(self.route_key.to_vec()).map_err(captured_route_contract)?,
            "ctx-captured-route-source-set-v1",
            revision.finalize().to_vec(),
        )
        .map_err(captured_route_contract)?;
        CertifiedSourceInventory::certify(
            observation.clone(),
            observation,
            "ctx-captured-route-inventory-v1",
            sources,
        )
        .map_err(captured_route_contract)
    }
}

pub(crate) fn certify_captured_route_inventory(
    route: &ProviderSource,
    certificates: &[CertifiedSource],
) -> SourceBackedRouteResult<CertifiedSourceInventory> {
    CapturedRouteInventoryAuthority::new(route).certify(certificates)
}

struct CapturedRouteEvidence {
    certificates: Vec<CertifiedSource>,
    certificates_by_identity: HashMap<[u8; 32], CertifiedSource>,
    append_proofs: HashMap<[u8; 32], CertifiedSourceAppend>,
    inventory: CertifiedSourceInventory,
}

/// Adapts a provider's complete route capture into generation staging.
///
/// The callback must enumerate every currently owned source or return
/// `Unavailable`; an empty successful capture is authoritative. Captures that
/// do not expose a provider-native inventory receive a route-scoped inventory
/// derived from their complete certified source set.
///
/// The first pass plans exact replay without constructing an `IndexWriter`.
/// Adapters can expose provider-native append-prefix receipts through the
/// capture sink. Changed sources without such evidence are deliberately
/// replacement-only, while unchanged siblings still use exact replay.
pub(crate) fn captured_route_driver(
    route: &ProviderSource,
    capture: impl Fn(&mut dyn ProviderCaptureSink) -> SourceBackedRouteResult<()>
        + Send
        + Sync
        + 'static,
    owns_source: impl Fn(&SourceKey) -> bool + Send + Sync + 'static,
    hydrate: impl Fn(&EventHydrationRequest) -> Result<HydratedProviderRecord, HydrationFailure>
        + Send
        + Sync
        + 'static,
) -> SourceBackedRouteDriver {
    let authority = CapturedRouteInventoryAuthority::new(route);
    let capture: Arc<ProviderCaptureCallback> = Arc::new(capture);
    let scan_capture = Arc::clone(&capture);
    let terminal_capture = Arc::clone(&capture);
    let terminal_inventory_capture = Arc::clone(&capture);
    let scan_authority = authority.clone();
    let terminal_authority = authority.clone();
    let terminal_inventory_authority = authority;
    let owns_source: Arc<SourcePredicate> = Arc::new(owns_source);
    let scan_owns_source = Arc::clone(&owns_source);
    let driver_owns_source = owns_source;
    let terminal_evidence = Arc::new(Mutex::new(
        None::<Result<CapturedRouteEvidence, SourceBackedRouteError>>,
    ));
    let scan_terminal_evidence = Arc::clone(&terminal_evidence);
    let source_terminal_evidence = Arc::clone(&terminal_evidence);
    let inventory_terminal_evidence = terminal_evidence;
    SourceBackedRouteDriver::new(
        move |sink| {
            reset_captured_terminal_evidence(&scan_terminal_evidence)?;
            let base_sources = sink
                .writer
                .base_manifest()
                .map(|manifest| {
                    manifest
                        .sources
                        .iter()
                        .filter(|certificate| scan_owns_source(certificate.observation().source()))
                        .cloned()
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default();
            let staged =
                capture_route_evidence(scan_capture.as_ref(), &scan_authority, &base_sources)?;
            validate_captured_route_ownership(&staged, scan_owns_source.as_ref())?;
            let plans = captured_source_plans(&staged, &base_sources);
            let omitted = captured_omitted_sources(&staged, &base_sources);
            let exact_route = omitted.is_empty()
                && plans
                    .values()
                    .all(|plan| matches!(plan, CapturedSourcePlan::Exact { .. }));

            if exact_route {
                for plan in plans.values() {
                    let CapturedSourcePlan::Exact { base, expected } = plan else {
                        return Err(captured_route_internal(
                            "captured exact route contained a replacement plan",
                        ));
                    };
                    stage_captured_exact_replay(sink, base, expected.clone())?;
                }
                sink.certify_complete_inventory(staged.inventory)
                    .map_err(route_coordinator_error)?;
                return Ok(());
            }

            let mut bridge = WriterCaptureSink {
                sink,
                plans: &plans,
                active: None,
                certificates: Vec::new(),
                append_proofs: HashMap::new(),
                inventory: None,
            };
            scan_capture(&mut bridge)?;
            if bridge.active.is_some() {
                return Err(captured_route_internal(
                    "provider capture ended with an uncertified active source",
                ));
            }
            let current = finish_captured_route_evidence(
                bridge.certificates,
                bridge.append_proofs,
                bridge.inventory,
                &scan_authority,
            )?;
            if current.certificates != staged.certificates
                || current.append_proofs != staged.append_proofs
                || current.inventory != staged.inventory
            {
                return Err(captured_route_changed(
                    "provider capture changed between planning and staging",
                ));
            }
            bridge
                .sink
                .certify_complete_inventory(current.inventory.clone())
                .map_err(route_coordinator_error)?;
            for base in omitted {
                let deletion = CertifiedSourceDeletion::from_inventory(
                    base.observation().source().clone(),
                    &current.inventory,
                )
                .map_err(captured_route_contract)?;
                bridge
                    .sink
                    .delete_source(deletion, current.inventory.clone())
                    .map_err(route_coordinator_error)?;
            }
            Ok(())
        },
        move |source| driver_owns_source(source),
        move |target| match target {
            SourceBackedRevalidationTarget::Source(expected) => {
                with_cached_captured_route_evidence(
                    &source_terminal_evidence,
                    terminal_capture.as_ref(),
                    &terminal_authority,
                    |evidence| {
                        evidence
                            .certificates_by_identity
                            .get(&expected.observation().source().identity().digest())
                            == Some(expected)
                    },
                )
                .unwrap_or(false)
            }
            SourceBackedRevalidationTarget::Deletion(deletion) => {
                with_cached_captured_route_evidence(
                    &source_terminal_evidence,
                    terminal_capture.as_ref(),
                    &terminal_authority,
                    |evidence| captured_deletion_verifies(deletion, evidence),
                )
                .unwrap_or(false)
            }
        },
        hydrate,
    )
    .with_complete_inventory_revalidation(move |expected| {
        with_cached_captured_route_evidence(
            &inventory_terminal_evidence,
            terminal_inventory_capture.as_ref(),
            &terminal_inventory_authority,
            |evidence| evidence.inventory == *expected,
        )
        .unwrap_or(false)
    })
}

fn capture_route_evidence(
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
        evidence.inventory,
        authority,
    )
}

fn finish_captured_route_evidence(
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

fn validate_captured_route_ownership(
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

fn captured_source_plans(
    evidence: &CapturedRouteEvidence,
    base_sources: &[CertifiedSource],
) -> HashMap<[u8; 32], CapturedSourcePlan> {
    let base_sources_by_identity = base_sources
        .iter()
        .map(|base| (base.observation().source().identity().digest(), base))
        .collect::<HashMap<_, _>>();
    evidence
        .certificates
        .iter()
        .map(|expected| {
            let source = expected.observation().source();
            let identity = source.identity().digest();
            let base = base_sources_by_identity
                .get(&identity)
                .copied()
                .filter(|base| base.observation().source().exact_descriptor_eq(source));
            let plan = if let Some(base) =
                base.filter(|base| *base == expected && base.frontier().is_some())
            {
                CapturedSourcePlan::Exact {
                    base: base.clone(),
                    expected: expected.clone(),
                }
            } else if let Some(proof) = evidence.append_proofs.get(&identity) {
                CapturedSourcePlan::Append {
                    proof: proof.clone(),
                }
            } else {
                CapturedSourcePlan::Replace {
                    base: base.cloned(),
                    expected: expected.clone(),
                }
            };
            (identity, plan)
        })
        .collect()
}

fn captured_omitted_sources(
    evidence: &CapturedRouteEvidence,
    base_sources: &[CertifiedSource],
) -> Vec<CertifiedSource> {
    base_sources
        .iter()
        .filter(|base| {
            evidence
                .certificates_by_identity
                .get(&base.observation().source().identity().digest())
                .is_none_or(|current| {
                    !current
                        .observation()
                        .source()
                        .exact_descriptor_eq(base.observation().source())
                })
        })
        .cloned()
        .collect()
}

fn stage_captured_exact_replay(
    sink: &mut SourceBackedGenerationSink<'_>,
    base: &CertifiedSource,
    current: CertifiedSource,
) -> SourceBackedRouteResult<()> {
    let frontier = base.frontier().ok_or_else(|| {
        captured_route_internal("captured exact replay base has no replay frontier")
    })?;
    sink.begin_source_append(current.observation().source().clone())
        .map_err(route_coordinator_error)?;
    let append = CertifiedSourceAppend::certify(
        base,
        current,
        frontier.certified_prefix_bytes(),
        *frontier.certified_prefix_digest(),
    )
    .map_err(captured_route_contract)?;
    sink.certify_source_append(append)
        .map_err(route_coordinator_error)
}

fn captured_replay_certificate(
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

fn reset_captured_terminal_evidence(
    evidence: &Mutex<Option<Result<CapturedRouteEvidence, SourceBackedRouteError>>>,
) -> SourceBackedRouteResult<()> {
    let mut evidence = evidence
        .lock()
        .map_err(|_| captured_route_internal("captured route evidence lock was poisoned"))?;
    *evidence = None;
    Ok(())
}

fn with_cached_captured_route_evidence<T>(
    cached: &Mutex<Option<Result<CapturedRouteEvidence, SourceBackedRouteError>>>,
    capture: &ProviderCaptureCallback,
    authority: &CapturedRouteInventoryAuthority,
    evaluate: impl FnOnce(&CapturedRouteEvidence) -> T,
) -> Option<T> {
    let mut cached = cached.lock().ok()?;
    if cached.is_none() {
        *cached = Some(capture_route_evidence(capture, authority, &[]));
    }
    cached.as_ref()?.as_ref().ok().map(evaluate)
}

fn captured_deletion_verifies(
    deletion: &CertifiedSourceDeletion,
    evidence: &CapturedRouteEvidence,
) -> bool {
    let inventory = &evidence.inventory;
    deletion.source().provider() == inventory.observation().provider()
        && deletion.inventory() == inventory.observation()
        && deletion.discovery_revision() == inventory.discovery_revision()
        && deletion.inventory_digest() == inventory.inventory_digest()
        && deletion.observed_sources() == inventory.observed_sources() as u64
        && !evidence
            .certificates_by_identity
            .contains_key(&deletion.source().identity().digest())
}

fn captured_route_contract(error: impl fmt::Display) -> SourceBackedRouteError {
    SourceBackedRouteError::new(SourceBackedRouteErrorKind::Internal, error.to_string())
}

fn captured_route_internal(detail: impl Into<String>) -> SourceBackedRouteError {
    SourceBackedRouteError::new(SourceBackedRouteErrorKind::Internal, detail)
}

fn captured_route_changed(detail: impl Into<String>) -> SourceBackedRouteError {
    SourceBackedRouteError::new(SourceBackedRouteErrorKind::SourceChanged, detail)
}

type ScanCallback = dyn for<'writer> Fn(&mut SourceBackedGenerationSink<'writer>) -> SourceBackedRouteResult<()>
    + Send
    + Sync;
type SourcePredicate = dyn Fn(&SourceKey) -> bool + Send + Sync;
type RevalidationCallback =
    dyn for<'a> Fn(SourceBackedRevalidationTarget<'a>) -> bool + Send + Sync;
type CompleteInventoryRevalidationCallback =
    dyn Fn(&CertifiedSourceInventory) -> bool + Send + Sync;
type HydrationCallback = dyn Fn(&EventHydrationRequest) -> Result<HydratedProviderRecord, HydrationFailure>
    + Send
    + Sync;
type BatchHydrationCallback =
    dyn Fn(&BatchHydrationRequest) -> Result<BatchHydrationResult, HydrationFailure> + Send + Sync;

/// Closure bundle at the coordinator boundary. This deliberately does not
/// pretend provider scanners share a provider-local trait.
#[derive(Clone)]
pub struct SourceBackedRouteDriver {
    pub(super) scan: Arc<ScanCallback>,
    pub(super) owns_source: Arc<SourcePredicate>,
    pub(super) revalidate: Arc<RevalidationCallback>,
    pub(super) revalidate_complete_inventory: Option<Arc<CompleteInventoryRevalidationCallback>>,
    pub(super) hydrate: Arc<HydrationCallback>,
    pub(super) hydrate_batch: Option<Arc<BatchHydrationCallback>>,
}

impl fmt::Debug for SourceBackedRouteDriver {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("SourceBackedRouteDriver")
    }
}

impl SourceBackedRouteDriver {
    pub fn new(
        scan: impl for<'writer> Fn(&mut SourceBackedGenerationSink<'writer>) -> SourceBackedRouteResult<()>
            + Send
            + Sync
            + 'static,
        owns_source: impl Fn(&SourceKey) -> bool + Send + Sync + 'static,
        revalidate: impl for<'a> Fn(SourceBackedRevalidationTarget<'a>) -> bool + Send + Sync + 'static,
        hydrate: impl Fn(&EventHydrationRequest) -> Result<HydratedProviderRecord, HydrationFailure>
            + Send
            + Sync
            + 'static,
    ) -> Self {
        Self {
            scan: Arc::new(scan),
            owns_source: Arc::new(owns_source),
            revalidate: Arc::new(revalidate),
            revalidate_complete_inventory: None,
            hydrate: Arc::new(hydrate),
            hydrate_batch: None,
        }
    }

    pub fn with_complete_inventory_revalidation(
        mut self,
        revalidate: impl Fn(&CertifiedSourceInventory) -> bool + Send + Sync + 'static,
    ) -> Self {
        self.revalidate_complete_inventory = Some(Arc::new(revalidate));
        self
    }

    /// Installs a provider-native ordered batch reader without changing the
    /// mechanically compatible event hydration constructor.
    pub fn with_batch_hydration(
        mut self,
        hydrate_batch: impl Fn(&BatchHydrationRequest) -> Result<BatchHydrationResult, HydrationFailure>
            + Send
            + Sync
            + 'static,
    ) -> Self {
        self.hydrate_batch = Some(Arc::new(hydrate_batch));
        self
    }

    pub(super) fn hydrate_batch(
        &self,
        request: &BatchHydrationRequest,
    ) -> Result<BatchHydrationResult, HydrationFailure> {
        if let Some(hydrate_batch) = &self.hydrate_batch {
            return hydrate_batch(request);
        }
        let records = request
            .events()
            .iter()
            .map(|event| (self.hydrate)(event))
            .collect::<Result<Vec<_>, _>>()?;
        BatchHydrationResult::new(records).map_err(|error| {
            hydration_failure(
                HydrationFailureKind::InvalidLocator,
                format!("invalid default route batch result: {error}"),
            )
        })
    }
}

#[derive(Debug, Clone)]
pub struct SourceBackedRoute {
    pub(super) metadata: SourceBackedRouteMetadata,
    pub(super) driver: Option<SourceBackedRouteDriver>,
}

impl SourceBackedRoute {
    pub fn automatic(
        source: ProviderSource,
        selector_authority: SourceBackedSelectorAuthority,
        driver: SourceBackedRouteDriver,
    ) -> SourceBackedCoordinatorResult<Self> {
        let known = validate_executable_route(
            &source,
            SourceBackedRouteSelection::Automatic,
            selector_authority,
        )?;
        Ok(Self {
            metadata: SourceBackedRouteMetadata {
                source,
                certified_source_format: known.certified_source_format,
                selection: Some(SourceBackedRouteSelection::Automatic),
                selector_authority,
                unsupported_reason: None,
            },
            driver: Some(driver),
        })
    }

    pub fn explicit_manual(
        source: ProviderSource,
        selector_authority: SourceBackedSelectorAuthority,
        driver: SourceBackedRouteDriver,
    ) -> SourceBackedCoordinatorResult<Self> {
        let known = validate_executable_route(
            &source,
            SourceBackedRouteSelection::ExplicitManual,
            selector_authority,
        )?;
        Ok(Self {
            metadata: SourceBackedRouteMetadata {
                source,
                certified_source_format: known.certified_source_format,
                selection: Some(SourceBackedRouteSelection::ExplicitManual),
                selector_authority,
                unsupported_reason: None,
            },
            driver: Some(driver),
        })
    }

    pub fn unsupported(source: ProviderSource, reason: impl Into<String>) -> Self {
        let certified_source_format = landed_format_route(source.provider, source.source_format)
            .map_or(source.source_format, |route| route.certified_source_format);
        Self {
            metadata: SourceBackedRouteMetadata {
                source,
                certified_source_format,
                selection: None,
                selector_authority: SourceBackedSelectorAuthority::ExplicitPath,
                unsupported_reason: Some(reason.into()),
            },
            driver: None,
        }
    }

    pub fn metadata(&self) -> &SourceBackedRouteMetadata {
        &self.metadata
    }
}

#[derive(Debug, Clone, Default)]
pub struct SourceBackedProviderRegistry {
    pub(super) routes: Vec<SourceBackedRoute>,
}

impl SourceBackedProviderRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register(&mut self, route: SourceBackedRoute) {
        self.routes.push(route);
    }

    pub fn routes(&self) -> impl ExactSizeIterator<Item = &SourceBackedRouteMetadata> {
        self.routes.iter().map(SourceBackedRoute::metadata)
    }

    pub fn resolver_registry(&self) -> SourceBackedResolverRegistry {
        SourceBackedResolverRegistry {
            routes: self.routes.clone(),
        }
    }

    pub fn executable_route_count(&self) -> usize {
        self.routes
            .iter()
            .filter(|route| route.driver.is_some())
            .count()
    }

    pub fn unsupported_route_count(&self) -> usize {
        self.routes
            .iter()
            .filter(|route| route.driver.is_none())
            .count()
    }
}
