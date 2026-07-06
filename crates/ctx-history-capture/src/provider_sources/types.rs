use std::path::PathBuf;

use ctx_history_core::{CaptureProvider, ProviderRawRetention, ProviderRedactionBoundary};

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

#[derive(Debug, Clone, Copy)]
pub struct ProviderDefaultLocation {
    pub path_components: &'static [&'static str],
    pub source_format: &'static str,
    pub source_kind: ProviderSourceKind,
}

#[derive(Debug, Clone, Copy)]
pub struct ProviderSourceSpec {
    pub provider: CaptureProvider,
    pub display_name: &'static str,
    pub default_locations: &'static [ProviderDefaultLocation],
    pub import_support: ProviderImportSupport,
    pub catalog_support: ProviderCatalogSupport,
    pub raw_retention: ProviderRawRetention,
    pub redaction_boundary: ProviderRedactionBoundary,
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
    pub raw_retention: ProviderRawRetention,
    pub redaction_boundary: ProviderRedactionBoundary,
    pub unsupported_reason: Option<&'static str>,
}
