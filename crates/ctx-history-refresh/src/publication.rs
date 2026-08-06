use super::*;

pub(crate) mod metadata;
pub(crate) mod observation;
pub(crate) use metadata::SOURCE_REFRESH_PUBLICATION_METADATA_VERSION;

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum SourceBackedZeroSourceAuthorityKind {
    CompleteEmptyInventory,
    ConfirmedDeletion,
}

impl SourceBackedZeroSourceAuthorityKind {
    const fn compact_code(self) -> char {
        match self {
            Self::CompleteEmptyInventory => 'e',
            Self::ConfirmedDeletion => 'd',
        }
    }

    fn from_compact_code(value: char) -> Result<Self> {
        match value {
            'e' => Ok(Self::CompleteEmptyInventory),
            'd' => Ok(Self::ConfirmedDeletion),
            _ => bail!("Core zero-source authority has an unknown disposition"),
        }
    }
}

/// One route-local proof that a zero-source generation is authoritative.
///
/// The generation binding is repeated on every in-memory entry so covered
/// continuations cannot accidentally carry predecessor authority into a new
/// publication without explicitly rebinding it at the terminal fence.
#[derive(Debug, Clone, Eq, PartialEq)]
pub struct SourceBackedZeroSourceAuthority {
    pub generation_id: String,
    pub route_identity: SourceRouteIdentity,
    pub kind: SourceBackedZeroSourceAuthorityKind,
}

impl SourceBackedZeroSourceAuthority {
    fn rebound_to(&self, generation_id: &str) -> Self {
        Self {
            generation_id: generation_id.to_owned(),
            route_identity: self.route_identity.clone(),
            kind: self.kind,
        }
    }
}

#[derive(Debug)]
pub(crate) struct ZeroSourcePublicationBlocked {
    detail: String,
}

impl ZeroSourcePublicationBlocked {
    pub(crate) fn new(detail: impl Into<String>) -> Self {
        Self {
            detail: detail.into(),
        }
    }
}

impl fmt::Display for ZeroSourcePublicationBlocked {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{TERMINAL_COVERAGE_ERROR_CODE}: {}", self.detail)
    }
}

impl std::error::Error for ZeroSourcePublicationBlocked {}

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

/// Evaluates query readiness from the committed generation and its opaque
/// refresh metadata, independently of any later mutable refresh attempt.
pub fn verified_generation_is_query_ready(index: &VerifiedIndex) -> Result<bool> {
    match index.publication_metadata() {
        Some(_) => {
            let metadata = SourceBackedPublicationMetadata::decode(index)
                .context("decode Core source-refresh publication authority")?;
            Ok(metadata.certifies_generation(index))
        }
        None => Ok(!index.manifest().sources.is_empty()),
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

pub fn nonzero_duration_micros(duration: StdDuration) -> u64 {
    u64::try_from(duration.as_micros())
        .unwrap_or(u64::MAX)
        .max(1)
}

/// Provider-neutral publication returned by the capture-owned refresh
/// executor after it atomically advances the source-backed generation.
#[derive(Clone)]
pub struct SourceBackedRefreshPublication {
    pub generation_id: String,
    /// Exact request-scoped explicit-source overlay incorporated into this publication.
    pub published_explicit_source_catalog: Option<ExplicitSourceCatalogAuthority>,
    pub unsupported_routes: usize,
    pub certified_source_count: usize,
    pub certified_source_bytes: u64,
    pub current: SourceBackedRefreshCurrent,
    pub timings: SourceBackedRefreshTimings,
    pub route_results: Vec<SourceBackedRefreshRouteResult>,
    /// Present only when the exact generation contains no certified sources.
    pub zero_source_authority: Vec<SourceBackedZeroSourceAuthority>,
    pub catalog_route_bindings: Vec<ExplicitSourceCatalogRouteBinding>,
    /// Exact Core pin returned by the metadata-aware publication primitive.
    /// Synthetic executor tests may leave this absent.
    pub verified_index: Option<Arc<VerifiedIndex>>,
}

/// Immutable request facts already certified by the exact predecessor of a
/// manual all-route continuation. These facts must join the request receipt
/// before Core advances its pointer so crash recovery sees the same result as
/// the live coordinator.
#[derive(Debug, Clone, Default)]
pub struct SourceBackedRefreshCoveredPublication {
    pub route_results: Vec<SourceBackedRefreshRouteResult>,
    pub zero_source_authority: Vec<SourceBackedZeroSourceAuthority>,
    pub removed_source_count: usize,
    pub timings: SourceBackedRefreshTimings,
}

impl SourceBackedRefreshCoveredPublication {
    pub(crate) fn apply_receipt(&self, publication: &mut SourceBackedRefreshPublication) {
        publication
            .route_results
            .extend(self.route_results.iter().cloned());
        publication
            .route_results
            .sort_by(|left, right| left.route_identity.cmp(&right.route_identity));
        publication.zero_source_authority.extend(
            self.zero_source_authority
                .iter()
                .map(|authority| authority.rebound_to(&publication.generation_id)),
        );
        publication
            .zero_source_authority
            .sort_by(|left, right| left.route_identity.cmp(&right.route_identity));
        publication.current.removed_source_count = publication
            .current
            .removed_source_count
            .saturating_add(self.removed_source_count);
    }

    pub(crate) fn apply_timings(&self, publication: &mut SourceBackedRefreshPublication) {
        publication.timings.discovery_us = publication
            .timings
            .discovery_us
            .saturating_add(self.timings.discovery_us);
        publication.timings.scan_stage_us = publication
            .timings
            .scan_stage_us
            .saturating_add(self.timings.scan_stage_us);
        publication.timings.commit_us = publication
            .timings
            .commit_us
            .saturating_add(self.timings.commit_us);
    }

    pub fn apply(&self, publication: &mut SourceBackedRefreshPublication) {
        self.apply_receipt(publication);
        self.apply_timings(publication);
    }
}

impl fmt::Debug for SourceBackedRefreshPublication {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SourceBackedRefreshPublication")
            .field("generation_id", &self.generation_id)
            .field("route_results", &self.route_results)
            .field("has_verified_index", &self.verified_index.is_some())
            .finish_non_exhaustive()
    }
}

pub fn source_backed_index_root(data_root: &Path) -> PathBuf {
    data_root.join(SEARCH_DIRECTORY).join(LEXICAL_DIRECTORY)
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

pub(super) fn open_published_generation(
    data_root: &Path,
    journal: &dyn RefreshJournal,
) -> Result<Option<VerifiedIndex>> {
    let index_root = source_backed_index_root(data_root);
    if !index_root.is_dir() {
        if let Some(generation_id) = published_generation_receipt(data_root, journal)? {
            bail!(
                "verified Core generation {generation_id} is missing from {}",
                index_root.display()
            );
        }
        return Ok(None);
    }
    match open_verified_index(&index_root) {
        Ok(index) => Ok(Some(index)),
        Err(IndexError::MissingActiveGenerationPointer) => {
            if let Some(generation_id) = published_generation_receipt(data_root, journal)? {
                bail!(
                    "verified Core generation {generation_id} is missing from {}",
                    index_root.display()
                );
            }
            Ok(None)
        }
        Err(error) if generation_incompatibility_requires_rebuild(&error) => Ok(None),
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
                    SourceBackedRefreshRouteOutcome::Failed {
                        carried_forward: false,
                        ..
                    }
                )
        })
    }) {
        bail!("Core refresh publication catalog binding has no generation-bound authority or cold request failure");
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
                                SourceBackedRefreshRouteOutcome::Failed {
                                    carried_forward: false,
                                    ..
                                }
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
        Err(error) if generation_incompatibility_requires_rebuild(&error) => Ok(receipt_generation),
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

pub fn published_refresh_receipt_for_index(
    response: &Value,
    verified_index: &VerifiedIndex,
) -> Result<SourceBackedRefreshReceipt> {
    let value = response
        .get("receipt")
        .and_then(Value::as_object)
        .ok_or_else(|| anyhow!("published daemon source refresh has no terminal receipt"))?;
    let previous_generation = optional_generation(value.get("previous_generation"))?;
    let published_generation = required_generation(
        value.get("published_generation"),
        "terminal receipt published generation",
    )?;
    let generation_changed = value
        .get("generation_changed")
        .and_then(Value::as_bool)
        .ok_or_else(|| {
            anyhow!("published daemon source refresh receipt has no generation_changed fact")
        })?;
    let published_explicit_source_catalog = value
        .get("published_explicit_source_catalog")
        .map(ExplicitSourceCatalogAuthority::from_json)
        .transpose()?;
    let current_value = value
        .get("current")
        .and_then(Value::as_object)
        .ok_or_else(|| {
            anyhow!("published daemon source refresh receipt has no current generation facts")
        })?;
    let current = SourceBackedRefreshCurrent {
        source_count: required_usize(current_value, "current_source_count")?,
        indexed_documents: required_u64(current_value, "current_indexed_documents")?,
        complete_records: required_u64(current_value, "current_complete_records")?,
        retained_records: required_u64(current_value, "current_retained_records")?,
        rejected_records: required_u64(current_value, "current_rejected_records")?,
        ignored_records: required_u64(current_value, "current_ignored_records")?,
        certified_source_bytes: required_u64(current_value, "current_certified_source_bytes")?,
        sources_with_rejections: required_usize(current_value, "current_sources_with_rejections")?,
        removed_source_count: required_usize(current_value, "removed_source_count")?,
    };
    let selected_route_total = required_usize(value, "selected_route_total")?;
    let successful_route_total = required_usize(value, "successful_route_total")?;
    let route_results = required_route_results(value.get("route_results"))?;
    let zero_source_authority =
        parse_zero_source_authority(value.get("zero_source_authority"), &route_results)?;
    let expected_catalog_lineages = published_explicit_source_catalog
        .as_ref()
        .map(ExplicitSourceCatalogAuthority::route_lineages)
        .unwrap_or_default();
    let catalog_route_bindings = required_catalog_route_bindings(
        value.get("catalog_route_bindings"),
        verified_index.manifest(),
        &route_results,
        &expected_catalog_lineages,
    )?;
    let actual_catalog_lineages = catalog_route_bindings
        .iter()
        .map(|binding| binding.catalog_lineage.clone())
        .collect::<BTreeSet<_>>();
    let derived_successful_route_total = route_results
        .iter()
        .filter(|result| result.outcome.is_success())
        .count();
    let derived_source_failure_total =
        route_results.iter().try_fold(0_usize, |total, result| {
            total
                .checked_add(result.source_failure_total)
                .ok_or_else(|| anyhow!("published daemon source-failure total overflow"))
        })?;
    let source_failure_diagnostic_total =
        route_results.iter().try_fold(0_usize, |total, result| {
            total
                .checked_add(result.source_failures.len())
                .ok_or_else(|| anyhow!("published daemon source-failure diagnostic total overflow"))
        })?;
    let derived_rejected_record_total = route_results.iter().try_fold(0_u64, |total, result| {
        total
            .checked_add(result.rejected_record_total)
            .ok_or_else(|| anyhow!("published daemon rejected-record total overflow"))
    })?;
    let rejection_diagnostic_total = route_results.iter().try_fold(0_u64, |total, result| {
        total
            .checked_add(result.rejection_diagnostics.len() as u64)
            .ok_or_else(|| anyhow!("published daemon rejection diagnostic total overflow"))
    })?;
    let source_failure_total = required_usize(value, "source_failure_total")?;
    let source_failures_omitted = required_usize(value, "source_failures_omitted")?;
    let rejected_record_total = required_u64(value, "rejected_record_total")?;
    let rejection_diagnostics_omitted = required_u64(value, "rejection_diagnostics_omitted")?;
    if selected_route_total != route_results.len()
        || successful_route_total != derived_successful_route_total
        || source_failure_total != derived_source_failure_total
        || source_failures_omitted
            != source_failure_total.saturating_sub(source_failure_diagnostic_total)
        || rejected_record_total != derived_rejected_record_total
        || rejected_record_total > current.rejected_records
        || rejection_diagnostics_omitted
            != rejected_record_total.saturating_sub(rejection_diagnostic_total)
        || !expected_catalog_lineages.is_subset(&actual_catalog_lineages)
    {
        bail!("published daemon source refresh has an invalid route-result partition");
    }
    validate_zero_source_authority(
        &published_generation,
        current.source_count,
        &route_results,
        &zero_source_authority,
        false,
    )?;

    let top_previous_generation = optional_generation(response.get("previous_generation"))?;
    let top_published_generation = required_generation(
        response.get("published_generation"),
        "published daemon source refresh generation",
    )?;
    let top_generation_changed = response
        .get("generation_changed")
        .and_then(Value::as_bool)
        .ok_or_else(|| anyhow!("published daemon source refresh has no generation_changed fact"))?;
    let identity_changed = previous_generation.as_deref() != Some(published_generation.as_str());
    let request_identity_changed =
        top_previous_generation.as_deref() != Some(top_published_generation.as_str());
    if published_generation != top_published_generation
        || generation_changed != identity_changed
        || top_generation_changed != request_identity_changed
    {
        bail!(
            "published daemon source refresh receipt has inconsistent publication identity facts"
        );
    }

    let manifest = verified_index.manifest();
    let verified_current =
        SourceBackedRefreshCurrent::from_sources(&manifest.sources, current.removed_source_count)?;
    if current != verified_current
        || current.source_count
            != required_usize_from_value(
                response.get("certified_source_count"),
                "certified_source_count",
            )?
        || current.certified_source_bytes
            != required_u64_from_value(
                response.get("certified_source_bytes"),
                "certified_source_bytes",
            )?
    {
        bail!(
            "published daemon source refresh receipt does not match the verified current generation"
        );
    }

    Ok(SourceBackedRefreshReceipt {
        previous_generation,
        published_generation,
        generation_changed,
        published_explicit_source_catalog,
        current,
        route_results,
        zero_source_authority,
        catalog_route_bindings,
    })
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
    let receipt = published_refresh_receipt_for_index(&metadata.response_value(), &verified)?;
    receipt
        .published_explicit_source_catalog
        .as_ref()
        .map(|catalog| catalog.relocation_authority(old_path, &receipt.catalog_route_bindings))
        .transpose()
        .map(Option::flatten)
}

pub(super) fn required_route_results(
    value: Option<&Value>,
) -> Result<Vec<SourceBackedRefreshRouteResult>> {
    let value = value
        .ok_or_else(|| anyhow!("published daemon source refresh receipt has no route_results"))?;
    let values = value.as_object().ok_or_else(|| {
        anyhow!("published daemon source refresh receipt route_results must be an object")
    })?;
    if values.len() > SOURCE_REFRESH_TERMINAL_ROUTE_LIMIT {
        bail!(
            "published daemon source refresh exceeds the bounded route-result limit of {SOURCE_REFRESH_TERMINAL_ROUTE_LIMIT}"
        );
    }
    values
        .iter()
        .map(|(route_identity, value)| {
            if !is_sha256_identity(route_identity) {
                bail!("published daemon source refresh route identity is invalid");
            }
            let fields = value.as_array().ok_or_else(|| {
                anyhow!("published daemon source refresh compact route result must be an array")
            })?;
            let (
                outcome,
                source_failure_total,
                source_failures,
                rejected_record_total,
                rejection_diagnostics,
            ) = match fields.first().and_then(Value::as_str) {
                Some("s") if fields.len() == 2 => {
                    let changed = fields[1].as_bool().ok_or_else(|| {
                        anyhow!("published daemon successful route result has no changed fact")
                    })?;
                    (
                        SourceBackedRefreshRouteOutcome::Succeeded { changed },
                        0,
                        Vec::new(),
                        0,
                        Vec::new(),
                    )
                }
                Some("s") if fields.len() == 6 => {
                    let changed = fields[1].as_bool().ok_or_else(|| {
                        anyhow!("published daemon successful route result has no changed fact")
                    })?;
                    let total =
                        required_usize_from_value(fields.get(2), "route source_failure_total")?;
                    let failures = required_route_source_failures(route_identity, fields.get(3))?;
                    let rejected_record_total =
                        required_u64_from_value(fields.get(4), "route rejected_record_total")?;
                    let rejection_diagnostics =
                        required_route_rejection_diagnostics(route_identity, fields.get(5))?;
                    (
                        SourceBackedRefreshRouteOutcome::Succeeded { changed },
                        total,
                        failures,
                        rejected_record_total,
                        rejection_diagnostics,
                    )
                }
                Some("f") if fields.len() == 5 => {
                    let class = compact_source_failure_class(fields[1].as_str())?;
                    let carried_forward = fields[2].as_bool().ok_or_else(|| {
                        anyhow!("published daemon failed route result has no carried-forward fact")
                    })?;
                    let total =
                        required_usize_from_value(fields.get(3), "route source_failure_total")?;
                    let failures = required_route_source_failures(route_identity, fields.get(4))?;
                    (
                        SourceBackedRefreshRouteOutcome::Failed {
                            class,
                            carried_forward,
                        },
                        total,
                        failures,
                        0,
                        Vec::new(),
                    )
                }
                _ => bail!("published daemon source refresh route result has inconsistent fields"),
            };
            let result = SourceBackedRefreshRouteResult {
                route_identity: route_identity.clone(),
                outcome,
                source_failure_total,
                source_failures,
                rejected_record_total,
                rejection_diagnostics,
            };
            result.validate_source_failures()?;
            Ok(result)
        })
        .collect()
}

pub(super) fn zero_source_authority_json(
    authority: &[SourceBackedZeroSourceAuthority],
    route_results: &[SourceBackedRefreshRouteResult],
) -> Option<Value> {
    let generation_id = authority.first()?.generation_id.clone();
    let authority = authority
        .iter()
        .map(|entry| (entry.route_identity.as_str(), entry.kind))
        .collect::<BTreeMap<_, _>>();
    let mut route_results = route_results.iter().collect::<Vec<_>>();
    route_results.sort_by(|left, right| left.route_identity.cmp(&right.route_identity));
    // The disposition string is positionally bound to the sorted route-result
    // identities: `e` is complete-empty inventory and `d` is confirmed
    // deletion. This avoids repeating 64-byte route IDs and keeps the full
    // bounded route set inside the durable receipt budget.
    let route_kinds = route_results
        .iter()
        .filter_map(|result| authority.get(result.route_identity.as_str()))
        .map(|kind| kind.compact_code())
        .collect::<String>();
    Some(json!({
        "generation_id": generation_id,
        "route_kinds": route_kinds,
    }))
}

pub(super) fn parse_zero_source_authority(
    value: Option<&Value>,
    route_results: &[SourceBackedRefreshRouteResult],
) -> Result<Vec<SourceBackedZeroSourceAuthority>> {
    let Some(value) = value else {
        return Ok(Vec::new());
    };
    let fields = value
        .as_object()
        .ok_or_else(|| anyhow!("Core zero-source authority must be an object"))?;
    if fields.keys().map(String::as_str).collect::<BTreeSet<_>>()
        != BTreeSet::from(["generation_id", "route_kinds"])
    {
        bail!("Core zero-source authority has unknown or missing fields");
    }
    let generation_id = fields
        .get("generation_id")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| anyhow!("Core zero-source authority has no generation binding"))?;
    let route_kinds = fields
        .get("route_kinds")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("Core zero-source authority has no route entries"))?;
    let route_kinds = route_kinds.chars().collect::<Vec<_>>();
    if route_kinds.is_empty()
        || route_kinds.len() > SOURCE_REFRESH_TERMINAL_ROUTE_LIMIT
        || route_kinds.len() != route_results.len()
    {
        bail!(
            "Core zero-source authority must contain 1..={SOURCE_REFRESH_TERMINAL_ROUTE_LIMIT} routes"
        );
    }
    let mut route_results = route_results.iter().collect::<Vec<_>>();
    route_results.sort_by(|left, right| left.route_identity.cmp(&right.route_identity));
    route_results
        .into_iter()
        .zip(route_kinds)
        .map(|(result, kind)| {
            Ok(SourceBackedZeroSourceAuthority {
                generation_id: generation_id.to_owned(),
                route_identity: SourceRouteIdentity::from_sha256(result.route_identity.clone())?,
                kind: SourceBackedZeroSourceAuthorityKind::from_compact_code(kind)?,
            })
        })
        .collect()
}

pub(super) fn validate_zero_source_authority(
    generation_id: &str,
    source_count: usize,
    route_results: &[SourceBackedRefreshRouteResult],
    authority: &[SourceBackedZeroSourceAuthority],
    required_for_empty: bool,
) -> Result<()> {
    if source_count != 0 {
        if !authority.is_empty() {
            bail!("nonempty Core generation carries zero-source authority");
        }
        return Ok(());
    }
    if authority.is_empty() {
        if required_for_empty {
            bail!("zero-source Core generation has no publication authority");
        }
        return Ok(());
    }
    if authority.len() > SOURCE_REFRESH_TERMINAL_ROUTE_LIMIT
        || authority
            .iter()
            .any(|entry| entry.generation_id != generation_id)
    {
        bail!("Core zero-source authority is not bound to its exact generation");
    }
    let authority_routes = authority
        .iter()
        .map(|entry| entry.route_identity.as_str())
        .collect::<BTreeSet<_>>();
    if authority_routes.len() != authority.len() {
        bail!("Core zero-source authority contains a duplicate route");
    }
    let successful_routes = route_results
        .iter()
        .filter(|result| result.outcome.is_success())
        .map(|result| result.route_identity.as_str())
        .collect::<BTreeSet<_>>();
    if successful_routes.len() != route_results.len() || successful_routes != authority_routes {
        bail!("Core zero-source authority does not cover every successful terminal route");
    }
    Ok(())
}

fn required_route_rejection_diagnostics(
    route_identity: &str,
    value: Option<&Value>,
) -> Result<Vec<SourceBackedRefreshRecordRejection>> {
    let value =
        value.ok_or_else(|| anyhow!("terminal route result has no rejection diagnostics"))?;
    value
        .as_array()
        .ok_or_else(|| anyhow!("terminal route result rejection diagnostics must be an array"))?
        .iter()
        .map(|value| {
            let fields = value
                .as_array()
                .filter(|fields| fields.len() == 7)
                .ok_or_else(|| {
                    anyhow!("daemon source refresh compact rejection diagnostic is malformed")
                })?;
            let required = |index: usize, field: &'static str| {
                fields[index]
                    .as_str()
                    .filter(|value| !value.is_empty())
                    .map(str::to_owned)
                    .ok_or_else(|| {
                        anyhow!("daemon source refresh rejection diagnostic has no {field}")
                    })
            };
            Ok(SourceBackedRefreshRecordRejection {
                route_identity: route_identity.to_owned(),
                source_identity: required(0, "source_identity")?
                    .into_sha256_identity("source_identity")?,
                provider: required(1, "provider")?,
                source_selector: required(2, "source_selector")?,
                line: required_u64_from_value(fields.get(3), "rejection line")?,
                payload_type: required(4, "payload_type")?,
                class: compact_record_rejection_class(fields[5].as_str())?,
                detail: required(6, "detail")?,
            })
        })
        .collect()
}

fn compact_record_rejection_class(value: Option<&str>) -> Result<String> {
    Ok(match value {
        Some("m") => "malformed_record",
        Some("u") => "unsupported_record",
        _ => bail!("published daemon source refresh record rejection class is invalid"),
    }
    .to_owned())
}

fn required_catalog_route_bindings(
    value: Option<&Value>,
    manifest: &GenerationManifest,
    route_results: &[SourceBackedRefreshRouteResult],
    expected_catalog_lineages: &BTreeSet<String>,
) -> Result<Vec<ExplicitSourceCatalogRouteBinding>> {
    let values = value
        .ok_or_else(|| {
            anyhow!("published daemon source refresh receipt has no catalog_route_bindings")
        })?
        .as_object()
        .ok_or_else(|| {
            anyhow!(
                "published daemon source refresh receipt catalog_route_bindings must be an object"
            )
        })?;
    let retained = manifest
        .source_routes()
        .iter()
        .map(|route| route.route_identity().as_str())
        .collect::<BTreeSet<_>>();
    values
        .iter()
        .map(|(catalog_lineage, route_identity)| {
            if !is_sha256_identity(catalog_lineage) {
                bail!("published daemon source refresh catalog lineage is invalid");
            }
            let route_identity = route_identity.as_str().ok_or_else(|| {
                anyhow!("published daemon source refresh catalog binding route is invalid")
            })?;
            let retained_witness = expected_catalog_lineages.contains(catalog_lineage)
                && retained.contains(route_identity);
            let cold_request_failure = !expected_catalog_lineages.contains(catalog_lineage)
                && route_results.iter().any(|result| {
                    result.route_identity == route_identity
                        && matches!(
                            result.outcome,
                            SourceBackedRefreshRouteOutcome::Failed {
                                carried_forward: false,
                                ..
                            }
                        )
                });
            if !retained_witness && !cold_request_failure {
                bail!("published daemon source refresh catalog binding is neither a retained witness nor a cold request failure");
            }
            Ok(ExplicitSourceCatalogRouteBinding {
                catalog_lineage: catalog_lineage.clone(),
                route_identity: route_identity.to_owned(),
            })
        })
        .collect()
}

fn required_route_source_failures(
    route_identity: &str,
    value: Option<&Value>,
) -> Result<Vec<SourceBackedRefreshSourceFailure>> {
    let value = value.ok_or_else(|| anyhow!("terminal route result has no source diagnostics"))?;
    value
        .as_array()
        .ok_or_else(|| anyhow!("terminal route result source diagnostics must be an array"))?
        .iter()
        .map(|value| {
            let fields = value
                .as_array()
                .filter(|fields| fields.len() == 6)
                .ok_or_else(|| {
                    anyhow!("daemon source refresh compact source diagnostic is malformed")
                })?;
            let required = |index: usize, field: &'static str| {
                fields[index]
                    .as_str()
                    .filter(|value| !value.is_empty())
                    .map(str::to_owned)
                    .ok_or_else(|| {
                        anyhow!("daemon source refresh source diagnostic has no {field}")
                    })
            };
            Ok(SourceBackedRefreshSourceFailure {
                route_identity: route_identity.to_owned(),
                source_identity: required(0, "source_identity")?
                    .into_sha256_identity("source_identity")?,
                provider: required(1, "provider")?,
                class: compact_source_failure_class(fields[2].as_str())?,
                carried_forward: fields[3].as_bool().ok_or_else(|| {
                    anyhow!("daemon source refresh source diagnostic has no carried_forward fact")
                })?,
                source_selector: required(4, "source_selector")?,
                detail: required(5, "detail")?,
            })
        })
        .collect()
}

fn compact_source_failure_class(value: Option<&str>) -> Result<String> {
    Ok(match value {
        Some("u") => "unavailable",
        Some("c") => "source_changed",
        Some("r") => "unreadable",
        Some("i") => "incompatible",
        _ => bail!("published daemon source refresh source failure class is invalid"),
    }
    .to_owned())
}

trait Sha256IdentityString {
    fn into_sha256_identity(self, field: &'static str) -> Result<String>;
}

impl Sha256IdentityString for String {
    fn into_sha256_identity(self, field: &'static str) -> Result<String> {
        if is_sha256_identity(&self) {
            Ok(self)
        } else {
            bail!("daemon source refresh source failure {field} is malformed")
        }
    }
}

pub(super) fn is_sha256_identity(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

pub fn optional_generation(value: Option<&Value>) -> Result<Option<String>> {
    match value {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(value)) if !value.is_empty() => Ok(Some(value.clone())),
        _ => bail!("daemon source refresh generation identity is malformed"),
    }
}

pub(super) fn required_generation(value: Option<&Value>, label: &str) -> Result<String> {
    value
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
        .ok_or_else(|| anyhow!("{label} is missing"))
}

fn required_usize(value: &serde_json::Map<String, Value>, field: &str) -> Result<usize> {
    required_usize_from_value(value.get(field), field)
}

fn required_usize_from_value(value: Option<&Value>, field: &str) -> Result<usize> {
    value
        .and_then(Value::as_u64)
        .and_then(|value| usize::try_from(value).ok())
        .ok_or_else(|| anyhow!("published daemon source refresh receipt has invalid {field}"))
}

fn required_u64(value: &serde_json::Map<String, Value>, field: &str) -> Result<u64> {
    required_u64_from_value(value.get(field), field)
}

fn required_u64_from_value(value: Option<&Value>, field: &str) -> Result<u64> {
    value
        .and_then(Value::as_u64)
        .ok_or_else(|| anyhow!("published daemon source refresh receipt has invalid {field}"))
}

pub fn pin_published_generation(
    data_root: &Path,
    journal: &dyn RefreshJournal,
) -> Result<Option<PinnedSourceBackedGeneration>> {
    let Some(index) = open_published_generation(data_root, journal)? else {
        return Ok(None);
    };
    if !verified_generation_is_query_ready(&index)? {
        return Ok(None);
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
    if !verified_generation_is_query_ready(&index)? {
        bail!("retained Core generation {generation_id} has no zero-source publication authority");
    }
    Ok(PinnedSourceBackedGeneration { index })
}

pub fn pin_active_verified_generation(
    data_root: &Path,
    journal: &dyn RefreshJournal,
) -> Result<PinnedSourceBackedGeneration> {
    pin_published_generation(data_root, journal)
        .context("source_unavailable: verify active Core generation")?
        .ok_or_else(|| anyhow!("source_unavailable: active verified Core generation is missing"))
}
