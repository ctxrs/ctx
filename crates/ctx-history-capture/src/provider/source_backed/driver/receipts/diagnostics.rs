use super::*;
use std::collections::BTreeMap;

/// Typed result for one independently replaceable logical source.
#[derive(Debug)]
pub(crate) enum SourceBackedSourceOutcome<T> {
    Success(T),
    Failed(Box<SourceBackedLogicalSourceFailureFact>),
}

#[derive(Debug)]
pub struct SourceBackedLogicalSourceFailureFact {
    pub(crate) source: SourceKey,
    pub(crate) retained: Option<CertifiedSource>,
    pub(crate) failure: SourceBackedRouteError,
    pub(crate) record_rejections: SourceBackedRecordRejectionDrafts,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceBackedRecordRejectionClass {
    /// The provider owned the record shape, but its encoded input was invalid.
    MalformedRecord,
    /// The record was well formed, but its payload shape is not projectable by
    /// the current provider parser revision.
    UnsupportedRecord,
}

/// Record-level status for a refresh that otherwise completed and published.
///
/// This status does not describe route or logical-source failures. Shared
/// resource exhaustion and other systemic failures return an error instead of
/// a completion value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceBackedRecordCompletion {
    Completed,
    CompletedWithRejections,
}

impl SourceBackedRecordCompletion {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Completed => "completed",
            Self::CompletedWithRejections => "completed_with_rejections",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SourceBackedRecordRejectionDraft {
    pub(crate) source: SourceKey,
    pub(crate) provider: CaptureProvider,
    pub(crate) source_selector: String,
    pub(crate) line_number: u64,
    pub(crate) payload_type: Option<String>,
    pub(crate) class: SourceBackedRecordRejectionClass,
    pub(crate) detail: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(crate) struct SourceBackedRecordRejectionDrafts {
    rejections: Vec<SourceBackedRecordRejectionDraft>,
    omitted: usize,
}

impl SourceBackedRecordRejectionDrafts {
    pub(crate) fn record(&mut self, rejection: SourceBackedRecordRejectionDraft) {
        if self.rejections.len() < MAX_RECORDED_SOURCE_BACKED_RECORD_REJECTIONS {
            self.rejections.push(rejection);
        } else {
            self.omitted = self.omitted.saturating_add(1);
        }
    }

    pub(crate) fn first(&self) -> Option<&SourceBackedRecordRejectionDraft> {
        self.rejections.first()
    }

    pub(crate) fn merge(&mut self, other: Self) {
        let (rejections, omitted) = other.into_parts();
        for rejection in rejections {
            self.record(rejection);
        }
        self.omitted = self.omitted.saturating_add(omitted);
    }

    pub(super) fn into_parts(self) -> (Vec<SourceBackedRecordRejectionDraft>, usize) {
        (self.rejections, self.omitted)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceBackedRecordRejection {
    pub(in super::super::super) route_index: usize,
    /// The successful route that published valid peers alongside this
    /// rejected provider record.
    pub route_identity: SourceRouteIdentity,
    /// Canonical identity of the logical source containing the rejected
    /// provider record.
    pub source: SourceKey,
    pub provider: CaptureProvider,
    pub source_selector: String,
    pub line_number: u64,
    pub payload_type: Option<String>,
    pub class: SourceBackedRecordRejectionClass,
    pub detail: String,
}

/// Bounded record-level diagnostics from routes that still certified and
/// published their valid records. Entries are kept in canonical route/scan
/// order. `omitted` counts additional committed-route rejections beyond the
/// diagnostic bound; diagnostics staged by a rolled-back route are removed.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SourceBackedRecordRejections {
    rejections: Vec<SourceBackedRecordRejection>,
    omitted: usize,
}

impl SourceBackedRecordRejections {
    pub fn rejections(&self) -> &[SourceBackedRecordRejection] {
        &self.rejections
    }

    pub fn omitted(&self) -> usize {
        self.omitted
    }

    pub fn total(&self) -> usize {
        self.rejections.len().saturating_add(self.omitted)
    }

    pub fn is_empty(&self) -> bool {
        self.total() == 0
    }

    pub(in super::super::super) fn record(&mut self, rejection: SourceBackedRecordRejection) {
        if self.rejections.len() < MAX_RECORDED_SOURCE_BACKED_RECORD_REJECTIONS {
            self.rejections.push(rejection);
        } else {
            self.omitted = self.omitted.saturating_add(1);
        }
    }

    pub(in super::super::super) fn record_omitted(&mut self, omitted: usize) {
        self.omitted = self.omitted.saturating_add(omitted);
    }

    pub(in super::super::super) fn checkpoint(&self) -> (usize, usize) {
        (self.rejections.len(), self.omitted)
    }

    pub(in super::super::super) fn truncate(&mut self, retained: usize, omitted: usize) {
        self.rejections.truncate(retained);
        self.omitted = omitted;
    }
}

/// Three independently committed certified whole-route absences bound grace.
pub const AUTOMATIC_ROUTE_DELETION_MISSING_OBSERVATIONS: u32 =
    ctx_history_index::policy::AUTOMATIC_ROUTE_DELETION_GRACE_OBSERVATIONS;

/// Selects the provider routes incorporated into one global generation.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum SourceBackedRefreshScope {
    #[default]
    All,
    Exact(BTreeSet<SourceRouteIdentity>),
}

impl SourceBackedRefreshScope {
    pub fn exact(route_identities: impl IntoIterator<Item = SourceRouteIdentity>) -> Self {
        Self::Exact(route_identities.into_iter().collect())
    }
}

/// Narrow source-authority failures that may be isolated to one route.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceBackedSourceFailureClass {
    Unavailable,
    SourceChanged,
    Unreadable,
    Incompatible,
}

impl SourceBackedSourceFailureClass {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Unavailable => "unavailable",
            Self::SourceChanged => "source_changed",
            Self::Unreadable => "unreadable",
            Self::Incompatible => "incompatible",
        }
    }

    const fn index(self) -> usize {
        match self {
            Self::Unavailable => 0,
            Self::SourceChanged => 1,
            Self::Unreadable => 2,
            Self::Incompatible => 3,
        }
    }
}

#[cfg(test)]
mod bounded_failure_tests {
    use super::*;

    #[test]
    fn bounded_diagnostics_retain_exact_aggregate_class_totals() {
        let failures = (0..70).map(|index| {
            SourceBackedFailedRoute::new(
                SourceRouteIdentity::from_sha256(format!("{index:064x}")).unwrap(),
                format!("source-{index}"),
                CaptureProvider::OpenCode,
                if index % 2 == 0 {
                    SourceBackedSourceFailureClass::Unreadable
                } else {
                    SourceBackedSourceFailureClass::Incompatible
                },
                false,
                format!("selector-{index}"),
                format!("detail-{index}"),
            )
        });
        let bounded = SourceBackedSourceFailures::from_failures(failures);

        assert_eq!(
            bounded.failures().len(),
            MAX_RECORDED_SOURCE_BACKED_FAILURES
        );
        assert_eq!(bounded.omitted(), 6);
        assert_eq!(bounded.total(), 70);
        assert_eq!(
            bounded.class_total(SourceBackedSourceFailureClass::Unreadable),
            35
        );
        assert_eq!(
            bounded.class_total(SourceBackedSourceFailureClass::Incompatible),
            35
        );
    }
}

/// Content-free identity and class for one route-local failure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceBackedFailedRoute {
    pub route_identity: SourceRouteIdentity,
    pub source_identity: String,
    pub provider: CaptureProvider,
    pub class: SourceBackedSourceFailureClass,
    pub carried_forward: bool,
    pub source_selector: String,
    pub detail: String,
}

/// Compact lifecycle result retained for every failed route. Human-readable
/// selector/detail diagnostics live only in [`SourceBackedSourceFailures`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceBackedFailedRouteOutcome {
    pub route_identity: SourceRouteIdentity,
    pub source_identity: String,
    pub provider: CaptureProvider,
    pub class: SourceBackedSourceFailureClass,
    pub carried_forward: bool,
}

impl From<&SourceBackedFailedRoute> for SourceBackedFailedRouteOutcome {
    fn from(failure: &SourceBackedFailedRoute) -> Self {
        Self {
            route_identity: failure.route_identity.clone(),
            source_identity: failure.source_identity.clone(),
            provider: failure.provider,
            class: failure.class,
            carried_forward: failure.carried_forward,
        }
    }
}

impl SourceBackedFailedRoute {
    pub fn new(
        route_identity: SourceRouteIdentity,
        source_identity: String,
        provider: CaptureProvider,
        class: SourceBackedSourceFailureClass,
        carried_forward: bool,
        source_selector: impl AsRef<str>,
        detail: impl AsRef<str>,
    ) -> Self {
        Self {
            route_identity,
            source_identity,
            provider,
            class,
            carried_forward,
            source_selector: bounded_text(
                source_selector.as_ref(),
                MAX_SOURCE_BACKED_FAILURE_SELECTOR_BYTES,
            ),
            detail: bounded_text(detail.as_ref(), MAX_SOURCE_BACKED_FAILURE_DETAIL_BYTES),
        }
    }

    pub(in super::super::super) fn from_route(
        route: &SourceBackedRoute,
        class: SourceBackedSourceFailureClass,
        carried_forward: bool,
        detail: impl AsRef<str>,
    ) -> SourceBackedCoordinatorResult<Self> {
        let route_identity = route.metadata.route_identity.clone().ok_or_else(|| {
            SourceBackedCoordinatorError::InvalidRoute {
                provider: route.metadata.source.provider,
                detail: "failed executable route has no route identity".to_owned(),
            }
        })?;
        let mut digest = Sha256::new();
        digest.update(b"ctx.source-failure-identity-v1\0");
        digest.update(route.metadata.source.provider.as_str().as_bytes());
        digest.update([0]);
        digest.update(route.metadata.certified_source_format.as_bytes());
        digest.update([0]);
        let path = route.metadata.source.path.as_os_str().as_encoded_bytes();
        digest.update((path.len() as u64).to_be_bytes());
        digest.update(path);
        Ok(Self::new(
            route_identity,
            format!("{:x}", digest.finalize()),
            route.metadata.source.provider,
            class,
            carried_forward,
            route.metadata.source.path.display().to_string(),
            detail,
        ))
    }
}

/// One independently owned logical source that could not be replaced during
/// an otherwise safe provider route. The source descriptor is retained so the
/// outcome is typed and deterministic without exposing a provider path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceBackedLogicalSourceFailure {
    pub(in super::super::super) route_index: usize,
    /// The successful route whose independently owned logical source failed.
    pub route_identity: SourceRouteIdentity,
    pub source: SourceKey,
    pub class: SourceBackedSourceFailureClass,
    pub carried_forward: bool,
    pub detail: String,
}

/// Bounded logical-source failures from provider routes that still committed.
///
/// Entries are ordered by canonical route and logical-source scan order. Each
/// entry represents one independently owned source that was not replaced; when
/// `carried_forward` is true, its exact prior certificate and records remain in
/// the committed generation. `omitted` counts additional failures beyond the
/// diagnostic bound. Failures from a rolled-back route are removed, while a
/// systemic refresh failure returns an error and publishes no receipt.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SourceBackedLogicalSourceFailures {
    failures: Vec<SourceBackedLogicalSourceFailure>,
    omitted: usize,
    route_totals: BTreeMap<SourceRouteIdentity, usize>,
}

pub(in super::super::super) struct SourceBackedLogicalSourceFailureCheckpoint {
    retained: usize,
    omitted: usize,
    route_identity: SourceRouteIdentity,
    route_total: usize,
}

impl SourceBackedLogicalSourceFailures {
    pub fn failures(&self) -> &[SourceBackedLogicalSourceFailure] {
        &self.failures
    }

    pub fn omitted(&self) -> usize {
        self.omitted
    }

    pub fn total(&self) -> usize {
        self.failures.len().saturating_add(self.omitted)
    }

    pub fn is_empty(&self) -> bool {
        self.total() == 0
    }

    pub(in super::super::super) fn record(&mut self, failure: SourceBackedLogicalSourceFailure) {
        *self
            .route_totals
            .entry(failure.route_identity.clone())
            .or_default() += 1;
        if self.failures.len() < MAX_RECORDED_SOURCE_BACKED_FAILURES {
            self.failures.push(failure);
        } else {
            self.omitted = self.omitted.saturating_add(1);
        }
    }

    pub(in super::super::super) fn route_total(
        &self,
        route_identity: &SourceRouteIdentity,
    ) -> usize {
        self.route_totals
            .get(route_identity)
            .copied()
            .unwrap_or_default()
    }

    pub(in super::super::super) fn checkpoint(
        &self,
        route_identity: SourceRouteIdentity,
    ) -> SourceBackedLogicalSourceFailureCheckpoint {
        SourceBackedLogicalSourceFailureCheckpoint {
            retained: self.failures.len(),
            omitted: self.omitted,
            route_total: self.route_total(&route_identity),
            route_identity,
        }
    }

    pub(in super::super::super) fn truncate(
        &mut self,
        checkpoint: SourceBackedLogicalSourceFailureCheckpoint,
    ) {
        self.failures.truncate(checkpoint.retained);
        self.omitted = checkpoint.omitted;
        if checkpoint.route_total == 0 {
            self.route_totals.remove(&checkpoint.route_identity);
        } else {
            self.route_totals
                .insert(checkpoint.route_identity, checkpoint.route_total);
        }
    }
}

pub(super) fn bounded_text(value: &str, maximum_bytes: usize) -> String {
    if value.len() <= maximum_bytes {
        return value.to_owned();
    }
    let mut boundary = maximum_bytes;
    while !value.is_char_boundary(boundary) {
        boundary = boundary.saturating_sub(1);
    }
    value[..boundary].to_owned()
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SourceBackedSourceFailures {
    failures: Vec<SourceBackedFailedRoute>,
    omitted: usize,
    class_totals: [usize; 4],
}

impl SourceBackedSourceFailures {
    pub fn from_failures(failures: impl IntoIterator<Item = SourceBackedFailedRoute>) -> Self {
        let mut bounded = Self::default();
        for failure in failures {
            bounded.record(failure);
        }
        bounded
    }

    pub fn failures(&self) -> &[SourceBackedFailedRoute] {
        &self.failures
    }

    pub fn omitted(&self) -> usize {
        self.omitted
    }

    pub fn total(&self) -> usize {
        self.failures.len().saturating_add(self.omitted)
    }

    pub fn class_total(&self, class: SourceBackedSourceFailureClass) -> usize {
        self.class_totals[class.index()]
    }

    pub fn is_empty(&self) -> bool {
        self.total() == 0
    }

    pub(in super::super::super) fn record(&mut self, failure: SourceBackedFailedRoute) {
        self.class_totals[failure.class.index()] =
            self.class_totals[failure.class.index()].saturating_add(1);
        if self.failures.len() < MAX_RECORDED_SOURCE_BACKED_FAILURES {
            self.failures.push(failure);
        } else {
            self.omitted = self.omitted.saturating_add(1);
        }
    }

    pub fn extend(&mut self, failures: impl IntoIterator<Item = SourceBackedFailedRoute>) {
        for failure in failures {
            self.record(failure);
        }
    }
}

impl std::ops::Deref for SourceBackedSourceFailures {
    type Target = [SourceBackedFailedRoute];

    fn deref(&self) -> &Self::Target {
        self.failures()
    }
}

impl fmt::Display for SourceBackedSourceFailures {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        const MAX_DISPLAYED_FAILURES: usize = 3;

        for (index, failure) in self
            .failures
            .iter()
            .take(MAX_DISPLAYED_FAILURES)
            .enumerate()
        {
            if index != 0 {
                formatter.write_str("; ")?;
            }
            write!(
                formatter,
                "source-backed scan failed for {} at {}: {}: {}",
                failure.provider.as_str(),
                failure.source_selector,
                failure.class.display_label(),
                failure.detail,
            )?;
        }
        let undisplayed = self
            .total()
            .saturating_sub(self.failures.len().min(MAX_DISPLAYED_FAILURES));
        if undisplayed != 0 {
            write!(
                formatter,
                "; {undisplayed} additional source failure(s) omitted"
            )?;
        }
        Ok(())
    }
}

impl SourceBackedSourceFailureClass {
    fn display_label(self) -> &'static str {
        match self {
            Self::Unavailable => "unavailable (provider source unavailable)",
            Self::SourceChanged => {
                "source_changed (provider source changed during bounded capture)"
            }
            Self::Unreadable => "invalid_source (invalid capture payload)",
            Self::Incompatible => "unsupported (unsupported provider schema)",
        }
    }
}

#[cfg(test)]
mod failure_tests {
    use super::*;

    #[test]
    fn source_failure_diagnostics_bound_count_selector_detail_and_display() {
        let failures = SourceBackedSourceFailures::from_failures((0_u8..70).map(|index| {
            SourceBackedFailedRoute::new(
                SourceRouteIdentity::from_sha256(format!("{index:02x}").repeat(32)).unwrap(),
                format!("{:02x}", index.saturating_add(1)).repeat(32),
                CaptureProvider::Codex,
                SourceBackedSourceFailureClass::Unavailable,
                false,
                "é".repeat(MAX_SOURCE_BACKED_FAILURE_SELECTOR_BYTES),
                "δ".repeat(MAX_SOURCE_BACKED_FAILURE_DETAIL_BYTES),
            )
        }));

        assert_eq!(
            failures.failures().len(),
            MAX_RECORDED_SOURCE_BACKED_FAILURES
        );
        assert_eq!(failures.omitted(), 6);
        assert_eq!(failures.total(), 70);
        assert!(failures.failures().iter().all(|failure| {
            failure.source_selector.len() <= MAX_SOURCE_BACKED_FAILURE_SELECTOR_BYTES
                && failure.detail.len() <= MAX_SOURCE_BACKED_FAILURE_DETAIL_BYTES
                && failure
                    .source_selector
                    .is_char_boundary(failure.source_selector.len())
                && failure.detail.is_char_boundary(failure.detail.len())
        }));
        let displayed = failures.to_string();
        assert_eq!(displayed.matches("source-backed scan failed").count(), 3);
        assert!(displayed.contains("67 additional source failure(s) omitted"));
    }
}
