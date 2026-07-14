use std::path::PathBuf;

use ctx_history_core::CaptureProvider;

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

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum ProviderFileMutationContract {
    #[default]
    WholeReplacement,
    /// The provider owns an append-only, newline-delimited log. Incremental
    /// validation proves stable identity, monotonic size, and unchanged head
    /// and committed-boundary sentinels; it is not a cryptographic proof that
    /// arbitrary bytes in the historical middle were never externally edited.
    /// Such edits violate this source contract and require explicit replacement.
    AppendOnlyNewlineDelimited,
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderImportUnitOwner {
    SourceFile,
    FileNames {
        names: &'static [&'static str],
        required_component: Option<&'static str>,
    },
    Extensions {
        extensions: &'static [&'static str],
        required_component: Option<&'static str>,
        excluded_names: &'static [&'static str],
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderImportUnitGrouping {
    Each,
    FirstPerDirectory,
    AntigravitySession,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderImportDependency {
    SqliteSidecars,
    SiblingFile(&'static str),
    AncestorFile { levels: usize, name: &'static str },
    NearestAncestorFile(&'static str),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderImportUnitSpec {
    WholeSource,
    PerFile {
        owner: ProviderImportUnitOwner,
        grouping: ProviderImportUnitGrouping,
        dependencies: &'static [ProviderImportDependency],
    },
}

impl ProviderImportUnitSpec {
    pub fn uses_file_manifest(self) -> bool {
        matches!(self, Self::PerFile { .. })
    }
}

#[derive(Debug, Clone, Copy)]
pub struct ProviderSourceSpec {
    pub provider: CaptureProvider,
    pub display_name: &'static str,
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
    pub import_revision: u32,
    pub source_kind: ProviderSourceKind,
    pub import_support: ProviderImportSupport,
    pub catalog_support: ProviderCatalogSupport,
    pub import_unit: ProviderImportUnitSpec,
    pub mutation_contract: ProviderFileMutationContract,
    pub status: ProviderSourceStatus,
    pub unsupported_reason: Option<&'static str>,
}
