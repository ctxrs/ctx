use std::{
    ops::Deref,
    path::{Path, PathBuf},
};

use ctx_history_core::CaptureProvider;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

pub const MAX_CONFIGURED_PROVIDER_ROOTS: usize = 64;
pub const MAX_PROVIDER_ROOT_SELECTOR_BYTES: usize = 64;
pub const MAX_PROVIDER_ROOT_ENCODED_PATH_BYTES: usize = 16 * 1024;

/// Bounded, deterministic provider-root input for discovery and persistence.
///
/// This type owns only resource bounds and ordering. Provider support,
/// filesystem kind, physical equivalence, and overlap policy require the
/// caller's current configuration or filesystem authority and remain outside
/// this value object.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProviderRootSet(Vec<ProviderRootDefinition>);

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ProviderRootSetError {
    #[error("configured provider roots exceed the maximum of {MAX_CONFIGURED_PROVIDER_ROOTS}")]
    TooMany,
    #[error(
        "configured provider root `{root_id}` exceeds the {MAX_PROVIDER_ROOT_ENCODED_PATH_BYTES}-byte encoded path limit"
    )]
    PathTooLong { root_id: String },
}

impl ProviderRootSet {
    /// Validates every supplied root instead of silently dropping input.
    pub fn try_new(mut roots: Vec<ProviderRootDefinition>) -> Result<Self, ProviderRootSetError> {
        if roots.len() > MAX_CONFIGURED_PROVIDER_ROOTS {
            return Err(ProviderRootSetError::TooMany);
        }
        if let Some(root) = roots.iter().find(|root| !root.has_bounded_path()) {
            return Err(ProviderRootSetError::PathTooLong {
                root_id: root.id.clone(),
            });
        }
        roots.sort_by(|left, right| left.id.cmp(&right.id));
        Ok(Self(roots))
    }

    /// Compatibility boundary for public discovery contexts built from
    /// untrusted definitions. Invalid paths are omitted and excess roots are
    /// deterministically truncated, preserving the historical non-failing
    /// builder while enforcing the shared discovery work bound.
    pub fn from_untrusted_lossy(mut roots: Vec<ProviderRootDefinition>) -> Self {
        roots.retain(ProviderRootDefinition::has_bounded_path);
        roots.sort_by(|left, right| left.id.cmp(&right.id));
        roots.truncate(MAX_CONFIGURED_PROVIDER_ROOTS);
        Self(roots)
    }

    pub fn as_slice(&self) -> &[ProviderRootDefinition] {
        &self.0
    }
}

impl Deref for ProviderRootSet {
    type Target = [ProviderRootDefinition];

    fn deref(&self) -> &Self::Target {
        self.as_slice()
    }
}

/// Platform-native encoded length used by configured-root resource bounds.
pub fn provider_root_encoded_path_len(path: &Path) -> usize {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        path.as_os_str().as_bytes().len()
    }
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;
        path.as_os_str().encode_wide().count().saturating_mul(2)
    }
    #[cfg(not(any(unix, windows)))]
    {
        path.as_os_str().to_string_lossy().len()
    }
}

pub fn provider_root_path_within_limit(path: &Path) -> bool {
    provider_root_encoded_path_len(path) <= MAX_PROVIDER_ROOT_ENCODED_PATH_BYTES
}

/// Exact persisted OpenHands history layout selected by a configured root.
///
/// This is deliberately not a provider-general selector: OpenHands has two
/// incompatible native history layouts whose paths alone do not establish the
/// intended contract.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ProviderRootKind {
    #[serde(rename = "current-conversations")]
    OpenHandsCurrentConversations,
    #[serde(rename = "legacy-persistence")]
    OpenHandsLegacyPersistence,
}

impl ProviderRootKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::OpenHandsCurrentConversations => "current-conversations",
            Self::OpenHandsLegacyPersistence => "legacy-persistence",
        }
    }
}

impl std::fmt::Display for ProviderRootKind {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl std::str::FromStr for ProviderRootKind {
    type Err = &'static str;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "current-conversations" => Ok(Self::OpenHandsCurrentConversations),
            "legacy-persistence" => Ok(Self::OpenHandsLegacyPersistence),
            _ => Err("expected current-conversations or legacy-persistence"),
        }
    }
}

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

/// Immutable automatic-discovery authority retained by a released root.
///
/// A configured definition records the current scan path. This binding records
/// whether released identity is path-independent or retains an original
/// automatic root that must survive later configured-path moves.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReleasedProviderRootAutomaticRole {
    source_format: String,
    configured_route_role: Vec<u8>,
    role: Vec<u8>,
}

impl ReleasedProviderRootAutomaticRole {
    pub fn new(source_format: String, configured_route_role: Vec<u8>, role: Vec<u8>) -> Self {
        Self {
            source_format,
            configured_route_role,
            role,
        }
    }

    pub fn source_format(&self) -> &str {
        &self.source_format
    }

    pub fn configured_route_role(&self) -> &[u8] {
        &self.configured_route_role
    }

    pub fn role(&self) -> &[u8] {
        &self.role
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum ProviderRootConnectorBinding {
    ReleasedPathIndependentV1,
    ReleasedRootedV1 {
        identity_root: PathBuf,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        automatic_route_roles: Vec<ReleasedProviderRootAutomaticRole>,
    },
}

impl ProviderRootConnectorBinding {
    pub const fn released_path_independent_v1() -> Self {
        Self::ReleasedPathIndependentV1
    }

    pub fn released_rooted_v1(identity_root: impl Into<PathBuf>) -> Self {
        Self::ReleasedRootedV1 {
            identity_root: identity_root.into(),
            automatic_route_roles: Vec::new(),
        }
    }

    pub fn identity_root(&self) -> Option<&std::path::Path> {
        match self {
            Self::ReleasedPathIndependentV1 => None,
            Self::ReleasedRootedV1 { identity_root, .. } => Some(identity_root),
        }
    }

    pub fn automatic_route_roles(&self) -> &[ReleasedProviderRootAutomaticRole] {
        match self {
            Self::ReleasedPathIndependentV1 => &[],
            Self::ReleasedRootedV1 {
                automatic_route_roles,
                ..
            } => automatic_route_roles,
        }
    }

    pub fn automatic_route_role(
        &self,
        source_format: &str,
        configured_route_role: &[u8],
    ) -> Option<&[u8]> {
        self.automatic_route_roles()
            .iter()
            .find(|role| {
                role.source_format == source_format
                    && role.configured_route_role == configured_route_role
            })
            .map(ReleasedProviderRootAutomaticRole::role)
    }

    pub fn with_automatic_route_roles(
        mut self,
        automatic_route_roles: Vec<ReleasedProviderRootAutomaticRole>,
    ) -> Self {
        if let Self::ReleasedRootedV1 {
            automatic_route_roles: retained,
            ..
        } = &mut self
        {
            *retained = automatic_route_roles;
        }
        self
    }
}

/// Minimal retained root state needed to reconstruct discovery authority.
///
/// Generation route membership remains index-owned and is intentionally not
/// exposed through the capture facade.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RetainedProviderRootAuthority {
    NamedV1,
    Released(ProviderRootConnectorBinding),
}

impl RetainedProviderRootAuthority {
    pub const fn named_v1() -> Self {
        Self::NamedV1
    }

    pub fn released(binding: ProviderRootConnectorBinding) -> Self {
        Self::Released(binding)
    }

    pub const fn source_identity(&self) -> ProviderRootSourceIdentity {
        match self {
            Self::NamedV1 => ProviderRootSourceIdentity::NamedV1,
            Self::Released(_) => ProviderRootSourceIdentity::Released,
        }
    }

    pub fn connector_binding(&self) -> Option<&ProviderRootConnectorBinding> {
        match self {
            Self::NamedV1 => None,
            Self::Released(binding) => Some(binding),
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<ProviderRootKind>,
}

impl ProviderRootDefinition {
    /// Whether this definition can cross discovery and persistence boundaries.
    pub fn has_bounded_path(&self) -> bool {
        provider_root_path_within_limit(&self.path)
    }

    /// Validates the narrow provider/kind pairing at every persisted boundary.
    pub const fn has_valid_kind(&self) -> bool {
        match self.provider {
            CaptureProvider::OpenHands => self.kind.is_some(),
            _ => self.kind.is_none(),
        }
    }

    /// OpenHands legacy persistence recursively owns its configured directory.
    /// A legacy/current ancestor relationship can therefore select the same
    /// history from both roots, while disjoint roots remain independently
    /// valid.
    pub fn openhands_selected_histories_overlap(&self, other: &Self) -> bool {
        let (legacy, current) = match (self.provider, self.kind, other.provider, other.kind) {
            (
                CaptureProvider::OpenHands,
                Some(ProviderRootKind::OpenHandsLegacyPersistence),
                CaptureProvider::OpenHands,
                Some(ProviderRootKind::OpenHandsCurrentConversations),
            ) => (self, other),
            (
                CaptureProvider::OpenHands,
                Some(ProviderRootKind::OpenHandsCurrentConversations),
                CaptureProvider::OpenHands,
                Some(ProviderRootKind::OpenHandsLegacyPersistence),
            ) => (other, self),
            _ => return false,
        };
        current.path.starts_with(&legacy.path) || legacy.path.starts_with(&current.path)
    }
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
                if let Some(kind) = root.kind {
                    // Kindless non-Unicode definitions used this native-path
                    // fallback before root kinds existed. Append only a
                    // present kind so their released digest stays byte-for-byte
                    // compatible with that parent contract.
                    digest.update([1]);
                    let kind = kind.as_str().as_bytes();
                    digest.update((kind.len() as u64).to_be_bytes());
                    digest.update(kind);
                }
            }
        }
    }
    format!("{:x}", digest.finalize())
}

#[cfg(test)]
mod tests {
    #[cfg(unix)]
    use std::{ffi::OsString, os::unix::ffi::OsStringExt};

    use super::*;

    fn claude_root(id: impl Into<String>, path: impl Into<PathBuf>) -> ProviderRootDefinition {
        ProviderRootDefinition {
            id: id.into(),
            provider: CaptureProvider::Claude,
            path: path.into(),
            group: None,
            kind: None,
        }
    }

    #[test]
    fn provider_root_set_rejects_unbounded_input_and_orders_valid_roots() {
        let roots = ProviderRootSet::try_new(vec![
            claude_root("work", "/history/work"),
            claude_root("personal", "/history/personal"),
        ])
        .unwrap();
        assert_eq!(
            roots
                .iter()
                .map(|root| root.id.as_str())
                .collect::<Vec<_>>(),
            ["personal", "work"]
        );

        let overlong = PathBuf::from("x".repeat(MAX_PROVIDER_ROOT_ENCODED_PATH_BYTES + 1));
        assert_eq!(
            ProviderRootSet::try_new(vec![claude_root("oversized", overlong)]),
            Err(ProviderRootSetError::PathTooLong {
                root_id: "oversized".to_owned(),
            })
        );

        let excess = (0..=MAX_CONFIGURED_PROVIDER_ROOTS)
            .map(|index| claude_root(format!("root-{index:03}"), format!("/history/{index}")))
            .collect();
        assert_eq!(
            ProviderRootSet::try_new(excess),
            Err(ProviderRootSetError::TooMany)
        );
    }

    #[test]
    fn lossy_provider_root_set_enforces_the_same_work_bound_deterministically() {
        let roots = (0..=MAX_CONFIGURED_PROVIDER_ROOTS)
            .rev()
            .map(|index| claude_root(format!("root-{index:03}"), format!("/history/{index}")))
            .collect();
        let roots = ProviderRootSet::from_untrusted_lossy(roots);

        assert_eq!(roots.len(), MAX_CONFIGURED_PROVIDER_ROOTS);
        assert_eq!(roots.first().unwrap().id, "root-000");
        assert_eq!(roots.last().unwrap().id, "root-063");
    }

    #[cfg(unix)]
    #[test]
    fn digest_is_total_and_distinct_for_non_unicode_public_api_paths() {
        let root = |byte| ProviderRootDefinition {
            id: "fixture".to_owned(),
            provider: CaptureProvider::Claude,
            path: PathBuf::from(OsString::from_vec(vec![b'/', b't', b'm', b'p', b'/', byte])),
            group: None,
            kind: None,
        };

        assert_ne!(
            provider_source_config_digest(true, &[root(0xfe)]),
            provider_source_config_digest(true, &[root(0xff)])
        );
        assert_ne!(
            provider_source_config_digest(true, &[root(0xfe)]),
            provider_source_config_digest(false, &[root(0xfe)])
        );
        assert_eq!(
            provider_source_config_digest(true, &[root(0xfe)]),
            "c22563f67d60c115ac159e0a18909e5b25e56477b6dac4eb97493fae201c66c7",
            "kindless native-path fallback must match the pre-kind parent golden"
        );
    }

    #[test]
    fn named_source_identity_is_logical_and_path_independent() {
        let mut root = ProviderRootDefinition {
            id: "personal".to_owned(),
            provider: CaptureProvider::Claude,
            path: PathBuf::from("/old/claude"),
            group: None,
            kind: None,
        };
        let original = ProviderRootSourceIdentity::NamedV1.lineage(&root);
        root.path = PathBuf::from("/new/claude");
        assert_eq!(original, ProviderRootSourceIdentity::NamedV1.lineage(&root));
        root.id = "work".to_owned();
        assert_ne!(original, ProviderRootSourceIdentity::NamedV1.lineage(&root));
        assert_eq!(ProviderRootSourceIdentity::Released.lineage(&root), None);
    }

    #[test]
    fn openhands_kind_has_exact_wire_spellings_and_changes_config_digest_only() {
        let mut root = ProviderRootDefinition {
            id: "work".to_owned(),
            provider: CaptureProvider::OpenHands,
            path: PathBuf::from("/history/openhands"),
            group: None,
            kind: Some(ProviderRootKind::OpenHandsCurrentConversations),
        };
        assert_eq!(
            serde_json::to_string(&root).unwrap(),
            r#"{"id":"work","provider":"openhands","path":"/history/openhands","kind":"current-conversations"}"#
        );
        assert_eq!(
            "legacy-persistence".parse(),
            Ok(ProviderRootKind::OpenHandsLegacyPersistence)
        );
        assert!("Current-Conversations".parse::<ProviderRootKind>().is_err());
        let current_digest = provider_source_config_digest(true, std::slice::from_ref(&root));
        let lineage = ProviderRootSourceIdentity::NamedV1.lineage(&root);
        root.kind = Some(ProviderRootKind::OpenHandsLegacyPersistence);
        assert_ne!(
            current_digest,
            provider_source_config_digest(true, std::slice::from_ref(&root))
        );
        assert_eq!(lineage, ProviderRootSourceIdentity::NamedV1.lineage(&root));
    }

    #[test]
    fn openhands_cross_kind_overlap_rejects_either_ancestor_orientation() {
        let root = |id: &str, path: &str, kind| ProviderRootDefinition {
            id: id.to_owned(),
            provider: CaptureProvider::OpenHands,
            path: PathBuf::from(path),
            group: None,
            kind: Some(kind),
        };
        let legacy_parent = root(
            "legacy-parent",
            "/history/openhands",
            ProviderRootKind::OpenHandsLegacyPersistence,
        );
        let current_child = root(
            "current-child",
            "/history/openhands/conversations",
            ProviderRootKind::OpenHandsCurrentConversations,
        );
        assert!(legacy_parent.openhands_selected_histories_overlap(&current_child));
        assert!(current_child.openhands_selected_histories_overlap(&legacy_parent));

        let current_parent = root(
            "current-parent",
            "/history",
            ProviderRootKind::OpenHandsCurrentConversations,
        );
        let legacy_child = root(
            "legacy-child",
            "/history/persistence",
            ProviderRootKind::OpenHandsLegacyPersistence,
        );
        assert!(current_parent.openhands_selected_histories_overlap(&legacy_child));
        assert!(legacy_child.openhands_selected_histories_overlap(&current_parent));

        let disjoint = root(
            "disjoint",
            "/other/history",
            ProviderRootKind::OpenHandsCurrentConversations,
        );
        assert!(!legacy_parent.openhands_selected_histories_overlap(&disjoint));
    }

    #[test]
    fn old_provider_json_and_digest_remain_byte_compatible() {
        let root = ProviderRootDefinition {
            id: "work".to_owned(),
            provider: CaptureProvider::Claude,
            path: PathBuf::from("/history/claude"),
            group: Some("team".to_owned()),
            kind: None,
        };
        assert_eq!(
            serde_json::to_string(&root).unwrap(),
            r#"{"id":"work","provider":"claude","path":"/history/claude","group":"team"}"#
        );
        assert_eq!(
            provider_source_config_digest(true, std::slice::from_ref(&root)),
            "3ed4b8cc54b28c0c87bde2fb771ee2b60d57fd27c84833b6e245b262f3c24bcd"
        );
    }
}
