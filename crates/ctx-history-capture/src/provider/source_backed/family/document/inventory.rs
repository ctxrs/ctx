use ctx_history_core::{CertifiedSourceInventory, SourceInventoryObservation, SourceKey, TypedKey};
use sha2::{Digest, Sha256};

use super::{document_contract_error, ProviderSource, SourceBackedRouteResult};

const DOCUMENT_INVENTORY_AUTHORITY_NAMESPACE: &str = "ctx.document-tree";
const DOCUMENT_INVENTORY_REVISION_KIND: &str = "ctx-document-tree-fingerprint-v1";
const DOCUMENT_INVENTORY_DISCOVERY_REVISION: &str = "ctx-document-tree-discovery-v1";

#[derive(Clone)]
pub(super) struct DocumentInventoryAuthority {
    provider: String,
    route_key: [u8; 32],
}

impl DocumentInventoryAuthority {
    pub(super) fn new(route: &ProviderSource) -> Self {
        let path = route.path.as_os_str().as_encoded_bytes();
        let mut digest = Sha256::new();
        digest.update(b"ctx.document-tree-route-authority-v1\0");
        digest.update((route.provider.as_str().len() as u64).to_be_bytes());
        digest.update(route.provider.as_str().as_bytes());
        digest.update((route.source_format.len() as u64).to_be_bytes());
        digest.update(route.source_format.as_bytes());
        digest.update((path.len() as u64).to_be_bytes());
        digest.update(path);
        Self {
            provider: route.provider.as_str().to_owned(),
            route_key: digest.finalize().into(),
        }
    }

    pub(super) fn certify(
        &self,
        tree_fingerprint: [u8; 32],
        sources: Vec<SourceKey>,
    ) -> SourceBackedRouteResult<CertifiedSourceInventory> {
        let observation = SourceInventoryObservation::new(
            self.provider.clone(),
            DOCUMENT_INVENTORY_AUTHORITY_NAMESPACE,
            TypedKey::bytes(self.route_key.to_vec()).map_err(document_contract_error)?,
            DOCUMENT_INVENTORY_REVISION_KIND,
            tree_fingerprint.to_vec(),
        )
        .map_err(document_contract_error)?;
        CertifiedSourceInventory::certify(
            observation.clone(),
            observation,
            DOCUMENT_INVENTORY_DISCOVERY_REVISION,
            sources,
        )
        .map_err(document_contract_error)
    }
}
