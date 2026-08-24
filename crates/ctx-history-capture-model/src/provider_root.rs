use std::path::PathBuf;

use ctx_history_core::CaptureProvider;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

pub const MAX_CONFIGURED_PROVIDER_ROOTS: usize = 64;
pub const MAX_PROVIDER_ROOT_SELECTOR_BYTES: usize = 64;

/// Source-identity namespace applied to one configured provider home.
///
/// Released homes retain the identity contract used by automatic discovery
/// before named roots existed. Independently named homes use a logical
/// provider/root-id namespace so duplicate native session IDs remain distinct
/// without tying public identities to a machine-specific filesystem path.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderRootSourceIdentity {
    Released,
    #[default]
    NamedV1,
}

impl ProviderRootSourceIdentity {
    pub fn lineage(self, root: &ProviderRootDefinition) -> Option<[u8; 32]> {
        match self {
            Self::Released => None,
            Self::NamedV1 => {
                let mut digest = Sha256::new();
                digest.update(b"ctx-provider-root-source-identity-v1\0");
                digest.update(root.provider.as_str().as_bytes());
                digest.update([0]);
                digest.update((root.id.len() as u64).to_be_bytes());
                digest.update(root.id.as_bytes());
                Some(digest.finalize().into())
            }
        }
    }
}

/// Canonical desired/applied identity for one user-named provider home.
///
/// A provider adapter expands the home into physical routes. Human group
/// membership stays here rather than being copied into every Core record.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProviderRootDefinition {
    pub id: String,
    pub provider: CaptureProvider,
    pub path: PathBuf,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group: Option<String>,
}

pub fn provider_source_config_digest(
    automatic_discovery: bool,
    roots: &[ProviderRootDefinition],
) -> String {
    let mut canonical = roots.to_vec();
    canonical.sort_by(|left, right| left.id.cmp(&right.id));
    let mut digest = Sha256::new();
    digest.update(b"ctx-provider-source-config-v1\0");
    digest.update([u8::from(automatic_discovery)]);
    match serde_json::to_vec(&canonical) {
        Ok(encoded) => digest.update(encoded),
        Err(_) => {
            // PathBuf's JSON representation rejects non-Unicode paths. Public
            // discovery-context constructors can still receive one before
            // config/manifest validation returns its typed error, so digesting
            // that untrusted definition must remain total and collision-safe.
            digest.update(b"native-path-fallback-v1\0");
            digest.update((canonical.len() as u64).to_be_bytes());
            for root in canonical {
                for value in [root.id.as_bytes(), root.provider.as_str().as_bytes()] {
                    digest.update((value.len() as u64).to_be_bytes());
                    digest.update(value);
                }
                let path = root.path.as_os_str().as_encoded_bytes();
                digest.update((path.len() as u64).to_be_bytes());
                digest.update(path);
                match root.group {
                    Some(group) => {
                        digest.update([1]);
                        digest.update((group.len() as u64).to_be_bytes());
                        digest.update(group.as_bytes());
                    }
                    None => digest.update([0]),
                }
            }
        }
    }
    format!("{:x}", digest.finalize())
}

#[cfg(all(test, unix))]
mod tests {
    use std::{ffi::OsString, os::unix::ffi::OsStringExt};

    use super::*;

    #[test]
    fn digest_is_total_and_distinct_for_non_unicode_public_api_paths() {
        let root = |byte| ProviderRootDefinition {
            id: "fixture".to_owned(),
            provider: CaptureProvider::Claude,
            path: PathBuf::from(OsString::from_vec(vec![b'/', b't', b'm', b'p', b'/', byte])),
            group: None,
        };

        assert_ne!(
            provider_source_config_digest(true, &[root(0xfe)]),
            provider_source_config_digest(true, &[root(0xff)])
        );
        assert_ne!(
            provider_source_config_digest(true, &[root(0xfe)]),
            provider_source_config_digest(false, &[root(0xfe)])
        );
    }

    #[test]
    fn named_source_identity_is_logical_and_path_independent() {
        let mut root = ProviderRootDefinition {
            id: "personal".to_owned(),
            provider: CaptureProvider::Claude,
            path: PathBuf::from("/old/claude"),
            group: None,
        };
        let original = ProviderRootSourceIdentity::NamedV1.lineage(&root);
        root.path = PathBuf::from("/new/claude");
        assert_eq!(original, ProviderRootSourceIdentity::NamedV1.lineage(&root));
        root.id = "work".to_owned();
        assert_ne!(original, ProviderRootSourceIdentity::NamedV1.lineage(&root));
        assert_eq!(ProviderRootSourceIdentity::Released.lineage(&root), None);
    }
}
