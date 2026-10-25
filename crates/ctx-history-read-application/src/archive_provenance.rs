use std::collections::BTreeMap;

use ctx_history_core::StableEntityId;
use serde::Serialize;
use serde_json::Value;

/// Display provenance supplied by an archive creator, never a verified identity.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ArchiveProvenance {
    pub origin: String,
    pub view: String,
    pub label_authority: ArchiveLabelAuthority,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ArchiveLabelAuthority {
    ArchiveCreator,
}

/// Query-scoped labels bound by the adapter to the opened generation's sources.
#[derive(Debug, Clone, Default)]
pub struct ArchiveProvenanceMap(pub BTreeMap<[u8; 32], ArchiveProvenance>);

impl ArchiveProvenanceMap {
    pub fn for_session(&self, session: StableEntityId) -> Option<&ArchiveProvenance> {
        self.0.get(&session.source_digest())
    }

    pub fn decorate(&self, value: &mut Value, session: StableEntityId) {
        if let Some(provenance) = self.for_session(session) {
            value["archive_provenance"] = serde_json::json!(provenance);
        }
    }
}
