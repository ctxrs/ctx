use serde::Deserialize;
use serde_json::Value;

#[derive(Debug, Default, Clone, Deserialize)]
pub struct ProviderStatusRouteQuery {
    target: Option<String>,
}

impl ProviderStatusRouteQuery {
    pub fn new(target: Option<String>) -> Self {
        Self { target }
    }

    pub fn target(&self) -> Option<&str> {
        self.target.as_deref()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProviderStatusListRouteError;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderStatusRouteErrorKind {
    BadRequest,
    NotFound,
}

#[derive(Debug, Clone)]
pub struct ProviderStatusRouteError {
    kind: ProviderStatusRouteErrorKind,
    body: Value,
}

impl ProviderStatusRouteError {
    pub fn new(kind: ProviderStatusRouteErrorKind, body: Value) -> Self {
        Self { kind, body }
    }

    pub fn bad_request(message: impl Into<String>) -> Self {
        Self::message(ProviderStatusRouteErrorKind::BadRequest, message)
    }

    pub fn not_found(message: impl Into<String>) -> Self {
        Self::message(ProviderStatusRouteErrorKind::NotFound, message)
    }

    pub fn kind(&self) -> ProviderStatusRouteErrorKind {
        self.kind
    }

    pub fn body(&self) -> &Value {
        &self.body
    }

    fn message(kind: ProviderStatusRouteErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            body: serde_json::json!({
                "error": message.into(),
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn query_preserves_optional_target() {
        assert_eq!(ProviderStatusRouteQuery::default().target(), None);
        assert_eq!(
            ProviderStatusRouteQuery::new(Some("host".to_string())).target(),
            Some("host")
        );
        assert_eq!(
            ProviderStatusRouteQuery::new(Some(String::new())).target(),
            Some("")
        );
    }

    #[test]
    fn error_preserves_kind_and_json_message_body() {
        let error = ProviderStatusRouteError::not_found("provider not found: codex");

        assert_eq!(error.kind(), ProviderStatusRouteErrorKind::NotFound);
        assert_eq!(
            error.body()["error"].as_str(),
            Some("provider not found: codex")
        );
    }
}
