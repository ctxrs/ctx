use super::*;

#[derive(Debug, Deserialize)]
pub(crate) struct WebSessionStreamAccessQuery {
    pub(crate) token: Option<String>,
}
