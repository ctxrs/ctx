#[allow(unused_imports)]
use super::*;

#[derive(Debug, Clone)]
pub(crate) struct DeepAgentsWriteRow {
    pub(crate) thread_id: String,
    pub(crate) checkpoint_id: String,
    pub(crate) task_id: String,
    pub(crate) idx: i64,
    pub(crate) value_type: Option<String>,
    pub(crate) value: Vec<u8>,
    pub(crate) row_number: u64,
}
