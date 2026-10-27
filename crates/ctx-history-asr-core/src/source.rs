use crate::Provider;
use serde_json::Value;
use std::path::PathBuf;
#[derive(Clone)]
pub struct Source {
    pub provider: Provider,
    pub path: PathBuf,
    pub session: String,
    pub header: Value,
    pub stamp: String,
    pub segment: String,
}
