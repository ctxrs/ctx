use std::{io, path::PathBuf};

use thiserror::Error;

use crate::{ExitClass, ProtocolVersion};

#[derive(Debug, Error)]
pub enum BridgeError {
    #[error("installed companion executable path must be absolute")]
    InvalidExecutablePath,
    #[error("installed companion executable is missing: {path}")]
    MissingExecutable { path: PathBuf },
    #[error("installed companion path is not a file: {path}")]
    ExecutableNotFile { path: PathBuf },
    #[error("installed companion executable could not be inspected: {path}")]
    ExecutableMetadata {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("managed-pair installation verification failed: {0}")]
    Verification(String),
    #[error("installed companion failed the Core protocol {expected} handshake")]
    ProtocolMismatch {
        expected: ProtocolVersion,
        observed: ProtocolVersion,
    },
    #[error("installed companion exited before completing the Core companion handshake")]
    HandshakeFailed {
        exit: ExitClass,
        stderr: Vec<u8>,
        stderr_truncated: bool,
    },
    #[error("installed companion returned an invalid Core companion {0} response")]
    InvalidProtocolResponse(&'static str),
    #[error("installed companion did not complete the MCP exchange: {exit:?}")]
    McpExchangeFailed { exit: ExitClass },
    #[error("managed companion request exceeds the {0} limit")]
    Limit(&'static str),
    #[error("managed companion request contains an invalid environment name")]
    InvalidEnvironmentName,
    #[error("managed companion request deadline expired before spawn")]
    QueueTimeout,
    #[error("managed companion launch was cancelled before spawn")]
    CancelledBeforeSpawn,
    #[error("managed companion process could not be started: {0}")]
    Spawn(#[source] io::Error),
    #[error("managed companion transport failed: {0}")]
    Transport(#[source] io::Error),
    #[error("managed companion transport worker failed")]
    WorkerFailed,
    #[error("managed companion transport is unsupported on this platform")]
    UnsupportedPlatform,
}
