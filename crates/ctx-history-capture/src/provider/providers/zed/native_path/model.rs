use serde_json::Value;

use super::{
    dto::{
        ZedNativeEvent, ZedNativeEventIdentity, ZedNativeMessageIdentity, ZedNativeOrder,
        ZedNativePage, ZedNativeRejection, ZedNativeSession, ZedNativeSink,
        ZED_NATIVE_PAGE_MAX_BYTES, ZED_NATIVE_PAGE_MAX_UNITS,
    },
    ZedNativePathError, ZedNativeResult,
};
use crate::record_evidence::RecordDigest;

const ZED_NATIVE_PAGE_ENCODING_ENVELOPE_BYTES: usize = 1_024;
const ZED_NATIVE_PAGE_ITEM_ENCODING_ENVELOPE_BYTES: usize = 64;

pub(super) struct ZedDecodedCoreEvent {
    pub(super) provider_message_id: Option<String>,
    pub(super) thread_ordinal: u64,
    pub(super) message_ordinal: u64,
    pub(super) event_type: ctx_history_core::EventType,
    pub(super) role: ctx_history_core::EventRole,
    pub(super) occurred_at: chrono::DateTime<chrono::Utc>,
    pub(super) kind: &'static str,
    pub(super) call_ids: Vec<String>,
    pub(super) native_content: Value,
    pub(super) body: String,
    pub(super) safe_file_touches: Vec<String>,
}

impl ZedNativeEvent {
    pub(super) fn from_draft(
        sqlite_rowid: i64,
        thread_id: &str,
        draft: ZedDecodedCoreEvent,
        record_digest: RecordDigest,
    ) -> ZedNativeResult<Self> {
        let identity = event_identity(
            thread_id,
            draft.provider_message_id.as_deref(),
            draft.message_ordinal,
        );
        Ok(Self {
            sqlite_rowid,
            identity,
            native_order: ZedNativeOrder {
                thread_ordinal: draft.thread_ordinal,
                message_ordinal: draft.message_ordinal,
                sub_ordinal: 0,
            },
            record_digest,
            event_type: draft.event_type,
            role: draft.role,
            occurred_at: draft.occurred_at,
            kind: draft.kind.to_owned(),
            call_ids: draft.call_ids,
            native_content: draft.native_content,
            normalized_body: draft.body,
            safe_file_touches: draft.safe_file_touches,
        })
    }

    fn estimated_bytes(&self) -> usize {
        serde_json::to_vec(self)
            .map_or(usize::MAX, |encoded| encoded.len())
            .saturating_add(self.normalized_body.len())
    }
}

pub(super) struct ZedNativePageBuilder<'a> {
    sink: &'a mut dyn ZedNativeSink,
    page: ZedNativePage,
}

impl<'a> ZedNativePageBuilder<'a> {
    pub(super) fn new(sink: &'a mut dyn ZedNativeSink) -> Self {
        Self {
            sink,
            page: ZedNativePage::default(),
        }
    }

    pub(super) fn push_session(&mut self, session: ZedNativeSession) -> ZedNativeResult<()> {
        let bytes = self.reserve(1, session_estimated_bytes(&session))?;
        self.page.estimated_bytes = self.page.estimated_bytes.saturating_add(bytes);
        self.page.sessions.push(session);
        Ok(())
    }

    pub(super) fn push_event(&mut self, event: ZedNativeEvent) -> ZedNativeResult<()> {
        let units = 1_usize.saturating_add(event.safe_file_touches.len());
        let bytes = self.reserve(units, event.estimated_bytes())?;
        self.page.estimated_bytes = self.page.estimated_bytes.saturating_add(bytes);
        self.page.events.push(event);
        Ok(())
    }

    pub(super) fn push_rejection(&mut self, rejection: ZedNativeRejection) -> ZedNativeResult<()> {
        let bytes = self.reserve(1, rejection_estimated_bytes(&rejection))?;
        self.page.estimated_bytes = self.page.estimated_bytes.saturating_add(bytes);
        self.page.rejections.push(rejection);
        Ok(())
    }

    pub(super) fn finish(mut self) -> ZedNativeResult<()> {
        self.flush()
    }

    fn reserve(&mut self, units: usize, bytes: usize) -> ZedNativeResult<usize> {
        if units == 0 || units > ZED_NATIVE_PAGE_MAX_UNITS {
            return Err(ZedNativePathError::UnsupportedSchema(format!(
                "one prepared Zed row expands past the {ZED_NATIVE_PAGE_MAX_UNITS}-unit page bound"
            )));
        }
        let bytes = bytes.saturating_add(ZED_NATIVE_PAGE_ITEM_ENCODING_ENVELOPE_BYTES);
        if ZED_NATIVE_PAGE_ENCODING_ENVELOPE_BYTES.saturating_add(bytes) > ZED_NATIVE_PAGE_MAX_BYTES
        {
            return Err(ZedNativePathError::UnsupportedSchema(format!(
                "one prepared Zed row exceeds the {ZED_NATIVE_PAGE_MAX_BYTES}-byte page bound"
            )));
        }
        if !self.page.is_empty()
            && (self.page.logical_units().saturating_add(units) > ZED_NATIVE_PAGE_MAX_UNITS
                || self.page.estimated_bytes.saturating_add(bytes) > ZED_NATIVE_PAGE_MAX_BYTES)
        {
            self.flush()?;
        }
        if self.page.is_empty() {
            self.page.estimated_bytes = ZED_NATIVE_PAGE_ENCODING_ENVELOPE_BYTES;
        }
        Ok(bytes)
    }

    fn flush(&mut self) -> ZedNativeResult<()> {
        if self.page.is_empty() {
            return Ok(());
        }
        let page = std::mem::take(&mut self.page);
        self.sink.push_page(page)?;
        Ok(())
    }
}

pub(super) fn event_identity(
    thread_id: &str,
    provider_message_id: Option<&str>,
    message_ordinal: u64,
) -> ZedNativeEventIdentity {
    ZedNativeEventIdentity {
        thread_id: thread_id.to_owned(),
        message: provider_message_id.map_or(
            ZedNativeMessageIdentity::MessageOrdinal(message_ordinal),
            |value| ZedNativeMessageIdentity::ProviderId {
                value: value.to_owned(),
                message_ordinal,
            },
        ),
    }
}

fn session_estimated_bytes(session: &ZedNativeSession) -> usize {
    serde_json::to_vec(session).map_or(usize::MAX, |encoded| encoded.len())
}

fn rejection_estimated_bytes(rejection: &ZedNativeRejection) -> usize {
    serde_json::to_vec(rejection).map_or(usize::MAX, |encoded| encoded.len())
}

pub(super) fn hex_digest(digest: [u8; 32]) -> String {
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}
