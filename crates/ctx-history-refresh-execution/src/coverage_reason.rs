/// Content-free publication blocker; never authority for recovery or retry policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ZeroSourcePublicationBlockReason {
    CatalogUnavailable,
    UnsafeRoot,
    MissingTerminalAuthority,
    RouteFailed,
    InvalidRouteIdentity,
    MissingEmptyAuthority,
}

impl ZeroSourcePublicationBlockReason {
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

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "catalog_unavailable" => Some(Self::CatalogUnavailable),
            "unsafe_root" => Some(Self::UnsafeRoot),
            "missing_terminal_authority" => Some(Self::MissingTerminalAuthority),
            "route_failed" => Some(Self::RouteFailed),
            "invalid_route_identity" => Some(Self::InvalidRouteIdentity),
            "missing_empty_authority" => Some(Self::MissingEmptyAuthority),
            _ => None,
        }
    }
}
