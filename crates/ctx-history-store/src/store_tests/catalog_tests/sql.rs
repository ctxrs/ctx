#[allow(unused_imports)]
use super::*;

pub(crate) fn legacy_history_record_sql(sql: &str) -> String {
    sql.replace("history_record_links", "work_record_links")
        .replace("history_record_tags", "work_record_tags")
        .replace("history_records", "work_records")
        .replace("history_record_id", "work_record_id")
}
