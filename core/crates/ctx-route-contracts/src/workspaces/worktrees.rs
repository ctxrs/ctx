use serde::Deserialize;

#[derive(Debug, Clone, Deserialize, Default, Eq, PartialEq)]
pub struct WorkspaceFileCompletionsRouteQuery {
    #[serde(default)]
    query: Option<String>,
    #[serde(default)]
    limit: Option<u32>,
}

impl WorkspaceFileCompletionsRouteQuery {
    pub fn into_parts(self) -> (Option<String>, Option<u32>) {
        (self.query, self.limit)
    }
}
