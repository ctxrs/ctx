use std::{
    thread,
    time::{Duration, Instant},
};

#[cfg(test)]
use std::cell::Cell;

use rusqlite::Connection;
use serde_json::Value;

use crate::captured_batch::{CAPTURE_BATCH_MAX_OVERSIZE_RECORD_BYTES, CAPTURE_BATCH_MAX_RECORDS};
use crate::provider::normalization::provider_value_text;
use crate::provider::sqlite::{sqlite_table_columns, sqlite_table_exists};
use crate::{CaptureError, Result};

use super::source::with_astrbot_length_preflight;

pub(super) const ASTRBOT_PREFERENCE_SCAN_MAX_SOURCE_ROWS_PER_PAGE: usize =
    CAPTURE_BATCH_MAX_RECORDS;
const ASTRBOT_PREFERENCE_SCAN_MIN_PAGE_INTERVAL: Duration = Duration::from_millis(5);

#[cfg(test)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) struct AstrBotPreferenceScanTestPacing {
    pub(super) pages: usize,
    pub(super) max_source_rows: usize,
}

#[cfg(test)]
thread_local! {
    static ASTRBOT_PREFERENCE_SCAN_TEST_PACING: Cell<AstrBotPreferenceScanTestPacing> =
        const { Cell::new(AstrBotPreferenceScanTestPacing {
            pages: 0,
            max_source_rows: 0,
        }) };
    static ASTRBOT_PREFERENCE_SCAN_TEST_WAIT_COUNT: Cell<Option<usize>> =
        const { Cell::new(None) };
}

struct AstrBotPreferenceScanPacer {
    page_started: Instant,
}

impl AstrBotPreferenceScanPacer {
    fn new() -> Self {
        Self {
            page_started: Instant::now(),
        }
    }

    fn finish_page(&mut self, source_rows: usize) {
        #[cfg(not(test))]
        let _ = source_rows;
        #[cfg(test)]
        ASTRBOT_PREFERENCE_SCAN_TEST_PACING.with(|pacing| {
            let current = pacing.get();
            pacing.set(AstrBotPreferenceScanTestPacing {
                pages: current.pages.saturating_add(1),
                max_source_rows: current.max_source_rows.max(source_rows),
            });
        });
        let elapsed = self.page_started.elapsed();
        let wait = ASTRBOT_PREFERENCE_SCAN_MIN_PAGE_INTERVAL.saturating_sub(elapsed);
        #[cfg(test)]
        let intercepted = ASTRBOT_PREFERENCE_SCAN_TEST_WAIT_COUNT.with(|count| {
            let Some(current) = count.get() else {
                return false;
            };
            count.set(Some(current.saturating_add(1)));
            true
        });
        #[cfg(not(test))]
        let intercepted = false;
        if !intercepted && !wait.is_zero() {
            thread::sleep(wait);
        }
        self.page_started = Instant::now();
    }
}

#[cfg(test)]
pub(super) fn astrbot_reset_preference_scan_test_pacing() {
    ASTRBOT_PREFERENCE_SCAN_TEST_PACING
        .with(|pacing| pacing.set(AstrBotPreferenceScanTestPacing::default()));
    ASTRBOT_PREFERENCE_SCAN_TEST_WAIT_COUNT.with(|count| count.set(Some(0)));
}

#[cfg(test)]
pub(super) fn astrbot_preference_scan_test_pacing() -> AstrBotPreferenceScanTestPacing {
    ASTRBOT_PREFERENCE_SCAN_TEST_PACING.with(Cell::get)
}

#[cfg(test)]
pub(super) fn astrbot_preference_scan_test_wait_count() -> usize {
    ASTRBOT_PREFERENCE_SCAN_TEST_WAIT_COUNT.with(|count| count.get().unwrap_or_default())
}

#[cfg(test)]
pub(super) fn astrbot_disable_preference_scan_test_wait_hook() {
    ASTRBOT_PREFERENCE_SCAN_TEST_WAIT_COUNT.with(|count| count.set(None));
}

pub(super) fn astrbot_selected_conversation_bounded(conn: &Connection) -> Result<Option<String>> {
    if !sqlite_table_exists(conn, "preferences")? {
        return Ok(None);
    }
    let columns = sqlite_table_columns(conn, "preferences")?;
    if !columns.contains("key") || !columns.contains("value") {
        return Ok(None);
    }
    if ["rowid", "_rowid_", "oid"]
        .iter()
        .any(|alias| columns.contains(*alias))
    {
        return Err(CaptureError::InvalidPayload(
            "AstrBot preferences table shadows SQLite's native rowid frontier".to_owned(),
        ));
    }
    conn.prepare("select rowid from preferences order by rowid limit 0")
        .map_err(|_| {
            CaptureError::InvalidPayload(
                "AstrBot preferences table requires a native rowid frontier".to_owned(),
            )
        })?;
    let has_scope = columns.contains("scope");
    let scope_length = if has_scope {
        "coalesce(octet_length(scope), 0)"
    } else {
        "0"
    };
    let candidate_initial = format!(
        "select rowid, coalesce(octet_length(key), 0), \
                coalesce(octet_length(value), 0), {scope_length} \
         from preferences order by rowid limit ?1"
    );
    let candidate_after = format!(
        "select rowid, coalesce(octet_length(key), 0), \
                coalesce(octet_length(value), 0), {scope_length} \
         from preferences where rowid > ?1 order by rowid limit ?2"
    );
    let scope = if has_scope {
        "CAST(scope AS TEXT)"
    } else {
        "NULL"
    };
    let key_hydration =
        format!("select CAST(key AS TEXT), {scope} from preferences where rowid = ?1");
    let value_hydration = "select CAST(value AS TEXT) from preferences where rowid = ?1";
    let page_limit =
        i64::try_from(ASTRBOT_PREFERENCE_SCAN_MAX_SOURCE_ROWS_PER_PAGE).map_err(|_| {
            CaptureError::SystemInvariant("AstrBot preference page limit exceeds SQLite integer")
        })?;
    let mut after_rowid = None;
    let mut pacer = AstrBotPreferenceScanPacer::new();
    loop {
        // The unindexed key/scope selection advances only through bounded native-rowid pages.
        // Its raised-limit query returns integer metadata; the provider cap is restored before
        // any exact key/scope or selected value TEXT is hydrated.
        let page = with_astrbot_length_preflight(conn, || {
            let sql = if after_rowid.is_some() {
                candidate_after.as_str()
            } else {
                candidate_initial.as_str()
            };
            let mut statement = conn.prepare(sql)?;
            let mut rows = match after_rowid {
                Some(rowid) => statement.query(rusqlite::params![rowid, page_limit])?,
                None => statement.query([page_limit])?,
            };
            let mut page = Vec::with_capacity(ASTRBOT_PREFERENCE_SCAN_MAX_SOURCE_ROWS_PER_PAGE);
            while let Some(row) = rows.next()? {
                page.push(AstrBotPreferenceRowMetadata {
                    rowid: row.get(0)?,
                    key_bytes: row.get(1)?,
                    value_bytes: row.get(2)?,
                    scope_bytes: row.get(3)?,
                });
            }
            Ok(page)
        })?;
        let Some(last) = page.last() else {
            return Ok(None);
        };
        after_rowid = Some(last.rowid);
        let mut selected = None;
        for metadata in &page {
            let key_bytes = astrbot_preference_length(metadata.key_bytes, "key")?;
            let scope_bytes = astrbot_preference_length(metadata.scope_bytes, "scope")?;
            if key_bytes != "sel_conv_id".len() || (has_scope && scope_bytes != "umo".len()) {
                continue;
            }
            let (key, scope) = conn.query_row(&key_hydration, [metadata.rowid], |row| {
                Ok((
                    row.get::<_, Option<String>>(0)?,
                    row.get::<_, Option<String>>(1)?,
                ))
            })?;
            if key.as_deref() != Some("sel_conv_id")
                || (has_scope && scope.as_deref() != Some("umo"))
            {
                continue;
            }
            selected = Some((metadata.rowid, metadata.value_bytes));
            break;
        }
        pacer.finish_page(page.len());
        if let Some((rowid, value_bytes)) = selected {
            let value_bytes = astrbot_preference_length(value_bytes, "value")?;
            if value_bytes > CAPTURE_BATCH_MAX_OVERSIZE_RECORD_BYTES {
                return Err(CaptureError::InvalidPayload(
                    "AstrBot selected-conversation preference exceeds the provider record limit"
                        .to_owned(),
                ));
            }
            let value = conn.query_row(value_hydration, [rowid], |row| {
                row.get::<_, Option<String>>(0)
            })?;
            return Ok(value.and_then(astrbot_selected_conversation_value));
        }
        if page.len() < ASTRBOT_PREFERENCE_SCAN_MAX_SOURCE_ROWS_PER_PAGE {
            return Ok(None);
        }
    }
}

struct AstrBotPreferenceRowMetadata {
    rowid: i64,
    key_bytes: i64,
    value_bytes: i64,
    scope_bytes: i64,
}

pub(super) fn astrbot_preference_length(value: i64, field: &str) -> Result<usize> {
    usize::try_from(value).map_err(|_| {
        CaptureError::InvalidPayload(format!(
            "AstrBot preference {field} byte count must be nonnegative"
        ))
    })
}

pub(super) fn astrbot_selected_conversation_value(value: String) -> Option<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return None;
    }
    if let Ok(parsed) = serde_json::from_str::<Value>(trimmed) {
        let selected = match &parsed {
            Value::Object(object) => object.get("val").and_then(provider_value_text),
            Value::String(_) | Value::Number(_) | Value::Bool(_) => provider_value_text(&parsed),
            Value::Array(_) | Value::Null => None,
        };
        if let Some(selected) = selected
            .map(|selected| selected.trim().to_owned())
            .filter(|selected| !selected.is_empty())
        {
            return Some(selected);
        }
    }
    Some(trimmed.to_owned())
}
