use std::{
    collections::{BTreeMap, BTreeSet},
    path::Path,
    sync::{Arc, Mutex},
};

use anyhow::{anyhow, bail, Context, Result};
use base64::{engine::general_purpose::STANDARD, Engine as _};
use ctx_history_capture::{
    ImportProfile, OutputAssociations as CaptureOutputAssociations,
    OutputCommandContext as CaptureOutputCommandContext,
    OutputNativeCoordinate as CaptureOutputNativeCoordinate,
    OutputNativeCursor as CaptureOutputNativeCursor,
    OutputObservationKind as CaptureOutputObservationKind, OutputOutcome as CaptureOutputOutcome,
    OutputOutcomeMetadata as CaptureOutputOutcomeMetadata,
    OutputRepositoryContext as CaptureOutputRepositoryContext,
    OutputSourceIdentity as CaptureOutputSourceIdentity,
    OutputSourceLocator as CaptureOutputSourceLocator,
    ProOutputMaterializationPage as CaptureOutputPage,
    ProOutputObservation as CaptureOutputObservation, ProOutputPageResult, ProOutputProgress,
    ProOutputSink, ProOutputSinkError, ProOutputSourceDisposition,
};
use ctx_history_core::{
    CertifiedSource, CertifiedSourceDeletion, CertifiedSourceInventory, EventHydrationRequest,
    SessionHydrationRequest, SourceFrontier, SourceKey, SourceRecordLocator, StableEntityId,
};
use ctx_pro_host_protocol::{
    BeginOutputInventoryRequest, Capability, FinishOutputInventoryRequest, HelperMessage,
    HostMessage, ObserveOutputSourceRequest, OutputAssociations, OutputCommandContext,
    OutputInventoryFinished, OutputNativeCoordinate, OutputNativeCursor, OutputObservationKind,
    OutputOutcome, OutputOutcomeMetadata, OutputProgressRequest, OutputProgressResult,
    OutputRepositoryContext, OutputSourceAvailability, OutputSourceDisposition,
    OutputSourceIdentity, OutputSourceLocator, ProOutputMaterializationPage, ProOutputObservation,
    TransientOutputContent, OUTPUT_MATERIALIZATION_CONTRACT_VERSION,
};
use sha2::{Digest, Sha256};

use super::{protocol_error, stable_error_code, ProClient, BATCH_TIMEOUT};

struct SharedProClient {
    client: Mutex<ProClient>,
}

impl SharedProClient {
    fn new(client: ProClient) -> Self {
        Self {
            client: Mutex::new(client),
        }
    }

    fn exchange(
        &self,
        message: HostMessage,
        timeout: std::time::Duration,
    ) -> Result<HelperMessage> {
        // Each lane holds the client only for one request/response exchange.
        // The coordinator invokes canonical and output sequentially, so no
        // adapter callback can re-enter this mutex while it is locked.
        self.client
            .lock()
            .map_err(|_| anyhow!("helper_crashed: Pro client lock was poisoned"))?
            .exchange(message, timeout)
    }
}

/// One helper connection and one immutable import profile selected before provider parsing.
///
/// The caller passes `profile()` through the public profiled import entrypoint and calls
/// `finish()` only after the complete source inventory succeeds.
pub(crate) struct ProOutputImport {
    profile: ImportProfile,
    sink: Arc<ClientProOutputSink>,
    finished: bool,
}

impl ProOutputImport {
    /// Selects CoreAndPro only when a helper negotiates the complete output capability.
    ///
    /// Import remains a Core operation when Pro is absent, disabled, unlicensed, or
    /// temporarily unavailable; later sink failures likewise never unwind Core commits.
    pub(crate) fn begin_if_available(data_root: &Path) -> Option<Self> {
        Self::begin(data_root).ok()
    }

    fn begin(data_root: &Path) -> Result<Self> {
        let required = BTreeSet::from([Capability::OutputMaterialization]);
        let client = ProClient::connect(data_root, &required)?;
        Self::begin_with_shared_client(Arc::new(SharedProClient::new(client)))
    }

    /// Compatibility entrypoint for the legacy materialization command.
    ///
    /// The second argument is deliberately generic and ignored: a Store
    /// projection checkpoint is not authority for output inventory or the
    /// source-backed canonical feed.
    pub(super) fn begin_with_client<LegacyFrontier>(
        client: ProClient,
        _legacy_frontier: Option<LegacyFrontier>,
    ) -> Result<Self> {
        Self::begin_with_shared_client(Arc::new(SharedProClient::new(client)))
    }

    fn begin_with_shared_client(client: Arc<SharedProClient>) -> Result<Self> {
        let progress = client.exchange(
            HostMessage::GetOutputProgress(OutputProgressRequest {
                sources: Vec::new(),
            }),
            BATCH_TIMEOUT,
        )?;
        let generation = output_inventory_generation(progress)?;
        let began = match client.exchange(
            HostMessage::BeginOutputInventory(BeginOutputInventoryRequest { generation }),
            BATCH_TIMEOUT,
        )? {
            HelperMessage::OutputInventoryBegan(began) => began,
            HelperMessage::Error(error) => return Err(protocol_error(error)),
            _ => bail!("invalid_response: helper returned a non-output-inventory response"),
        };
        began
            .validate()
            .map_err(|error| anyhow!("invalid_response: {}", error.message))?;
        if began.generation != generation {
            bail!("invalid_response: helper began the wrong output inventory generation");
        }
        let sink = Arc::new(ClientProOutputSink {
            client,
            inventory_generation: generation,
            materializer_revision: began.materializer_revision,
            behind: Arc::new(Mutex::new(None)),
        });
        let sink_trait: Arc<dyn ProOutputSink> = sink.clone();
        let profile = ImportProfile::CoreAndPro(sink_trait);
        Ok(Self {
            profile,
            sink,
            finished: false,
        })
    }

    pub(crate) fn profile(&self) -> &ImportProfile {
        &self.profile
    }

    pub(crate) fn replay_only_profile(&self) -> ImportProfile {
        let sink: Arc<dyn ProOutputSink> = self.sink.clone();
        ImportProfile::ProReplayOnly(sink)
    }

    pub(crate) fn mark_output_replay_behind(&self, error: &anyhow::Error) {
        self.sink.mark_behind(ProOutputSinkError::new(
            "nativepath_output_replay",
            error.to_string(),
        ));
    }

    /// Legacy call-site compatibility.
    ///
    /// Source-backed canonical catch-up consumes a committed source manifest
    /// independently through `sync_source_backed_pro_feed`; it is not advanced
    /// by a Core Store commit callback.
    pub(crate) fn note_core_source_committed(&mut self) {
        // Intentionally empty.
    }

    pub(crate) fn finish(mut self) -> Result<OutputInventoryFinished> {
        if let Some(error) = self
            .sink
            .behind
            .lock()
            .map_err(|_| anyhow!("helper_crashed: Pro output sink lock was poisoned"))?
            .clone()
        {
            bail!("{}: {}", error.code, error.message);
        }
        let response = self.sink.exchange(HostMessage::FinishOutputInventory(
            FinishOutputInventoryRequest {
                generation: self.sink.inventory_generation,
            },
        ))?;
        let finished = match response {
            HelperMessage::OutputInventoryFinished(finished) => finished,
            HelperMessage::Error(error) => return Err(protocol_error(error)),
            _ => bail!("invalid_response: helper returned a non-output-inventory response"),
        };
        if finished.generation != self.sink.inventory_generation {
            bail!("invalid_response: helper finished the wrong output inventory generation");
        }
        self.finished = true;
        Ok(finished)
    }

    pub(crate) fn finish_warning(error: &anyhow::Error) -> String {
        let code = stable_error_code(error).unwrap_or("pro_output_unavailable");
        format!(
            "Core history update succeeded, but Pro output catch-up remains incomplete ({code}); a later import or refresh will retry it"
        )
    }
}

/// Public source-authority seam awaiting the matching Pro protocol messages.
///
/// The module is production code, but its only current consumer is the focused
/// contract suite until the private helper implements `SourceBackedProConsumer`.
#[allow(dead_code)]
pub(crate) mod source_backed_feed {
    use super::*;

    pub(crate) const SOURCE_BACKED_PRO_FEED_CONTRACT_VERSION: u16 = 1;
    const MAX_TRANSIENT_CANONICAL_RECORD_BYTES: usize = 16 * 1024 * 1024;
    const MAX_TRANSIENT_CANONICAL_PAGE_BYTES: usize = 32 * 1024 * 1024;
    const MAX_CANONICAL_SOURCE_PAGES: u64 = 1_000_000;

    /// A Core-accepted deletion paired with the exact complete provider inventory
    /// that witnessed it.
    #[derive(Debug, Clone)]
    pub(crate) struct SourceBackedProRemoval {
        pub(crate) deletion: CertifiedSourceDeletion,
        pub(crate) inventory: CertifiedSourceInventory,
    }

    impl SourceBackedProRemoval {
        pub(crate) fn new(
            deletion: CertifiedSourceDeletion,
            inventory: CertifiedSourceInventory,
        ) -> Result<Self> {
            if !deletion.verifies(&inventory) {
                bail!("invalid_request: source deletion does not match its complete inventory");
            }
            Ok(Self {
                deletion,
                inventory,
            })
        }
    }

    /// Immutable, content-addressed Core source authority consumed by Pro.
    ///
    /// It contains source certificates and complete-inventory deletion witnesses,
    /// never provider record bodies. `core_generation_id` is the committed Core
    /// receipt and is not advanced or rolled back by this bridge.
    #[derive(Debug, Clone)]
    pub(crate) struct SourceBackedProManifest {
        pub(crate) contract_version: u16,
        pub(crate) core_generation_id: String,
        pub(crate) sources: Vec<CertifiedSource>,
        pub(crate) removals: Vec<SourceBackedProRemoval>,
    }

    impl SourceBackedProManifest {
        pub(crate) fn new(
            core_generation_id: impl Into<String>,
            mut sources: Vec<CertifiedSource>,
            mut removals: Vec<SourceBackedProRemoval>,
        ) -> Result<Self> {
            let core_generation_id = core_generation_id.into();
            if !is_sha256_hex(&core_generation_id) {
                bail!("invalid_request: Core generation ID is not canonical SHA-256");
            }
            for source in &sources {
                source.validate_contract().map_err(|error| {
                    anyhow!("invalid_request: invalid source certificate: {error}")
                })?;
            }
            sources.sort_by_key(source_identity_digest);
            if sources
                .windows(2)
                .any(|pair| source_identity_digest(&pair[0]) == source_identity_digest(&pair[1]))
            {
                bail!("invalid_request: source manifest contains duplicate source lineages");
            }
            removals.sort_by_key(|removal| removal.deletion.source().identity().digest());
            if removals.windows(2).any(|pair| {
                pair[0].deletion.source().identity().digest()
                    == pair[1].deletion.source().identity().digest()
            }) {
                bail!("invalid_request: source manifest contains duplicate deletion witnesses");
            }
            if removals
                .iter()
                .any(|removal| !removal.deletion.verifies(&removal.inventory))
            {
                bail!("invalid_request: source manifest contains an invalid deletion witness");
            }
            if removals.iter().any(|removal| {
                sources.iter().any(|source| {
                    source_identity_digest(source) == removal.deletion.source().identity().digest()
                })
            }) {
                bail!("invalid_request: source manifest both retains and deletes one source");
            }
            Ok(Self {
                contract_version: SOURCE_BACKED_PRO_FEED_CONTRACT_VERSION,
                core_generation_id,
                sources,
                removals,
            })
        }
    }

    #[derive(Debug, Clone, Default, PartialEq, Eq)]
    pub(crate) struct SourceBackedProRecordMetadata {
        pub(crate) provider_session_id: Option<String>,
        pub(crate) event_sequence: u64,
        pub(crate) occurred_at_unix_ms: Option<i64>,
        pub(crate) event_type: String,
        pub(crate) role: Option<String>,
        pub(crate) workspace: Option<String>,
        pub(crate) cwd: Option<String>,
        pub(crate) touched_files: Vec<String>,
    }

    /// One provider record hydrated through its typed locator for this exchange.
    ///
    /// `provider_bytes` is transient. Consumers must derive facts during
    /// `materialize_source_page` and retain only identities, locators, frontiers,
    /// and derived graph state.
    #[derive(Debug, Clone)]
    pub(crate) struct SourceBackedProRecord {
        pub(crate) event_id: StableEntityId,
        pub(crate) session_id: StableEntityId,
        pub(crate) locator: SourceRecordLocator,
        pub(crate) metadata: SourceBackedProRecordMetadata,
        pub(crate) provider_bytes: Vec<u8>,
    }

    impl SourceBackedProRecord {
        pub(crate) fn new(
            event_id: StableEntityId,
            session_id: StableEntityId,
            locator: SourceRecordLocator,
            metadata: SourceBackedProRecordMetadata,
            provider_bytes: Vec<u8>,
        ) -> Result<Self> {
            let record = Self {
                event_id,
                session_id,
                locator,
                metadata,
                provider_bytes,
            };
            record.validate()?;
            Ok(record)
        }

        fn validate(&self) -> Result<()> {
            if self.provider_bytes.len() > MAX_TRANSIENT_CANONICAL_RECORD_BYTES {
                bail!("invalid_request: transient provider record exceeds 16 MiB");
            }
            let event = EventHydrationRequest::new(self.event_id, self.locator.clone())
                .map_err(|error| anyhow!("invalid_request: invalid event locator: {error}"))?;
            SessionHydrationRequest::new(self.session_id, vec![event]).map_err(|error| {
                anyhow!("invalid_request: invalid session locator set: {error}")
            })?;
            let actual_digest: [u8; 32] = Sha256::digest(&self.provider_bytes).into();
            if actual_digest != *self.locator.record_digest() {
                bail!("source_changed: hydrated provider record failed locator digest validation");
            }
            Ok(())
        }
    }

    /// Provider-owned page produced by rereading an exact certified source.
    #[derive(Debug, Clone)]
    pub(crate) struct SourceBackedProviderPage {
        pub(crate) source: SourceKey,
        pub(crate) expected_prior_frontier: Option<SourceFrontier>,
        pub(crate) next_frontier: Option<SourceFrontier>,
        pub(crate) terminal: bool,
        pub(crate) records: Vec<SourceBackedProRecord>,
    }

    /// Content-free Pro progress for one canonical source.
    #[derive(Debug, Clone)]
    pub(crate) struct SourceBackedProProgress {
        pub(crate) source: SourceKey,
        pub(crate) source_epoch: u64,
        pub(crate) certified_revision: [u8; 32],
        pub(crate) frontier: Option<SourceFrontier>,
        pub(crate) materializer_revision: String,
        pub(crate) terminal: bool,
    }

    #[derive(Debug, Clone)]
    pub(crate) struct SourceBackedProConsumerState {
        pub(crate) materializer_revision: String,
        pub(crate) sources: Vec<SourceBackedProProgress>,
    }

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub(crate) enum SourceBackedProDisposition {
        NewSource,
        Resume,
        Rewrite,
    }

    #[derive(Debug, Clone)]
    pub(crate) struct PrepareSourceBackedProSource {
        pub(crate) core_generation_id: String,
        pub(crate) source: SourceKey,
        pub(crate) certified_revision: [u8; 32],
        pub(crate) materializer_revision: String,
        pub(crate) disposition: SourceBackedProDisposition,
        pub(crate) expected_prior: Option<SourceBackedProProgress>,
    }

    #[derive(Debug, Clone)]
    pub(crate) struct PreparedSourceBackedProSource {
        pub(crate) source: SourceKey,
        pub(crate) source_epoch: u64,
        pub(crate) certified_revision: [u8; 32],
        pub(crate) frontier: Option<SourceFrontier>,
        pub(crate) terminal: bool,
    }

    /// One independently CAS-guarded source page sent to Pro.
    #[derive(Debug)]
    pub(crate) struct MaterializeSourceBackedProPage {
        pub(crate) core_generation_id: String,
        pub(crate) source: SourceKey,
        pub(crate) source_epoch: u64,
        pub(crate) certified_revision: [u8; 32],
        pub(crate) expected_prior_frontier: Option<SourceFrontier>,
        pub(crate) next_frontier: Option<SourceFrontier>,
        pub(crate) terminal: bool,
        pub(crate) records: Vec<SourceBackedProRecord>,
    }

    #[derive(Debug, Clone)]
    pub(crate) struct MaterializedSourceBackedProPage {
        pub(crate) source: SourceKey,
        pub(crate) source_epoch: u64,
        pub(crate) certified_revision: [u8; 32],
        pub(crate) committed_frontier: Option<SourceFrontier>,
        pub(crate) terminal: bool,
        pub(crate) accepted_records: u64,
    }

    #[derive(Debug, Clone)]
    pub(crate) struct DeleteSourceBackedProSource {
        pub(crate) core_generation_id: String,
        pub(crate) removal: SourceBackedProRemoval,
        pub(crate) expected_prior: SourceBackedProProgress,
    }

    #[derive(Debug, Clone)]
    pub(crate) struct DeletedSourceBackedProSource {
        pub(crate) source: SourceKey,
        pub(crate) removed_source_epoch: u64,
    }

    #[derive(Debug, Clone)]
    pub(crate) struct SourceBackedProReceipt {
        pub(crate) core_generation_id: String,
        pub(crate) materializer_revision: String,
        pub(crate) sources: Vec<SourceBackedProProgress>,
    }

    #[derive(Debug, Clone)]
    pub(crate) struct SourceBackedProSyncReport {
        pub(crate) receipt: SourceBackedProReceipt,
        pub(crate) prepared_sources: u64,
        pub(crate) rewritten_sources: u64,
        pub(crate) deleted_sources: u64,
        pub(crate) reread_pages: u64,
        pub(crate) reread_records: u64,
    }

    /// Provider adapter boundary for canonical Pro catch-up.
    ///
    /// Implementations enumerate the committed source manifest separately, then
    /// reread native records from the supplied source frontier. They must hydrate
    /// every returned record from its `SourceRecordLocator`; canonical Store bytes
    /// are not an input to this interface.
    pub(crate) trait SourceBackedProProvider {
        fn reread_source_page(
            &mut self,
            source: &CertifiedSource,
            expected_prior_frontier: Option<&SourceFrontier>,
        ) -> Result<SourceBackedProviderPage>;
    }

    /// Public-side protocol adapter implemented by the Pro helper client.
    ///
    /// Every durable input and acknowledgement is content-free. Provider bytes
    /// cross only `materialize_source_page` and must be dropped before it returns.
    pub(crate) trait SourceBackedProConsumer {
        fn begin_source_manifest(
            &mut self,
            manifest: &SourceBackedProManifest,
        ) -> Result<SourceBackedProConsumerState>;

        fn prepare_source(
            &mut self,
            request: &PrepareSourceBackedProSource,
        ) -> Result<PreparedSourceBackedProSource>;

        fn materialize_source_page(
            &mut self,
            request: &MaterializeSourceBackedProPage,
        ) -> Result<MaterializedSourceBackedProPage>;

        fn delete_source(
            &mut self,
            request: &DeleteSourceBackedProSource,
        ) -> Result<DeletedSourceBackedProSource>;

        fn finish_source_manifest(
            &mut self,
            manifest: &SourceBackedProManifest,
            expected_sources: &[SourceBackedProProgress],
        ) -> Result<SourceBackedProReceipt>;
    }

    /// Reconciles Pro to one already-committed Core source manifest.
    ///
    /// Core publication is neither prepared nor committed here. An unavailable
    /// helper or provider reread leaves only Pro behind; a later or newly installed
    /// Pro instance resumes from its own per-source epoch/frontier.
    pub(crate) fn sync_source_backed_pro_feed<P, C>(
        manifest: SourceBackedProManifest,
        provider: &mut P,
        consumer: &mut C,
    ) -> Result<SourceBackedProSyncReport>
    where
        P: SourceBackedProProvider,
        C: SourceBackedProConsumer,
    {
        if manifest.contract_version != SOURCE_BACKED_PRO_FEED_CONTRACT_VERSION {
            bail!("invalid_request: unsupported source-backed Pro feed contract");
        }
        let consumer_state = consumer.begin_source_manifest(&manifest)?;
        validate_consumer_state(&consumer_state)?;
        let mut progress_by_source = BTreeMap::new();
        for progress in consumer_state.sources {
            let source_id = progress.source.identity().digest();
            if progress_by_source.insert(source_id, progress).is_some() {
                bail!("invalid_response: Pro returned duplicate canonical source progress");
            }
        }

        let current_source_ids = manifest
            .sources
            .iter()
            .map(source_identity_digest)
            .collect::<BTreeSet<_>>();
        let stale_source_ids = progress_by_source
            .keys()
            .filter(|source_id| !current_source_ids.contains(*source_id))
            .copied()
            .collect::<Vec<_>>();
        let mut deleted_sources = 0_u64;
        for source_id in stale_source_ids {
            let prior = progress_by_source
                .get(&source_id)
                .cloned()
                .ok_or_else(|| anyhow!("invalid_response: missing stale source progress"))?;
            let removal = manifest
            .removals
            .iter()
            .find(|removal| removal.deletion.source().identity().digest() == source_id)
            .ok_or_else(|| {
                anyhow!(
                    "source_changed: Pro source is absent from the manifest without a certified deletion"
                )
            })?;
            if !removal.deletion.source().exact_descriptor_eq(&prior.source) {
                bail!("source_changed: deletion witness does not describe Pro's source generation");
            }
            let request = DeleteSourceBackedProSource {
                core_generation_id: manifest.core_generation_id.clone(),
                removal: removal.clone(),
                expected_prior: prior.clone(),
            };
            let deleted = consumer.delete_source(&request)?;
            if !deleted.source.exact_descriptor_eq(&prior.source)
                || deleted.removed_source_epoch != prior.source_epoch
            {
                bail!("invalid_response: Pro acknowledged the wrong source deletion CAS");
            }
            progress_by_source.remove(&source_id);
            deleted_sources = deleted_sources.saturating_add(1);
        }

        let mut final_sources = Vec::with_capacity(manifest.sources.len());
        let mut prepared_sources = 0_u64;
        let mut rewritten_sources = 0_u64;
        let mut reread_pages = 0_u64;
        let mut reread_records = 0_u64;
        for source in &manifest.sources {
            let source_key = source.observation().source();
            let source_id = source_identity_digest(source);
            let certified_revision = certified_source_revision(source)?;
            let prior = progress_by_source.remove(&source_id);
            let disposition = match prior.as_ref() {
                None => SourceBackedProDisposition::NewSource,
                Some(progress)
                    if progress.source.exact_descriptor_eq(source_key)
                        && progress.certified_revision == certified_revision
                        && progress.materializer_revision
                            == consumer_state.materializer_revision =>
                {
                    SourceBackedProDisposition::Resume
                }
                Some(_) => SourceBackedProDisposition::Rewrite,
            };
            let prepare = PrepareSourceBackedProSource {
                core_generation_id: manifest.core_generation_id.clone(),
                source: source_key.clone(),
                certified_revision,
                materializer_revision: consumer_state.materializer_revision.clone(),
                disposition,
                expected_prior: prior.clone(),
            };
            let mut prepared = consumer.prepare_source(&prepare)?;
            validate_prepared_source(&prepare, &prepared)?;
            prepared_sources = prepared_sources.saturating_add(1);
            if disposition == SourceBackedProDisposition::Rewrite {
                rewritten_sources = rewritten_sources.saturating_add(1);
            }

            let mut source_pages = 0_u64;
            while !prepared.terminal {
                source_pages = source_pages.saturating_add(1);
                if source_pages > MAX_CANONICAL_SOURCE_PAGES {
                    bail!("invalid_response: provider source page count exceeds safety bound");
                }
                let page = provider.reread_source_page(source, prepared.frontier.as_ref())?;
                validate_provider_page(source, &prepared, &page)?;
                let accepted_records = u64::try_from(page.records.len())
                    .map_err(|_| anyhow!("invalid_request: source page record count overflow"))?;
                let request = MaterializeSourceBackedProPage {
                    core_generation_id: manifest.core_generation_id.clone(),
                    source: source_key.clone(),
                    source_epoch: prepared.source_epoch,
                    certified_revision,
                    expected_prior_frontier: prepared.frontier.clone(),
                    next_frontier: page.next_frontier,
                    terminal: page.terminal,
                    records: page.records,
                };
                let materialized = consumer.materialize_source_page(&request)?;
                validate_materialized_page(&request, &materialized, accepted_records)?;
                prepared.frontier = materialized.committed_frontier;
                prepared.terminal = materialized.terminal;
                reread_pages = reread_pages.saturating_add(1);
                reread_records = reread_records.saturating_add(accepted_records);
            }
            if prepared.frontier.as_ref() != source.frontier() {
                bail!("invalid_response: Pro terminated at the wrong certified source frontier");
            }
            final_sources.push(SourceBackedProProgress {
                source: source_key.clone(),
                source_epoch: prepared.source_epoch,
                certified_revision,
                frontier: prepared.frontier,
                materializer_revision: consumer_state.materializer_revision.clone(),
                terminal: true,
            });
        }
        if !progress_by_source.is_empty() {
            bail!("invalid_response: unreconciled Pro source progress remains");
        }
        final_sources.sort_by_key(|progress| progress.source.identity().digest());
        let receipt = consumer.finish_source_manifest(&manifest, &final_sources)?;
        validate_source_backed_receipt(
            &manifest,
            &consumer_state.materializer_revision,
            &final_sources,
            &receipt,
        )?;
        Ok(SourceBackedProSyncReport {
            receipt,
            prepared_sources,
            rewritten_sources,
            deleted_sources,
            reread_pages,
            reread_records,
        })
    }

    fn validate_consumer_state(state: &SourceBackedProConsumerState) -> Result<()> {
        if state.materializer_revision.is_empty() {
            bail!("invalid_response: Pro materializer revision is empty");
        }
        for progress in &state.sources {
            progress.source.validate_contract().map_err(|error| {
                anyhow!("invalid_response: invalid Pro source identity: {error}")
            })?;
            if progress.source_epoch == 0 || progress.materializer_revision.is_empty() {
                bail!("invalid_response: invalid Pro source progress");
            }
        }
        Ok(())
    }

    fn validate_prepared_source(
        request: &PrepareSourceBackedProSource,
        prepared: &PreparedSourceBackedProSource,
    ) -> Result<()> {
        if !prepared.source.exact_descriptor_eq(&request.source)
            || prepared.certified_revision != request.certified_revision
            || prepared.source_epoch == 0
        {
            bail!("invalid_response: Pro prepared the wrong canonical source");
        }
        match (request.disposition, request.expected_prior.as_ref()) {
            (SourceBackedProDisposition::NewSource, None) => {
                if prepared.frontier.is_some() || prepared.terminal {
                    bail!("invalid_response: new Pro source did not start from genesis");
                }
            }
            (SourceBackedProDisposition::Resume, Some(prior)) => {
                if prepared.source_epoch != prior.source_epoch
                    || prepared.frontier != prior.frontier
                    || prepared.terminal != prior.terminal
                {
                    bail!("invalid_response: Pro did not resume the exact source CAS");
                }
            }
            (SourceBackedProDisposition::Rewrite, Some(prior)) => {
                if prepared.source_epoch == prior.source_epoch
                    || prepared.frontier.is_some()
                    || prepared.terminal
                {
                    bail!("invalid_response: Pro did not invalidate the rewritten source epoch");
                }
            }
            _ => bail!("invalid_request: source disposition and prior progress disagree"),
        }
        Ok(())
    }

    fn validate_provider_page(
        source: &CertifiedSource,
        prepared: &PreparedSourceBackedProSource,
        page: &SourceBackedProviderPage,
    ) -> Result<()> {
        let source_key = source.observation().source();
        if !page.source.exact_descriptor_eq(source_key)
            || page.expected_prior_frontier != prepared.frontier
        {
            bail!("source_changed: provider returned a page for the wrong source frontier");
        }
        if page.terminal {
            if page.next_frontier.as_ref() != source.frontier() {
                bail!("source_changed: terminal provider page missed the certified frontier");
            }
        } else if page.records.is_empty()
            || page.next_frontier.is_none()
            || page.next_frontier == page.expected_prior_frontier
            || page.next_frontier.as_ref() == source.frontier()
        {
            bail!("source_changed: provider returned a non-progressing source page");
        }
        let mut page_bytes = 0_usize;
        let mut event_ids = BTreeSet::new();
        for record in &page.records {
            record.validate()?;
            if !record.locator.source().exact_descriptor_eq(source_key) {
                bail!("source_changed: provider record locator belongs to another source");
            }
            page_bytes = page_bytes
                .checked_add(record.provider_bytes.len())
                .ok_or_else(|| anyhow!("invalid_request: transient source page size overflow"))?;
            if page_bytes > MAX_TRANSIENT_CANONICAL_PAGE_BYTES {
                bail!("invalid_request: transient source page exceeds 32 MiB");
            }
            if !event_ids.insert(record.event_id.digest()) {
                bail!("invalid_request: source page contains a duplicate stable event ID");
            }
        }
        Ok(())
    }

    fn validate_materialized_page(
        request: &MaterializeSourceBackedProPage,
        materialized: &MaterializedSourceBackedProPage,
        accepted_records: u64,
    ) -> Result<()> {
        if !materialized.source.exact_descriptor_eq(&request.source)
            || materialized.source_epoch != request.source_epoch
            || materialized.certified_revision != request.certified_revision
            || materialized.committed_frontier != request.next_frontier
            || materialized.terminal != request.terminal
            || materialized.accepted_records != accepted_records
        {
            bail!("invalid_response: Pro acknowledged the wrong source page CAS");
        }
        Ok(())
    }

    fn validate_source_backed_receipt(
        manifest: &SourceBackedProManifest,
        materializer_revision: &str,
        expected_sources: &[SourceBackedProProgress],
        receipt: &SourceBackedProReceipt,
    ) -> Result<()> {
        if receipt.core_generation_id != manifest.core_generation_id
            || receipt.materializer_revision != materializer_revision
            || receipt.sources.len() != expected_sources.len()
            || !receipt
                .sources
                .iter()
                .zip(expected_sources)
                .all(|(actual, expected)| source_progress_exact_eq(actual, expected))
        {
            bail!("invalid_response: Pro published the wrong source-backed receipt");
        }
        Ok(())
    }

    fn source_progress_exact_eq(
        left: &SourceBackedProProgress,
        right: &SourceBackedProProgress,
    ) -> bool {
        left.source.exact_descriptor_eq(&right.source)
            && left.source_epoch == right.source_epoch
            && left.certified_revision == right.certified_revision
            && left.frontier == right.frontier
            && left.materializer_revision == right.materializer_revision
            && left.terminal == right.terminal
    }

    pub(crate) fn certified_source_revision(source: &CertifiedSource) -> Result<[u8; 32]> {
        source
            .validate_contract()
            .map_err(|error| anyhow!("invalid_request: invalid source certificate: {error}"))?;
        let bytes = serde_json::to_vec(source)
            .context("invalid_request: encode certified source revision")?;
        Ok(Sha256::digest(bytes).into())
    }

    fn source_identity_digest(source: &CertifiedSource) -> [u8; 32] {
        source.observation().source().identity().digest()
    }

    pub(crate) fn is_sha256_hex(value: &str) -> bool {
        value.len() == 64
            && value
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    }
}

impl Drop for ProOutputImport {
    fn drop(&mut self) {
        // An unfinished inventory deliberately remains incomplete. The helper uses that state to
        // invalidate missing-source conclusions after a failed or interrupted Core import.
        let _ = self.finished;
    }
}

fn output_inventory_generation(response: HelperMessage) -> Result<u64> {
    match response {
        HelperMessage::OutputProgress(progress) => next_output_inventory_generation(progress),
        HelperMessage::Error(error)
            if error.class == ctx_pro_host_protocol::ErrorClass::NotMaterialized =>
        {
            Ok(1)
        }
        HelperMessage::Error(error) => Err(protocol_error(error)),
        _ => bail!("invalid_response: helper returned a non-output-progress response"),
    }
}

fn next_output_inventory_generation(progress: OutputProgressResult) -> Result<u64> {
    if progress.inventory_generation == 0 {
        return Ok(1);
    }
    if progress.inventory_complete {
        progress.inventory_generation.checked_add(1).ok_or_else(|| {
            anyhow!("invalid_response: helper output inventory generation is exhausted")
        })
    } else {
        Ok(progress.inventory_generation)
    }
}

struct ClientProOutputSink {
    client: Arc<SharedProClient>,
    inventory_generation: u64,
    materializer_revision: String,
    behind: Arc<Mutex<Option<ProOutputSinkError>>>,
}

impl ClientProOutputSink {
    fn exchange(&self, message: HostMessage) -> Result<HelperMessage> {
        self.client.exchange(message, BATCH_TIMEOUT)
    }

    fn sink_error(error: anyhow::Error) -> ProOutputSinkError {
        ProOutputSinkError::new(
            stable_error_code(&error).unwrap_or("pro_output_unavailable"),
            error.to_string(),
        )
    }
}

impl ProOutputSink for ClientProOutputSink {
    fn inventory_generation(&self) -> u64 {
        self.inventory_generation
    }

    fn materializer_revision(&self) -> &str {
        &self.materializer_revision
    }

    fn observe_source(
        &self,
        source: &CaptureOutputSourceIdentity,
    ) -> std::result::Result<Option<ProOutputProgress>, ProOutputSinkError> {
        let source = protocol_source(source);
        let observed = self
            .exchange(HostMessage::ObserveOutputSource(
                ObserveOutputSourceRequest {
                    generation: self.inventory_generation,
                    source: source.clone(),
                    availability: OutputSourceAvailability::Available,
                },
            ))
            .map_err(Self::sink_error)?;
        match observed {
            HelperMessage::OutputSourceObserved(observed)
                if observed.generation == self.inventory_generation
                    && observed.source == source
                    && observed.availability == OutputSourceAvailability::Available => {}
            HelperMessage::Error(error) => return Err(Self::sink_error(protocol_error(error))),
            _ => {
                return Err(ProOutputSinkError::new(
                    "invalid_response",
                    "helper returned an invalid output-source acknowledgement",
                ));
            }
        }
        let progress = self
            .exchange(HostMessage::GetOutputProgress(OutputProgressRequest {
                sources: vec![source.clone()],
            }))
            .map_err(Self::sink_error)?;
        match progress {
            HelperMessage::OutputProgress(progress) => {
                if progress.inventory_generation != self.inventory_generation
                    || progress.sources.len() > 1
                    || progress
                        .sources
                        .first()
                        .is_some_and(|value| value.source != source)
                {
                    return Err(ProOutputSinkError::new(
                        "invalid_response",
                        "helper returned invalid output progress",
                    ));
                }
                progress
                    .sources
                    .into_iter()
                    .next()
                    .map(capture_progress)
                    .transpose()
            }
            HelperMessage::Error(error) => Err(Self::sink_error(protocol_error(error))),
            _ => Err(ProOutputSinkError::new(
                "invalid_response",
                "helper returned a non-output-progress response",
            )),
        }
    }

    fn materialize_page(
        &self,
        page: CaptureOutputPage,
    ) -> std::result::Result<ProOutputPageResult, ProOutputSinkError> {
        let response = self
            .exchange(HostMessage::MaterializeOutputPage(protocol_page(page)?))
            .map_err(Self::sink_error)?;
        match response {
            HelperMessage::OutputPageMaterialized(result) => Ok(ProOutputPageResult {
                source_epoch: result.source_epoch,
                committed_cursor: capture_cursor(result.committed_cursor)?,
                accepted_outputs: result.accepted_outputs,
                materialized_facts: result.materialized_facts,
                replayed: result.replayed,
            }),
            HelperMessage::Error(error) => Err(Self::sink_error(protocol_error(error))),
            _ => Err(ProOutputSinkError::new(
                "invalid_response",
                "helper returned a non-output-page response",
            )),
        }
    }

    fn mark_behind(&self, error: ProOutputSinkError) {
        if let Ok(mut behind) = self.behind.lock() {
            behind.get_or_insert(error);
        }
    }
}

fn protocol_source(source: &CaptureOutputSourceIdentity) -> OutputSourceIdentity {
    OutputSourceIdentity {
        provider: source.provider.clone(),
        namespace_id: source.namespace_id.clone(),
        source_id: source.source_id.clone(),
    }
}

fn protocol_cursor(cursor: CaptureOutputNativeCursor) -> OutputNativeCursor {
    OutputNativeCursor {
        version: cursor.version,
        payload_base64: STANDARD.encode(cursor.payload),
    }
}

fn capture_cursor(
    cursor: OutputNativeCursor,
) -> std::result::Result<CaptureOutputNativeCursor, ProOutputSinkError> {
    cursor.validate().map_err(|error| {
        ProOutputSinkError::new(
            "invalid_response",
            format!("invalid output cursor: {}", error.message),
        )
    })?;
    let payload = STANDARD.decode(cursor.payload_base64).map_err(|_| {
        ProOutputSinkError::new(
            "invalid_response",
            "helper returned invalid output cursor base64",
        )
    })?;
    Ok(CaptureOutputNativeCursor {
        version: cursor.version,
        payload,
    })
}

fn capture_progress(
    progress: ctx_pro_host_protocol::OutputSourceProgress,
) -> std::result::Result<ProOutputProgress, ProOutputSinkError> {
    Ok(ProOutputProgress {
        source_epoch: progress.source_epoch,
        observed_revision: progress.observed_revision,
        cursor: progress.cursor.map(capture_cursor).transpose()?,
        parser_revision: progress.parser_revision,
        materializer_revision: progress.materializer_revision,
        terminal: progress.terminal,
    })
}

fn protocol_page(
    page: CaptureOutputPage,
) -> std::result::Result<ProOutputMaterializationPage, ProOutputSinkError> {
    let observations = page
        .observations
        .into_iter()
        .map(protocol_observation)
        .collect::<std::result::Result<Vec<_>, _>>()?;
    let page = ProOutputMaterializationPage {
        contract_version: OUTPUT_MATERIALIZATION_CONTRACT_VERSION,
        inventory_generation: page.inventory_generation,
        source: protocol_source(&page.source),
        source_epoch: page.source_epoch,
        observed_revision: page.observed_revision,
        parser_revision: page.parser_revision,
        materializer_revision: page.materializer_revision,
        disposition: match page.disposition {
            ProOutputSourceDisposition::AppendOrResume => OutputSourceDisposition::AppendOrResume,
            ProOutputSourceDisposition::NewSource => OutputSourceDisposition::NewSource,
            ProOutputSourceDisposition::Rewrite => OutputSourceDisposition::Rewrite,
        },
        expected_prior_source_epoch: page.expected_prior_source_epoch,
        expected_prior_cursor: page.expected_prior_cursor.map(protocol_cursor),
        next_safe_cursor: protocol_cursor(page.next_safe_cursor),
        terminal: page.terminal,
        observations,
    };
    page.validate().map_err(|error| {
        ProOutputSinkError::new(
            "invalid_request",
            format!("invalid transient output page: {}", error.message),
        )
    })?;
    Ok(page)
}

fn protocol_observation(
    observation: CaptureOutputObservation,
) -> std::result::Result<ProOutputObservation, ProOutputSinkError> {
    let content = TransientOutputContent::from_bytes(&observation.content).ok_or_else(|| {
        ProOutputSinkError::new(
            "pro_output_record_too_large",
            "transient output exceeds the accepted 16 MiB record bound",
        )
    })?;
    Ok(ProOutputObservation {
        kind: match observation.kind {
            CaptureOutputObservationKind::Command => OutputObservationKind::Command,
            CaptureOutputObservationKind::Tool => OutputObservationKind::Tool,
        },
        coordinate: protocol_coordinate(observation.coordinate),
        occurred_at_unix_ms: observation.occurred_at_unix_ms,
        associations: protocol_associations(observation.associations),
        call_id: observation.call_id,
        command: observation.command.map(protocol_command),
        outcome: protocol_outcome(observation.outcome),
        locator: protocol_locator(observation.locator),
        content,
    })
}

fn protocol_coordinate(value: CaptureOutputNativeCoordinate) -> OutputNativeCoordinate {
    OutputNativeCoordinate {
        unit_key: value.unit_key,
        native_sequence: value.native_sequence,
        native_record_id: value.native_record_id,
        source_record_ordinal: value.source_record_ordinal,
        source_record_subrecord_index: value.source_record_subrecord_index,
        byte_start: value.byte_start,
        byte_end_exclusive: value.byte_end_exclusive,
    }
}

fn protocol_associations(value: CaptureOutputAssociations) -> OutputAssociations {
    OutputAssociations {
        direct_session_id: value.direct_session_id,
        root_session_id: value.root_session_id,
        parent_session_id: value.parent_session_id,
        provider_session_id: value.provider_session_id,
        agent_id: value.agent_id,
        repository: value.repository.map(protocol_repository),
    }
}

fn protocol_repository(value: CaptureOutputRepositoryContext) -> OutputRepositoryContext {
    OutputRepositoryContext {
        repository_id: value.repository_id,
        checkout_id: value.checkout_id,
        worktree_id: value.worktree_id,
        object_format: value.object_format,
    }
}

fn protocol_command(value: CaptureOutputCommandContext) -> OutputCommandContext {
    OutputCommandContext {
        tool_name: value.tool_name,
        command: value.command,
        working_directory: value.working_directory,
    }
}

fn protocol_outcome(value: CaptureOutputOutcomeMetadata) -> OutputOutcomeMetadata {
    OutputOutcomeMetadata {
        outcome: match value.outcome {
            CaptureOutputOutcome::Success => OutputOutcome::Success,
            CaptureOutputOutcome::Failure => OutputOutcome::Failure,
            CaptureOutputOutcome::Timeout => OutputOutcome::Timeout,
            CaptureOutputOutcome::Unknown => OutputOutcome::Unknown,
        },
        exit_code: value.exit_code,
        duration_ms: value.duration_ms,
    }
}

fn protocol_locator(value: CaptureOutputSourceLocator) -> OutputSourceLocator {
    OutputSourceLocator {
        version: value.version,
        kind: value.kind,
        payload_base64: STANDARD.encode(value.payload),
    }
}

#[cfg(test)]
mod tests {
    use super::source_backed_feed::*;
    use super::*;
    use std::fs;

    use ctx_history_capture::{ingest_codex_source_backed_v0, CodexLocatorResolverV0};
    use ctx_history_core::{
        CertifiedSource, CertifiedSourceDeletion, CertifiedSourceInventory, ScannedSourceCounts,
        SourceInventoryObservation, SourceObservation, TypedKey,
    };
    use ctx_history_index::VerifiedIndex;
    use ctx_pro_host_protocol::{ErrorClass, ProtocolError};
    use tempfile::tempdir;

    const MATERIALIZER_REVISION: &str = "pro-source-materializer-v1";

    #[derive(Debug, Clone)]
    struct FixtureSourceFeed {
        generation_id: String,
        source: CertifiedSource,
        records: Vec<SourceBackedProRecord>,
        intermediate_frontier: SourceFrontier,
    }

    impl FixtureSourceFeed {
        fn provider(&self) -> FixtureProvider {
            let terminal_frontier = self
                .source
                .frontier()
                .expect("Codex fixture has a certified frontier")
                .clone();
            FixtureProvider {
                pages: vec![
                    SourceBackedProviderPage {
                        source: self.source.observation().source().clone(),
                        expected_prior_frontier: None,
                        next_frontier: Some(self.intermediate_frontier.clone()),
                        terminal: false,
                        records: vec![self.records[0].clone()],
                    },
                    SourceBackedProviderPage {
                        source: self.source.observation().source().clone(),
                        expected_prior_frontier: Some(self.intermediate_frontier.clone()),
                        next_frontier: Some(terminal_frontier),
                        terminal: true,
                        records: vec![self.records[1].clone()],
                    },
                ],
                requests: Vec::new(),
            }
        }

        fn manifest(&self) -> SourceBackedProManifest {
            SourceBackedProManifest::new(
                self.generation_id.clone(),
                vec![self.source.clone()],
                Vec::new(),
            )
            .expect("fixture manifest")
        }
    }

    #[derive(Default)]
    struct FixtureProvider {
        pages: Vec<SourceBackedProviderPage>,
        requests: Vec<Option<SourceFrontier>>,
    }

    impl SourceBackedProProvider for FixtureProvider {
        fn reread_source_page(
            &mut self,
            source: &CertifiedSource,
            expected_prior_frontier: Option<&SourceFrontier>,
        ) -> Result<SourceBackedProviderPage> {
            self.requests.push(expected_prior_frontier.cloned());
            self.pages
                .iter()
                .find(|page| {
                    page.source
                        .exact_descriptor_eq(source.observation().source())
                        && page.expected_prior_frontier.as_ref() == expected_prior_frontier
                })
                .cloned()
                .ok_or_else(|| anyhow!("fixture provider has no matching source page"))
        }
    }

    struct FixtureConsumer {
        materializer_revision: String,
        progress: BTreeMap<[u8; 32], SourceBackedProProgress>,
        durable_event_ids: BTreeMap<[u8; 32], BTreeSet<[u8; 32]>>,
        transient_record_digests: Vec<[u8; 32]>,
        dispositions: Vec<SourceBackedProDisposition>,
        deleted_epochs: Vec<u64>,
        finish_called: bool,
        corrupt_page_ack: bool,
    }

    impl FixtureConsumer {
        fn new(progress: Vec<SourceBackedProProgress>) -> Self {
            Self {
                materializer_revision: MATERIALIZER_REVISION.to_owned(),
                progress: progress
                    .into_iter()
                    .map(|progress| (progress.source.identity().digest(), progress))
                    .collect(),
                durable_event_ids: BTreeMap::new(),
                transient_record_digests: Vec::new(),
                dispositions: Vec::new(),
                deleted_epochs: Vec::new(),
                finish_called: false,
                corrupt_page_ack: false,
            }
        }

        fn durable_ids_for(&self, source: &SourceKey) -> BTreeSet<[u8; 32]> {
            self.durable_event_ids
                .get(&source.identity().digest())
                .cloned()
                .unwrap_or_default()
        }
    }

    impl SourceBackedProConsumer for FixtureConsumer {
        fn begin_source_manifest(
            &mut self,
            _manifest: &SourceBackedProManifest,
        ) -> Result<SourceBackedProConsumerState> {
            Ok(SourceBackedProConsumerState {
                materializer_revision: self.materializer_revision.clone(),
                sources: self.progress.values().cloned().collect(),
            })
        }

        fn prepare_source(
            &mut self,
            request: &PrepareSourceBackedProSource,
        ) -> Result<PreparedSourceBackedProSource> {
            assert!(is_sha256_hex(&request.core_generation_id));
            assert_eq!(request.materializer_revision, self.materializer_revision);
            self.dispositions.push(request.disposition);
            let (source_epoch, frontier, terminal) = match request.disposition {
                SourceBackedProDisposition::NewSource => (1, None, false),
                SourceBackedProDisposition::Resume => {
                    let prior = request.expected_prior.as_ref().expect("resume progress");
                    (prior.source_epoch, prior.frontier.clone(), prior.terminal)
                }
                SourceBackedProDisposition::Rewrite => {
                    let prior = request.expected_prior.as_ref().expect("rewrite progress");
                    self.durable_event_ids
                        .remove(&request.source.identity().digest());
                    (prior.source_epoch.saturating_add(1), None, false)
                }
            };
            Ok(PreparedSourceBackedProSource {
                source: request.source.clone(),
                source_epoch,
                certified_revision: request.certified_revision,
                frontier,
                terminal,
            })
        }

        fn materialize_source_page(
            &mut self,
            request: &MaterializeSourceBackedProPage,
        ) -> Result<MaterializedSourceBackedProPage> {
            assert!(is_sha256_hex(&request.core_generation_id));
            let source_id = request.source.identity().digest();
            let durable = self.durable_event_ids.entry(source_id).or_default();
            for record in &request.records {
                self.transient_record_digests
                    .push(Sha256::digest(&record.provider_bytes).into());
                durable.insert(record.event_id.digest());
            }
            self.progress.insert(
                source_id,
                SourceBackedProProgress {
                    source: request.source.clone(),
                    source_epoch: request.source_epoch,
                    certified_revision: request.certified_revision,
                    frontier: request.next_frontier.clone(),
                    materializer_revision: self.materializer_revision.clone(),
                    terminal: request.terminal,
                },
            );
            let accepted_records = u64::try_from(request.records.len())
                .expect("fixture page record count fits u64")
                .saturating_add(u64::from(self.corrupt_page_ack));
            Ok(MaterializedSourceBackedProPage {
                source: request.source.clone(),
                source_epoch: request.source_epoch,
                certified_revision: request.certified_revision,
                committed_frontier: request.next_frontier.clone(),
                terminal: request.terminal,
                accepted_records,
            })
        }

        fn delete_source(
            &mut self,
            request: &DeleteSourceBackedProSource,
        ) -> Result<DeletedSourceBackedProSource> {
            assert!(is_sha256_hex(&request.core_generation_id));
            assert!(request
                .removal
                .deletion
                .source()
                .exact_descriptor_eq(&request.expected_prior.source));
            let source_id = request.expected_prior.source.identity().digest();
            self.progress.remove(&source_id);
            self.durable_event_ids.remove(&source_id);
            self.deleted_epochs
                .push(request.expected_prior.source_epoch);
            Ok(DeletedSourceBackedProSource {
                source: request.expected_prior.source.clone(),
                removed_source_epoch: request.expected_prior.source_epoch,
            })
        }

        fn finish_source_manifest(
            &mut self,
            manifest: &SourceBackedProManifest,
            expected_sources: &[SourceBackedProProgress],
        ) -> Result<SourceBackedProReceipt> {
            self.finish_called = true;
            self.progress = expected_sources
                .iter()
                .cloned()
                .map(|progress| (progress.source.identity().digest(), progress))
                .collect();
            Ok(SourceBackedProReceipt {
                core_generation_id: manifest.core_generation_id.clone(),
                materializer_revision: self.materializer_revision.clone(),
                sources: expected_sources.to_vec(),
            })
        }
    }

    #[test]
    fn first_activation_starts_inventory_when_output_progress_is_absent() {
        let generation = output_inventory_generation(HelperMessage::Error(ProtocolError::new(
            ErrorClass::NotMaterialized,
            "graph does not exist",
        )))
        .expect("first activation generation");

        assert_eq!(generation, 1);
    }

    #[test]
    fn inventory_generation_resumes_incomplete_and_advances_complete_runs() {
        assert_eq!(
            next_output_inventory_generation(OutputProgressResult {
                inventory_generation: 7,
                inventory_complete: false,
                sources: Vec::new(),
            })
            .unwrap(),
            7
        );
        assert_eq!(
            next_output_inventory_generation(OutputProgressResult {
                inventory_generation: 7,
                inventory_complete: true,
                sources: Vec::new(),
            })
            .unwrap(),
            8
        );
        assert_eq!(
            next_output_inventory_generation(OutputProgressResult {
                inventory_generation: 0,
                inventory_complete: false,
                sources: Vec::new(),
            })
            .unwrap(),
            1
        );
    }

    #[test]
    fn finish_failure_warning_is_explicit_and_nonfatal() {
        let warning = ProOutputImport::finish_warning(&anyhow!("helper_timeout"));

        assert!(warning.contains("Core history update succeeded"));
        assert!(warning.contains("Pro output catch-up remains incomplete"));
        assert!(warning.contains("helper_timeout"));
    }

    #[test]
    fn new_and_lagging_pro_reread_public_codex_records_from_independent_frontiers() {
        let fixture = public_codex_fixture();

        let mut new_provider = fixture.provider();
        let mut new_consumer = FixtureConsumer::new(Vec::new());
        let new_report =
            sync_source_backed_pro_feed(fixture.manifest(), &mut new_provider, &mut new_consumer)
                .expect("new Pro source catch-up");

        assert_eq!(
            new_consumer.dispositions,
            [SourceBackedProDisposition::NewSource]
        );
        assert_eq!(new_report.reread_pages, 2);
        assert_eq!(new_report.reread_records, 2);
        assert_eq!(new_report.prepared_sources, 1);
        assert_eq!(new_report.receipt.core_generation_id, fixture.generation_id);
        assert_eq!(
            new_provider.requests,
            [None, Some(fixture.intermediate_frontier.clone())]
        );
        assert_eq!(new_consumer.transient_record_digests.len(), 2);
        assert_eq!(
            new_consumer.durable_ids_for(fixture.source.observation().source()),
            fixture
                .records
                .iter()
                .map(|record| record.event_id.digest())
                .collect()
        );

        let revision = certified_source_revision(&fixture.source).expect("source revision");
        let lagging_progress = SourceBackedProProgress {
            source: fixture.source.observation().source().clone(),
            source_epoch: 7,
            certified_revision: revision,
            frontier: Some(fixture.intermediate_frontier.clone()),
            materializer_revision: MATERIALIZER_REVISION.to_owned(),
            terminal: false,
        };
        let mut lagging_provider = fixture.provider();
        let mut lagging_consumer = FixtureConsumer::new(vec![lagging_progress]);
        lagging_consumer
            .durable_event_ids
            .entry(fixture.source.observation().source().identity().digest())
            .or_default()
            .insert(fixture.records[0].event_id.digest());
        let lagging_report = sync_source_backed_pro_feed(
            fixture.manifest(),
            &mut lagging_provider,
            &mut lagging_consumer,
        )
        .expect("lagging Pro source catch-up");

        assert_eq!(
            lagging_consumer.dispositions,
            [SourceBackedProDisposition::Resume]
        );
        assert_eq!(
            lagging_provider.requests,
            [Some(fixture.intermediate_frontier)]
        );
        assert_eq!(lagging_report.reread_pages, 1);
        assert_eq!(lagging_report.reread_records, 1);
        assert_eq!(lagging_report.rewritten_sources, 0);
        assert_eq!(
            lagging_consumer.durable_ids_for(fixture.source.observation().source()),
            fixture
                .records
                .iter()
                .map(|record| record.event_id.digest())
                .collect()
        );
    }

    #[test]
    fn rewritten_source_invalidates_old_epoch_before_stable_id_reread() {
        let fixture = public_codex_fixture();
        let rewritten = rewritten_certificate(&fixture.source);
        let old_revision = certified_source_revision(&fixture.source).expect("old revision");
        let prior = SourceBackedProProgress {
            source: fixture.source.observation().source().clone(),
            source_epoch: 11,
            certified_revision: old_revision,
            frontier: fixture.source.frontier().cloned(),
            materializer_revision: MATERIALIZER_REVISION.to_owned(),
            terminal: true,
        };
        let source_id = prior.source.identity().digest();
        let retained_record = fixture.records[1].clone();
        let mut consumer = FixtureConsumer::new(vec![prior]);
        consumer.durable_event_ids.insert(
            source_id,
            fixture
                .records
                .iter()
                .map(|record| record.event_id.digest())
                .collect(),
        );
        let mut provider = FixtureProvider {
            pages: vec![SourceBackedProviderPage {
                source: rewritten.observation().source().clone(),
                expected_prior_frontier: None,
                next_frontier: rewritten.frontier().cloned(),
                terminal: true,
                records: vec![retained_record.clone()],
            }],
            requests: Vec::new(),
        };
        let manifest =
            SourceBackedProManifest::new("c".repeat(64), vec![rewritten], Vec::new()).unwrap();

        let report = sync_source_backed_pro_feed(manifest, &mut provider, &mut consumer)
            .expect("rewrite catch-up");

        assert_eq!(consumer.dispositions, [SourceBackedProDisposition::Rewrite]);
        assert_eq!(report.rewritten_sources, 1);
        assert_eq!(provider.requests, [None]);
        assert_eq!(
            consumer.durable_ids_for(fixture.source.observation().source()),
            BTreeSet::from([retained_record.event_id.digest()])
        );
        assert!(!consumer
            .durable_ids_for(fixture.source.observation().source())
            .contains(&fixture.records[0].event_id.digest()));
        assert_eq!(report.receipt.sources[0].source_epoch, 12);
    }

    #[test]
    fn certified_inventory_deletion_removes_source_and_missing_proof_fails_closed() {
        let fixture = public_codex_fixture();
        let source = fixture.source.observation().source().clone();
        let prior = SourceBackedProProgress {
            source: source.clone(),
            source_epoch: 17,
            certified_revision: certified_source_revision(&fixture.source).unwrap(),
            frontier: fixture.source.frontier().cloned(),
            materializer_revision: MATERIALIZER_REVISION.to_owned(),
            terminal: true,
        };
        let inventory_observation = SourceInventoryObservation::new(
            source.provider(),
            "fixture-codex-root",
            TypedKey::utf8("public-codex-fixture").unwrap(),
            "fixture-inventory-v1",
            vec![1],
        )
        .unwrap();
        let inventory = CertifiedSourceInventory::certify(
            inventory_observation.clone(),
            inventory_observation,
            "fixture-discovery-v1",
            Vec::new(),
        )
        .unwrap();
        let deletion = CertifiedSourceDeletion::from_inventory(source.clone(), &inventory).unwrap();
        let removal = SourceBackedProRemoval::new(deletion, inventory).unwrap();
        let manifest =
            SourceBackedProManifest::new("d".repeat(64), Vec::new(), vec![removal]).unwrap();
        let mut provider = FixtureProvider::default();
        let mut consumer = FixtureConsumer::new(vec![prior.clone()]);
        consumer.durable_event_ids.insert(
            source.identity().digest(),
            BTreeSet::from([fixture.records[0].event_id.digest()]),
        );

        let report = sync_source_backed_pro_feed(manifest, &mut provider, &mut consumer)
            .expect("certified deletion");

        assert_eq!(report.deleted_sources, 1);
        assert_eq!(consumer.deleted_epochs, [17]);
        assert!(consumer.durable_ids_for(&source).is_empty());
        assert!(consumer.finish_called);
        assert!(report.receipt.sources.is_empty());

        let manifest_without_proof =
            SourceBackedProManifest::new("e".repeat(64), Vec::new(), Vec::new()).unwrap();
        let mut provider = FixtureProvider::default();
        let mut consumer = FixtureConsumer::new(vec![prior]);
        let error =
            sync_source_backed_pro_feed(manifest_without_proof, &mut provider, &mut consumer)
                .expect_err("missing deletion proof must fail");

        assert!(error.to_string().contains("without a certified deletion"));
        assert!(!consumer.finish_called);
    }

    #[test]
    fn mismatched_source_page_ack_never_publishes_manifest_receipt() {
        let fixture = public_codex_fixture();
        let mut provider = fixture.provider();
        let mut consumer = FixtureConsumer::new(Vec::new());
        consumer.corrupt_page_ack = true;

        let error = sync_source_backed_pro_feed(fixture.manifest(), &mut provider, &mut consumer)
            .expect_err("wrong CAS acknowledgement");

        assert!(error.to_string().contains("wrong source page CAS"));
        assert!(!consumer.finish_called);
    }

    fn public_codex_fixture() -> FixtureSourceFeed {
        let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
        let manifest_dir = if manifest_dir.is_absolute() {
            manifest_dir.to_path_buf()
        } else {
            std::env::current_dir()
                .expect("Bazel test current directory")
                .join(manifest_dir)
        };
        let public_fixture_root = fs::canonicalize(
            manifest_dir.join("../../tests/fixtures/provider-history/codex-sessions"),
        )
        .expect("canonical public Codex fixture root");
        let temp = tempdir().expect("temporary source-backed index");
        let fixture_root = temp.path().join("sessions");
        let fixture_day = fixture_root.join("2026/06/23");
        fs::create_dir_all(&fixture_day).expect("fixture destination");
        for filename in ["root.jsonl", "subagent.jsonl"] {
            fs::copy(
                public_fixture_root.join("2026/06/23").join(filename),
                fixture_day.join(filename),
            )
            .expect("copy public Codex fixture into ordinary test files");
        }
        let index_root = temp.path().join("index");
        ingest_codex_source_backed_v0(&fixture_root, &index_root)
            .expect("ingest public Codex fixture");
        let index = VerifiedIndex::open(&index_root).expect("open fixture source manifest");
        let candidate = index
            .search_event_candidates("Follow repo instructions", 1)
            .expect("search fixture")
            .into_iter()
            .next()
            .expect("fixture event");
        let events = index
            .events_for_session(candidate.event.session_id.as_uuid())
            .expect("fixture session events");
        assert!(events.len() >= 2);
        let source_key = events[0].locator.source().clone();
        let source = index
            .manifest()
            .sources
            .iter()
            .find(|source| {
                source
                    .observation()
                    .source()
                    .exact_descriptor_eq(&source_key)
            })
            .expect("fixture source certificate")
            .clone();
        let resolver =
            CodexLocatorResolverV0::discover([&fixture_root]).expect("fixture locator resolver");
        let records = events
            .into_iter()
            .take(2)
            .map(|event| {
                let hydrated = resolver
                    .hydrate(&event.locator)
                    .expect("hydrate fixture record");
                SourceBackedProRecord::new(
                    event.event_id,
                    event.session_id,
                    event.locator,
                    SourceBackedProRecordMetadata {
                        provider_session_id: event.provider_session_id,
                        event_sequence: event.event_sequence,
                        occurred_at_unix_ms: event.occurred_at_unix_ms,
                        event_type: event.event_type,
                        role: event.role,
                        workspace: event.workspace,
                        cwd: event.cwd,
                        touched_files: event.touched_files,
                    },
                    hydrated.provider_bytes,
                )
                .expect("source-backed Pro record")
            })
            .collect::<Vec<_>>();
        let intermediate_frontier =
            SourceFrontier::new("fixture-event", TypedKey::U64(1), 1, [1; 32])
                .expect("intermediate source frontier");
        FixtureSourceFeed {
            generation_id: index.generation_id().to_owned(),
            source,
            records,
            intermediate_frontier,
        }
    }

    fn rewritten_certificate(base: &CertifiedSource) -> CertifiedSource {
        let source = base.observation().source().clone();
        let observation = SourceObservation::new(source, "fixture-rewrite-v1", vec![9]).unwrap();
        let counts = ScannedSourceCounts {
            certified_bytes: base.counts().certified_bytes,
            ..base.counts()
        };
        let digest = [9; 32];
        let frontier = SourceFrontier::new(
            "fixture-rewrite-record",
            TypedKey::U64(counts.complete_records),
            counts.certified_bytes,
            digest,
        )
        .unwrap();

        CertifiedSource::certify_with_frontier(
            observation.clone(),
            observation,
            base.parser_revision(),
            digest,
            counts,
            Some(frontier),
        )
        .unwrap()
    }
}
