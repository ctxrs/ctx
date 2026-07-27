//! Bounded NativePath page and Pro-output primitives.
//!
//! Providers retain their own fast Core representation; this module defines no
//! universal event DTO or private facts. Output-only activation and catch-up
//! use `NativeProReplayPage`, which cannot carry Core payload or invoke the
//! canonical lane.
//!
//! Core commits first. A later Pro-output failure marks only that lane behind
//! and returns the bounded owned page for retry without reparsing.

use std::fmt;

use sha2::{Digest, Sha256};
use thiserror::Error;

use crate::{
    OutputNativeCursor, OutputObservationKind, OutputOutcome, OutputSourceIdentity,
    ProOutputMaterializationPage, ProOutputObservation, ProOutputPageResult, ProOutputSink,
    ProOutputSinkError, ProOutputSourceDisposition,
};

pub(crate) const NATIVE_INGESTION_PAGE_MAX_UNITS: usize = 64;
pub(crate) const NATIVE_INGESTION_PAGE_MAX_BYTES: usize = 8 * 1024 * 1024;
const NATIVE_INGESTION_FRONTIER_MAX_BYTES: usize = 256 * 1024;

/// A provider-certified, opaque native cursor at a safe page boundary.
#[derive(Clone, PartialEq, Eq)]
pub(crate) struct NativeSafeFrontier {
    pub(crate) version: u32,
    pub(crate) bytes: Vec<u8>,
}

impl NativeSafeFrontier {
    pub(crate) fn new(version: u32, bytes: Vec<u8>) -> Result<Self, NativeIngestionPageError> {
        if bytes.len() > NATIVE_INGESTION_FRONTIER_MAX_BYTES {
            return Err(NativeIngestionPageError::FrontierTooLarge { bytes: bytes.len() });
        }
        Ok(Self { version, bytes })
    }

    fn as_output_cursor(&self) -> OutputNativeCursor {
        OutputNativeCursor {
            version: self.version,
            payload: self.bytes.clone(),
        }
    }
}

impl fmt::Debug for NativeSafeFrontier {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("NativeSafeFrontier")
            .field("version", &self.version)
            .field("bytes", &self.bytes.len())
            .finish()
    }
}

/// Stable identity for one independently bounded transient output page.
///
/// This identity binds the output page's routing authority, frontiers,
/// terminal signal, metadata, and observations. It is intentionally absent
/// from Core page and group identity.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) struct NativeOutputPageIdentity([u8; 32]);

impl fmt::Debug for NativeOutputPageIdentity {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_tuple("NativeOutputPageIdentity")
            .field(&format_args!("{:02x?}", &self.0[..8]))
            .finish()
    }
}

fn hash_field(digest: &mut Sha256, value: &[u8]) {
    digest.update((value.len() as u64).to_le_bytes());
    digest.update(value);
}

fn hash_frontier(digest: &mut Sha256, frontier: &NativeSafeFrontier) {
    digest.update(frontier.version.to_le_bytes());
    hash_field(digest, &frontier.bytes);
}

fn hash_optional_field(digest: &mut Sha256, value: Option<&[u8]>) {
    digest.update([u8::from(value.is_some())]);
    if let Some(value) = value {
        hash_field(digest, value);
    }
}

fn hash_optional_u64(digest: &mut Sha256, value: Option<u64>) {
    digest.update([u8::from(value.is_some())]);
    if let Some(value) = value {
        digest.update(value.to_le_bytes());
    }
}

fn hash_optional_u32(digest: &mut Sha256, value: Option<u32>) {
    digest.update([u8::from(value.is_some())]);
    if let Some(value) = value {
        digest.update(value.to_le_bytes());
    }
}

fn hash_optional_i64(digest: &mut Sha256, value: Option<i64>) {
    digest.update([u8::from(value.is_some())]);
    if let Some(value) = value {
        digest.update(value.to_le_bytes());
    }
}

fn hash_optional_i32(digest: &mut Sha256, value: Option<i32>) {
    digest.update([u8::from(value.is_some())]);
    if let Some(value) = value {
        digest.update(value.to_le_bytes());
    }
}

impl NativeOutputPageIdentity {
    fn derive(
        source_identity: &NativeSourceIdentity,
        expected_frontier: &NativeSafeFrontier,
        next_safe_frontier: &NativeSafeFrontier,
        terminal: bool,
        output: &NativeProOutputPage,
    ) -> Self {
        let mut digest = Sha256::new();
        digest.update(b"ctx-native-output-page-v1\0");
        hash_field(&mut digest, source_identity.provider.as_bytes());
        hash_field(&mut digest, source_identity.source_identity.as_bytes());
        hash_frontier(&mut digest, expected_frontier);
        hash_frontier(&mut digest, next_safe_frontier);
        digest.update([u8::from(terminal)]);
        hash_pro_output_page(&mut digest, output);
        Self(digest.finalize().into())
    }
}

fn hash_pro_output_page(digest: &mut Sha256, output: &NativeProOutputPage) {
    digest.update(output.inventory_generation.to_le_bytes());
    hash_field(digest, output.source.provider.as_bytes());
    hash_field(digest, output.source.namespace_id.as_bytes());
    hash_field(digest, output.source.source_id.as_bytes());
    digest.update(output.source_epoch.to_le_bytes());
    hash_field(digest, output.observed_revision.as_bytes());
    hash_field(digest, output.parser_revision.as_bytes());
    hash_field(digest, output.materializer_revision.as_bytes());
    digest.update([match output.disposition {
        ProOutputSourceDisposition::AppendOrResume => 0,
        ProOutputSourceDisposition::NewSource => 1,
        ProOutputSourceDisposition::Rewrite => 2,
    }]);
    hash_optional_u64(digest, output.expected_prior_source_epoch);
    digest.update([u8::from(output.expected_prior_frontier.is_some())]);
    if let Some(frontier) = &output.expected_prior_frontier {
        hash_frontier(digest, frontier);
    }
    digest.update((output.observations.len() as u64).to_le_bytes());
    for observation in &output.observations {
        hash_output_observation(digest, observation);
    }
}

fn hash_output_observation(digest: &mut Sha256, observation: &ProOutputObservation) {
    digest.update([match observation.kind {
        OutputObservationKind::Command => 0,
        OutputObservationKind::Tool => 1,
    }]);
    hash_field(digest, observation.coordinate.unit_key.as_bytes());
    digest.update(observation.coordinate.native_sequence.to_le_bytes());
    hash_optional_field(
        digest,
        observation
            .coordinate
            .native_record_id
            .as_deref()
            .map(str::as_bytes),
    );
    hash_optional_u64(digest, observation.coordinate.source_record_ordinal);
    hash_optional_u32(digest, observation.coordinate.source_record_subrecord_index);
    hash_optional_u64(digest, observation.coordinate.byte_start);
    hash_optional_u64(digest, observation.coordinate.byte_end_exclusive);
    hash_optional_i64(digest, observation.occurred_at_unix_ms);
    hash_field(
        digest,
        observation.associations.direct_session_id.as_bytes(),
    );
    hash_field(digest, observation.associations.root_session_id.as_bytes());
    hash_optional_field(
        digest,
        observation
            .associations
            .parent_session_id
            .as_deref()
            .map(str::as_bytes),
    );
    hash_optional_field(
        digest,
        observation
            .associations
            .provider_session_id
            .as_deref()
            .map(str::as_bytes),
    );
    hash_optional_field(
        digest,
        observation
            .associations
            .agent_id
            .as_deref()
            .map(str::as_bytes),
    );
    digest.update([u8::from(observation.associations.repository.is_some())]);
    if let Some(repository) = &observation.associations.repository {
        hash_field(digest, repository.repository_id.as_bytes());
        hash_optional_field(digest, repository.checkout_id.as_deref().map(str::as_bytes));
        hash_optional_field(digest, repository.worktree_id.as_deref().map(str::as_bytes));
        hash_optional_field(
            digest,
            repository.object_format.as_deref().map(str::as_bytes),
        );
    }
    hash_optional_field(digest, observation.call_id.as_deref().map(str::as_bytes));
    digest.update([u8::from(observation.command.is_some())]);
    if let Some(command) = &observation.command {
        hash_field(digest, command.tool_name.as_bytes());
        hash_field(digest, command.command.as_bytes());
        hash_optional_field(
            digest,
            command.working_directory.as_deref().map(str::as_bytes),
        );
    }
    digest.update([match observation.outcome.outcome {
        OutputOutcome::Success => 0,
        OutputOutcome::Failure => 1,
        OutputOutcome::Timeout => 2,
        OutputOutcome::Unknown => 3,
    }]);
    hash_optional_i32(digest, observation.outcome.exit_code);
    hash_optional_u64(digest, observation.outcome.duration_ms);
    digest.update(observation.locator.version.to_le_bytes());
    hash_field(digest, observation.locator.kind.as_bytes());
    hash_field(digest, &observation.locator.payload);
    hash_field(digest, &observation.content);
}

/// Provider-certified conservative accounting for the full owned page.
///
/// `conservative_serialized_bytes` includes the provider-specific Core
/// encoding, the routing source key, both safe frontier/checkpoint encodings,
/// and any transient Pro output encoding retained for retry. The coordinator
/// revalidates each page and sums this complete claim before Core sees a group.
/// The claim does not change Core or group identity.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct NativePageAccounting {
    pub(crate) logical_units: usize,
    pub(crate) conservative_serialized_bytes: usize,
}

/// Metadata needed to adapt transient observations to `ProOutputSink`.
///
/// Its expected cursor is independent from the Core frontier because output
/// materialization has its own durable cursor.  The next cursor is always the
/// enclosing page's provider-certified safe frontier.
#[derive(Debug)]
pub(crate) struct NativeProOutputPage {
    pub(crate) inventory_generation: u64,
    pub(crate) source: OutputSourceIdentity,
    pub(crate) source_epoch: u64,
    pub(crate) observed_revision: String,
    pub(crate) parser_revision: String,
    pub(crate) materializer_revision: String,
    pub(crate) disposition: ProOutputSourceDisposition,
    pub(crate) expected_prior_source_epoch: Option<u64>,
    pub(crate) expected_prior_frontier: Option<NativeSafeFrontier>,
    pub(crate) observations: Vec<ProOutputObservation>,
}

/// One bounded output-only source range for grouped delivery or later replay.
///
/// The source frontier can differ from the output sink's expected prior cursor
/// during a rewrite: source replay starts from a reset safe prefix while the
/// sink compare-and-swaps its prior epoch/cursor. This page owns neither Core
/// payload nor canonical-journal input, and it is transient retry state rather
/// than a durable outbox.
#[derive(Debug)]
pub(crate) struct NativeProReplayPage {
    pub(crate) identity: NativeOutputPageIdentity,
    pub(crate) next_safe_frontier: NativeSafeFrontier,
    pub(crate) terminal: bool,
    pub(crate) accounting: NativePageAccounting,
    output: NativeProOutputPage,
}

impl NativeProReplayPage {
    /// Builds an output-only page using the output sink source as its routing
    /// authority. Group adapters with a distinct provider-native routing key
    /// should use `new_with_source_identity`.
    pub(crate) fn new(
        expected_frontier: NativeSafeFrontier,
        next_safe_frontier: NativeSafeFrontier,
        terminal: bool,
        accounting: NativePageAccounting,
        output: NativeProOutputPage,
    ) -> Result<Self, NativeIngestionPageError> {
        let source_identity = NativeSourceIdentity::from_output_source(&output.source);
        Self::new_with_source_identity(
            source_identity,
            expected_frontier,
            next_safe_frontier,
            terminal,
            accounting,
            output,
        )
    }

    pub(crate) fn new_with_source_identity(
        source_identity: NativeSourceIdentity,
        expected_frontier: NativeSafeFrontier,
        next_safe_frontier: NativeSafeFrontier,
        terminal: bool,
        accounting: NativePageAccounting,
        output: NativeProOutputPage,
    ) -> Result<Self, NativeIngestionPageError> {
        validate_page_accounting(accounting)?;
        let identity = NativeOutputPageIdentity::derive(
            &source_identity,
            &expected_frontier,
            &next_safe_frontier,
            terminal,
            &output,
        );
        validate_known_owned_payload_bytes(
            accounting,
            known_replay_owned_payload_bytes(&identity, &next_safe_frontier, &output),
        )?;
        Ok(Self {
            identity,
            next_safe_frontier,
            terminal,
            accounting,
            output,
        })
    }
}

/// One owned bounded page.  `C` remains provider-specific Core data.
#[derive(Debug)]
pub(crate) struct NativeIngestionPage<C> {
    pub(crate) expected_frontier: NativeSafeFrontier,
    pub(crate) next_safe_frontier: NativeSafeFrontier,
    pub(crate) terminal: bool,
    pub(crate) accounting: NativePageAccounting,
    pub(crate) core: C,
}

impl<C> NativeIngestionPage<C> {
    pub(crate) fn new(
        expected_frontier: NativeSafeFrontier,
        next_safe_frontier: NativeSafeFrontier,
        terminal: bool,
        accounting: NativePageAccounting,
        core: C,
    ) -> Result<Self, NativeIngestionPageError> {
        validate_page_accounting(accounting)?;
        validate_known_owned_payload_bytes(
            accounting,
            known_ingestion_page_owned_payload_bytes(&expected_frontier, &next_safe_frontier),
        )?;
        Ok(Self {
            expected_frontier,
            next_safe_frontier,
            terminal,
            accounting,
            core,
        })
    }
}

fn validate_page_accounting(
    accounting: NativePageAccounting,
) -> Result<(), NativeIngestionPageError> {
    if accounting.logical_units > NATIVE_INGESTION_PAGE_MAX_UNITS {
        return Err(NativeIngestionPageError::TooManyLogicalUnits {
            units: accounting.logical_units,
        });
    }
    if accounting.conservative_serialized_bytes > NATIVE_INGESTION_PAGE_MAX_BYTES {
        return Err(NativeIngestionPageError::TooManySerializedBytes {
            bytes: accounting.conservative_serialized_bytes,
        });
    }
    Ok(())
}

fn validate_known_owned_payload_bytes(
    accounting: NativePageAccounting,
    minimum: usize,
) -> Result<(), NativeIngestionPageError> {
    if accounting.conservative_serialized_bytes < minimum {
        return Err(NativeIngestionPageError::OwnedEncodedBytesUnderreported {
            claimed: accounting.conservative_serialized_bytes,
            minimum,
        });
    }
    Ok(())
}

#[derive(Default)]
struct NativeOwnedEncodedByteCounter {
    bytes: usize,
}

impl NativeOwnedEncodedByteCounter {
    fn add_fixed(&mut self, bytes: usize) {
        self.bytes = self.bytes.saturating_add(bytes);
    }

    fn add_bytes(&mut self, bytes: &[u8]) {
        self.add_fixed(size_of::<u64>());
        self.add_fixed(bytes.len());
    }

    fn add_string(&mut self, value: &str) {
        self.add_bytes(value.as_bytes());
    }

    fn add_optional_string(&mut self, value: Option<&str>) {
        self.add_fixed(size_of::<u8>());
        if let Some(value) = value {
            self.add_string(value);
        }
    }

    fn add_optional_fixed(&mut self, present: bool, bytes: usize) {
        self.add_fixed(size_of::<u8>());
        if present {
            self.add_fixed(bytes);
        }
    }

    fn add_frontier(&mut self, frontier: &NativeSafeFrontier) {
        self.add_fixed(size_of::<u32>());
        self.add_bytes(&frontier.bytes);
    }

    fn add_optional_frontier(&mut self, frontier: Option<&NativeSafeFrontier>) {
        self.add_fixed(size_of::<u8>());
        if let Some(frontier) = frontier {
            self.add_frontier(frontier);
        }
    }

    fn finish(self) -> usize {
        self.bytes
    }
}

fn known_ingestion_page_owned_payload_bytes(
    expected_frontier: &NativeSafeFrontier,
    next_safe_frontier: &NativeSafeFrontier,
) -> usize {
    let mut counter = NativeOwnedEncodedByteCounter::default();
    counter.add_frontier(expected_frontier);
    counter.add_frontier(next_safe_frontier);
    counter.finish()
}

fn known_replay_owned_payload_bytes(
    _identity: &NativeOutputPageIdentity,
    next_safe_frontier: &NativeSafeFrontier,
    output: &NativeProOutputPage,
) -> usize {
    let mut counter = NativeOwnedEncodedByteCounter::default();
    counter.add_fixed(32);
    counter.add_frontier(next_safe_frontier);
    counter.add_fixed(size_of::<u8>());
    add_known_pro_output_payload_bytes(&mut counter, output);
    counter.finish()
}

fn add_known_pro_output_payload_bytes(
    counter: &mut NativeOwnedEncodedByteCounter,
    output: &NativeProOutputPage,
) {
    counter.add_fixed(size_of::<u64>());
    counter.add_string(&output.source.provider);
    counter.add_string(&output.source.namespace_id);
    counter.add_string(&output.source.source_id);
    counter.add_fixed(size_of::<u64>());
    counter.add_string(&output.observed_revision);
    counter.add_string(&output.parser_revision);
    counter.add_string(&output.materializer_revision);
    counter.add_fixed(size_of::<u8>());
    counter.add_optional_fixed(
        output.expected_prior_source_epoch.is_some(),
        size_of::<u64>(),
    );
    counter.add_optional_frontier(output.expected_prior_frontier.as_ref());
    counter.add_fixed(size_of::<u64>());
    for observation in &output.observations {
        add_known_output_observation_payload_bytes(counter, observation);
    }
}

fn add_known_output_observation_payload_bytes(
    counter: &mut NativeOwnedEncodedByteCounter,
    observation: &ProOutputObservation,
) {
    counter.add_fixed(size_of::<u8>());
    counter.add_string(&observation.coordinate.unit_key);
    counter.add_fixed(size_of::<u64>());
    counter.add_optional_string(observation.coordinate.native_record_id.as_deref());
    counter.add_optional_fixed(
        observation.coordinate.source_record_ordinal.is_some(),
        size_of::<u64>(),
    );
    counter.add_optional_fixed(
        observation
            .coordinate
            .source_record_subrecord_index
            .is_some(),
        size_of::<u32>(),
    );
    counter.add_optional_fixed(
        observation.coordinate.byte_start.is_some(),
        size_of::<u64>(),
    );
    counter.add_optional_fixed(
        observation.coordinate.byte_end_exclusive.is_some(),
        size_of::<u64>(),
    );
    counter.add_optional_fixed(observation.occurred_at_unix_ms.is_some(), size_of::<i64>());
    counter.add_string(&observation.associations.direct_session_id);
    counter.add_string(&observation.associations.root_session_id);
    counter.add_optional_string(observation.associations.parent_session_id.as_deref());
    counter.add_optional_string(observation.associations.provider_session_id.as_deref());
    counter.add_optional_string(observation.associations.agent_id.as_deref());
    counter.add_fixed(size_of::<u8>());
    if let Some(repository) = &observation.associations.repository {
        counter.add_string(&repository.repository_id);
        counter.add_optional_string(repository.checkout_id.as_deref());
        counter.add_optional_string(repository.worktree_id.as_deref());
        counter.add_optional_string(repository.object_format.as_deref());
    }
    counter.add_optional_string(observation.call_id.as_deref());
    counter.add_fixed(size_of::<u8>());
    if let Some(command) = &observation.command {
        counter.add_string(&command.tool_name);
        counter.add_string(&command.command);
        counter.add_optional_string(command.working_directory.as_deref());
    }
    counter.add_fixed(size_of::<u8>());
    counter.add_optional_fixed(observation.outcome.exit_code.is_some(), size_of::<i32>());
    counter.add_optional_fixed(observation.outcome.duration_ms.is_some(), size_of::<u64>());
    counter.add_fixed(size_of::<u32>());
    counter.add_string(&observation.locator.kind);
    counter.add_bytes(&observation.locator.payload);
    counter.add_bytes(&observation.content);
}

/// A source authority/routing key, never provider projection data.
#[derive(Clone, PartialEq, Eq, Hash)]
pub(crate) struct NativeSourceIdentity {
    provider: String,
    source_identity: String,
}

impl NativeSourceIdentity {
    pub(crate) fn new(provider: impl Into<String>, source_identity: impl Into<String>) -> Self {
        Self {
            provider: provider.into(),
            source_identity: source_identity.into(),
        }
    }

    pub(crate) fn provider(&self) -> &str {
        &self.provider
    }

    pub(crate) fn source_identity(&self) -> &str {
        &self.source_identity
    }

    fn from_output_source(source: &OutputSourceIdentity) -> Self {
        Self {
            provider: source.provider.clone(),
            source_identity: format!(
                "output:{}:{}{}",
                source.namespace_id.len(),
                source.namespace_id,
                source.source_id
            ),
        }
    }
}

impl fmt::Debug for NativeSourceIdentity {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("NativeSourceIdentity")
            .field("provider_bytes", &self.provider.len())
            .field("source_identity_bytes", &self.source_identity.len())
            .finish()
    }
}

/// One owned page plus the source authority key used to route its cursor.
#[derive(Debug)]
pub(crate) struct NativePublicationPage<C> {
    source_identity: NativeSourceIdentity,
    page: NativeIngestionPage<C>,
}

impl<C> NativePublicationPage<C> {
    pub(crate) fn new(source_identity: NativeSourceIdentity, page: NativeIngestionPage<C>) -> Self {
        Self {
            source_identity,
            page,
        }
    }

    pub(crate) fn into_parts(self) -> (NativeSourceIdentity, NativeIngestionPage<C>) {
        (self.source_identity, self.page)
    }
}

#[derive(Debug, Error, PartialEq, Eq)]
pub(crate) enum NativeIngestionPageError {
    #[error(
        "NativePath page has {units} logical units; maximum is {NATIVE_INGESTION_PAGE_MAX_UNITS}"
    )]
    TooManyLogicalUnits { units: usize },
    #[error("NativePath page conservatively serializes to {bytes} bytes; maximum is {NATIVE_INGESTION_PAGE_MAX_BYTES}")]
    TooManySerializedBytes { bytes: usize },
    #[error("NativePath safe frontier has {bytes} bytes; maximum is {NATIVE_INGESTION_FRONTIER_MAX_BYTES}")]
    FrontierTooLarge { bytes: usize },
    #[error(
        "NativePath page claims {claimed} owned encoded payload bytes but its known frontier/output payload requires at least {minimum}"
    )]
    OwnedEncodedBytesUnderreported { claimed: usize, minimum: usize },
}

/// Sends one output-only page without making Core or canonical Pro available.
///
/// Failure returns the unchanged owned page, allowing an idempotent retry from
/// the same certified source range without reparsing. A successful terminal
/// page advances only the output sink; inventory/catalog completion remains a
/// separate orchestration concern.
pub(crate) fn process_pro_replay_only(
    page: NativeProReplayPage,
    output_sink: &dyn ProOutputSink,
) -> Result<NativeOutputPageReceipt, Box<NativeProReplayFailure>> {
    let output_page_identity = page.identity;
    let output_page =
        output_materialization_page(&page.output, &page.next_safe_frontier, page.terminal);
    match materialize_output_page(output_sink, output_page) {
        Ok(receipt) => Ok(NativeOutputPageReceipt {
            output_page_identity,
            receipt,
        }),
        Err(output_error) => Err(Box::new(NativeProReplayFailure { page, output_error })),
    }
}

#[derive(Debug)]
pub(crate) struct NativeOutputPageReceipt {
    #[allow(dead_code)]
    pub(crate) output_page_identity: NativeOutputPageIdentity,
    // Retained for callers that need the sink's full acknowledgement contract.
    #[allow(dead_code)]
    pub(crate) receipt: ProOutputPageResult,
}

#[derive(Debug)]
pub(crate) struct NativeProReplayFailure {
    #[allow(dead_code)]
    pub(crate) page: NativeProReplayPage,
    pub(crate) output_error: NativeOutputProFailure,
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct NativeOutputReceiptIdentity {
    pub(crate) source_epoch: u64,
    pub(crate) committed_cursor: OutputNativeCursor,
}

#[derive(Debug)]
pub(crate) enum NativeOutputProFailure {
    Sink(ProOutputSinkError),
    ReceiptMismatch {
        // Preserve both identities for typed retry diagnostics.
        #[allow(dead_code)]
        expected: NativeOutputReceiptIdentity,
        #[allow(dead_code)]
        actual: NativeOutputReceiptIdentity,
    },
}

impl NativeOutputProFailure {
    fn behind_signal(&self) -> ProOutputSinkError {
        match self {
            Self::Sink(error) => error.clone(),
            Self::ReceiptMismatch { .. } => ProOutputSinkError::new(
                "invalid_response",
                "output sink acknowledgement did not match the requested source epoch and cursor",
            ),
        }
    }
}

fn output_materialization_page(
    output: &NativeProOutputPage,
    next_safe_frontier: &NativeSafeFrontier,
    terminal: bool,
) -> ProOutputMaterializationPage {
    ProOutputMaterializationPage {
        inventory_generation: output.inventory_generation,
        source: output.source.clone(),
        source_epoch: output.source_epoch,
        observed_revision: output.observed_revision.clone(),
        parser_revision: output.parser_revision.clone(),
        materializer_revision: output.materializer_revision.clone(),
        disposition: output.disposition,
        expected_prior_source_epoch: output.expected_prior_source_epoch,
        expected_prior_cursor: output
            .expected_prior_frontier
            .as_ref()
            .map(NativeSafeFrontier::as_output_cursor),
        next_safe_cursor: next_safe_frontier.as_output_cursor(),
        terminal,
        observations: output
            .observations
            .iter()
            .map(clone_output_observation)
            .collect(),
    }
}

fn materialize_output_page(
    output_sink: &dyn ProOutputSink,
    output_page: ProOutputMaterializationPage,
) -> Result<ProOutputPageResult, NativeOutputProFailure> {
    let expected = NativeOutputReceiptIdentity {
        source_epoch: output_page.source_epoch,
        committed_cursor: output_page.next_safe_cursor.clone(),
    };
    let result = match output_sink.materialize_page(output_page) {
        Ok(result)
            if result.source_epoch == expected.source_epoch
                && result.committed_cursor == expected.committed_cursor =>
        {
            Ok(result)
        }
        Ok(result) => Err(NativeOutputProFailure::ReceiptMismatch {
            expected,
            actual: NativeOutputReceiptIdentity {
                source_epoch: result.source_epoch,
                committed_cursor: result.committed_cursor,
            },
        }),
        Err(error) => Err(NativeOutputProFailure::Sink(error)),
    };
    if let Err(error) = &result {
        output_sink.mark_behind(error.behind_signal());
    }
    result
}

// The sink consumes its page, so clone only the bounded transient payload and
// retain the original scanner page for a no-reparse retry.
fn clone_output_observation(observation: &ProOutputObservation) -> ProOutputObservation {
    ProOutputObservation {
        kind: observation.kind,
        coordinate: observation.coordinate.clone(),
        occurred_at_unix_ms: observation.occurred_at_unix_ms,
        associations: observation.associations.clone(),
        call_id: observation.call_id.clone(),
        command: observation.command.clone(),
        outcome: observation.outcome.clone(),
        locator: observation.locator.clone(),
        content: observation.content.clone(),
    }
}
