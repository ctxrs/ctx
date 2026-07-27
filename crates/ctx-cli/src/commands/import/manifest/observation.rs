use std::{
    collections::hash_map::DefaultHasher,
    fs,
    hash::{Hash, Hasher},
    path::Path,
    time::UNIX_EPOCH,
};

use anyhow::{Context, Result};
use serde_json::{json, Value};

use ctx_history_core::CaptureProvider;
use ctx_history_store::SourceImportFile;

use crate::commands::import::{provider_path_text, system_time_ms};
use crate::provider_sources::SourceInfo;

#[derive(Debug, Clone, Copy, Default)]
pub(super) struct SourceChangeFingerprint {
    count: u64,
    xor: u64,
    sum: u64,
    rotated_sum: u64,
}

impl SourceChangeFingerprint {
    pub(super) fn observe(
        &mut self,
        path: &Path,
        observation: &ctx_history_capture::OrdinaryFileObservation,
    ) {
        let modified = observation
            .modified_at()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default();
        let mut hasher = DefaultHasher::new();
        path.as_os_str().hash(&mut hasher);
        observation.len().hash(&mut hasher);
        modified.as_secs().hash(&mut hasher);
        modified.subsec_nanos().hash(&mut hasher);
        observation.token().hash(&mut hasher);
        let hash = hasher.finish();
        self.count = self.count.saturating_add(1);
        self.xor ^= hash;
        self.sum = self.sum.wrapping_add(hash);
        self.rotated_sum = self.rotated_sum.wrapping_add(hash.rotate_left(23));
    }

    pub(super) fn finish(self) -> [u8; 32] {
        let mut token = [0_u8; 32];
        token[..8].copy_from_slice(&self.count.to_le_bytes());
        token[8..16].copy_from_slice(&self.xor.to_le_bytes());
        token[16..24].copy_from_slice(&self.sum.to_le_bytes());
        token[24..].copy_from_slice(&self.rotated_sum.to_le_bytes());
        token
    }
}

pub(super) fn source_import_file(
    source: &SourceInfo,
    path: &Path,
    _metadata: &fs::Metadata,
    observed_at_ms: i64,
) -> Result<SourceImportFile> {
    let observation = ctx_history_capture::observe_ordinary_file(path)
        .with_context(|| format!("observe import source file {}", path.display()))?;
    Ok(SourceImportFile {
        provider: source.provider,
        source_format: source.source_format.to_owned(),
        source_root: provider_path_text(&source.path)?.to_owned(),
        source_path: provider_path_text(path)?.to_owned(),
        file_size_bytes: observation.len(),
        file_modified_at_ms: system_time_ms(observation.modified_at()),
        observed_at_ms,
        metadata: source_import_file_metadata(source, path, &observation)?,
    })
}

fn source_import_file_metadata(
    source: &SourceInfo,
    path: &Path,
    observation: &ctx_history_capture::OrdinaryFileObservation,
) -> Result<Value> {
    let preferred_path = if source.provider == CaptureProvider::Antigravity {
        if path.file_name().and_then(|name| name.to_str()) == Some("transcript.jsonl") {
            Some(provider_path_text(&path.with_file_name("transcript_full.jsonl"))?.to_owned())
        } else {
            None
        }
    } else {
        None
    };
    Ok(json!({
        "inventory_file_change_token_v1": observation.token_hex(),
        "inventory_preferred_path_v1": preferred_path,
    }))
}
