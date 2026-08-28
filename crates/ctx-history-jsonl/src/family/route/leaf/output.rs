use ctx_history_core::CoreRecord;

use super::*;
use crate::JsonlFamilySemanticPage;

// The large variant deliberately carries CoreRecord by value: boxing every
// projected record would add one allocation to the generic JSONL hot path.
#[allow(clippy::large_enum_variant)]
pub(crate) enum JsonlLeafOutputEvent {
    Page {
        append: bool,
        completed_bytes: u64,
        records: Vec<CoreRecord>,
    },
    Record {
        append: bool,
        record: CoreRecord,
    },
    Flush,
}

pub(crate) struct JsonlLeafOutput<'emit, E: JsonlFamilyError> {
    emit: &'emit mut dyn FnMut(JsonlLeafOutputEvent) -> JsonlResult<(), E>,
}

impl<'emit, E: JsonlFamilyError> JsonlLeafOutput<'emit, E> {
    pub(crate) fn new(
        emit: &'emit mut dyn FnMut(JsonlLeafOutputEvent) -> JsonlResult<(), E>,
    ) -> Self {
        Self { emit }
    }

    pub(crate) fn emit_page(
        &mut self,
        append: bool,
        completed_bytes: u64,
        records: Vec<CoreRecord>,
    ) -> JsonlResult<(), E> {
        let pages = JsonlFamilySemanticPage::split_bounded::<E>(records)?;
        let final_page = pages.len().saturating_sub(1);
        for (index, page) in pages.into_iter().enumerate() {
            self.emit_bounded_page(
                append,
                if index == final_page {
                    completed_bytes
                } else {
                    0
                },
                page.into_bounded_records::<E>()?,
            )?;
        }
        Ok(())
    }

    pub(crate) fn emit_record(&mut self, append: bool, record: CoreRecord) -> JsonlResult<(), E> {
        let mut record = record;
        crate::fit_jsonl_semantic_page_record(&mut record)
            .map_err(|error| E::invalid_payload(error.to_string()))?;
        (self.emit)(JsonlLeafOutputEvent::Record { append, record })
    }

    fn emit_bounded_page(
        &mut self,
        append: bool,
        completed_bytes: u64,
        records: Vec<CoreRecord>,
    ) -> JsonlResult<(), E> {
        (self.emit)(JsonlLeafOutputEvent::Page {
            append,
            completed_bytes,
            records,
        })
    }

    pub(crate) fn flush(&mut self) -> JsonlResult<(), E> {
        (self.emit)(JsonlLeafOutputEvent::Flush)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ctx_history_core::{
        derive_event_id, derive_session_id, CaptureProvider, EventIdentityInput, NativeItemKey,
        NativeSessionKey, SessionIdentityInput, SourceAnchor, TypedKey,
    };
    use ctx_history_source_io::SourceIoError;

    fn record(ordinal: u64) -> CoreRecord {
        let source = ctx_history_core::SourceKey::derive(
            CaptureProvider::Pi.as_str(),
            "jsonl-semantic-page-output-test",
            "v1",
            1,
            SourceAnchor::provider_native("session", TypedKey::utf8("page.jsonl").unwrap())
                .unwrap(),
        )
        .unwrap();
        let session_key = NativeSessionKey::native_id(
            "session",
            TypedKey::utf8("semantic-page-session").unwrap(),
        )
        .unwrap();
        let session_id = derive_session_id(SessionIdentityInput {
            source: &source,
            logical_session_kind: "session",
            native_session_key: &session_key,
        })
        .unwrap();
        let item_key = NativeItemKey::native_id("event", TypedKey::U64(ordinal)).unwrap();
        let event_id = derive_event_id(EventIdentityInput {
            source: &source,
            session_id,
            logical_item_kind: "event",
            native_item_key: &item_key,
            subrecord_selector: None,
        })
        .unwrap();
        CoreRecord::new_selected(
            event_id,
            session_id,
            source,
            ordinal,
            "event",
            "jsonl-semantic-page-output-test-v1",
            "x".repeat(128 * 1024),
        )
        .unwrap()
    }

    #[test]
    fn split_pages_report_completed_bytes_once() {
        let mut emissions = Vec::new();
        let mut emit = |event| {
            emissions.push(event);
            Ok(())
        };
        let mut output = JsonlLeafOutput::<SourceIoError>::new(&mut emit);
        output
            .emit_page(false, 4_096, (0..=64).map(record).collect::<Vec<_>>())
            .unwrap();

        let pages = emissions
            .into_iter()
            .map(|event| match event {
                JsonlLeafOutputEvent::Page {
                    completed_bytes,
                    records,
                    ..
                } => (completed_bytes, records),
                JsonlLeafOutputEvent::Record { .. } | JsonlLeafOutputEvent::Flush => {
                    panic!("page publication emitted a non-page event")
                }
            })
            .collect::<Vec<_>>();
        assert!(pages.len() > 1);
        assert_eq!(
            pages
                .iter()
                .map(|(completed_bytes, _)| completed_bytes)
                .sum::<u64>(),
            4_096
        );
        assert_eq!(pages.last().unwrap().0, 4_096);
        assert!(pages[..pages.len() - 1]
            .iter()
            .all(|(completed_bytes, _)| *completed_bytes == 0));
        assert_eq!(
            pages
                .iter()
                .map(|(_, records)| records.len())
                .sum::<usize>(),
            65
        );
    }
}
