use std::{fs, path::Path, sync::Arc, time::UNIX_EPOCH};

use ctx_history_core::{
    CaptureProvider, CertifiedSourceInventory, SourceInventoryObservation, SourceKey,
    SourceObservation, TypedKey,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::{
    CustomHistorySourceBackedError, CustomHistorySourceBackedInput, CustomHistorySourceBackedResult,
};
use crate::{
    common::io::{open_provider_source_file, OpenedProviderSourceFile, ProviderSourceRoot},
    provider::source_backed::family::jsonl::{JsonlFamilyInventory, JsonlFamilyLeaf},
    CaptureError,
};

const CUSTOM_SOURCE_REVISION_KIND: &str = "custom-history-ordinary-file-observation-v1";
const CUSTOM_INVENTORY_AUTHORITY_NAMESPACE: &str = "custom-history.explicit-registration";
const CUSTOM_INVENTORY_REVISION_KIND: &str = "custom-history-explicit-inventory-v1";
const CUSTOM_DISCOVERY_REVISION: &str = "custom-history-explicit-only-v1";
const INVENTORY_DIGEST_DOMAIN: &[u8] = b"ctx.custom-history.explicit-inventory.v1\0";

pub(super) fn custom_history_jsonl_family_inventory(
    input: &CustomHistorySourceBackedInput,
    source: &SourceKey,
    root: &Path,
) -> CustomHistorySourceBackedResult<JsonlFamilyInventory> {
    let selected = std::path::absolute(root)?;
    if selected != std::path::absolute(input.path())? {
        return Err(CaptureError::InvalidProviderTranscriptPath {
            path: selected,
            reason: "Custom History family root changed from its explicit selection",
        }
        .into());
    }
    match fs::symlink_metadata(&selected) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(JsonlFamilyInventory::missing(
                CaptureProvider::Custom,
                &selected,
            )?);
        }
        Err(error) => return Err(error.into()),
        Ok(_) => {}
    }
    let authority_path = selected
        .parent()
        .ok_or(CaptureError::InvalidProviderTranscriptPath {
            path: selected.clone(),
            reason: "Custom History selected file has no authority directory",
        })?;
    let authority = Arc::new(ProviderSourceRoot::open(authority_path)?);
    let authority_relative_path = selected
        .strip_prefix(authority.named_path())
        .map(Path::to_path_buf)
        .map_err(|_| CaptureError::InvalidProviderTranscriptPath {
            path: selected.clone(),
            reason: "Custom History source escaped its retained authority",
        })?;
    let leaves = vec![JsonlFamilyLeaf::observe(
        source.clone(),
        selected.clone(),
        Arc::clone(&authority),
        authority_relative_path,
        TypedKey::bytes(source.exact_descriptor_digest().to_vec())?,
    )?];
    Ok(JsonlFamilyInventory::present(
        CaptureProvider::Custom,
        &selected,
        authority,
        leaves,
    )?)
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(super) struct CustomHistoryFileObservationWire {
    length: u64,
    modified_after_epoch: bool,
    modified_seconds: u64,
    modified_nanos: u32,
    strong_token: [u8; 32],
}

impl CustomHistoryFileObservationWire {
    fn from_opened(opened: &OpenedProviderSourceFile) -> Result<Self, CaptureError> {
        let metadata = opened.file().metadata()?;
        let (modified_after_epoch, duration) = match metadata.modified()?.duration_since(UNIX_EPOCH)
        {
            Ok(duration) => (true, duration),
            Err(error) => (false, error.duration()),
        };
        let mut token = Sha256::new();
        token.update(b"ctx.custom-history-opened-file-observation-v1\0");
        token.update(metadata.len().to_be_bytes());
        token.update([u8::from(metadata.permissions().readonly())]);
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            token.update(metadata.dev().to_be_bytes());
            token.update(metadata.ino().to_be_bytes());
            token.update(metadata.ctime().to_be_bytes());
            token.update(metadata.ctime_nsec().to_be_bytes());
        }
        #[cfg(not(unix))]
        {
            token.update([u8::from(modified_after_epoch)]);
            token.update(duration.as_secs().to_be_bytes());
            token.update(duration.subsec_nanos().to_be_bytes());
        }
        Ok(Self {
            length: metadata.len(),
            modified_after_epoch,
            modified_seconds: duration.as_secs(),
            modified_nanos: duration.subsec_nanos(),
            strong_token: token.finalize().into(),
        })
    }
}

#[derive(Debug, Clone)]
enum CustomHistoryInventoryState {
    Present {
        observation: CustomHistoryFileObservationWire,
        opened: Arc<OpenedProviderSourceFile>,
    },
    Missing,
}

impl PartialEq for CustomHistoryInventoryState {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (
                Self::Present {
                    observation: left, ..
                },
                Self::Present {
                    observation: right, ..
                },
            ) => left == right,
            (Self::Missing, Self::Missing) => true,
            _ => false,
        }
    }
}

impl Eq for CustomHistoryInventoryState {}

/// One finite observation of exactly the explicitly registered file.
#[derive(Debug, Clone)]
pub(crate) struct CustomHistorySourceBackedInventory {
    input: CustomHistorySourceBackedInput,
    source: SourceKey,
    observation: SourceInventoryObservation,
    state: CustomHistoryInventoryState,
}

impl CustomHistorySourceBackedInventory {
    pub(crate) fn source(&self) -> &SourceKey {
        &self.source
    }

    pub(crate) fn is_missing(&self) -> bool {
        self.state == CustomHistoryInventoryState::Missing
    }

    pub(crate) fn certify_against(
        &self,
        closing: &Self,
    ) -> CustomHistorySourceBackedResult<CertifiedSourceInventory> {
        if self.input != closing.input
            || !self.source.exact_descriptor_eq(&closing.source)
            || self.state != closing.state
        {
            return Err(CustomHistorySourceBackedError::InventoryChanged);
        }
        let sources = match &self.state {
            CustomHistoryInventoryState::Present { .. } => vec![self.source.clone()],
            CustomHistoryInventoryState::Missing => Vec::new(),
        };
        Ok(CertifiedSourceInventory::certify(
            self.observation.clone(),
            closing.observation.clone(),
            CUSTOM_DISCOVERY_REVISION,
            sources,
        )?)
    }

    pub(super) fn ordinary(&self) -> Option<&CustomHistoryFileObservationWire> {
        match &self.state {
            CustomHistoryInventoryState::Present { observation, .. } => Some(observation),
            CustomHistoryInventoryState::Missing => None,
        }
    }

    pub(super) fn opened(&self) -> Option<&Arc<OpenedProviderSourceFile>> {
        match &self.state {
            CustomHistoryInventoryState::Present { opened, .. } => Some(opened),
            CustomHistoryInventoryState::Missing => None,
        }
    }
}

pub(super) fn observe_custom_history_source_backed_explicit(
    input: &CustomHistorySourceBackedInput,
) -> CustomHistorySourceBackedResult<CustomHistorySourceBackedInventory> {
    let source = input.source_key()?;
    let state = match open_explicit_source(input.path()) {
        Ok(opened) => {
            let observation = CustomHistoryFileObservationWire::from_opened(&opened)?;
            opened.revalidate()?;
            CustomHistoryInventoryState::Present {
                observation,
                opened,
            }
        }
        Err(CustomHistorySourceBackedError::Capture(CaptureError::Io(error)))
            if error.kind() == std::io::ErrorKind::NotFound =>
        {
            CustomHistoryInventoryState::Missing
        }
        Err(error) => return Err(error),
    };
    let mut digest = Sha256::new();
    digest.update(INVENTORY_DIGEST_DOMAIN);
    match &state {
        CustomHistoryInventoryState::Present { observation, .. } => {
            digest.update(b"present\0");
            digest.update(serde_json::to_vec(observation)?);
        }
        CustomHistoryInventoryState::Missing => digest.update(b"missing\0"),
    }
    let observation = SourceInventoryObservation::new(
        CaptureProvider::Custom.as_str(),
        CUSTOM_INVENTORY_AUTHORITY_NAMESPACE,
        TypedKey::bytes(input.catalog_lineage.to_vec())?,
        CUSTOM_INVENTORY_REVISION_KIND,
        digest.finalize().to_vec(),
    )?;
    Ok(CustomHistorySourceBackedInventory {
        input: input.clone(),
        source,
        observation,
        state,
    })
}

pub(super) fn open_explicit_source(
    path: &Path,
) -> CustomHistorySourceBackedResult<Arc<OpenedProviderSourceFile>> {
    let path = std::path::absolute(path)?;
    Ok(Arc::new(open_provider_source_file(&path)?))
}

pub(super) fn source_observation(
    source: SourceKey,
    observation: &CustomHistoryFileObservationWire,
) -> CustomHistorySourceBackedResult<SourceObservation> {
    Ok(SourceObservation::new(
        source,
        CUSTOM_SOURCE_REVISION_KIND,
        serde_json::to_vec(observation)?,
    )?)
}
