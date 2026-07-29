use super::super::*;
use super::observation::{
    captured_replay_certificate, captured_route_changed, captured_route_contract,
    captured_route_internal, CapturedRouteEvidence, ProviderCaptureSink,
};

pub(super) struct WriterCaptureSink<'sink, 'writer> {
    pub(super) sink: &'sink mut SourceBackedGenerationSink<'writer>,
    pub(super) plans: &'sink HashMap<[u8; 32], CapturedSourcePlan>,
    pub(super) active: Option<CapturedSourceMode>,
    pub(super) certificates: Vec<CertifiedSource>,
    pub(super) append_proofs: HashMap<[u8; 32], CertifiedSourceAppend>,
    pub(super) inventory: Option<CertifiedSourceInventory>,
}

pub(super) enum CapturedSourcePlan {
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

pub(super) enum CapturedSourceMode {
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

pub(super) fn captured_source_plans(
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

pub(super) fn captured_omitted_sources(
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

pub(super) fn stage_captured_exact_replay(
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
