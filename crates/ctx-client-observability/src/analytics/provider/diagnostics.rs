/// Closed wire projection of the refresh engine's typed coverage blocker.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderRefreshCoverageReason {
    CatalogUnavailable,
    UnsafeRoot,
    MissingTerminalAuthority,
    RouteFailed,
    InvalidRouteIdentity,
    MissingEmptyAuthority,
}

impl ProviderRefreshCoverageReason {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::CatalogUnavailable => "catalog_unavailable",
            Self::UnsafeRoot => "unsafe_root",
            Self::MissingTerminalAuthority => "missing_terminal_authority",
            Self::RouteFailed => "route_failed",
            Self::InvalidRouteIdentity => "invalid_route_identity",
            Self::MissingEmptyAuthority => "missing_empty_authority",
        }
    }
}

/// The exact complete source-failure classification, without local detail.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderRefreshSourceFailureClass {
    Unavailable,
    SourceChanged,
    Unreadable,
    Incompatible,
    Mixed,
}

impl ProviderRefreshSourceFailureClass {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Unavailable => "unavailable",
            Self::SourceChanged => "source_changed",
            Self::Unreadable => "unreadable",
            Self::Incompatible => "incompatible",
            Self::Mixed => "mixed",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "unavailable" => Some(Self::Unavailable),
            "source_changed" => Some(Self::SourceChanged),
            "unreadable" => Some(Self::Unreadable),
            "incompatible" => Some(Self::Incompatible),
            "mixed" => Some(Self::Mixed),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderRefreshFailureReason {
    IoNotFound,
    IoPermissionDenied,
    IoStorageFull,
    IoReadOnlyFilesystem,
    IoOutOfMemory,
    IoTimedOut,
    RouteOutputLimit,
    RouteScratchLimit,
    IndexMemoryLimit,
    IndexScratchLimit,
    IndexWriterInvariant,
}

impl ProviderRefreshFailureReason {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::IoNotFound => "io_not_found",
            Self::IoPermissionDenied => "io_permission_denied",
            Self::IoStorageFull => "io_storage_full",
            Self::IoReadOnlyFilesystem => "io_read_only_filesystem",
            Self::IoOutOfMemory => "io_out_of_memory",
            Self::IoTimedOut => "io_timed_out",
            Self::RouteOutputLimit => "route_output_limit",
            Self::RouteScratchLimit => "route_scratch_limit",
            Self::IndexMemoryLimit => "index_memory_limit",
            Self::IndexScratchLimit => "index_scratch_limit",
            Self::IndexWriterInvariant => "index_writer_invariant",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        Some(match value {
            "io_not_found" => Self::IoNotFound,
            "io_permission_denied" => Self::IoPermissionDenied,
            "io_storage_full" => Self::IoStorageFull,
            "io_read_only_filesystem" => Self::IoReadOnlyFilesystem,
            "io_out_of_memory" => Self::IoOutOfMemory,
            "io_timed_out" => Self::IoTimedOut,
            "route_output_limit" => Self::RouteOutputLimit,
            "route_scratch_limit" => Self::RouteScratchLimit,
            "index_memory_limit" => Self::IndexMemoryLimit,
            "index_scratch_limit" => Self::IndexScratchLimit,
            "index_writer_invariant" => Self::IndexWriterInvariant,
            _ => return None,
        })
    }

    pub const fn permits(self, kind: super::ProviderRefreshFailureKind) -> bool {
        use super::ProviderRefreshFailureKind as Kind;
        match self {
            Self::RouteOutputLimit | Self::RouteScratchLimit => matches!(kind, Kind::Provider),
            Self::IndexMemoryLimit | Self::IndexScratchLimit | Self::IndexWriterInvariant => {
                matches!(kind, Kind::Index)
            }
            _ => matches!(kind, Kind::Io | Kind::Index | Kind::Provider),
        }
    }
}

#[cfg(test)]
mod tests;
