use super::*;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct NanoClawCertifiedReplayCheckpoint {
    physical_fingerprint: [u8; 32],
    logical_fingerprint: [u8; 32],
}

pub(super) struct NanoClawPreparedAuthority {
    pub(super) frontier: NanoClawReplayFrontier,
    pub(super) projection: Option<NanoClawPreparedProjection>,
}

#[derive(Clone)]
pub(super) struct NanoClawReplayFrontier {
    pub(super) snapshot: super::super::super::project::NanoClawProjectSnapshot,
    pub(super) physical_fingerprint: [u8; 32],
    pub(super) logical_fingerprint: [u8; 32],
}

impl<B> NanoClawDocumentTreeAdapter<B> {
    pub fn new_with_base_sources(
        data_root: &Path,
        path: PathBuf,
        catalog_lineage: [u8; 32],
        base_sources: &[CertifiedSource],
    ) -> NanoClawSourceBackedResult<Self> {
        Self::new_with_base_sources_scoped(
            data_root,
            path,
            catalog_lineage,
            SourceAnchorScope::Unqualified,
            base_sources,
        )
    }

    pub fn new_with_base_sources_scoped(
        data_root: &Path,
        path: PathBuf,
        catalog_lineage: [u8; 32],
        source_anchor_scope: SourceAnchorScope,
        base_sources: &[CertifiedSource],
    ) -> NanoClawSourceBackedResult<Self> {
        let source = nanoclaw_source_key_scoped(catalog_lineage, source_anchor_scope)?;
        let certified_checkpoint =
            NanoClawCertifiedReplayCheckpoint::from_base_sources(&source, base_sources)?;
        Ok(Self {
            data_root: data_root.to_path_buf(),
            path,
            source,
            certified_checkpoint,
            replay_frontier: Arc::default(),
            _binding: std::marker::PhantomData,
        })
    }

    pub(super) fn prepare_authority(&self) -> SourceBackedRouteResult<NanoClawPreparedAuthority> {
        let project = NanoClawSourceBackedProject::open(&self.data_root, &self.path)
            .map_err(nanoclaw_route_project_open_error)?;
        self.prepare_open_project(project)
    }

    fn prepare_open_project(
        &self,
        mut project: NanoClawSourceBackedProject,
    ) -> SourceBackedRouteResult<NanoClawPreparedAuthority> {
        let projection = prepare_nanoclaw_project(&self.data_root, &mut project, &self.source)?;
        let snapshot = project.snapshot().clone();
        Ok(NanoClawPreparedAuthority {
            frontier: NanoClawReplayFrontier {
                physical_fingerprint: snapshot.physical_fingerprint(),
                logical_fingerprint: projection.logical_fingerprint,
                snapshot,
            },
            projection: Some(projection),
        })
    }

    pub(super) fn prepare_certified_checkpoint(
        &self,
        checkpoint: NanoClawCertifiedReplayCheckpoint,
    ) -> SourceBackedRouteResult<NanoClawPreparedAuthority> {
        let mut project = NanoClawSourceBackedProject::open(&self.data_root, &self.path)
            .map_err(nanoclaw_route_project_open_error)?;
        let snapshot = project.snapshot().clone();
        let frontier = NanoClawReplayFrontier {
            physical_fingerprint: snapshot.physical_fingerprint(),
            logical_fingerprint: checkpoint.logical_fingerprint,
            snapshot,
        };
        if frontier.physical_fingerprint != checkpoint.physical_fingerprint {
            return self.prepare_open_project(project);
        }
        project.finish().map_err(nanoclaw_route_capture_error)?;
        Ok(NanoClawPreparedAuthority {
            frontier,
            projection: None,
        })
    }
}

impl NanoClawCertifiedReplayCheckpoint {
    fn from_base_sources(
        source: &SourceKey,
        base_sources: &[CertifiedSource],
    ) -> NanoClawSourceBackedResult<Option<Self>> {
        let mut matching = base_sources.iter().filter(|base| {
            base.parser_revision() == NANOCLAW_SOURCE_BACKED_PARSER_REVISION
                && base.observation().source().exact_descriptor_eq(source)
        });
        let Some(base) = matching.next() else {
            return Ok(None);
        };
        if matching.next().is_some() {
            return Err(NanoClawSourceBackedError::DuplicateReplayCheckpoint);
        }
        base.validate_contract()?;
        let checkpoint =
            ctx_history_capture_runtime::decode_document_full_snapshot_checkpoint(base)
                .map_err(NanoClawSourceBackedError::InvalidReplayCheckpoint)?;
        Ok(Some(Self {
            physical_fingerprint: checkpoint.physical_fingerprint(),
            logical_fingerprint: checkpoint.logical_fingerprint(),
        }))
    }
}
