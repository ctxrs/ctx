use serde::Serialize;

#[derive(Debug, Serialize)]
pub(crate) struct ApiErrorResp {
    pub(crate) error: String,
}
