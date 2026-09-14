use std::{
    collections::{BTreeMap, BTreeSet},
    path::PathBuf,
    time::SystemTime,
};

use ctx_history_core::CaptureProvider;
use serde::{Deserialize, Serialize};

use ctx_history_capture_model::time::system_time_ms;

use crate::{
    common::io::ProviderSourceRoot, provider::codex::catalog::CatalogSession,
    provider::source_backed::family::jsonl::JsonlFileObservation, CODEX_SESSION_SOURCE_FORMAT,
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
    pub(crate) source_path: PathBuf,
    /// Stable physical provider-home lineage for session-tree routes. Exact
    /// one-file imports retain their released path-independent identity.
    pub(crate) source_root_lineage: Option<[u8; 32]>,
    pub(crate) catalog_observation: CodexFileObservation,
    /// JSONL admission observation gathered by session-tree inventory while
    /// the source was already securely open. Other catalog routes leave this
    /// absent and retain the conservative reopen path.
    pub(crate) carried_jsonl_observation: Option<JsonlFileObservation>,
    /// SHA-256 of exactly `catalog_observation.len` bytes from the retained
    /// discovery authority. This is task-local admission evidence, not a
    /// second transcript store.
    pub(crate) catalog_prefix_sha256: Option<[u8; 32]>,
    pub(crate) catalog_native_session_id: Option<String>,
    pub(crate) authority_root: Option<ProviderSourceRoot>,
    pub(crate) authority_relative_path: Option<PathBuf>,
}

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
        source_path: PathBuf::from(&session.source_path),
        source_root_lineage: None,
        catalog_observation: CodexFileObservation {
            len: session.file_size_bytes,
            modified_at_ms: session.file_modified_at_ms,
            stable_token,
            change_token,
        },
        carried_jsonl_observation: None,
        catalog_prefix_sha256: None,
        catalog_native_session_id: session.external_session_id.clone(),
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
