use crate::{records::Writer, spool::Spool, Result};
use serde_json::Value;
pub(super) fn emit(
    spool: &Spool,
    writer: &mut Writer,
    record: Value,
    source: &Value,
    response: Option<&str>,
    first: bool,
) -> Result<()> {
    crate::tree::emit(
        spool,
        writer,
        record,
        source,
        response,
        first,
        ("uuid", "parentUuid"),
    )
}
