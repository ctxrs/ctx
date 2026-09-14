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

#[cfg(test)]
mod tests;
