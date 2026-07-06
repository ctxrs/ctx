use std::path::PathBuf;

#[derive(Debug, Clone, Default)]
pub struct InitOptions {
    pub catalog_only: bool,
}

#[derive(Debug, Clone, Default)]
pub struct ImportOptions {
    pub provider: Option<String>,
    pub path: Option<PathBuf>,
    pub all: bool,
    pub resume: bool,
}

#[derive(Debug, Clone)]
pub struct SearchOptions {
    pub query: Option<String>,
    pub terms: Vec<String>,
    pub limit: usize,
    pub provider: Option<String>,
    pub workspace: Option<String>,
    pub since: Option<String>,
    pub file: Option<PathBuf>,
    pub session: Option<String>,
    pub events: bool,
    pub refresh: SearchRefresh,
    pub include_current_session: bool,
}

impl Default for SearchOptions {
    fn default() -> Self {
        Self {
            query: None,
            terms: Vec::new(),
            limit: 20,
            provider: None,
            workspace: None,
            since: None,
            file: None,
            session: None,
            events: false,
            refresh: SearchRefresh::Auto,
            include_current_session: false,
        }
    }
}

impl SearchOptions {
    pub(crate) fn has_intent(&self) -> bool {
        self.query
            .as_deref()
            .map(str::trim)
            .is_some_and(|query| !query.is_empty())
            || self.terms.iter().any(|term| !term.trim().is_empty())
            || self
                .file
                .as_ref()
                .map(|path| !path.to_string_lossy().trim().is_empty())
                .unwrap_or(false)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SearchRefresh {
    Auto,
    Off,
    Strict,
}

impl SearchRefresh {
    pub(crate) fn as_arg(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Off => "off",
            Self::Strict => "strict",
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct ShowEventOptions {
    pub before: usize,
    pub after: usize,
    pub window: Option<usize>,
}

#[derive(Debug, Clone)]
pub struct ShowSessionOptions {
    pub mode: String,
}

impl Default for ShowSessionOptions {
    fn default() -> Self {
        Self {
            mode: "lite".to_owned(),
        }
    }
}
