use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use super::{
    dto::{
        ZedNativeCompleteMessageEvidence, ZedNativeEvent, ZedNativeEventIdentity,
        ZedNativeMessageIdentity, ZedNativeOrder, ZedNativePage, ZedNativeRejection,
        ZedNativeSession, ZedNativeSink, ZED_NATIVE_PAGE_MAX_BYTES, ZED_NATIVE_PAGE_MAX_UNITS,
    },
    ZedNativePathError, ZedNativeResult,
};
use crate::{
    complete_content::CompleteContentBodyDigest, compute_payload_hash,
    provider::normalization::provider_policy_event_text,
};
use ctx_history_core::{CaptureProvider, ContentRef, EventType};

const ZED_NATIVE_BODY_MAX_CHARS: usize = 16_000;
const ZED_NATIVE_PREVIEW_MAX_CHARS: usize = 240;
const ZED_NATIVE_PAGE_ENCODING_ENVELOPE_BYTES: usize = 1_024;
const ZED_NATIVE_PAGE_ITEM_ENCODING_ENVELOPE_BYTES: usize = 64;
const ZED_CORE_DIGEST_DOMAIN: &[u8] = b"ctx-zed-core-generation-v1\0";
const ZED_EVENT_HASH_DOMAIN: &[u8] = b"ctx-zed-retained-event-v1\0";

pub(super) struct ZedDecodedCoreEvent {
    pub(super) provider_message_id: Option<String>,
    pub(super) thread_ordinal: u64,
    pub(super) message_ordinal: u64,
    pub(super) event_type: ctx_history_core::EventType,
    pub(super) role: ctx_history_core::EventRole,
    pub(super) occurred_at: chrono::DateTime<chrono::Utc>,
    pub(super) kind: &'static str,
    pub(super) call_ids: Vec<String>,
    pub(super) body: String,
    pub(super) safe_file_touches: Vec<String>,
}

impl ZedNativeEvent {
    pub(super) fn from_draft(
        sqlite_rowid: i64,
        thread_id: &str,
        draft: ZedDecodedCoreEvent,
        record_digest: CompleteContentBodyDigest,
    ) -> ZedNativeResult<Self> {
        let legacy_body = truncate_chars(&draft.body, ZED_NATIVE_BODY_MAX_CHARS);
        let message = draft.provider_message_id.map_or_else(
            || ZedNativeMessageIdentity::MessageOrdinal(draft.message_ordinal),
            |value| ZedNativeMessageIdentity::ProviderId {
                value,
                message_ordinal: draft.message_ordinal,
            },
        );
        let identity = ZedNativeEventIdentity {
            thread_id: thread_id.to_owned(),
            message,
        };
        let legacy_content_hash = retained_event_hash(&identity, &legacy_body);
        let provider_event_index = draft.message_ordinal.checked_mul(2).ok_or_else(|| {
            ZedNativePathError::UnsupportedSchema("Zed provider event index overflowed".to_owned())
        })?;
        let body_shape = json!({
            "message_kind": draft.kind,
            "text": draft.body,
            "call_ids": draft.call_ids,
        });
        let retained = provider_policy_event_text(draft.event_type, &draft.body, &body_shape);
        let body = retained.text;
        let preview = truncate_chars(&body, ZED_NATIVE_PREVIEW_MAX_CHARS);
        let cursor = event_cursor(&identity);
        let payload = json!({
            "provider": CaptureProvider::Zed.as_str(),
            "provider_session_id": thread_id,
            "provider_event_index": provider_event_index,
            "cursor": cursor,
            "artifacts": [],
            "text": body,
            "text_retention": retained.retention.as_json(),
            "body": {
                "message_kind": draft.kind,
                "text": body,
                "preview": preview,
                "call_ids": draft.call_ids,
            },
        });
        let content_hash = compute_payload_hash(&payload)?;
        let complete_message = (draft.event_type == EventType::Message
            && payload
                .pointer("/text_retention/truncated")
                .and_then(Value::as_bool)
                == Some(true))
        .then(|| {
            ContentRef::from_bytes(draft.body.as_bytes()).map(|content_ref| {
                ZedNativeCompleteMessageEvidence {
                    record_digest: record_digest.clone(),
                    content_ref,
                }
            })
        })
        .flatten();
        if draft.event_type == EventType::Message
            && payload
                .pointer("/text_retention/truncated")
                .and_then(Value::as_bool)
                == Some(true)
            && complete_message.is_none()
        {
            return Err(crate::CaptureError::SystemInvariant(
                "Zed complete message length exceeds ContentRef bounds",
            )
            .into());
        }
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
            body,
            content_hash,
            legacy_content_hash,
            payload,
            preview,
            safe_file_touches: draft.safe_file_touches,
            complete_message,
        })
    }

    fn estimated_bytes(&self) -> usize {
        serde_json::to_vec(self).map_or(usize::MAX, |encoded| encoded.len())
    }
}

pub(super) struct ZedNativePageBuilder<'a> {
    sink: &'a mut dyn ZedNativeSink,
    page: ZedNativePage,
    core_hasher: Sha256,
    pages_emitted: u64,
}

impl<'a> ZedNativePageBuilder<'a> {
    pub(super) fn new(sink: &'a mut dyn ZedNativeSink) -> Self {
        let mut core_hasher = Sha256::new();
        core_hasher.update(ZED_CORE_DIGEST_DOMAIN);
        Self {
            sink,
            page: ZedNativePage::default(),
            core_hasher,
            pages_emitted: 0,
        }
    }

    pub(super) fn push_session(&mut self, session: ZedNativeSession) -> ZedNativeResult<()> {
        let bytes = self.reserve(1, session_estimated_bytes(&session))?;
        hash_session(&mut self.core_hasher, &session);
        self.page.estimated_bytes = self.page.estimated_bytes.saturating_add(bytes);
        self.page.sessions.push(session);
        Ok(())
    }

    pub(super) fn push_event(&mut self, event: ZedNativeEvent) -> ZedNativeResult<()> {
        let units = 1_usize.saturating_add(event.safe_file_touches.len());
        let bytes = self.reserve(units, event.estimated_bytes())?;
        hash_event(&mut self.core_hasher, &event);
        self.page.estimated_bytes = self.page.estimated_bytes.saturating_add(bytes);
        self.page.events.push(event);
        Ok(())
    }

    pub(super) fn push_rejection(&mut self, rejection: ZedNativeRejection) -> ZedNativeResult<()> {
        let bytes = self.reserve(1, rejection_estimated_bytes(&rejection))?;
        hash_rejection(&mut self.core_hasher, &rejection);
        self.page.estimated_bytes = self.page.estimated_bytes.saturating_add(bytes);
        self.page.rejections.push(rejection);
        Ok(())
    }

    pub(super) fn finish(mut self) -> ZedNativeResult<(String, u64)> {
        self.flush()?;
        Ok((
            hex_digest(self.core_hasher.finalize().into()),
            self.pages_emitted,
        ))
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
            && (self.page.publication_units().saturating_add(units) > ZED_NATIVE_PAGE_MAX_UNITS
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
        self.pages_emitted = self.pages_emitted.checked_add(1).ok_or_else(|| {
            ZedNativePathError::UnsupportedSchema("Zed NativePath page count overflowed".to_owned())
        })?;
        Ok(())
    }
}

fn truncate_chars(value: &str, limit: usize) -> String {
    match value.char_indices().nth(limit) {
        Some((end, _)) => value[..end].to_owned(),
        None => value.to_owned(),
    }
}

fn retained_event_hash(identity: &ZedNativeEventIdentity, body: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(ZED_EVENT_HASH_DOMAIN);
    hash_text(&mut hasher, &identity.thread_id);
    match &identity.message {
        ZedNativeMessageIdentity::ProviderId {
            value,
            message_ordinal,
        } => {
            hasher.update([1]);
            hash_text(&mut hasher, value);
            hasher.update(message_ordinal.to_le_bytes());
        }
        ZedNativeMessageIdentity::MessageOrdinal(value) => {
            hasher.update([2]);
            hasher.update(value.to_le_bytes());
        }
    }
    hash_text(&mut hasher, body);
    hex_digest(hasher.finalize().into())
}

fn session_estimated_bytes(session: &ZedNativeSession) -> usize {
    serde_json::to_vec(session).map_or(usize::MAX, |encoded| encoded.len())
}

fn rejection_estimated_bytes(rejection: &ZedNativeRejection) -> usize {
    serde_json::to_vec(rejection).map_or(usize::MAX, |encoded| encoded.len())
}

fn hash_session(hasher: &mut Sha256, session: &ZedNativeSession) {
    hasher.update(b"session\0");
    hash_text(hasher, &session.thread_id);
    hash_optional_text(hasher, session.parent_thread_id.as_deref());
    hash_text(hasher, &session.root_thread_id);
    hash_text(hasher, &session.title);
    hash_text(hasher, &session.summary);
    hasher.update(session.created_at.timestamp_millis().to_le_bytes());
    hasher.update(session.updated_at.timestamp_millis().to_le_bytes());
    hash_optional_text(hasher, session.cwd.as_deref());
    for path in &session.folder_paths {
        hash_text(hasher, path);
    }
    hasher.update([match session.encoding {
        super::dto::ZedNativeEncoding::Json => 1,
        super::dto::ZedNativeEncoding::Zstd => 2,
    }]);
}

fn hash_event(hasher: &mut Sha256, event: &ZedNativeEvent) {
    hasher.update(b"event\0");
    hasher.update(event.sqlite_rowid.to_le_bytes());
    hash_text(hasher, event.record_digest.as_str());
    hash_text(hasher, &event.identity.thread_id);
    match &event.identity.message {
        ZedNativeMessageIdentity::ProviderId {
            value,
            message_ordinal,
        } => {
            hasher.update([1]);
            hash_text(hasher, value);
            hasher.update(message_ordinal.to_le_bytes());
        }
        ZedNativeMessageIdentity::MessageOrdinal(value) => {
            hasher.update([2]);
            hasher.update(value.to_le_bytes());
        }
    }
    hasher.update(event.native_order.thread_ordinal.to_le_bytes());
    hasher.update(event.native_order.message_ordinal.to_le_bytes());
    hasher.update(event.native_order.sub_ordinal.to_le_bytes());
    hash_text(hasher, event.event_type.as_str());
    hash_text(hasher, event.role.as_str());
    hash_text(hasher, &event.kind);
    for call_id in &event.call_ids {
        hash_text(hasher, call_id);
    }
    hash_text(hasher, &event.body);
    hash_text(hasher, &event.content_hash);
    hash_text(hasher, &event.legacy_content_hash);
    for path in &event.safe_file_touches {
        hash_text(hasher, path);
    }
}

pub(super) fn event_cursor(identity: &ZedNativeEventIdentity) -> String {
    match &identity.message {
        ZedNativeMessageIdentity::ProviderId {
            value,
            message_ordinal,
        } => format!(
            "thread:{}:message:{message_ordinal}:id:{value}",
            identity.thread_id
        ),
        ZedNativeMessageIdentity::MessageOrdinal(message_ordinal) => {
            format!("thread:{}:message:{message_ordinal}", identity.thread_id)
        }
    }
}

fn hash_rejection(hasher: &mut Sha256, rejection: &ZedNativeRejection) {
    hasher.update(b"rejection\0");
    match rejection.thread_id.as_deref() {
        Some(thread_id) => {
            hasher.update([1]);
            hash_text(hasher, thread_id);
        }
        None => {
            hasher.update([0]);
            hasher.update(rejection.sqlite_rowid.to_le_bytes());
        }
    }
    hasher.update([rejection.kind as u8]);
    hash_text(hasher, &rejection.reason);
}

fn hash_optional_text(hasher: &mut Sha256, value: Option<&str>) {
    match value {
        Some(value) => {
            hasher.update([1]);
            hash_text(hasher, value);
        }
        None => hasher.update([0]),
    }
}

fn hash_text(hasher: &mut Sha256, value: &str) {
    hash_bytes(hasher, value.as_bytes());
}

fn hash_bytes(hasher: &mut Sha256, value: &[u8]) {
    hasher.update(u64::try_from(value.len()).unwrap_or(u64::MAX).to_le_bytes());
    hasher.update(value);
}

pub(super) fn hex_digest(digest: [u8; 32]) -> String {
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}
