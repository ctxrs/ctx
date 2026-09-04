use super::*;

#[cfg(test)]
pub(crate) mod observation {
    pub(crate) use ctx_history_refresh_execution::install_after_capture_scan_before_metadata_hook_for_test;
}

#[cfg(any(test, feature = "test-support"))]
thread_local! {
    static VERIFIED_INDEX_OPEN_COUNT: Cell<Option<usize>> = const { Cell::new(None) };
}

pub fn open_verified_index(index_root: &Path) -> std::result::Result<VerifiedIndex, IndexError> {
    open_verified_index_with_peer(index_root, false)
}

fn open_verified_index_with_peer(
    index_root: &Path,
    retain_peer: bool,
) -> std::result::Result<VerifiedIndex, IndexError> {
    #[cfg(any(test, feature = "test-support"))]
    VERIFIED_INDEX_OPEN_COUNT.with(|count| {
        if let Some(current) = count.get() {
            count.set(Some(current.saturating_add(1)));
        }
    });
    if retain_peer {
        VerifiedIndex::open_pinned_with_retained_peer(index_root)
    } else {
        VerifiedIndex::open_pinned(index_root)
    }
}

/// The refresh authority has no active verified Core generation to pin.
///
/// This is distinct from failures while reading or validating publication
/// state so application boundaries can classify only the actual absence case.
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub struct MissingActiveGeneration;

impl fmt::Display for MissingActiveGeneration {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("source_unavailable: active verified Core generation is missing")
    }
}

impl std::error::Error for MissingActiveGeneration {}

/// Evaluates query readiness from the committed generation and its opaque
/// refresh metadata, independently of any later mutable refresh attempt.
fn open_retained_verified_index(
    index_root: &Path,
    generation_id: &str,
    retain_peer: bool,
) -> std::result::Result<VerifiedIndex, IndexError> {
    #[cfg(any(test, feature = "test-support"))]
    VERIFIED_INDEX_OPEN_COUNT.with(|count| {
        if let Some(current) = count.get() {
            count.set(Some(current.saturating_add(1)));
        }
    });
    if retain_peer {
        VerifiedIndex::open_pinned_generation_with_retained_peer(index_root, generation_id)
    } else {
        VerifiedIndex::open_pinned_generation(index_root, generation_id)
    }
}

#[cfg(any(test, feature = "test-support"))]
pub fn count_verified_index_opens<T>(operation: impl FnOnce() -> T) -> (T, usize) {
    VERIFIED_INDEX_OPEN_COUNT.with(|count| {
        let previous = count.replace(Some(0));
        assert!(
            previous.is_none(),
            "verified-index open counters must not be nested"
        );
        let output = operation();
        let observed = count.replace(None).unwrap_or(0);
        (output, observed)
    })
}

pub(super) fn published_generation_id(
    data_root: &Path,
    journal: &dyn RefreshJournal,
) -> Result<Option<String>> {
    Ok(
        open_published_generation(data_root, journal)?
            .map(|index| index.generation_id().to_owned()),
    )
}

pub(super) enum PublishedGenerationOpen {
    Missing,
    RebuildRequired,
    Verified(Box<VerifiedIndex>),
}

pub(super) fn prepare_generation_control_state(data_root: &Path) -> Result<()> {
    let index_root = source_backed_index_root(data_root);
    match std::fs::symlink_metadata(&index_root) {
        Ok(_) => ctx_history_index::ensure_generation_control_state_private(&index_root)
            .context("protect source-backed generation control state"),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}

pub(super) fn open_published_generation(
    data_root: &Path,
    journal: &dyn RefreshJournal,
) -> Result<Option<VerifiedIndex>> {
    Ok(
        match open_published_generation_for_recovery(data_root, journal)? {
            PublishedGenerationOpen::Missing | PublishedGenerationOpen::RebuildRequired => None,
            PublishedGenerationOpen::Verified(index) => Some(*index),
        },
    )
}

pub(super) fn open_published_generation_for_recovery(
    data_root: &Path,
    journal: &dyn RefreshJournal,
) -> Result<PublishedGenerationOpen> {
    let index_root = source_backed_index_root(data_root);
    if !index_root.is_dir() {
        if let Some(generation_id) = published_generation_receipt(data_root, journal)? {
            bail!(
                "verified Core generation {generation_id} is missing from {}",
                index_root.display()
            );
        }
        return Ok(PublishedGenerationOpen::Missing);
    }
    match open_verified_index(&index_root) {
        Ok(index) => Ok(PublishedGenerationOpen::Verified(Box::new(index))),
        Err(IndexError::MissingActiveGenerationPointer) => {
            if let Some(generation_id) = published_generation_receipt(data_root, journal)? {
                bail!(
                    "verified Core generation {generation_id} is missing from {}",
                    index_root.display()
                );
            }
            Ok(PublishedGenerationOpen::Missing)
        }
        Err(error) if generation_incompatibility_requires_recovery_rebuild(&error) => {
            Ok(PublishedGenerationOpen::RebuildRequired)
        }
        Err(error) => {
            Err(error).with_context(|| format!("open verified Core index {}", index_root.display()))
        }
    }
}

pub(super) fn verify_source_backed_publication(
    publication: &SourceBackedRefreshPublication,
    verified: &VerifiedIndex,
) -> Result<()> {
    if verified.generation_id() != publication.generation_id {
        bail!(
            "source-backed refresh returned generation {}, but its verified pin carries {}",
            publication.generation_id,
            verified.generation_id()
        );
    }
    let manifest = verified.manifest();
    let verified_current = SourceBackedRefreshCurrent::from_sources(
        &manifest.sources,
        publication.current.removed_source_count,
    )?;
    if verified_current != publication.current
        || publication.certified_source_count != verified_current.source_count
        || publication.certified_source_bytes != verified_current.certified_source_bytes
        || manifest.indexed_documents != verified_current.indexed_documents
    {
        bail!("Core refresh publication facts do not match its exact verified generation");
    }
    let route_rejected_record_total =
        publication
            .route_results
            .iter()
            .try_fold(0_u64, |total, result| {
                total
                    .checked_add(result.rejected_record_total)
                    .ok_or_else(|| {
                        anyhow!("Core refresh publication route rejected-record total overflow")
                    })
            })?;
    if route_rejected_record_total > verified_current.rejected_records {
        bail!("Core refresh publication route rejections exceed its exact verified generation");
    }
    let witness_lineages = publication
        .published_explicit_source_catalog
        .as_ref()
        .map(ExplicitSourceCatalogAuthority::route_lineages)
        .unwrap_or_default();
    if publication.catalog_route_bindings.iter().any(|binding| {
        if witness_lineages.contains(&binding.catalog_lineage) {
            return SourceRouteIdentity::from_sha256(binding.route_identity.clone())
                .ok()
                .is_none_or(|route| manifest.source_route(&route).is_none());
        }
        !publication.route_results.iter().any(|result| {
            result.route_identity == binding.route_identity
                && matches!(
                    result.outcome,
                    SourceBackedRefreshRouteOutcome::Failed { .. }
                )
        })
    }) {
        bail!("Core refresh publication catalog binding has no generation-bound authority or failed request evidence");
    }
    Ok(())
}

pub fn explicit_catalog_request_is_accounted_for(
    requested: &ExplicitSourceCatalogAuthority,
    published: Option<&ExplicitSourceCatalogAuthority>,
    bindings: &[ExplicitSourceCatalogRouteBinding],
    route_results: &[SourceBackedRefreshRouteResult],
) -> bool {
    if published.is_some_and(|catalog| catalog.carries_request(requested)) {
        return true;
    }
    let lineages = requested.route_lineages();
    !lineages.is_empty()
        && lineages.iter().all(|lineage| {
            bindings
                .iter()
                .find(|binding| binding.catalog_lineage == *lineage)
                .is_some_and(|binding| {
                    route_results.iter().any(|result| {
                        result.route_identity == binding.route_identity
                            && matches!(
                                result.outcome,
                                SourceBackedRefreshRouteOutcome::Failed { .. }
                            )
                    })
                })
        })
}

fn published_generation_receipt(
    data_root: &Path,
    journal: &dyn RefreshJournal,
) -> Result<Option<String>> {
    let Some(job) = journal.load(data_root)? else {
        return Ok(None);
    };
    if job.get("request_state").and_then(Value::as_str)
        != Some(RefreshRequestState::Published.as_str())
    {
        return Ok(None);
    }
    Ok(job
        .get("published_generation")
        .and_then(Value::as_str)
        .filter(|generation_id| !generation_id.is_empty())
        .map(str::to_owned))
}

pub(super) fn retained_generation_hint(
    data_root: &Path,
    journal: &dyn RefreshJournal,
) -> Result<Option<String>> {
    let receipt_generation = journal.load(data_root)?.and_then(|job| {
        job.get("published_generation")
            .and_then(Value::as_str)
            .filter(|generation_id| !generation_id.is_empty())
            .map(str::to_owned)
    });
    let index_root = source_backed_index_root(data_root);
    if !index_root.is_dir() {
        if let Some(generation_id) = receipt_generation {
            bail!(
                "retained lexical generation hint {generation_id} has no active generation at {}",
                index_root.display()
            );
        }
        return Ok(None);
    }
    match VerifiedIndex::active_generation_id(&index_root) {
        Ok(Some(generation_id)) => Ok(Some(generation_id)),
        Ok(None) => {
            let Some(generation_id) = receipt_generation else {
                return Ok(None);
            };
            bail!(
                "retained lexical generation hint {generation_id} has no active generation at {}",
                index_root.display()
            )
        }
        Err(error) if generation_incompatibility_requires_recovery_rebuild(&error) => {
            Ok(receipt_generation)
        }
        Err(error) => Err(error.into()),
    }
}

pub struct PinnedSourceBackedGeneration {
    index: VerifiedIndex,
}

impl PinnedSourceBackedGeneration {
    #[allow(dead_code)] // Available to callers that report the selected pin.
    pub fn generation_id(&self) -> &str {
        self.index.generation_id()
    }

    pub fn semantic_eligible_event_count(&self) -> Result<u64> {
        self.index
            .semantic_eligible_event_count()
            .map_err(anyhow::Error::new)
    }

    pub fn into_index(self) -> VerifiedIndex {
        self.index
    }

    pub fn verified_index(&self) -> &VerifiedIndex {
        &self.index
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn from_index(index: VerifiedIndex) -> Self {
        Self { index }
    }
}

pub fn published_refresh_receipt(
    response: &Value,
    pin: &PinnedSourceBackedGeneration,
) -> Result<SourceBackedRefreshReceipt> {
    published_refresh_receipt_for_index(response, &pin.index)
}

pub fn published_explicit_source_relocation_authority(
    data_root: &Path,
    old_path: &Path,
    journal: &dyn RefreshJournal,
) -> Result<Option<ExplicitSourceRelocationAuthority>> {
    let verified = open_published_generation(data_root, journal)?
        .ok_or_else(|| anyhow!("explicit relocation requires an active Core publication"))?;
    let state = SourceBackedGenerationState::decode_from_verified_index(&verified)
        .context("load exact explicit relocation authority from Core generation state")?;
    state
        .applied_explicit_source_catalog()
        .map(|catalog| catalog.relocation_authority(old_path, state.catalog_route_bindings()))
        .transpose()
        .map(Option::flatten)
}

pub fn pin_published_generation(data_root: &Path) -> Result<Option<PinnedSourceBackedGeneration>> {
    pin_published_generation_with_peer(data_root, false)
}

/// Pins the published target and its optional pointer peer in one verified open.
pub fn pin_published_generation_with_retained_peer(
    data_root: &Path,
) -> Result<Option<PinnedSourceBackedGeneration>> {
    pin_published_generation_with_peer(data_root, true)
}

fn pin_published_generation_with_peer(
    data_root: &Path,
    retain_peer: bool,
) -> Result<Option<PinnedSourceBackedGeneration>> {
    let index_root = source_backed_index_root(data_root);
    match open_verified_index_with_peer(&index_root, retain_peer) {
        Ok(index) => Ok(Some(PinnedSourceBackedGeneration { index })),
        Err(IndexError::MissingActiveGenerationPointer) => Ok(None),
        Err(error) => Err(error).context("open active verified Core generation"),
    }
}

pub fn pin_retained_generation(
    data_root: &Path,
    generation_id: &str,
) -> Result<PinnedSourceBackedGeneration> {
    pin_retained_generation_with_peer(data_root, generation_id, false)
}

/// Pins the exact retained target and its optional pointer peer in one verified open.
pub fn pin_retained_generation_with_retained_peer(
    data_root: &Path,
    generation_id: &str,
) -> Result<PinnedSourceBackedGeneration> {
    pin_retained_generation_with_peer(data_root, generation_id, true)
}

fn pin_retained_generation_with_peer(
    data_root: &Path,
    generation_id: &str,
    retain_peer: bool,
) -> Result<PinnedSourceBackedGeneration> {
    let index_root = source_backed_index_root(data_root);
    let index = open_retained_verified_index(&index_root, generation_id, retain_peer)
        .with_context(|| {
            format!(
                "open retained Core generation {generation_id} from {}",
                index_root.display()
            )
        })?;
    Ok(PinnedSourceBackedGeneration { index })
}

pub fn pin_active_verified_generation(data_root: &Path) -> Result<PinnedSourceBackedGeneration> {
    pin_active_verified_generation_with_peer(data_root, false)
}

/// Pins the active target and its optional pointer peer in one verified open.
pub fn pin_active_verified_generation_with_retained_peer(
    data_root: &Path,
) -> Result<PinnedSourceBackedGeneration> {
    pin_active_verified_generation_with_peer(data_root, true)
}

fn pin_active_verified_generation_with_peer(
    data_root: &Path,
    retain_peer: bool,
) -> Result<PinnedSourceBackedGeneration> {
    let index_root = source_backed_index_root(data_root);
    let index = match open_verified_index_with_peer(&index_root, retain_peer) {
        Ok(index) => index,
        Err(IndexError::MissingActiveGenerationPointer) => {
            return Err(anyhow::Error::new(MissingActiveGeneration));
        }
        Err(error) => {
            return Err(error).with_context(|| {
                format!(
                    "source_unavailable: open verified Core index {}",
                    index_root.display()
                )
            });
        }
    };
    Ok(PinnedSourceBackedGeneration { index })
}
