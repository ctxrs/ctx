use std::collections::{BTreeSet, HashMap};

use crate::entitlement::{base64url, decode_base64url, AUTHORIZATION_CHALLENGE_BYTES};
use crate::error::{ErrorClass, ProtocolError};
use crate::journal::{JournalCheckpoint, JournalSyncMode, JournalSyncRequest, JournalSyncResult};
use crate::lifecycle::{
    ConfirmGraphKeyDeletionRequest, GraphKeyDeleted, GraphKeyDeletionPrepared,
    PrepareGraphKeyDeletionRequest, GRAPH_KEY_DELETION_CHALLENGE_BYTES,
};
use crate::message::{
    Capability, GraphState, HelloRequest, HelloResult, HelperEnvelope, HelperMessage, HostEnvelope,
    HostMessage, StatusResult,
};
use crate::query::QueryRequest;
use crate::{QueryResult, PROTOCOL_FINGERPRINT, PROTOCOL_VERSION};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FakeQueryFailure {
    SourceUnavailable,
    RepositoryUnavailable,
    StaleSnapshot,
}

impl FakeQueryFailure {
    const fn class(self) -> ErrorClass {
        match self {
            Self::SourceUnavailable => ErrorClass::MissingSource,
            Self::RepositoryUnavailable => ErrorClass::MissingRepository,
            Self::StaleSnapshot => ErrorClass::StaleFact,
        }
    }

    const fn message(self) -> &'static str {
        match self {
            Self::SourceUnavailable => "canonical source is unavailable",
            Self::RepositoryUnavailable => "repository is unavailable",
            Self::StaleSnapshot => "query snapshot is stale",
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
    query_failure: Option<FakeQueryFailure>,
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
                Capability::Query,
            ]),
            negotiated_capabilities: BTreeSet::new(),
            checkpoint: None,
            accepted_requests: HashMap::new(),
            query_failure: None,
            graph_key_deletion_challenge: None,
            graph_key_present: true,
        }
    }

    #[must_use]
    pub const fn with_query_failure(mut self, failure: FakeQueryFailure) -> Self {
        self.query_failure = Some(failure);
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
                    checkpoint: self.checkpoint.clone(),
                })
            }
            HostMessage::SyncJournal(request) if self.selected(Capability::JournalSync) => {
                self.handle_journal_sync(request)
            }
            HostMessage::Query(query) if self.selected(Capability::Query) => {
                self.handle_query(query)
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
        let mut durable_request = request.clone();
        durable_request.result_contents.clear();
        let encoded = match serde_json::to_vec(&durable_request) {
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

    fn handle_query(&self, query: QueryRequest) -> HelperMessage {
        if let Err(error) = query.validate() {
            return HelperMessage::Error(error);
        }
        if self.checkpoint.as_ref() != Some(&query.expected_snapshot.checkpoint) {
            return HelperMessage::Error(ProtocolError::new(
                ErrorClass::StaleFact,
                "query checkpoint does not match durable graph state",
            ));
        }
        if let Some(failure) = self.query_failure {
            return HelperMessage::Error(ProtocolError::new(failure.class(), failure.message()));
        }
        HelperMessage::Query(QueryResult {
            records: Vec::new(),
            next_cursor: None,
            truncated: false,
            stale: false,
        })
    }
}
