//! Canonical indexing policy, including the retained legacy daemon mapping.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum IndexingMode {
    #[default]
    Automatic,
    Manual,
}

impl IndexingMode {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Automatic => "auto",
            Self::Manual => "manual",
        }
    }

    pub const fn is_automatic(self) -> bool {
        matches!(self, Self::Automatic)
    }

    pub(super) const fn from_legacy_daemon_enabled(enabled: bool) -> Self {
        if enabled {
            Self::Automatic
        } else {
            Self::Manual
        }
    }
}
