//! Optional producer ports. The CLI composition owns consent and wire mapping.
use ctx_client_observability::analytics::{HostedFailureV1, HostedOperationV1, OutputKind};
use std::{sync::Arc, time::Duration};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum HostedOperation {
    Existing(HostedOperationV1),
    ArchiveVerify,
    ServerCollectionCreate,
    ServerUserCreate,
    ServerUserList,
    ServerUserCredentials,
    ServerUserCredential,
    ServerPublications,
    ServerStatus,
    RemotePause,
    RemoteResume,
    RemoteStatus,
    RemoteRemove,
}

#[derive(Debug)]
pub(crate) struct HostedCompletion {
    pub operation: HostedOperation,
    pub output: OutputKind,
    pub duration: Duration,
    pub result: Result<(), HostedFailureV1>,
}

#[derive(Default, Clone)]
pub(crate) struct HostedObservers {
    pub completion: Option<Arc<dyn Fn(HostedCompletion) + Send + Sync>>,
    pub server: Option<ctx_history_server::ServerObserver>,
    pub runtime: Option<ctx_history_server::ServerRuntimeHook>,
    pub sharing: Option<ctx_history_sharing::SharingObserver>,
}
