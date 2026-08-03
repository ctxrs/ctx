use std::collections::BTreeSet;

use crate::entitlement::{base64url, decode_base64url, AUTHORIZATION_CHALLENGE_BYTES};
use crate::error::{ErrorClass, ProtocolError};
use crate::lifecycle::{
    ConfirmGraphKeyDeletionRequest, GraphKeyDeleted, GraphKeyDeletionPrepared,
    PrepareGraphKeyDeletionRequest, GRAPH_KEY_DELETION_CHALLENGE_BYTES,
};
use crate::message::{
    Capability, CoreProjectionCurrentness, HelloRequest, HelloResult, HelperEnvelope,
    HelperMessage, HostEnvelope, HostMessage, MaterializedCoverage, ProAccessState,
    ProAccessStatus, StatusResult,
};
use crate::query::{BlameRequest, BlameTarget, ResolvedBlameTarget};
use crate::{
    BlameResult, GitSnapshot, ResourceKind, ResourceRef, WorktreeStatus, PROTOCOL_FINGERPRINT,
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
                Capability::CoreMaterialization,
                Capability::Query,
                Capability::GitRead,
            ]),
            negotiated_capabilities: BTreeSet::new(),
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
                    currentness: CoreProjectionCurrentness::NotMaterialized,
                    requested_core_generation_id: None,
                    core_receipt: None,
                    coverage: MaterializedCoverage::NotMaterialized,
                    repository_coverage: Default::default(),
                    core_preparation_peak_workers: 0,
                    access: ProAccessStatus {
                        entitlement: ProAccessState::Available,
                        graph_key: ProAccessState::Available,
                        local_repository: ProAccessState::Unavailable,
                    },
                    supported_operations: BTreeSet::new(),
                    available_operations: BTreeSet::new(),
                    storage_evidence: None,
                })
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

    fn handle_blame(&self, request: BlameRequest) -> HelperMessage {
        if let Err(error) = request.validate() {
            return HelperMessage::Error(error);
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
            snapshot: request.expected_snapshot,
            target,
            git_snapshot,
            matches: Vec::new(),
            evidence: Vec::new(),
            next: None,
        })
    }
}
