use std::path::PathBuf;

use ctx_history_core::CaptureProvider;

use crate::ProviderRouteRole;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiscoveryIssueKind {
    NoDiskHistory,
    SelectorUnreconstructible,
    InsufficientOfficialEvidence,
    ConfiguredRootConflict,
    /// A persistent configured root is absent and cannot yield a concrete
    /// provider route to list. The root remains configured for refresh and
    /// removal purposes.
    ConfiguredRootMissing,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiscoveryIssue {
    pub provider: CaptureProvider,
    pub path: Option<PathBuf>,
    pub kind: DiscoveryIssueKind,
    pub reason: &'static str,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct DiscoveryReport {
    pub sources: Vec<ProviderSource>,
    pub issues: Vec<DiscoveryIssue>,
}

/// Route identity provenance emitted by provider discovery.
///
/// Legacy automatic and exact sources remain unroled. Providers with multiple
/// independently owned routes of one format may emit an automatic role, while
/// configured expansion additionally carries exact root ownership. This is a
/// capture-only value and is not a source identity scope.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum ProviderSourceRouteProvenance {
    #[default]
    Unroled,
    Automatic {
        route_role: ProviderRouteRole,
    },
    ConfiguredRoot {
        root_id: String,
        root_path: PathBuf,
        route_role: ProviderRouteRole,
        automatic_route_role: Option<ProviderRouteRole>,
    },
}

impl ProviderSourceRouteProvenance {
    pub fn route_role(&self) -> Option<&ProviderRouteRole> {
        match self {
            Self::Unroled => None,
            Self::Automatic { route_role } | Self::ConfiguredRoot { route_role, .. } => {
                Some(route_role)
            }
        }
    }

    pub fn automatic_route_role(&self) -> Option<&ProviderRouteRole> {
        match self {
            Self::Automatic { route_role } => Some(route_role),
            Self::ConfiguredRoot {
                automatic_route_role,
                ..
            } => automatic_route_role.as_ref(),
            Self::Unroled => None,
        }
    }

    pub fn configured_root(&self) -> Option<(&str, &std::path::Path)> {
        match self {
            Self::ConfiguredRoot {
                root_id, root_path, ..
            } => Some((root_id, root_path)),
            Self::Unroled | Self::Automatic { .. } => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderSourceKind {
    NativeHistory,
    DetectionOnly,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderImportSupport {
    Native,
    Explicit,
    Unsupported,
}

impl ProviderImportSupport {
    pub fn is_importable(self) -> bool {
        matches!(self, Self::Native | Self::Explicit)
    }

    pub fn is_auto_importable(self) -> bool {
        matches!(self, Self::Native)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderCatalogSupport {
    Native,
    None,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderSourceStatus {
    Available,
    Empty,
    Unknown,
    Missing,
    Unsupported,
}

impl ProviderSourceStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Available => "available",
            Self::Empty => "empty",
            Self::Unknown => "unknown",
            Self::Missing => "missing",
            Self::Unsupported => "unsupported",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderSourceStatusReason {
    BlockedAuthOrEncryption,
}

impl ProviderSourceStatusReason {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::BlockedAuthOrEncryption => "blocked_auth_or_encryption",
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct ProviderDefaultLocation {
    pub path_components: &'static [&'static str],
    pub source_format: &'static str,
    pub source_kind: ProviderSourceKind,
}

#[derive(Debug, Clone, Copy)]
pub struct ProviderSourceSpec {
    pub provider: CaptureProvider,
    pub default_locations: &'static [ProviderDefaultLocation],
    pub import_support: ProviderImportSupport,
    pub catalog_support: ProviderCatalogSupport,
    pub unsupported_reason: Option<&'static str>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderSource {
    pub provider: CaptureProvider,
    pub path: PathBuf,
    pub exists: bool,
    pub source_format: &'static str,
    pub source_kind: ProviderSourceKind,
    pub import_support: ProviderImportSupport,
    pub catalog_support: ProviderCatalogSupport,
    pub status: ProviderSourceStatus,
    pub unsupported_reason: Option<&'static str>,
    pub route_provenance: ProviderSourceRouteProvenance,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderSourceFailureKind {
    NotFound,
    Permission,
    Locked,
    Corrupt,
    SchemaIncompatible,
    InvalidSource,
    SourceChanged,
    SourceDatabase,
    Io,
}

impl std::fmt::Display for ProviderSourceFailureKind {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let value = match self {
            Self::NotFound => "not_found",
            Self::Permission => "permission",
            Self::Locked => "locked",
            Self::Corrupt => "corrupt",
            Self::SchemaIncompatible => "schema_incompatible",
            Self::InvalidSource => "invalid_source",
            Self::SourceChanged => "source_changed",
            Self::SourceDatabase => "source_database",
            Self::Io => "io",
        };
        formatter.write_str(value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn source_status_and_failure_strings_are_stable() {
        for (value, spelling) in [
            (ProviderSourceStatus::Available, "available"),
            (ProviderSourceStatus::Empty, "empty"),
            (ProviderSourceStatus::Unknown, "unknown"),
            (ProviderSourceStatus::Missing, "missing"),
            (ProviderSourceStatus::Unsupported, "unsupported"),
        ] {
            assert_eq!(value.as_str(), spelling);
        }
        assert_eq!(
            ProviderSourceStatusReason::BlockedAuthOrEncryption.as_str(),
            "blocked_auth_or_encryption"
        );
        for (value, spelling) in [
            (ProviderSourceFailureKind::NotFound, "not_found"),
            (ProviderSourceFailureKind::Permission, "permission"),
            (ProviderSourceFailureKind::Locked, "locked"),
            (ProviderSourceFailureKind::Corrupt, "corrupt"),
            (
                ProviderSourceFailureKind::SchemaIncompatible,
                "schema_incompatible",
            ),
            (ProviderSourceFailureKind::InvalidSource, "invalid_source"),
            (ProviderSourceFailureKind::SourceChanged, "source_changed"),
            (ProviderSourceFailureKind::SourceDatabase, "source_database"),
            (ProviderSourceFailureKind::Io, "io"),
        ] {
            assert_eq!(value.to_string(), spelling);
        }
    }

    #[test]
    fn source_enum_debug_spellings_are_stable() {
        for (value, spelling) in [
            (DiscoveryIssueKind::NoDiskHistory, "NoDiskHistory"),
            (
                DiscoveryIssueKind::SelectorUnreconstructible,
                "SelectorUnreconstructible",
            ),
            (
                DiscoveryIssueKind::InsufficientOfficialEvidence,
                "InsufficientOfficialEvidence",
            ),
            (
                DiscoveryIssueKind::ConfiguredRootConflict,
                "ConfiguredRootConflict",
            ),
            (
                DiscoveryIssueKind::ConfiguredRootMissing,
                "ConfiguredRootMissing",
            ),
        ] {
            assert_eq!(format!("{value:?}"), spelling);
        }
        for (value, spelling) in [
            (ProviderSourceKind::NativeHistory, "NativeHistory"),
            (ProviderSourceKind::DetectionOnly, "DetectionOnly"),
        ] {
            assert_eq!(format!("{value:?}"), spelling);
        }
        for (value, spelling) in [
            (ProviderImportSupport::Native, "Native"),
            (ProviderImportSupport::Explicit, "Explicit"),
            (ProviderImportSupport::Unsupported, "Unsupported"),
        ] {
            assert_eq!(format!("{value:?}"), spelling);
        }
        for (value, spelling) in [
            (ProviderCatalogSupport::Native, "Native"),
            (ProviderCatalogSupport::None, "None"),
        ] {
            assert_eq!(format!("{value:?}"), spelling);
        }
    }

    #[test]
    fn import_support_predicates_are_stable() {
        assert!(ProviderImportSupport::Native.is_importable());
        assert!(ProviderImportSupport::Native.is_auto_importable());
        assert!(ProviderImportSupport::Explicit.is_importable());
        assert!(!ProviderImportSupport::Explicit.is_auto_importable());
        assert!(!ProviderImportSupport::Unsupported.is_importable());
    }
}
