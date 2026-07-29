use std::collections::{BTreeMap, BTreeSet, HashMap};

use crate::entitlement::{base64url, decode_base64url, AUTHORIZATION_CHALLENGE_BYTES};
use crate::error::{ErrorClass, ProtocolError};
use crate::journal::{JournalCheckpoint, JournalSyncMode, JournalSyncRequest, JournalSyncResult};
use crate::lifecycle::{
    ConfirmGraphKeyDeletionRequest, GraphKeyDeleted, GraphKeyDeletionPrepared,
    PrepareGraphKeyDeletionRequest, GRAPH_KEY_DELETION_CHALLENGE_BYTES,
};
use crate::message::{
    Capability, GraphState, HelloRequest, HelloResult, HelperEnvelope, HelperMessage, HostEnvelope,
    HostMessage, MaterializationAuthority, StatusResult,
};
use crate::query::{BlameRequest, BlameTarget, QuerySnapshotExpectation, ResolvedBlameTarget};
use crate::{
    BeginOutputInventoryRequest, BlameResult, FinishOutputInventoryRequest, GitSnapshot,
    ObserveOutputSourceRequest, OutputInventoryBegan, OutputInventoryFinished,
    OutputPageMaterialized, OutputProgressRequest, OutputProgressResult, OutputSourceAvailability,
    OutputSourceDisposition, OutputSourceIdentity, OutputSourceObserved, OutputSourceProgress,
    ProOutputMaterializationPage, ResourceKind, ResourceRef, WorktreeStatus, PROTOCOL_FINGERPRINT,
    PROTOCOL_VERSION,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FakeBlameFailure {
    SourceUnavailable,
    RepositoryUnavailable,
    StaleSnapshot,
}

impl FakeBlameFailure {
    const fn class(self) -> ErrorClass {
        match self {
            Self::SourceUnavailable => ErrorClass::MissingSource,
            Self::RepositoryUnavailable => ErrorClass::MissingRepository,
            Self::StaleSnapshot => ErrorClass::StaleSnapshot,
        }
    }

    const fn message(self) -> &'static str {
        match self {
            Self::SourceUnavailable => "canonical source is unavailable",
            Self::RepositoryUnavailable => "repository is unavailable",
            Self::StaleSnapshot => "blame snapshot is stale",
        }
    }
}

/// Deterministic in-process Protocol V1 helper used by public conformance tests.
#[derive(Debug)]
pub struct FakeHelper {
    helper_version: String,
    expected_sequence: u64,
    negotiated: bool,
    capabilities: BTreeSet<Capability>,
    negotiated_capabilities: BTreeSet<Capability>,
    checkpoint: Option<JournalCheckpoint>,
    accepted_requests: HashMap<(u64, u64, u64), Vec<u8>>,
    active_output_inventory: Option<u64>,
    completed_output_inventory: u64,
    output_observations: BTreeMap<OutputSourceIdentity, OutputSourceAvailability>,
    output_progress: BTreeMap<OutputSourceIdentity, OutputSourceProgress>,
    accepted_output_pages: BTreeMap<(OutputSourceIdentity, u64, String), Vec<u8>>,
    blame_failure: Option<FakeBlameFailure>,
    graph_key_deletion_challenge: Option<([u8; GRAPH_KEY_DELETION_CHALLENGE_BYTES], String)>,
    graph_key_present: bool,
}

impl Default for FakeHelper {
    fn default() -> Self {
        Self::new("fake-helper-v1")
    }
}

impl FakeHelper {
    pub fn new(helper_version: impl Into<String>) -> Self {
        Self {
            helper_version: helper_version.into(),
            expected_sequence: 0,
            negotiated: false,
            capabilities: BTreeSet::from([
                Capability::GraphKeyDeletion,
                Capability::Status,
                Capability::JournalSync,
                Capability::OutputMaterialization,
                Capability::Query,
                Capability::GitRead,
            ]),
            negotiated_capabilities: BTreeSet::new(),
            checkpoint: None,
            accepted_requests: HashMap::new(),
            active_output_inventory: None,
            completed_output_inventory: 0,
            output_observations: BTreeMap::new(),
            output_progress: BTreeMap::new(),
            accepted_output_pages: BTreeMap::new(),
            blame_failure: None,
            graph_key_deletion_challenge: None,
            graph_key_present: true,
        }
    }

    #[must_use]
    pub const fn with_blame_failure(mut self, failure: FakeBlameFailure) -> Self {
        self.blame_failure = Some(failure);
        self
    }

    pub fn handle(&mut self, request: HostEnvelope) -> HelperEnvelope {
        let sequence = request.sequence;
        let request_id = request.request_id;
        if sequence != self.expected_sequence {
            return HelperEnvelope {
                sequence,
                request_id,
                message: HelperMessage::Error(ProtocolError::new(
                    ErrorClass::Sequence,
                    format!(
                        "expected sequence {}, received {sequence}",
                        self.expected_sequence
                    ),
                )),
            };
        }
        self.expected_sequence = self.expected_sequence.saturating_add(1);
        let message = match request.message {
            HostMessage::Hello(hello) => self.handle_hello(hello),
            HostMessage::Authorize(_) if self.negotiated => HelperMessage::Error(
                ProtocolError::new(ErrorClass::InvalidRequest, "authorization is not supported"),
            ),
            HostMessage::PrepareGraphKeyDeletion(request)
                if self.selected(Capability::GraphKeyDeletion) =>
            {
                self.handle_prepare_graph_key_deletion(request)
            }
            HostMessage::ConfirmGraphKeyDeletion(request)
                if self.selected(Capability::GraphKeyDeletion) =>
            {
                self.handle_confirm_graph_key_deletion(request)
            }
            HostMessage::Status(_) if self.selected(Capability::Status) => {
                HelperMessage::Status(StatusResult {
                    state: self
                        .checkpoint
                        .as_ref()
                        .map_or(GraphState::NotMaterialized, |_| GraphState::Ready),
                    authority: MaterializationAuthority::Journal,
                    checkpoint: self.checkpoint.clone(),
                    source_receipt: None,
                })
            }
            HostMessage::SyncJournal(request) if self.selected(Capability::JournalSync) => {
                self.handle_journal_sync(request)
            }
            HostMessage::BeginOutputInventory(request)
                if self.selected(Capability::OutputMaterialization) =>
            {
                self.handle_begin_output_inventory(request)
            }
            HostMessage::ObserveOutputSource(request)
                if self.selected(Capability::OutputMaterialization) =>
            {
                self.handle_observe_output_source(request)
            }
            HostMessage::MaterializeOutputPage(page)
                if self.selected(Capability::OutputMaterialization) =>
            {
                self.handle_materialize_output_page(page)
            }
            HostMessage::FinishOutputInventory(request)
                if self.selected(Capability::OutputMaterialization) =>
            {
                self.handle_finish_output_inventory(request)
            }
            HostMessage::GetOutputProgress(request)
                if self.selected(Capability::OutputMaterialization) =>
            {
                self.handle_output_progress(request)
            }
            HostMessage::Blame(request) if self.selected(Capability::Query) => {
                self.handle_blame(request)
            }
            _ if !self.negotiated => HelperMessage::Error(ProtocolError::new(
                ErrorClass::ProtocolMismatch,
                "exact Protocol V1 hello must be the first request",
            )),
            _ => HelperMessage::Error(ProtocolError::new(
                ErrorClass::ProtocolMismatch,
                "requested capability was not negotiated",
            )),
        };
        HelperEnvelope {
            sequence,
            request_id,
            message,
        }
    }

    fn selected(&self, capability: Capability) -> bool {
        self.negotiated && self.negotiated_capabilities.contains(&capability)
    }

    fn handle_hello(&mut self, hello: HelloRequest) -> HelperMessage {
        if self.negotiated {
            return HelperMessage::Error(ProtocolError::new(
                ErrorClass::ProtocolMismatch,
                "hello was already completed",
            ));
        }
        if hello.protocol_version != PROTOCOL_VERSION
            || hello.protocol_fingerprint != PROTOCOL_FINGERPRINT
        {
            return HelperMessage::Error(ProtocolError::new(
                ErrorClass::ProtocolMismatch,
                format!(
                    "host contract {}:{} does not exactly match helper contract {}:{}",
                    hello.protocol_version,
                    hello.protocol_fingerprint,
                    PROTOCOL_VERSION,
                    PROTOCOL_FINGERPRINT
                ),
            ));
        }
        self.negotiated = true;
        self.negotiated_capabilities = self
            .capabilities
            .intersection(&hello.capabilities)
            .copied()
            .collect();
        HelperMessage::Hello(HelloResult {
            protocol_version: PROTOCOL_VERSION,
            protocol_fingerprint: PROTOCOL_FINGERPRINT.to_owned(),
            helper_version: self.helper_version.clone(),
            capabilities: self.negotiated_capabilities.clone(),
            authorization_challenge_base64url: base64url(&[0x42; AUTHORIZATION_CHALLENGE_BYTES]),
        })
    }

    fn handle_prepare_graph_key_deletion(
        &mut self,
        request: PrepareGraphKeyDeletionRequest,
    ) -> HelperMessage {
        if decode_base64url(&request.installation_key_thumbprint)
            .as_deref()
            .map(<[u8]>::len)
            != Some(32)
        {
            return HelperMessage::Error(ProtocolError::new(
                ErrorClass::InvalidRequest,
                "installation key thumbprint is invalid",
            ));
        }
        let challenge = [0x33; GRAPH_KEY_DELETION_CHALLENGE_BYTES];
        self.graph_key_deletion_challenge = Some((challenge, request.installation_key_thumbprint));
        HelperMessage::GraphKeyDeletionPrepared(GraphKeyDeletionPrepared {
            challenge_base64url: base64url(&challenge),
            expires_at_unix: i64::MAX,
            key_present: self.graph_key_present,
        })
    }

    fn handle_confirm_graph_key_deletion(
        &mut self,
        request: ConfirmGraphKeyDeletionRequest,
    ) -> HelperMessage {
        let Some((challenge, installation_key_thumbprint)) =
            self.graph_key_deletion_challenge.take()
        else {
            return HelperMessage::Error(ProtocolError::new(
                ErrorClass::InvalidRequest,
                "graph-key deletion challenge is missing or expired",
            ));
        };
        if decode_base64url(&request.authorization.challenge_base64url).as_deref()
            != Some(challenge.as_slice())
            || request
                .authorization
                .entitlement
                .grant
                .installation_key_thumbprint
                != installation_key_thumbprint
        {
            return HelperMessage::Error(ProtocolError::new(
                ErrorClass::InvalidRequest,
                "graph-key deletion confirmation is invalid",
            ));
        }
        let deleted = self.graph_key_present;
        self.graph_key_present = false;
        HelperMessage::GraphKeyDeleted(GraphKeyDeleted { deleted })
    }

    fn handle_journal_sync(&mut self, request: JournalSyncRequest) -> HelperMessage {
        if let Err(error) = request.validate() {
            return HelperMessage::Error(error);
        }
        let committed = request.committed_checkpoint();
        let key = (
            request.prior_checkpoint.position.generation,
            request.prior_checkpoint.position.sequence,
            committed.position.sequence,
        );
        let encoded = match serde_json::to_vec(&request) {
            Ok(encoded) => encoded,
            Err(_) => {
                return HelperMessage::Error(ProtocolError::new(
                    ErrorClass::Internal,
                    "journal request could not be encoded",
                ));
            }
        };
        let replayed = match self.accepted_requests.get(&key) {
            Some(previous) if previous == &encoded => true,
            Some(_) => {
                return HelperMessage::Error(ProtocolError::new(
                    ErrorClass::Corrupt,
                    "journal checkpoint was reused with different contents",
                ));
            }
            None => false,
        };
        if !replayed {
            let starts_new_baseline = request.mode == JournalSyncMode::FullBaseline
                && request.prior_checkpoint.position.sequence == 0;
            if !starts_new_baseline && self.checkpoint.as_ref() != Some(&request.prior_checkpoint) {
                return HelperMessage::Error(ProtocolError::new(
                    ErrorClass::Sequence,
                    "journal prior checkpoint does not match durable helper state",
                ));
            }
            self.accepted_requests.insert(key, encoded);
            self.checkpoint = Some(committed.clone());
        }
        let frozen_complete = committed == request.frozen_through;
        HelperMessage::JournalSynced(JournalSyncResult {
            committed_through: committed,
            accepted_records: if replayed {
                0
            } else {
                request.records.len() as u32
            },
            replayed,
            frozen_complete,
        })
    }

    fn handle_begin_output_inventory(
        &mut self,
        request: BeginOutputInventoryRequest,
    ) -> HelperMessage {
        if let Err(error) = request.validate() {
            return HelperMessage::Error(error);
        }
        if self
            .active_output_inventory
            .is_some_and(|active| active != request.generation)
            || request.generation < self.completed_output_inventory
        {
            return HelperMessage::Error(ProtocolError::new(
                ErrorClass::Sequence,
                "output inventory generation is not the active generation",
            ));
        }
        if self.active_output_inventory != Some(request.generation) {
            self.output_observations.clear();
        }
        self.active_output_inventory = Some(request.generation);
        HelperMessage::OutputInventoryBegan(OutputInventoryBegan {
            generation: request.generation,
            materializer_revision: "fake-materializer-v1".to_owned(),
        })
    }

    fn handle_observe_output_source(
        &mut self,
        request: ObserveOutputSourceRequest,
    ) -> HelperMessage {
        if let Err(error) = request.validate() {
            return HelperMessage::Error(error);
        }
        if self.active_output_inventory != Some(request.generation) {
            return HelperMessage::Error(ProtocolError::new(
                ErrorClass::Sequence,
                "output source observation is outside its active inventory",
            ));
        }
        self.output_observations
            .insert(request.source.clone(), request.availability);
        HelperMessage::OutputSourceObserved(OutputSourceObserved {
            generation: request.generation,
            source: request.source,
            availability: request.availability,
        })
    }

    fn handle_materialize_output_page(
        &mut self,
        page: ProOutputMaterializationPage,
    ) -> HelperMessage {
        if let Err(error) = page.validate() {
            return HelperMessage::Error(error);
        }
        if self.active_output_inventory != Some(page.inventory_generation) {
            return HelperMessage::Error(ProtocolError::new(
                ErrorClass::Sequence,
                "output page is outside its active inventory",
            ));
        }
        let encoded = match serde_json::to_vec(&page) {
            Ok(encoded) => encoded,
            Err(_) => {
                return HelperMessage::Error(ProtocolError::new(
                    ErrorClass::Internal,
                    "output page could not be encoded",
                ));
            }
        };
        let page_key = (
            page.source.clone(),
            page.source_epoch,
            format!(
                "{}:{}",
                page.next_safe_cursor.version, page.next_safe_cursor.payload_base64
            ),
        );
        if let Some(previous) = self.accepted_output_pages.get(&page_key) {
            if previous != &encoded {
                return HelperMessage::Error(ProtocolError::new(
                    ErrorClass::Corrupt,
                    "output cursor was reused with different page contents",
                ));
            }
            return HelperMessage::OutputPageMaterialized(output_page_result(&page, true));
        }
        let prior_matches = match (&page.disposition, self.output_progress.get(&page.source)) {
            (OutputSourceDisposition::NewSource, None) => true,
            (OutputSourceDisposition::AppendOrResume, Some(progress)) => {
                page.expected_prior_source_epoch == Some(progress.source_epoch)
                    && page.expected_prior_cursor == progress.cursor
            }
            (OutputSourceDisposition::Rewrite, Some(progress)) => {
                page.expected_prior_source_epoch == Some(progress.source_epoch)
                    && page.expected_prior_cursor == progress.cursor
                    && page.source_epoch > progress.source_epoch
            }
            _ => false,
        };
        if !prior_matches {
            return HelperMessage::Error(ProtocolError::new(
                ErrorClass::Sequence,
                "output page compare-and-swap does not match private source progress",
            ));
        }
        let availability = self
            .output_observations
            .get(&page.source)
            .copied()
            .unwrap_or(OutputSourceAvailability::Available);
        self.output_progress.insert(
            page.source.clone(),
            OutputSourceProgress {
                source: page.source.clone(),
                source_epoch: page.source_epoch,
                observed_revision: page.observed_revision.clone(),
                cursor: Some(page.next_safe_cursor.clone()),
                parser_revision: page.parser_revision.clone(),
                materializer_revision: page.materializer_revision.clone(),
                terminal: page.terminal,
                availability,
                last_seen_inventory: Some(page.inventory_generation),
            },
        );
        self.accepted_output_pages.insert(page_key, encoded);
        HelperMessage::OutputPageMaterialized(output_page_result(&page, false))
    }

    fn handle_finish_output_inventory(
        &mut self,
        request: FinishOutputInventoryRequest,
    ) -> HelperMessage {
        if let Err(error) = request.validate() {
            return HelperMessage::Error(error);
        }
        if self.active_output_inventory != Some(request.generation) {
            return HelperMessage::Error(ProtocolError::new(
                ErrorClass::Sequence,
                "output inventory finish does not match its active generation",
            ));
        }
        let observed_sources = self.output_observations.len() as u32;
        let unavailable_sources = self
            .output_observations
            .values()
            .filter(|availability| **availability != OutputSourceAvailability::Available)
            .count() as u32;
        self.active_output_inventory = None;
        self.completed_output_inventory = request.generation;
        HelperMessage::OutputInventoryFinished(OutputInventoryFinished {
            generation: request.generation,
            observed_sources,
            unavailable_sources,
        })
    }

    fn handle_output_progress(&self, request: OutputProgressRequest) -> HelperMessage {
        if let Err(error) = request.validate() {
            return HelperMessage::Error(error);
        }
        HelperMessage::OutputProgress(OutputProgressResult {
            inventory_generation: self
                .active_output_inventory
                .unwrap_or(self.completed_output_inventory),
            inventory_complete: self.active_output_inventory.is_none(),
            sources: request
                .sources
                .iter()
                .filter_map(|source| self.output_progress.get(source).cloned())
                .collect(),
        })
    }

    fn handle_blame(&self, request: BlameRequest) -> HelperMessage {
        if let Err(error) = request.validate() {
            return HelperMessage::Error(error);
        }
        let expected_checkpoint = match &request.expected_snapshot {
            QuerySnapshotExpectation::Journal { checkpoint, .. } => Some(checkpoint),
            QuerySnapshotExpectation::Source { .. } => None,
        };
        if self.checkpoint.as_ref() != expected_checkpoint {
            return HelperMessage::Error(ProtocolError::new(
                ErrorClass::StaleFact,
                "blame checkpoint does not match durable graph state",
            ));
        }
        if let Some(failure) = self.blame_failure {
            return HelperMessage::Error(ProtocolError::new(failure.class(), failure.message()));
        }
        let repository = |selector: Option<String>| ResourceRef {
            id: selector
                .clone()
                .unwrap_or_else(|| "repository:fixture".to_owned()),
            kind: ResourceKind::Repository,
            display: selector.unwrap_or_else(|| "fixture/repository".to_owned()),
        };
        let (target, git_snapshot) = match request.target {
            BlameTarget::File {
                path,
                repository: selector,
                lines,
            } => (
                ResolvedBlameTarget::File {
                    path,
                    repository: repository(selector),
                    requested_lines: lines,
                },
                Some(GitSnapshot {
                    head_oid: "0123456789abcdef0123456789abcdef01234567".to_owned(),
                    worktree_status: WorktreeStatus::Clean,
                }),
            ),
            BlameTarget::Commit {
                oid,
                repository: selector,
            } => (
                ResolvedBlameTarget::Commit {
                    commit: ResourceRef {
                        id: format!("commit:{oid}"),
                        kind: ResourceKind::Commit,
                        display: oid,
                    },
                    repository: repository(selector),
                },
                None,
            ),
            BlameTarget::PullRequest {
                selector: pull_request,
                repository: selector,
            } => (
                ResolvedBlameTarget::PullRequest {
                    selector: pull_request.clone(),
                    pull_request: ResourceRef {
                        id: format!("pull_request:{pull_request}"),
                        kind: ResourceKind::PullRequest,
                        display: pull_request,
                    },
                    repository: repository(selector),
                },
                None,
            ),
        };
        HelperMessage::Blame(BlameResult {
            target,
            git_snapshot,
            matches: Vec::new(),
            evidence: Vec::new(),
            next: None,
        })
    }
}

fn output_page_result(
    page: &ProOutputMaterializationPage,
    replayed: bool,
) -> OutputPageMaterialized {
    OutputPageMaterialized {
        inventory_generation: page.inventory_generation,
        source: page.source.clone(),
        source_epoch: page.source_epoch,
        committed_cursor: page.next_safe_cursor.clone(),
        accepted_outputs: if replayed {
            0
        } else {
            page.observations.len() as u32
        },
        materialized_facts: 0,
        materialized_evidence: 0,
        replayed,
    }
}
