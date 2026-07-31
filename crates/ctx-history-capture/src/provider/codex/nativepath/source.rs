use std::{
    collections::{BTreeMap, BTreeSet},
    path::PathBuf,
    sync::Arc,
    time::SystemTime,
};

use ctx_history_core::CaptureProvider;
use serde::{Deserialize, Serialize};

use super::checkpoint::CodexNativeCheckpoint;
use crate::{
    common::io::{OpenedProviderSourceFile, ProviderSourceRoot},
    common::time::system_time_ms,
    provider::codex::catalog::CatalogSession,
    CaptureError, Result as CaptureResult, CODEX_SESSION_SOURCE_FORMAT,
};

const CATALOG_CHANGE_TOKEN_KEY: &str = "inventory_file_change_token_v1";
const CATALOG_STABLE_TOKEN_KEY: &str = "inventory_file_stable_token_v1";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CodexFileObservation {
    pub(crate) len: u64,
    pub(crate) modified_at_ms: i64,
    #[serde(default)]
    pub(crate) stable_token: Option<[u8; 32]>,
    pub(crate) change_token: [u8; 32],
}

impl CodexFileObservation {
    pub(crate) fn from_parts(
        len: u64,
        modified_at: SystemTime,
        stable_token: Option<[u8; 32]>,
        change_token: [u8; 32],
    ) -> Self {
        Self {
            len,
            modified_at_ms: system_time_ms(modified_at),
            stable_token,
            change_token,
        }
    }

    /// Returns true when `current` is either the exact observation or a
    /// strictly longer observation of the same retained ordinary file.
    pub(crate) fn admits_append_only_growth(&self, current: &Self) -> bool {
        self == current
            || (current.len > self.len
                && self.stable_token.is_some()
                && self.stable_token == current.stable_token)
    }
}

#[derive(Debug, Clone)]
pub(crate) struct CodexCatalogSource {
    pub(crate) source_root: String,
    pub(crate) source_path: PathBuf,
    pub(crate) cataloged_at_ms: i64,
    pub(crate) catalog_observation: CodexFileObservation,
    pub(crate) catalog_native_session_id: Option<String>,
    pub(crate) catalog_parent_native_session_id: Option<String>,
    pub(crate) catalog_root_native_session_id: Option<String>,
    pub(crate) opened: Option<Arc<OpenedProviderSourceFile>>,
    pub(crate) authority_root: Option<ProviderSourceRoot>,
    pub(crate) authority_relative_path: Option<PathBuf>,
}

impl PartialEq for CodexCatalogSource {
    fn eq(&self, other: &Self) -> bool {
        self.source_root == other.source_root
            && self.source_path == other.source_path
            && self.cataloged_at_ms == other.cataloged_at_ms
            && self.catalog_observation == other.catalog_observation
            && self.catalog_native_session_id == other.catalog_native_session_id
            && self.catalog_parent_native_session_id == other.catalog_parent_native_session_id
            && self.catalog_root_native_session_id == other.catalog_root_native_session_id
    }
}

impl Eq for CodexCatalogSource {}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CodexCatalogRejection {
    pub(crate) source_path: String,
    pub(crate) reason: &'static str,
}

#[derive(Debug, Default)]
pub(crate) struct CodexCatalogDiscovery {
    pub(crate) sources: Vec<CodexCatalogSource>,
    pub(crate) rejections: Vec<CodexCatalogRejection>,
    pub(crate) ineligible: usize,
}

pub(crate) fn discover_codex_catalog_sources(sessions: &[CatalogSession]) -> CodexCatalogDiscovery {
    let mut discovery = CodexCatalogDiscovery::default();
    let mut sources = BTreeMap::<PathBuf, CodexCatalogSource>::new();
    let mut duplicate_paths = BTreeSet::new();

    for session in sessions {
        if session.provider != CaptureProvider::Codex
            || session.source_format != CODEX_SESSION_SOURCE_FORMAT
        {
            discovery.ineligible = discovery.ineligible.saturating_add(1);
            continue;
        }
        match catalog_source(session) {
            Ok(source) => {
                if sources.insert(source.source_path.clone(), source).is_some() {
                    duplicate_paths.insert(PathBuf::from(&session.source_path));
                }
            }
            Err(reason) => discovery.rejections.push(CodexCatalogRejection {
                source_path: session.source_path.clone(),
                reason,
            }),
        }
    }

    for path in duplicate_paths {
        sources.remove(&path);
        discovery.rejections.push(CodexCatalogRejection {
            source_path: path.display().to_string(),
            reason: "duplicate Codex catalog path",
        });
    }
    discovery.sources.extend(sources.into_values());
    discovery
}

fn catalog_source(session: &CatalogSession) -> Result<CodexCatalogSource, &'static str> {
    if session.source_root.trim().is_empty() {
        return Err("Codex catalog source root is empty");
    }
    if session.source_path.trim().is_empty() {
        return Err("Codex catalog source path is empty");
    }
    let change_token = session
        .metadata
        .get(CATALOG_CHANGE_TOKEN_KEY)
        .and_then(serde_json::Value::as_str)
        .ok_or("Codex catalog change token is missing")
        .and_then(decode_change_token)?;
    let stable_token = session
        .metadata
        .get(CATALOG_STABLE_TOKEN_KEY)
        .and_then(serde_json::Value::as_str)
        .map(decode_change_token)
        .transpose()?;
    Ok(CodexCatalogSource {
        source_root: session.source_root.clone(),
        source_path: PathBuf::from(&session.source_path),
        cataloged_at_ms: session.cataloged_at_ms,
        catalog_observation: CodexFileObservation {
            len: session.file_size_bytes,
            modified_at_ms: session.file_modified_at_ms,
            stable_token,
            change_token,
        },
        catalog_native_session_id: session.external_session_id.clone(),
        catalog_parent_native_session_id: session.parent_external_session_id.clone(),
        catalog_root_native_session_id: session
            .metadata
            .get("root_external_session_id")
            .and_then(serde_json::Value::as_str)
            .map(str::to_owned),
        opened: None,
        authority_root: None,
        authority_relative_path: None,
    })
}

fn decode_change_token(value: &str) -> Result<[u8; 32], &'static str> {
    if value.len() != 64 {
        return Err("Codex catalog change token is malformed");
    }
    let mut decoded = [0_u8; 32];
    for (index, pair) in value.as_bytes().chunks_exact(2).enumerate() {
        let high = decode_hex_nibble(pair[0])
            .ok_or("Codex catalog change token contains invalid hexadecimal")?;
        let low = decode_hex_nibble(pair[1])
            .ok_or("Codex catalog change token contains invalid hexadecimal")?;
        decoded[index] = (high << 4) | low;
    }
    Ok(decoded)
}

fn decode_hex_nibble(value: u8) -> Option<u8> {
    match value {
        b'0'..=b'9' => Some(value - b'0'),
        b'a'..=b'f' => Some(value - b'a' + 10),
        b'A'..=b'F' => Some(value - b'A' + 10),
        _ => None,
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CodexSourceIdentity {
    pub(crate) canonical_source_key: String,
    pub(crate) source_root: String,
    pub(crate) source_path: PathBuf,
}

impl CodexSourceIdentity {
    pub(crate) fn new(
        canonical_source_key: impl Into<String>,
        source_root: impl Into<String>,
        source_path: PathBuf,
    ) -> CaptureResult<Self> {
        let identity = Self {
            canonical_source_key: canonical_source_key.into(),
            source_root: source_root.into(),
            source_path,
        };
        if identity.canonical_source_key.trim().is_empty()
            || identity.source_root.trim().is_empty()
            || identity.source_path.as_os_str().is_empty()
        {
            return Err(CaptureError::InvalidPayload(
                "Codex append proof identity is incomplete".to_owned(),
            ));
        }
        Ok(identity)
    }

    pub(crate) fn matches_catalog_source(&self, source: &CodexCatalogSource) -> bool {
        self.source_root == source.source_root && self.source_path == source.source_path
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct CodexCheckpointGeneration(u64);

impl CodexCheckpointGeneration {
    pub(crate) const fn new(value: u64) -> Self {
        Self(value)
    }

    pub(crate) const fn get(self) -> u64 {
        self.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CodexAppendProof {
    pub(crate) identity: CodexSourceIdentity,
    pub(crate) generation: CodexCheckpointGeneration,
    pub(crate) checkpoint: CodexNativeCheckpoint,
}

impl CodexAppendProof {
    pub(crate) fn new(
        identity: CodexSourceIdentity,
        generation: CodexCheckpointGeneration,
        checkpoint: CodexNativeCheckpoint,
    ) -> Self {
        Self {
            identity,
            generation,
            checkpoint,
        }
    }

    pub(crate) fn validate_source(&self, source: &CodexCatalogSource) -> CaptureResult<()> {
        if !self.identity.matches_catalog_source(source) {
            return Err(CaptureError::InvalidPayload(format!(
                "Codex append proof generation {} does not belong to catalog source {}",
                self.generation.get(),
                source.source_path.display()
            )));
        }
        Ok(())
    }
}
