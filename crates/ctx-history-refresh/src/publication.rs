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
    #[cfg(any(test, feature = "test-support"))]
    VERIFIED_INDEX_OPEN_COUNT.with(|count| {
        if let Some(current) = count.get() {
            count.set(Some(current.saturating_add(1)));
        }
    });
    VerifiedIndex::open_pinned(index_root)
}

/// A verified Core generation that cannot be admitted at the public query
/// boundary because its source-refresh publication authority is absent or
/// invalid.
#[derive(Debug)]
pub enum GenerationQueryAuthorityError {
    UncertifiedEmpty {
        generation_id: String,
    },
    Invalid {
        generation_id: String,
        detail: String,
    },
}

impl GenerationQueryAuthorityError {
    pub const fn error_code(&self) -> &'static str {
        match self {
            Self::UncertifiedEmpty { .. } => "source_unavailable",
            Self::Invalid { .. } => "publication_authority_invalid",
        }
    }

    pub const fn retryable(&self) -> bool {
        matches!(self, Self::UncertifiedEmpty { .. })
    }

    const fn is_uncertified_empty(&self) -> bool {
        matches!(self, Self::UncertifiedEmpty { .. })
    }
}

impl fmt::Display for GenerationQueryAuthorityError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UncertifiedEmpty { generation_id } => write!(
                formatter,
                "Core generation {generation_id} is empty without certified zero-source publication authority"
            ),
            Self::Invalid {
                generation_id,
                detail,
            } => write!(
                formatter,
                "Core generation {generation_id} has invalid source-refresh publication authority: {detail}"
            ),
        }
    }
}

impl std::error::Error for GenerationQueryAuthorityError {}

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

/// Applies the one generation-bound publication-authority check shared by all
/// public Core query openers. Physical verification alone is insufficient for
/// an empty generation because absence must be certified by refresh metadata.
pub fn verify_generation_query_authority(
    index: &VerifiedIndex,
) -> std::result::Result<(), GenerationQueryAuthorityError> {
    let generation_id = index.generation_id().to_owned();
    match verify_generation_query_readiness(index) {
        Ok(GenerationQueryReadiness::Ready) => Ok(()),
        Ok(GenerationQueryReadiness::Uncertified) => {
            Err(GenerationQueryAuthorityError::UncertifiedEmpty { generation_id })
        }
        Err(error) => Err(GenerationQueryAuthorityError::Invalid {
            generation_id,
            detail: format!("{error:#}"),
        }),
    }
}

/// Evaluates query readiness from the committed generation and its opaque
/// refresh metadata, independently of any later mutable refresh attempt.
pub fn verified_generation_is_query_ready(index: &VerifiedIndex) -> Result<bool> {
    match verify_generation_query_authority(index) {
        Ok(()) => Ok(true),
        Err(error) if error.is_uncertified_empty() => Ok(false),
        Err(error) => Err(anyhow::Error::new(error))
            .context("decode Core source-refresh publication authority"),
    }
}

fn open_retained_verified_index(
    index_root: &Path,
    generation_id: &str,
) -> std::result::Result<VerifiedIndex, IndexError> {
    #[cfg(any(test, feature = "test-support"))]
    VERIFIED_INDEX_OPEN_COUNT.with(|count| {
        if let Some(current) = count.get() {
            count.set(Some(current.saturating_add(1)));
        }
    });
    VerifiedIndex::open_pinned_generation(index_root, generation_id)
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
    Verified(VerifiedIndex),
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
            PublishedGenerationOpen::Verified(index) => Some(index),
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
        Ok(index) => Ok(PublishedGenerationOpen::Verified(index)),
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
    // This transient result has no predecessor identity of its own. Give the
    // typed physical receipt a no-op predecessor solely to validate every
    // generation-bound fact through the one receipt validator.
    let receipt = SourceBackedRefreshReceipt::from_verified_publication(
        Some(publication.generation_id.clone()),
        publication.generation_id.clone(),
        publication,
    )?;
    receipt.validate(Some(verified))?;
    if publication.certified_source_count != receipt.current.source_count
        || publication.certified_source_bytes != receipt.current.certified_source_bytes
    {
        bail!("Core refresh publication facts do not match its exact verified generation");
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
                    // Callers admit only a publication/receipt that was
                    // already checked against its exact manifest above (or
                    // while decoding it). That check establishes whether a
                    // failed route was retained, so both cold and retained
                    // transient request failures count here.
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
    if job.get("request_state").and_then(Value::as_str) != Some("published") {
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
    let metadata = SourceBackedPublicationMetadata::decode(&verified)
        .context("load exact explicit relocation authority from Core publication metadata")?;
    let receipt = &metadata.receipt;
    receipt
        .published_explicit_source_catalog
        .as_ref()
        .map(|catalog| catalog.relocation_authority(old_path, &receipt.catalog_route_bindings))
        .transpose()
        .map(Option::flatten)
}

pub fn pin_published_generation(
    data_root: &Path,
    journal: &dyn RefreshJournal,
) -> Result<Option<PinnedSourceBackedGeneration>> {
    let Some(index) = open_published_generation(data_root, journal)? else {
        return Ok(None);
    };
    match verify_generation_query_authority(&index) {
        Ok(()) => {}
        Err(error) if error.is_uncertified_empty() => return Ok(None),
        Err(error) => return Err(anyhow::Error::new(error)),
    }
    Ok(Some(PinnedSourceBackedGeneration { index }))
}

pub fn pin_retained_generation(
    data_root: &Path,
    generation_id: &str,
) -> Result<PinnedSourceBackedGeneration> {
    let index_root = source_backed_index_root(data_root);
    let index = open_retained_verified_index(&index_root, generation_id).with_context(|| {
        format!(
            "open retained Core generation {generation_id} from {}",
            index_root.display()
        )
    })?;
    verify_generation_query_authority(&index).map_err(anyhow::Error::new)?;
    Ok(PinnedSourceBackedGeneration { index })
}

pub fn pin_active_verified_generation(
    data_root: &Path,
    journal: &dyn RefreshJournal,
) -> Result<PinnedSourceBackedGeneration> {
    let index = open_published_generation(data_root, journal)
        .context("source_unavailable: verify active Core generation")?
        .ok_or_else(|| anyhow::Error::new(MissingActiveGeneration))?;
    verify_generation_query_authority(&index).map_err(anyhow::Error::new)?;
    Ok(PinnedSourceBackedGeneration { index })
}
