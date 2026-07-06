#[allow(unused_imports)]
use super::*;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Tag {
    pub id: Uuid,
    pub name: String,
    #[serde(default)]
    pub kind: TagKind,
    #[serde(flatten)]
    pub timestamps: EntityTimestamps,
    #[serde(default = "default_metadata")]
    pub metadata: serde_json::Value,
}

pub(crate) fn default_metadata() -> serde_json::Value {
    serde_json::Value::Object(serde_json::Map::new())
}
