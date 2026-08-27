#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum SemanticIndexingIntensity {
    #[default]
    Quiet,
    Full,
}

impl SemanticIndexingIntensity {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Quiet => "quiet",
            Self::Full => "full",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum SemanticEnabledSource {
    Default,
    Config,
    Environment,
}

impl SemanticEnabledSource {
    pub(super) const fn as_str(self) -> &'static str {
        match self {
            Self::Default => "default",
            Self::Config => "config",
            Self::Environment => "environment",
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(super) enum SemanticIndexingIntensitySource {
    #[default]
    Default,
    Config,
}

impl SemanticIndexingIntensitySource {
    pub(super) const fn as_str(self) -> &'static str {
        match self {
            Self::Default => "default",
            Self::Config => "config",
        }
    }
}
