//! Bounded newest-first merging across checked Flat layers.

use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet};

use crate::protocol::ResourceKind;
use crate::query::{Citation, Fact, QueryError, Resource, ResourceId, ResourceSelector};

use super::SegmentGraph;
use crate::graph::segment::model::{ServingRecordQueryExt as _, ServingResourceQueryExt as _};
use crate::graph::segment::{
    FactFamily, GIT_REMOTE_ALIAS, SCHEMA_CRITICAL_FACT_FAMILIES, ServingCitation,
    ServingConfidence, ServingRecord, ServingResource, TombstoneQueryWork,
    logical_repository_graph_id,
};

const MAX_BACKEND_ROWS: usize = 501;
const MAX_MERGED_RECORDS: usize = 64 * 1024;
const MAX_GRAPH_QUERY_CANDIDATES: u64 = 256 * 1024;
const MAX_GRAPH_TOMBSTONE_PROBES: u64 = 256 * 1024;
const MAX_GRAPH_TOMBSTONE_PAGES: u64 = 256;
const MAX_GRAPH_TOMBSTONE_PAGE_BYTES: u64 = 16 * 1024 * 1024;
const MAX_GRAPH_TOMBSTONE_AUTHENTICATED_CHUNKS: u64 = 512;
const MAX_GRAPH_TOMBSTONE_AUTHENTICATED_BYTES: u64 = 32 * 1024 * 1024;

pub(super) struct QueryWork {
    candidates: u64,
    retained_high_water: usize,
    tombstone_probes: u64,
    tombstone_pages: u64,
    tombstone_page_bytes: u64,
    tombstone_checked_chunks: u64,
    tombstone_checked_bytes: u64,
}

impl QueryWork {
    pub(super) const fn new() -> Self {
        Self {
            candidates: 0,
            retained_high_water: 0,
            tombstone_probes: 0,
            tombstone_pages: 0,
            tombstone_page_bytes: 0,
            tombstone_checked_chunks: 0,
            tombstone_checked_bytes: 0,
        }
    }

    fn charge_candidates(&mut self, candidates: usize) -> Result<(), QueryError> {
        self.candidates = bounded_add(
            self.candidates,
            u64::try_from(candidates).map_err(|_| SegmentGraph::query_error())?,
            MAX_GRAPH_QUERY_CANDIDATES,
        )?;
        Ok(())
    }

    pub(super) fn observe_retained(&mut self, retained: usize) {
        self.retained_high_water = self.retained_high_water.max(retained);
    }

    #[cfg(test)]
    pub(super) const fn candidate_count(&self) -> u64 {
        self.candidates
    }

    #[cfg(test)]
    pub(super) const fn retained_high_water(&self) -> usize {
        self.retained_high_water
    }

    fn charge_tombstones(
        &mut self,
        probes: usize,
        planned: TombstoneQueryWork,
    ) -> Result<(), QueryError> {
        let probes = bounded_add(
            self.tombstone_probes,
            u64::try_from(probes).map_err(|_| SegmentGraph::query_error())?,
            MAX_GRAPH_TOMBSTONE_PROBES,
        )?;
        let pages = bounded_add(
            self.tombstone_pages,
            planned.pages,
            MAX_GRAPH_TOMBSTONE_PAGES,
        )?;
        let page_bytes = bounded_add(
            self.tombstone_page_bytes,
            planned.page_bytes,
            MAX_GRAPH_TOMBSTONE_PAGE_BYTES,
        )?;
        let checked_chunks = bounded_add(
            self.tombstone_checked_chunks,
            planned.checked_chunks,
            MAX_GRAPH_TOMBSTONE_AUTHENTICATED_CHUNKS,
        )?;
        let checked_bytes = bounded_add(
            self.tombstone_checked_bytes,
            planned.checked_bytes,
            MAX_GRAPH_TOMBSTONE_AUTHENTICATED_BYTES,
        )?;
        self.tombstone_probes = probes;
        self.tombstone_pages = pages;
        self.tombstone_page_bytes = page_bytes;
        self.tombstone_checked_chunks = checked_chunks;
        self.tombstone_checked_bytes = checked_bytes;
        Ok(())
    }
}

fn bounded_add(current: u64, additional: u64, maximum: u64) -> Result<u64, QueryError> {
    let next = current
        .checked_add(additional)
        .ok_or_else(SegmentGraph::query_error)?;
    (next <= maximum)
        .then_some(next)
        .ok_or_else(SegmentGraph::query_error)
}

#[derive(Clone, Copy)]
pub(super) enum Lookup<'a> {
    Exact {
        repository: Option<&'a str>,
        term: &'a str,
    },
    Prefix {
        repository: Option<&'a str>,
        term: &'a str,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum ScanControl {
    Continue,
    Stop,
}

impl SegmentGraph {
    #[doc(hidden)]
    pub fn serving_records_for_test(
        &self,
        family_name: &str,
        repository: Option<&str>,
        term: &str,
    ) -> Result<Vec<ServingRecord>, QueryError> {
        self.merged_records(family_name, Lookup::Exact { repository, term })
    }

    pub(super) fn merged_records(
        &self,
        family_name: &str,
        lookup: Lookup<'_>,
    ) -> Result<Vec<ServingRecord>, QueryError> {
        let mut work = QueryWork::new();
        self.merged_records_with_work(family_name, lookup, &mut work)
    }

    pub(super) fn merged_records_with_work(
        &self,
        family_name: &str,
        lookup: Lookup<'_>,
        work: &mut QueryWork,
    ) -> Result<Vec<ServingRecord>, QueryError> {
        let mut records = Vec::new();
        self.scan_records_with_work(family_name, lookup, work, &mut |record| {
            records.push(record);
            if records.len() > MAX_MERGED_RECORDS {
                return Err(Self::query_error());
            }
            Ok(ScanControl::Continue)
        })?;
        work.observe_retained(records.len());
        Ok(records)
    }

    pub(super) fn scan_records_with_work(
        &self,
        family_name: &str,
        lookup: Lookup<'_>,
        work: &mut QueryWork,
        visit: &mut impl FnMut(ServingRecord) -> Result<ScanControl, QueryError>,
    ) -> Result<ScanControl, QueryError> {
        let family = FactFamily::new(family_name).map_err(|_| Self::query_error())?;
        let mut position = self
            .storage
            .first_reader()
            .map_err(|_| Self::query_error())?;
        while let Some(current) = position {
            // Same-publication chunks are one temporal layer. Candidate
            // tombstone membership is checked only in strictly newer layers,
            // so every current-layer row is visible before its layer can
            // suppress ownership in older rows.
            let mut continuation = None;
            loop {
                let page = match lookup {
                    Lookup::Exact {
                        repository: Some(repository),
                        term,
                    } => self.storage.query_exact_page(
                        current,
                        repository,
                        &family,
                        term,
                        continuation.as_ref(),
                    ),
                    Lookup::Exact {
                        repository: None,
                        term,
                    } => self.storage.query_exact_unscoped_page(
                        current,
                        &family,
                        term,
                        continuation.as_ref(),
                    ),
                    Lookup::Prefix {
                        repository: Some(repository),
                        term,
                    } => self.storage.query_prefix_page(
                        current,
                        repository,
                        &family,
                        term,
                        continuation.as_ref(),
                    ),
                    Lookup::Prefix {
                        repository: None,
                        term,
                    } => self.storage.query_prefix_unscoped_page(
                        current,
                        &family,
                        term,
                        continuation.as_ref(),
                    ),
                }
                .map_err(|_| Self::query_error())?;
                // Charge every physical candidate before survivor filtering,
                // then every candidate/strictly-newer-reader membership probe
                // before the checked tombstone lookup. Thus a fully
                // shadowed corpus cannot evade the graph-wide bound.
                work.charge_candidates(page.records.len())?;
                let owner_keys = page
                    .records
                    .iter()
                    .map(|record| record.event_owner.key())
                    .collect::<Vec<_>>();
                let mut shadowed = vec![false; owner_keys.len()];
                if !owner_keys.is_empty() {
                    self.storage
                        .scan_strictly_newer_tombstones(
                            current,
                            &owner_keys,
                            |planned| work.charge_tombstones(owner_keys.len(), planned),
                            |membership| {
                                for (shadowed, member) in shadowed.iter_mut().zip(membership) {
                                    *shadowed |= member;
                                }
                                Ok(shadowed.iter().all(|shadowed| *shadowed))
                            },
                        )
                        .map_err(|_| Self::query_error())??;
                }
                for (record, shadowed) in page.records.into_iter().zip(shadowed) {
                    if !shadowed && visit(record)? == ScanControl::Stop {
                        return Ok(ScanControl::Stop);
                    }
                }
                continuation = page.continuation;
                if continuation.is_none() {
                    break;
                }
            }
            position = self
                .storage
                .next_reader(current)
                .map_err(|_| Self::query_error())?;
        }
        Ok(ScanControl::Continue)
    }

    pub(super) fn scan_records_for_families_with_work(
        &self,
        families: &[&str],
        lookup: Lookup<'_>,
        work: &mut QueryWork,
        visit: &mut impl FnMut(ServingRecord) -> Result<ScanControl, QueryError>,
    ) -> Result<ScanControl, QueryError> {
        for family in families {
            if self.scan_records_with_work(family, lookup, work, visit)? == ScanControl::Stop {
                return Ok(ScanControl::Stop);
            }
        }
        Ok(ScanControl::Continue)
    }

    pub(super) fn records_for_families_with_work(
        &self,
        families: &[&str],
        lookup: Lookup<'_>,
        work: &mut QueryWork,
    ) -> Result<Vec<ServingRecord>, QueryError> {
        let mut records = Vec::new();
        for family in families {
            records.extend(self.merged_records_with_work(family, lookup, work)?);
            if records.len() > MAX_MERGED_RECORDS {
                return Err(Self::query_error());
            }
        }
        Ok(records)
    }

    pub(super) fn assemble_facts(
        &self,
        records: Vec<ServingRecord>,
    ) -> Result<Vec<Fact>, QueryError> {
        let mut facts = BTreeMap::<String, FactAssembly>::new();
        for record in records {
            let assembled = self.assemble_record(&record)?;
            match facts.get_mut(&assembled.fact.id) {
                None => {
                    facts.insert(assembled.fact.id.clone(), assembled);
                }
                Some(stored) => {
                    stored.merge(assembled)?;
                }
            }
        }
        facts.into_values().map(FactAssembly::into_fact).collect()
    }

    pub(super) fn assemble_record(
        &self,
        record: &ServingRecord,
    ) -> Result<FactAssembly, QueryError> {
        let mut fact = record.to_query_fact()?;
        fact.citations.clear();
        Ok(FactAssembly {
            fact,
            provenance: Provenance::new(record),
            citations: self.active_citation_selection(&record.citations)?,
        })
    }

    pub(super) fn active_citations(
        &self,
        citations: &[ServingCitation],
    ) -> Result<Vec<Citation>, QueryError> {
        self.active_citation_selection(citations)
            .map(CitationSelection::into_chronology)
    }

    fn active_citation_selection(
        &self,
        citations: &[ServingCitation],
    ) -> Result<CitationSelection, QueryError> {
        let mut active = CitationSelection::default();
        for stored in citations {
            let mut exact = stored
                .exact_core_citation()
                .map_err(|_| Self::query_error())?;
            // Evidence identity is event/source bound; generation is the active
            // snapshot frontier used for serving this immutable graph.
            exact.core_generation_id = self.completed_receipt().core_generation_id.clone();
            let citation = Citation::new(exact).map_err(|_| Self::query_error())?;
            active.insert(stored.citation_id.clone(), citation)?;
        }
        if active.is_empty() {
            return Err(Self::query_error());
        }
        Ok(active)
    }

    fn scan_resources_with_work(
        &self,
        families: &[&str],
        lookup: Lookup<'_>,
        work: &mut QueryWork,
        output: &mut BTreeMap<ResourceId, Resource>,
        limit: usize,
        predicate: &impl Fn(&Resource) -> bool,
    ) -> Result<ScanControl, QueryError> {
        if output.len() >= limit {
            return Ok(ScanControl::Stop);
        }
        let result =
            self.scan_records_for_families_with_work(families, lookup, work, &mut |record| {
                for resource in record_resources(&record)? {
                    if !predicate(&resource) {
                        continue;
                    }
                    if output
                        .insert(resource.id.clone(), resource.clone())
                        .is_some_and(|prior| prior != resource)
                    {
                        return Err(Self::query_error());
                    }
                    if output.len() >= limit {
                        return Ok(ScanControl::Stop);
                    }
                }
                Ok(ScanControl::Continue)
            })?;
        work.observe_retained(output.len());
        Ok(result)
    }

    pub(super) fn resolve_resources(
        &self,
        selector: &ResourceSelector,
        limit: usize,
    ) -> Result<Vec<Resource>, QueryError> {
        let limit = bounded_limit(limit);
        if limit == 0 || selector.value.is_empty() {
            return Ok(Vec::new());
        }
        let mut work = QueryWork::new();
        let scopes =
            self.repository_scopes(selector.repository.as_deref(), selector.kind, &mut work)?;
        let lookup_term = if selector.kind == ResourceKind::Repository {
            logical_repository_graph_id(&selector.value).map_err(|_| Self::query_error())?
        } else {
            selector.value.clone()
        };
        let selection_limit = if selector.kind == ResourceKind::Repository {
            1
        } else {
            limit
        };
        let mut resources = BTreeMap::<ResourceId, Resource>::new();
        for scope in scopes.as_deref().unwrap_or(&[None]) {
            self.scan_resources_with_work(
                &SCHEMA_CRITICAL_FACT_FAMILIES,
                Lookup::Exact {
                    repository: scope.as_deref(),
                    term: &lookup_term,
                },
                &mut work,
                &mut resources,
                selection_limit,
                &|resource| {
                    resource.kind == selector.kind
                        && (resource.display == selector.value || resource.id.0 == selector.value)
                },
            )?;

            if resources.len() < selection_limit
                && selector.kind == ResourceKind::Commit
                && selector.value.len() <= 64
                && selector.value.bytes().all(|byte| byte.is_ascii_hexdigit())
            {
                let prefix = selector.value.to_ascii_lowercase();
                self.scan_resources_with_work(
                    &SCHEMA_CRITICAL_FACT_FAMILIES,
                    Lookup::Prefix {
                        repository: scope.as_deref(),
                        term: &prefix,
                    },
                    &mut work,
                    &mut resources,
                    selection_limit,
                    &|resource| {
                        resource.kind == ResourceKind::Commit
                            && resource.display.starts_with(&prefix)
                    },
                )?;
            }
            if resources.len() >= selection_limit {
                break;
            }
        }
        Ok(resources.into_values().take(limit).collect())
    }

    pub(super) fn resolve_commit_resources(
        &self,
        commits: &[String],
        repository: Option<&str>,
        limit: usize,
    ) -> Result<Vec<(String, Resource)>, QueryError> {
        let limit = bounded_limit(limit);
        if commits.is_empty() || limit == 0 {
            return Ok(Vec::new());
        }
        let commits = commits
            .iter()
            .take(limit)
            .map(|commit| commit.to_ascii_lowercase())
            .collect::<BTreeSet<_>>();
        let mut work = QueryWork::new();
        let scopes = self.repository_scopes(repository, ResourceKind::Commit, &mut work)?;
        let mut resolved = Vec::new();
        for commit in commits {
            let mut matches = BTreeMap::<ResourceId, Resource>::new();
            for scope in scopes.as_deref().unwrap_or(&[None]) {
                self.scan_resources_with_work(
                    &SCHEMA_CRITICAL_FACT_FAMILIES,
                    Lookup::Exact {
                        repository: scope.as_deref(),
                        term: &commit,
                    },
                    &mut work,
                    &mut matches,
                    2,
                    &|resource| resource.kind == ResourceKind::Commit && resource.display == commit,
                )?;
                if matches.len() >= 2 {
                    break;
                }
            }
            resolved.extend(
                matches
                    .into_values()
                    .take(2)
                    .map(|resource| (commit.clone(), resource)),
            );
        }
        Ok(resolved)
    }

    pub(super) fn load_resources(
        &self,
        ids: &[ResourceId],
        limit: usize,
    ) -> Result<Vec<Resource>, QueryError> {
        let mut work = QueryWork::new();
        self.load_resources_with_work(ids, limit, &mut work)
    }

    pub(super) fn load_resources_with_work(
        &self,
        ids: &[ResourceId],
        limit: usize,
        work: &mut QueryWork,
    ) -> Result<Vec<Resource>, QueryError> {
        let mut loaded = Vec::new();
        for id in ids.iter().take(bounded_limit(limit)) {
            let mut matches = BTreeMap::<ResourceId, Resource>::new();
            self.scan_resources_with_work(
                &SCHEMA_CRITICAL_FACT_FAMILIES,
                Lookup::Exact {
                    repository: None,
                    term: &id.0,
                },
                work,
                &mut matches,
                1,
                &|resource| resource.id == *id,
            )?;
            match matches.len() {
                0 => {}
                1 => {
                    if let Some(resource) = matches.into_values().next() {
                        loaded.push(resource);
                    }
                }
                _ => return Err(Self::query_error()),
            }
        }
        Ok(loaded)
    }

    fn repository_scopes(
        &self,
        repository: Option<&str>,
        kind: ResourceKind,
        work: &mut QueryWork,
    ) -> Result<Option<Vec<Option<String>>>, QueryError> {
        let Some(repository) = repository.filter(|_| kind != ResourceKind::Repository) else {
            return Ok(None);
        };
        let mut scopes = BTreeSet::from([repository.to_owned()]);
        if let Some(forge) = repository.strip_prefix("forge:") {
            for remote in [format!("https://{forge}"), format!("ssh://{forge}")] {
                for record in self.merged_records_with_work(
                    GIT_REMOTE_ALIAS,
                    Lookup::Exact {
                        repository: None,
                        term: &remote,
                    },
                    work,
                )? {
                    if record.state == crate::graph::segment::ServingFactState::Asserted
                        && record.subject.typed_kind().ok() == Some(ResourceKind::Remote)
                        && record.subject.display().as_deref() == Ok(remote.as_str())
                    {
                        scopes.insert(record.repository_id);
                    }
                }
            }
        }
        Ok(Some(scopes.into_iter().map(Some).collect()))
    }
}

#[cfg(test)]
#[path = "merge_query_work_tests.rs"]
mod query_work_tests;

pub(super) struct FactAssembly {
    fact: Fact,
    provenance: Provenance,
    citations: CitationSelection,
}

impl FactAssembly {
    pub(super) const fn fact(&self) -> &Fact {
        &self.fact
    }

    pub(super) fn merge(&mut self, other: Self) -> Result<(), QueryError> {
        if !same_semantic_fact(&self.fact, &other.fact) {
            return Err(SegmentGraph::query_error());
        }
        self.citations.merge(other.citations)?;
        if other.provenance < self.provenance {
            self.fact.confidence = other.fact.confidence;
            self.fact.detector_version = other.fact.detector_version;
            self.provenance = other.provenance;
        }
        Ok(())
    }

    pub(super) fn into_fact(mut self) -> Result<Fact, QueryError> {
        self.fact.citations = self.citations.into_chronology();
        Ok(self.fact)
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd)]
struct Provenance {
    confidence_rank: u8,
    detector_id: String,
    detector_revision: String,
    citation_id: String,
}

impl Provenance {
    fn new(record: &ServingRecord) -> Self {
        Self {
            confidence_rank: confidence_rank(record.confidence),
            detector_id: record.detector_id.clone(),
            detector_revision: record.detector_revision.clone(),
            citation_id: record
                .citations
                .first()
                .map_or_else(String::new, |citation| citation.citation_id.clone()),
        }
    }
}

fn confidence_rank(confidence: ServingConfidence) -> u8 {
    match confidence {
        ServingConfidence::Verified => 0,
        ServingConfidence::High => 1,
        ServingConfidence::Medium => 2,
        ServingConfidence::Ambiguous => 3,
    }
}

fn same_semantic_fact(left: &Fact, right: &Fact) -> bool {
    left.id == right.id
        && left.fact_type == right.fact_type
        && left.subject == right.subject
        && left.predicate == right.predicate
        && left.object == right.object
        && left.value == right.value
        && left.occurred_at_ms == right.occurred_at_ms
        && left.state == right.state
        && left.root_run == right.root_run
        && left.direct_actor == right.direct_actor
}

#[derive(Default)]
struct CitationSelection {
    // The canonical projection orders citations by evidence_id. Serving
    // citation_id is that exact identifier, so keep the lowest 32 IDs;
    // authority/confidence never participates in citation selection.
    by_id: BTreeMap<String, Citation>,
}

impl CitationSelection {
    fn is_empty(&self) -> bool {
        self.by_id.is_empty()
    }

    fn insert(&mut self, id: String, citation: Citation) -> Result<(), QueryError> {
        if self
            .by_id
            .get(&id)
            .is_some_and(|stored| stored != &citation)
        {
            return Err(SegmentGraph::query_error());
        }
        self.by_id.entry(id).or_insert(citation);
        if self.by_id.len() > crate::protocol::MAX_CITATIONS_PER_FACT {
            self.by_id.pop_last();
        }
        Ok(())
    }

    fn merge(&mut self, other: Self) -> Result<(), QueryError> {
        for (id, citation) in other.by_id {
            self.insert(id, citation)?;
        }
        Ok(())
    }

    fn into_chronology(self) -> Vec<Citation> {
        let mut citations = self.by_id.into_values().collect::<Vec<_>>();
        sort_citations(&mut citations);
        citations
    }
}

fn sort_citations(citations: &mut [Citation]) {
    citations.sort_by(citation_order);
}

fn citation_order(left: &Citation, right: &Citation) -> Ordering {
    let left = left.evidence();
    let right = right.evidence();
    left.event_sequence
        .cmp(&right.event_sequence)
        .then_with(|| left.event_id.to_string().cmp(&right.event_id.to_string()))
        .then_with(|| {
            left.session_id
                .to_string()
                .cmp(&right.session_id.to_string())
        })
        .then_with(|| {
            left.source
                .exact_descriptor_digest()
                .cmp(&right.source.exact_descriptor_digest())
        })
        .then_with(|| byte_range_order(left.byte_range.as_ref(), right.byte_range.as_ref()))
        .then_with(|| left.evidence_sha256.cmp(&right.evidence_sha256))
}

fn byte_range_order(
    left: Option<&crate::protocol::ByteRange>,
    right: Option<&crate::protocol::ByteRange>,
) -> Ordering {
    match (left, right) {
        (None, None) => Ordering::Equal,
        (None, Some(_)) => Ordering::Less,
        (Some(_), None) => Ordering::Greater,
        (Some(left), Some(right)) => {
            (left.start, left.end_exclusive).cmp(&(right.start, right.end_exclusive))
        }
    }
}

pub(super) fn record_resources(record: &ServingRecord) -> Result<Vec<Resource>, QueryError> {
    let mut resources = Vec::new();
    for resource in [
        Some(&record.subject),
        record.object.as_ref(),
        record.scope.as_ref(),
        record.direct_actor.as_ref(),
    ]
    .into_iter()
    .flatten()
    {
        resources.push(
            resource
                .to_query_resource()
                .map_err(|_| SegmentGraph::query_error())?,
        );
    }
    Ok(resources)
}

pub(super) fn graph_id(resource: &ServingResource) -> Result<String, QueryError> {
    resource.graph_id().map_err(|_| SegmentGraph::query_error())
}

pub(super) const fn bounded_limit(limit: usize) -> usize {
    if limit > MAX_BACKEND_ROWS {
        MAX_BACKEND_ROWS
    } else {
        limit
    }
}

pub(super) const fn page_limit(limit: usize) -> usize {
    if limit > 500 { 500 } else { limit }
}
