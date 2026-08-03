#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub(crate) enum SourceBackedRefreshMode {
    Off,
    Background,
    Wait,
}

impl SourceBackedRefreshMode {
    pub(super) fn as_str(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Background => "background",
            Self::Wait => "wait",
        }
    }
}
