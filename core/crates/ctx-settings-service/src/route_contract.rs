#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SettingsRouteErrorKind {
    Forbidden,
    Internal,
}

#[derive(Debug, Clone)]
pub struct SettingsRouteError {
    kind: SettingsRouteErrorKind,
}

impl SettingsRouteError {
    pub fn forbidden(_error: impl std::fmt::Display) -> Self {
        Self {
            kind: SettingsRouteErrorKind::Forbidden,
        }
    }

    pub fn internal(_error: impl std::fmt::Display) -> Self {
        Self {
            kind: SettingsRouteErrorKind::Internal,
        }
    }

    pub fn kind(&self) -> SettingsRouteErrorKind {
        self.kind
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_route_errors_expose_only_status_classification() {
        assert_eq!(
            SettingsRouteError::forbidden("host execution denied").kind(),
            SettingsRouteErrorKind::Forbidden
        );
        assert_eq!(
            SettingsRouteError::internal("store failed").kind(),
            SettingsRouteErrorKind::Internal
        );
    }
}
