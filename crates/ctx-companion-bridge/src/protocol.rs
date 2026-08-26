use std::path::{Path, PathBuf};

use serde::Deserialize;

pub const CORE_COMPANION_PROTOCOL_VERSION: ProtocolVersion = ProtocolVersion::new(4);
pub const CORE_PRO_PROTOCOL_VERSION: ProtocolVersion = CORE_COMPANION_PROTOCOL_VERSION;
pub(crate) const PROTOCOL_ENTRYPOINT_ARGUMENT: &str = "--ctx-pro-protocol-v4";
pub(crate) const PROTOCOL_HANDSHAKE_COMMAND: &str = "handshake";
pub(crate) const PROTOCOL_CLI_COMMAND: &str = "cli";
pub(crate) const PROTOCOL_MCP_COMMAND: &str = "mcp-serve";
pub(crate) const PROTOCOL_MAINTENANCE_COMMAND: &str = "maintenance";
pub(crate) const MCP_WRITTEN_AND_FLUSHED_RECEIPT: &[u8] = b"written_and_flushed\n";
pub(crate) const MCP_OUTPUT_FAILED_RECEIPT: &[u8] = b"output_failed\n";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ProtocolVersion(u16);

impl ProtocolVersion {
    pub const fn new(value: u16) -> Self {
        Self(value)
    }

    pub const fn get(self) -> u16 {
        self.0
    }
}

impl std::fmt::Display for ProtocolVersion {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(formatter)
    }
}

/// The only launch authority accepted by the bridge.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InstalledCompanion {
    executable: PathBuf,
}

impl InstalledCompanion {
    pub fn new(executable: impl Into<PathBuf>) -> Self {
        Self {
            executable: executable.into(),
        }
    }

    pub fn executable(&self) -> &Path {
        &self.executable
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct HandshakeReceipt {
    protocol_version: u16,
}

pub(crate) fn parse_handshake_receipt(bytes: &[u8]) -> Option<ProtocolVersion> {
    let line = bytes.strip_suffix(b"\n")?;
    if line.is_empty() || line.contains(&b'\n') || line.contains(&b'\r') {
        return None;
    }
    serde_json::from_slice::<HandshakeReceipt>(line)
        .ok()
        .map(|receipt| ProtocolVersion::new(receipt.protocol_version))
}
