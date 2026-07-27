use std::path::Path;

use ctx_history_core::EventType;
use ctx_history_store::Store;
use serde_json::Value;

use crate::{
    complete_content::CompleteContentBodyDigest, ProviderAdapterContext, ProviderImportOptions,
    ProviderImportSummary, Result,
};

mod event;
mod native_path;
mod thread;

pub(crate) use thread::decode_zed_thread_for_complete;

pub(crate) struct ZedCompleteDecodedThread {
    decoded: event::ZedDecodedThread,
}

impl ZedCompleteDecodedThread {
    pub(crate) fn events<'a>(
        &'a self,
        provider_session_id: &'a str,
    ) -> ZedCompleteDecodedEvents<'a> {
        ZedCompleteDecodedEvents {
            native: self.decoded.native_events(provider_session_id),
        }
    }

    pub(crate) fn event_at<'a>(
        &'a self,
        provider_session_id: &'a str,
        event_index: usize,
    ) -> Result<Option<ZedCompleteDecodedEvent>> {
        self.events(provider_session_id)
            .nth(event_index)
            .transpose()
    }
}

pub(crate) struct ZedCompleteDecodedEvent {
    pub(crate) event: ZedCompleteEvent,
    pub(crate) complete_text: String,
}

/// Migration-only fields needed to verify a released complete-content locator.
pub(crate) struct ZedCompleteEvent {
    pub(crate) provider_event_index: u64,
    pub(crate) provider_event_hash: Option<String>,
    pub(crate) cursor: Option<String>,
    pub(crate) event_type: EventType,
    pub(crate) payload: Value,
}

pub(crate) struct ZedNativePathCompleteMessage {
    pub(crate) provider_event_index: u64,
    pub(crate) legacy_provider_event_hash: String,
    pub(crate) cursor: String,
    pub(crate) event_type: EventType,
    pub(crate) payload: Value,
    pub(crate) complete_text: String,
}

pub(crate) struct ZedCompleteDecodedEvents<'a> {
    native: event::ZedNativeEvents<'a>,
}

impl<'a> Iterator for ZedCompleteDecodedEvents<'a> {
    type Item = Result<ZedCompleteDecodedEvent>;

    fn next(&mut self) -> Option<Self::Item> {
        self.native.next().map(|native| {
            native.decode().map(|decoded| {
                let event = decoded.event;
                let _ = (
                    event.role,
                    event.occurred_at,
                    event.fidelity,
                    event.idempotency_key,
                    event.metadata,
                );
                ZedCompleteDecodedEvent {
                    event: ZedCompleteEvent {
                        provider_event_index: event.provider_event_index,
                        provider_event_hash: Some(event.provider_event_hash),
                        cursor: Some(event.cursor),
                        event_type: event.event_type,
                        payload: event.payload,
                    },
                    complete_text: decoded.complete_text,
                }
            })
        })
    }
}

pub(crate) fn decode_zed_thread_events(
    row: &thread::ZedThreadRow,
) -> Result<ZedCompleteDecodedThread> {
    event::decode_zed_thread_events(row).map(|decoded| ZedCompleteDecodedThread { decoded })
}

pub(crate) fn decode_zed_nativepath_complete_message(
    row: &thread::ZedThreadRow,
    message_ordinal: u64,
    record_digest: CompleteContentBodyDigest,
) -> Result<Option<ZedNativePathCompleteMessage>> {
    native_path::decode_complete_message(row, message_ordinal, record_digest)
        .map_err(native_path::into_capture_error)
}

pub(crate) fn import_zed_nativepath(
    path: &Path,
    store: &mut Store,
    context: ProviderAdapterContext,
    import_options: ProviderImportOptions,
) -> Result<ProviderImportSummary> {
    native_path::import_zed_nativepath(path, store, context, import_options)
}
