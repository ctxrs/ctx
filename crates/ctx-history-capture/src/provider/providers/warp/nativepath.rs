//! Source-backed Warp parsing and task-message evidence.

use rusqlite::Connection;

use super::schema::WarpSqliteSchema;
use crate::{CaptureError, Result};

mod decode;
mod model;
mod query;

pub(super) use model::{
    WarpNativeCounters, WarpNativeEvent, WarpNativeMessageIdentity, WarpNativePage,
    WarpNativeSession, WarpNativeSink,
};
#[cfg(test)]
pub(super) use model::{WarpNativeEventIdentity, WarpNativeOrder};
use query::scan_warp_native_pinned_snapshot;

pub(in super::super) struct WarpNativeSourceBackedScan {
    pub(in super::super) source_integrity_digest: String,
    pub(in super::super) counters: WarpNativeCounters,
}

/// Runs the Warp parser against a caller-owned, certified SQLite read
/// connection. Publication and source lifecycle are owned by the shared
/// source-backed coordinator.
pub(in super::super) fn scan_warp_source_backed_connection(
    connection: &Connection,
    sink: &mut dyn WarpNativeSink,
) -> Result<WarpNativeSourceBackedScan> {
    let schema = WarpSqliteSchema::detect(connection)?;
    validate_source_paging_compatibility(connection)?;
    let result = scan_warp_native_pinned_snapshot(connection, &schema, sink)?;
    Ok(WarpNativeSourceBackedScan {
        source_integrity_digest: result.source_integrity_digest,
        counters: result.counters,
    })
}

fn validate_source_paging_compatibility(connection: &Connection) -> Result<()> {
    let invalid_rowid: bool = connection.query_row(
        "select exists(select 1 from agent_conversations where rowid <= 0)
             or exists(select 1 from agent_tasks where rowid <= 0)",
        [],
        |row| row.get(0),
    )?;
    if invalid_rowid {
        return Err(CaptureError::InvalidPayload(
            "Warp source-backed paging requires positive 64-bit source rowids".to_owned(),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use rusqlite::{params, Connection};

    use super::*;

    #[derive(Default)]
    struct CollectingSink {
        pages: Vec<WarpNativePage>,
    }

    impl WarpNativeSink for CollectingSink {
        fn push_page(&mut self, page: WarpNativePage) -> Result<()> {
            self.pages.push(page);
            Ok(())
        }
    }

    #[test]
    fn unknown_message_oneof_is_a_complete_ignored_logical_unit() {
        let connection = Connection::open_in_memory().unwrap();
        connection
            .execute_batch(
                "create table agent_conversations (
                     conversation_id text not null,
                     conversation_data text not null,
                     last_modified_at text not null
                 );
                 create table agent_tasks (
                     conversation_id text not null,
                     task_id text not null,
                     task blob not null,
                     last_modified_at text not null
                 );
                 create unique index warp_agent_tasks_task_id
                     on agent_tasks(task_id collate binary);",
            )
            .unwrap();
        connection
            .execute(
                "insert into agent_conversations values (?1, ?2, ?3)",
                params!["conversation-unknown", "{}", "2026-01-01 00:00:00"],
            )
            .unwrap();
        let mut message = Vec::new();
        push_length_delimited(&mut message, 17, b"future-oneof-body");
        let mut task = Vec::new();
        push_length_delimited(&mut task, 5, &message);
        connection
            .execute(
                "insert into agent_tasks values (?1, ?2, ?3, ?4)",
                params![
                    "conversation-unknown",
                    "task-unknown",
                    task,
                    "2026-01-01 00:00:01"
                ],
            )
            .unwrap();

        let mut sink = CollectingSink::default();
        let scan = scan_warp_source_backed_connection(&connection, &mut sink).unwrap();
        assert_eq!(scan.counters.sessions_retained, 1);
        assert_eq!(scan.counters.retained_events, 0);
        assert_eq!(scan.counters.ignored_messages, 1);
        assert_eq!(scan.counters.unknown_oneofs, 1);
        assert_eq!(
            sink.pages
                .iter()
                .map(|page| page.logical_units)
                .sum::<usize>(),
            2
        );
        assert_eq!(
            sink.pages
                .iter()
                .map(|page| page.sessions.len())
                .sum::<usize>(),
            1
        );
        assert!(sink.pages.iter().all(|page| page.events.is_empty()));
    }

    fn push_length_delimited(target: &mut Vec<u8>, field: u32, payload: &[u8]) {
        push_varint(target, u64::from(field) << 3 | 2);
        push_varint(target, u64::try_from(payload.len()).unwrap());
        target.extend_from_slice(payload);
    }

    fn push_varint(target: &mut Vec<u8>, mut value: u64) {
        loop {
            let byte = (value & 0x7f) as u8;
            value >>= 7;
            target.push(byte | if value == 0 { 0 } else { 0x80 });
            if value == 0 {
                return;
            }
        }
    }
}
