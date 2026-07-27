use std::{fmt, io::Write, path::Path};

use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};
use chrono::{DateTime, Utc};
use ctx_history_core::{
    CaptureProvider, ProviderCaptureEnvelope, ProviderCursorCheckpoint, ProviderCursorRange,
    ProviderSourceEnvelope, SyncCursor,
};
use ctx_history_store::Store;
use serde::{de::DeserializeOwned, Deserialize, Serialize};

use crate::{
    captured_batch::{
        NativePosition, SourceObservation, CAPTURE_BATCH_MAX_PARSER_CHECKPOINT_BYTES,
    },
    stable_capture_uuid, CaptureError, Result,
};

use super::ids::{provider_source_identity_component, timestamps};

pub(crate) const CERTIFIED_PROVIDER_CURSOR_SCHEMA_VERSION: u32 = 1;
pub(crate) const MAX_PROVIDER_PARSER_CHECKPOINT_BYTES: usize =
    CAPTURE_BATCH_MAX_PARSER_CHECKPOINT_BYTES;
const MAX_PROVIDER_SOURCE_REVISION_BYTES: usize = 4 * 1024;
const MAX_PROVIDER_NATIVE_POSITION_BYTES: usize = 256 * 1024;
const MAX_CERTIFIED_PROVIDER_CURSOR_WIRE_BYTES: usize = 704 * 1024;
pub(crate) const MAX_PROVIDER_PATH_IDENTITY_RAW_BYTES: usize = 7 * 1024;

#[derive(Clone, PartialEq, Eq)]
pub(crate) struct BoundedParserCheckpoint(Vec<u8>);

impl BoundedParserCheckpoint {
    fn try_new_bytes(value: Vec<u8>) -> Result<Self> {
        validate_bounded_bytes(
            value.len(),
            MAX_PROVIDER_PARSER_CHECKPOINT_BYTES,
            "provider parser checkpoint",
        )?;
        Ok(Self(value))
    }

    pub(crate) fn from_serializable(value: &impl Serialize) -> Result<Self> {
        let mut encoded = BoundedCheckpointWriter::default();
        serde_json::to_writer(&mut encoded, value)?;
        Self::try_new_bytes(encoded.bytes)
    }

    pub(crate) fn as_bytes(&self) -> &[u8] {
        &self.0
    }

    pub(crate) fn deserialize<T: DeserializeOwned>(&self) -> Result<T> {
        serde_json::from_slice(&self.0).map_err(CaptureError::from)
    }
}

impl fmt::Debug for BoundedParserCheckpoint {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("BoundedParserCheckpoint")
            .field("bytes", &self.0.len())
            .finish()
    }
}

#[derive(Default)]
struct BoundedCheckpointWriter {
    bytes: Vec<u8>,
}

impl Write for BoundedCheckpointWriter {
    fn write(&mut self, buffer: &[u8]) -> std::io::Result<usize> {
        let next =
            self.bytes.len().checked_add(buffer.len()).ok_or_else(|| {
                std::io::Error::other("provider parser checkpoint length overflow")
            })?;
        if next > MAX_PROVIDER_PARSER_CHECKPOINT_BYTES {
            return Err(std::io::Error::other(
                "provider parser checkpoint exceeds its byte bound",
            ));
        }
        self.bytes.extend_from_slice(buffer);
        Ok(buffer.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[derive(Clone, PartialEq, Eq)]
pub(crate) struct CertifiedProviderCursor {
    source_revision: String,
    parser_revision: u32,
    policy_revision: u32,
    native_position: NativePosition,
    parser_checkpoint: BoundedParserCheckpoint,
    rejected_records: u64,
}

impl fmt::Debug for CertifiedProviderCursor {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CertifiedProviderCursor")
            .field("source_revision_bytes", &self.source_revision.len())
            .field("parser_revision", &self.parser_revision)
            .field("policy_revision", &self.policy_revision)
            .field("native_position_kind", &self.native_position.kind())
            .field("native_position_bytes", &self.native_position.value().len())
            .field("parser_checkpoint", &self.parser_checkpoint)
            .field("rejected_records", &self.rejected_records)
            .finish()
    }
}

impl CertifiedProviderCursor {
    pub(crate) fn decode_if_certified(encoded: &str) -> Result<Option<Self>> {
        if !encoded.trim_start().starts_with('{') {
            return Ok(None);
        }
        Self::decode(encoded).map(Some)
    }

    pub(crate) fn new(
        source_revision: impl Into<String>,
        parser_revision: u32,
        policy_revision: u32,
        native_position: NativePosition,
        parser_checkpoint: BoundedParserCheckpoint,
    ) -> Result<Self> {
        let source_revision = source_revision.into();
        validate_source_revision(&source_revision)?;
        Ok(Self {
            source_revision,
            parser_revision,
            policy_revision,
            native_position,
            parser_checkpoint,
            rejected_records: 0,
        })
    }

    pub(crate) fn with_rejected_records(mut self, rejected_records: u64) -> Self {
        self.rejected_records = rejected_records;
        self
    }

    pub(crate) fn encode(&self) -> Result<String> {
        validate_source_revision(&self.source_revision)?;
        validate_bounded_bytes(
            self.native_position.value().len(),
            MAX_PROVIDER_NATIVE_POSITION_BYTES,
            "provider native position",
        )?;
        validate_bounded_bytes(
            self.parser_checkpoint.as_bytes().len(),
            MAX_PROVIDER_PARSER_CHECKPOINT_BYTES,
            "provider parser checkpoint",
        )?;
        let encoded = serde_json::to_string(&CertifiedProviderCursorWireRef {
            schema_version: CERTIFIED_PROVIDER_CURSOR_SCHEMA_VERSION,
            source_revision: &self.source_revision,
            parser_revision: self.parser_revision,
            policy_revision: self.policy_revision,
            native_position_kind: self.native_position.kind(),
            native_position_base64: BASE64.encode(self.native_position.value()),
            parser_checkpoint_base64: BASE64.encode(self.parser_checkpoint.as_bytes()),
            rejected_records: self.rejected_records,
        })
        .map_err(CaptureError::from)?;
        validate_bounded_bytes(
            encoded.len(),
            MAX_CERTIFIED_PROVIDER_CURSOR_WIRE_BYTES,
            "certified provider cursor",
        )?;
        Ok(encoded)
    }

    pub(crate) fn decode(encoded: &str) -> Result<Self> {
        if encoded.len() > MAX_CERTIFIED_PROVIDER_CURSOR_WIRE_BYTES {
            return Err(CaptureError::InvalidPayload(format!(
                "certified provider cursor exceeds {MAX_CERTIFIED_PROVIDER_CURSOR_WIRE_BYTES} bytes"
            )));
        }
        let wire: CertifiedProviderCursorWire = serde_json::from_str(encoded)?;
        if wire.schema_version != CERTIFIED_PROVIDER_CURSOR_SCHEMA_VERSION {
            return Err(CaptureError::InvalidPayload(format!(
                "unsupported certified provider cursor schema version {}",
                wire.schema_version
            )));
        }
        let native_position = NativePosition::new(
            wire.native_position_kind,
            decode_bounded_base64(
                &wire.native_position_base64,
                MAX_PROVIDER_NATIVE_POSITION_BYTES,
                "provider native position",
            )?,
        )
        .map_err(|error| CaptureError::InvalidPayload(error.to_string()))?;
        let parser_checkpoint = BoundedParserCheckpoint::try_new_bytes(decode_bounded_base64(
            &wire.parser_checkpoint_base64,
            MAX_PROVIDER_PARSER_CHECKPOINT_BYTES,
            "provider parser checkpoint",
        )?)?;
        Ok(Self::new(
            wire.source_revision,
            wire.parser_revision,
            wire.policy_revision,
            native_position,
            parser_checkpoint,
        )?
        .with_rejected_records(wire.rejected_records))
    }

    pub(crate) fn source_revision(&self) -> &str {
        &self.source_revision
    }

    pub(crate) fn parser_revision(&self) -> u32 {
        self.parser_revision
    }

    pub(crate) fn policy_revision(&self) -> u32 {
        self.policy_revision
    }

    pub(crate) fn native_position(&self) -> &NativePosition {
        &self.native_position
    }

    pub(crate) fn parser_checkpoint(&self) -> &BoundedParserCheckpoint {
        &self.parser_checkpoint
    }

    pub(crate) fn rejected_records(&self) -> u64 {
        self.rejected_records
    }

    pub(crate) fn matches_revisions(
        &self,
        source_revision: &str,
        parser_revision: u32,
        policy_revision: u32,
    ) -> bool {
        self.source_revision == source_revision
            && self.parser_revision == parser_revision
            && self.policy_revision == policy_revision
    }

    pub(crate) fn validate_observation_position(
        &self,
        source: &crate::captured_batch::SourceObservation,
        position: &NativePosition,
    ) -> Result<()> {
        if self.source_revision != source.source_revision()
            || self.parser_revision != source.capture_revision()
            || self.policy_revision != source.policy_revision()
            || self.native_position != *position
        {
            return Err(CaptureError::SystemInvariant(
                "certified provider cursor does not match the exact observed source position",
            ));
        }
        Ok(())
    }
}

#[derive(Serialize)]
struct CertifiedProviderCursorWireRef<'a> {
    #[serde(rename = "v")]
    schema_version: u32,
    #[serde(rename = "s")]
    source_revision: &'a str,
    #[serde(rename = "p")]
    parser_revision: u32,
    #[serde(rename = "o")]
    policy_revision: u32,
    #[serde(rename = "k")]
    native_position_kind: &'a str,
    #[serde(rename = "n")]
    native_position_base64: String,
    #[serde(rename = "c")]
    parser_checkpoint_base64: String,
    #[serde(rename = "r")]
    rejected_records: u64,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CertifiedProviderCursorWire {
    #[serde(rename = "v", alias = "schema_version")]
    schema_version: u32,
    #[serde(rename = "s", alias = "source_revision")]
    source_revision: String,
    #[serde(rename = "p", alias = "parser_revision")]
    parser_revision: u32,
    #[serde(rename = "o", alias = "policy_revision")]
    policy_revision: u32,
    #[serde(rename = "k", alias = "native_position_kind")]
    native_position_kind: String,
    #[serde(rename = "n", alias = "native_position_base64")]
    native_position_base64: String,
    #[serde(rename = "c", alias = "parser_checkpoint_base64")]
    parser_checkpoint_base64: String,
    #[serde(rename = "r", alias = "rejected_records", default)]
    rejected_records: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ProviderCursorCommit {
    Committed,
    Conflict,
}

fn validate_source_revision(source_revision: &str) -> Result<()> {
    if source_revision.is_empty() {
        return Err(CaptureError::InvalidPayload(
            "provider source revision must not be empty".to_owned(),
        ));
    }
    if source_revision.len() > MAX_PROVIDER_SOURCE_REVISION_BYTES {
        return Err(CaptureError::InvalidPayload(format!(
            "provider source revision exceeds {MAX_PROVIDER_SOURCE_REVISION_BYTES} bytes"
        )));
    }
    Ok(())
}

fn validate_bounded_bytes(bytes: usize, max_bytes: usize, label: &str) -> Result<()> {
    if bytes > max_bytes {
        return Err(CaptureError::InvalidPayload(format!(
            "{label} exceeds {max_bytes} bytes"
        )));
    }
    Ok(())
}

fn decode_bounded_base64(encoded: &str, max_bytes: usize, label: &str) -> Result<Vec<u8>> {
    let max_encoded_bytes = max_bytes.div_ceil(3).saturating_mul(4);
    if encoded.len() > max_encoded_bytes {
        return Err(CaptureError::InvalidPayload(format!(
            "{label} exceeds {max_bytes} bytes"
        )));
    }
    let decoded = BASE64.decode(encoded).map_err(|error| {
        CaptureError::InvalidPayload(format!("{label} is not valid base64: {error}"))
    })?;
    validate_bounded_bytes(decoded.len(), max_bytes, label)?;
    Ok(decoded)
}

pub(crate) fn provider_cursor_stream(provider: CaptureProvider, source_format: &str) -> String {
    format!("provider:{}:{}", provider.as_str(), source_format)
}

pub(crate) fn provider_path_identity(path: &Path) -> Result<String> {
    #[cfg(unix)]
    let (platform, raw) = {
        use std::os::unix::ffi::OsStrExt;

        ("unix-bytes", path.as_os_str().as_bytes().to_vec())
    };
    #[cfg(windows)]
    let (platform, raw) = {
        use std::os::windows::ffi::OsStrExt;

        let mut raw = Vec::new();
        for unit in path.as_os_str().encode_wide() {
            raw.extend_from_slice(&unit.to_le_bytes());
        }
        ("windows-wtf16le", raw)
    };
    #[cfg(not(any(unix, windows)))]
    let (platform, raw) = (
        "platform-encoded-bytes",
        path.as_os_str().as_encoded_bytes().to_vec(),
    );

    if raw.len() > MAX_PROVIDER_PATH_IDENTITY_RAW_BYTES {
        return Err(CaptureError::InvalidProviderTranscriptPath {
            path: path.to_path_buf(),
            reason: "provider transcript path exceeds the durable identity limit",
        });
    }

    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut encoded = String::with_capacity(
        "provider-path-v1:"
            .len()
            .saturating_add(platform.len())
            .saturating_add(1)
            .saturating_add(raw.len().saturating_mul(2)),
    );
    encoded.push_str("provider-path-v1:");
    encoded.push_str(platform);
    encoded.push(':');
    for byte in raw {
        encoded.push(char::from(HEX[usize::from(byte >> 4)]));
        encoded.push(char::from(HEX[usize::from(byte & 0x0f)]));
    }
    Ok(encoded)
}

pub(crate) fn captured_batch_cursor_stream(source: &SourceObservation) -> String {
    source.cursor_stream().to_owned()
}

pub(crate) fn provider_source_cursor_stream_for_path(
    provider: CaptureProvider,
    source_format: &str,
    raw_source_path: &str,
) -> String {
    provider_source_cursor_stream_for_component(
        provider,
        source_format,
        provider_source_identity_component(
            None,
            Some(raw_source_path),
            None,
            &serde_json::Value::Null,
        )
        .unwrap_or(("default", "default".to_owned())),
    )
}

#[cfg(test)]
pub(crate) fn provider_source_cursor_stream(
    provider: CaptureProvider,
    source_format: &str,
    source_root: Option<&str>,
) -> String {
    provider_source_cursor_stream_for_component(
        provider,
        source_format,
        provider_source_identity_component(source_root, None, None, &serde_json::Value::Null)
            .unwrap_or(("default", "default".to_owned())),
    )
}

pub(crate) fn provider_source_cursor_range(
    capture: &ProviderCaptureEnvelope,
) -> Option<ProviderCursorRange> {
    if capture.provider == CaptureProvider::Custom {
        return capture.source.cursor.clone();
    }
    capture
        .source
        .cursor
        .as_ref()
        .map(|cursor| ProviderCursorRange {
            before: cursor
                .before
                .as_ref()
                .map(|checkpoint| source_scoped_checkpoint(capture, checkpoint)),
            after: cursor
                .after
                .as_ref()
                .map(|checkpoint| source_scoped_checkpoint(capture, checkpoint)),
        })
}

fn source_scoped_checkpoint(
    capture: &ProviderCaptureEnvelope,
    checkpoint: &ProviderCursorCheckpoint,
) -> ProviderCursorCheckpoint {
    if capture.provider == CaptureProvider::Custom {
        return checkpoint.clone();
    }
    ProviderCursorCheckpoint {
        stream: provider_source_cursor_stream_for_source(capture.provider, &capture.source),
        cursor: checkpoint.cursor.clone(),
        observed_at: checkpoint.observed_at,
    }
}

pub(crate) fn provider_source_cursor_stream_for_source(
    provider: CaptureProvider,
    source: &ProviderSourceEnvelope,
) -> String {
    provider_source_cursor_stream_for_component(
        provider,
        &source.source_format,
        provider_source_identity_component(
            source.source_root.as_deref(),
            source.raw_source_path.as_deref(),
            source.idempotency_key.as_deref(),
            &source.metadata,
        )
        .unwrap_or(("default", "default".to_owned())),
    )
}

fn provider_source_cursor_stream_for_component(
    provider: CaptureProvider,
    source_format: &str,
    component: (&'static str, String),
) -> String {
    let (component_kind, component_value) = component;
    let key = serde_json::to_string(&(
        "provider-source-cursor-v1",
        provider.as_str(),
        source_format,
        component_kind,
        component_value,
    ))
    .expect("provider cursor source identity key should serialize");
    let source_id = stable_capture_uuid(&key, "provider-cursor-source");
    format!(
        "{}:source:{}",
        provider_cursor_stream(provider, source_format),
        source_id.simple()
    )
}

pub(crate) fn provider_sync_cursor(capture: &ProviderCaptureEnvelope) -> Option<SyncCursor> {
    let checkpoint = capture
        .source
        .cursor
        .as_ref()
        .and_then(|cursor| {
            cursor
                .after
                .as_ref()
                .map(|after| source_scoped_checkpoint(capture, after))
        })
        .or_else(|| {
            capture.event.as_ref().and_then(|event| {
                event
                    .cursor
                    .as_ref()
                    .map(|cursor| ProviderCursorCheckpoint {
                        stream: provider_source_cursor_stream_for_source(
                            capture.provider,
                            &capture.source,
                        ),
                        cursor: cursor.clone(),
                        observed_at: event.occurred_at,
                    })
            })
        });
    let checkpoint = checkpoint?;

    Some(provider_sync_cursor_record(
        capture.provider,
        &capture.source.machine_id,
        checkpoint.stream,
        checkpoint.cursor,
        checkpoint.observed_at,
    ))
}

pub(crate) fn certified_provider_sync_cursor(
    provider: CaptureProvider,
    machine_id: &str,
    stream: String,
    cursor: &CertifiedProviderCursor,
    observed_at: DateTime<Utc>,
) -> Result<SyncCursor> {
    Ok(provider_sync_cursor_record(
        provider,
        machine_id,
        stream,
        cursor.encode()?,
        observed_at,
    ))
}

fn provider_sync_cursor_record(
    provider: CaptureProvider,
    machine_id: &str,
    stream: String,
    cursor: String,
    observed_at: DateTime<Utc>,
) -> SyncCursor {
    SyncCursor {
        id: stable_capture_uuid(
            &format!(
                "provider-cursor:{}:{}:{}",
                provider.as_str(),
                machine_id,
                stream
            ),
            "provider-sync-cursor",
        ),
        team_id: None,
        device_id: machine_id.to_owned(),
        stream,
        cursor,
        last_synced_at: Some(observed_at),
        timestamps: timestamps(observed_at),
    }
}

pub(crate) fn compare_and_set_provider_sync_cursor(
    store: &Store,
    expected: Option<&SyncCursor>,
    cursor: &SyncCursor,
) -> Result<ProviderCursorCommit> {
    if store.compare_and_set_sync_cursor(expected, cursor)? {
        Ok(ProviderCursorCommit::Committed)
    } else {
        Ok(ProviderCursorCommit::Conflict)
    }
}

pub(crate) fn persist_provider_sync_cursor(store: &mut Store, cursor: &SyncCursor) -> Result<()> {
    store.upsert_sync_cursor(cursor)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use serde_json::{json, Value};

    use super::*;

    const MAX_NATIVE_POSITION_KIND_BYTES: usize = 256;

    fn test_cursor() -> CertifiedProviderCursor {
        CertifiedProviderCursor::new(
            "sha256:test-source",
            3,
            7,
            NativePosition::new("jsonl-byte", 4096_u64.to_be_bytes().to_vec())
                .expect("valid native position"),
            BoundedParserCheckpoint::from_serializable(&json!({
                "pending_tool_call_ids": ["call-1", "call-2"]
            }))
            .expect("valid parser checkpoint"),
        )
        .expect("valid certified cursor")
        .with_rejected_records(11)
    }

    fn maximal_cursor(
        source_revision: String,
        native_position_kind: String,
    ) -> CertifiedProviderCursor {
        let mut state = 0x9e37_79b9_u32;
        let parser_checkpoint = (0..MAX_PROVIDER_PARSER_CHECKPOINT_BYTES)
            .map(|_| {
                state ^= state << 13;
                state ^= state >> 17;
                state ^= state << 5;
                state as u8
            })
            .collect();
        CertifiedProviderCursor::new(
            source_revision,
            u32::MAX,
            u32::MAX,
            NativePosition::new(
                native_position_kind,
                vec![0; MAX_PROVIDER_NATIVE_POSITION_BYTES],
            )
            .expect("maximal native position"),
            BoundedParserCheckpoint::try_new_bytes(parser_checkpoint)
                .expect("maximal parser checkpoint"),
        )
        .expect("maximal cursor fields")
    }

    #[test]
    fn certified_provider_cursor_round_trips_all_revisions_and_state() {
        let cursor = test_cursor();
        let encoded = cursor.encode().expect("encode cursor");
        let decoded = CertifiedProviderCursor::decode(&encoded).expect("decode cursor");

        assert_eq!(decoded, cursor);
        assert_eq!(decoded.source_revision(), "sha256:test-source");
        assert_eq!(decoded.parser_revision(), 3);
        assert_eq!(decoded.policy_revision(), 7);
        assert!(decoded.matches_revisions("sha256:test-source", 3, 7));
        assert_eq!(decoded.native_position().kind(), "jsonl-byte");
        assert_eq!(decoded.native_position().value(), 4096_u64.to_be_bytes());
        assert_eq!(decoded.rejected_records(), 11);
        let checkpoint: Value = decoded
            .parser_checkpoint()
            .deserialize()
            .expect("decode parser checkpoint");
        assert_eq!(checkpoint["pending_tool_call_ids"][0], "call-1");
    }

    #[test]
    fn certified_provider_cursor_defaults_legacy_rejection_count_to_zero() {
        let mut wire: Value = serde_json::from_str(
            &CertifiedProviderCursor::new(
                "sha256:legacy-source",
                1,
                1,
                NativePosition::new("jsonl-byte", 0_u64.to_be_bytes().to_vec()).unwrap(),
                BoundedParserCheckpoint::from_serializable(&()).unwrap(),
            )
            .unwrap()
            .encode()
            .unwrap(),
        )
        .unwrap();
        wire.as_object_mut().unwrap().remove("r");

        let decoded = CertifiedProviderCursor::decode(&wire.to_string()).unwrap();

        assert_eq!(decoded.rejected_records(), 0);
    }

    #[test]
    fn maximal_position_and_checkpoint_encode_to_a_decoder_acceptable_wire() {
        let cursor = maximal_cursor(
            "r".repeat(MAX_PROVIDER_SOURCE_REVISION_BYTES),
            "k".repeat(MAX_NATIVE_POSITION_KIND_BYTES),
        );

        let encoded = cursor.encode().expect("encode maximal cursor fields");
        assert!(encoded.len() <= MAX_CERTIFIED_PROVIDER_CURSOR_WIRE_BYTES);
        assert_eq!(
            CertifiedProviderCursor::decode(&encoded).expect("decode maximal cursor fields"),
            cursor
        );
    }

    #[test]
    fn escaped_revision_and_kind_cannot_cross_the_final_wire_bound() {
        let escaped_kind = "\0".repeat(MAX_NATIVE_POSITION_KIND_BYTES);
        let baseline = maximal_cursor(
            "r".repeat(MAX_PROVIDER_SOURCE_REVISION_BYTES),
            escaped_kind.clone(),
        )
        .encode()
        .expect("escaped kind leaves room below the wire bound");
        let escaped_revision_bytes =
            (MAX_CERTIFIED_PROVIDER_CURSOR_WIRE_BYTES - baseline.len()) / 5;
        assert!(escaped_revision_bytes < MAX_PROVIDER_SOURCE_REVISION_BYTES);

        let source_revision = format!(
            "{}{}",
            "\0".repeat(escaped_revision_bytes),
            "r".repeat(MAX_PROVIDER_SOURCE_REVISION_BYTES - escaped_revision_bytes)
        );
        let cursor = maximal_cursor(source_revision, escaped_kind.clone());
        let encoded = cursor
            .encode()
            .expect("encode cursor just below wire bound");
        assert!(MAX_CERTIFIED_PROVIDER_CURSOR_WIRE_BYTES - encoded.len() < 5);
        assert_eq!(
            CertifiedProviderCursor::decode(&encoded).expect("decode near-limit cursor"),
            cursor
        );

        let oversized_source_revision = format!(
            "{}{}",
            "\0".repeat(escaped_revision_bytes + 1),
            "r".repeat(MAX_PROVIDER_SOURCE_REVISION_BYTES - escaped_revision_bytes - 1)
        );
        let error = maximal_cursor(oversized_source_revision, escaped_kind)
            .encode()
            .expect_err("reject cursor whose escaped JSON crosses the wire bound");
        assert!(error.to_string().contains(&format!(
            "certified provider cursor exceeds {MAX_CERTIFIED_PROVIDER_CURSOR_WIRE_BYTES} bytes"
        )));
    }

    #[test]
    fn parser_checkpoint_accepts_exact_limit_and_rejects_larger_value() {
        BoundedParserCheckpoint::try_new_bytes(vec![0; MAX_PROVIDER_PARSER_CHECKPOINT_BYTES])
            .expect("checkpoint at byte limit");

        let error = BoundedParserCheckpoint::try_new_bytes(vec![
            0;
            MAX_PROVIDER_PARSER_CHECKPOINT_BYTES
                + 1
        ])
        .expect_err("reject oversized checkpoint");
        assert!(error.to_string().contains("exceeds 262144 bytes"));
    }

    #[test]
    fn serializable_checkpoint_enforces_the_bound_while_writing() {
        let oversized = "x".repeat(MAX_PROVIDER_PARSER_CHECKPOINT_BYTES + 1);
        let error = BoundedParserCheckpoint::from_serializable(&oversized)
            .expect_err("reject oversized serialized checkpoint");

        assert!(error
            .to_string()
            .contains("provider parser checkpoint exceeds its byte bound"));
    }

    #[test]
    fn cursor_debug_redacts_checkpoint_and_native_position_bytes() {
        let cursor = test_cursor();
        let debug = format!("{cursor:?}");

        assert!(!debug.contains("call-1"));
        assert!(!debug.contains("call-2"));
        assert!(!debug.contains("AAABAA"));
        assert!(debug.contains("native_position_bytes"));
        assert!(debug.contains("parser_checkpoint"));
    }

    #[test]
    fn cursor_decode_enforces_checkpoint_bound_and_schema_version() {
        let encoded = serde_json::to_string(&json!({
            "schema_version": CERTIFIED_PROVIDER_CURSOR_SCHEMA_VERSION,
            "source_revision": "sha256:test-source",
            "parser_revision": 1,
            "policy_revision": 1,
            "native_position_kind": "jsonl-byte",
            "native_position_base64": BASE64.encode(1_u64.to_be_bytes()),
            "parser_checkpoint_base64": BASE64.encode(
                vec![0; MAX_PROVIDER_PARSER_CHECKPOINT_BYTES + 1]
            ),
        }))
        .expect("encode oversized checkpoint fixture");
        assert!(CertifiedProviderCursor::decode(&encoded).is_err());

        let mut wire: Value =
            serde_json::from_str(&test_cursor().encode().expect("encode supported cursor"))
                .expect("decode cursor fixture");
        wire["v"] = json!(CERTIFIED_PROVIDER_CURSOR_SCHEMA_VERSION + 1);
        let error = CertifiedProviderCursor::decode(
            &serde_json::to_string(&wire).expect("encode unsupported cursor fixture"),
        )
        .expect_err("reject unsupported cursor version");
        assert!(error
            .to_string()
            .contains("unsupported certified provider cursor schema version 2"));
    }

    #[test]
    fn certified_sync_cursor_keeps_legacy_provider_cursor_identity() {
        let cursor = test_cursor();
        let observed_at = DateTime::<Utc>::UNIX_EPOCH;
        let legacy_stream = provider_source_cursor_stream_for_component(
            CaptureProvider::Codex,
            "jsonl",
            ("path", "/tmp/session.jsonl".to_owned()),
        );
        let source = SourceObservation::new(
            CaptureProvider::Codex,
            "jsonl",
            "session:test",
            "revision:test",
            legacy_stream.clone(),
            cursor.parser_revision(),
            cursor.policy_revision(),
            None,
        )
        .expect("valid source observation");
        assert_eq!(captured_batch_cursor_stream(&source), legacy_stream);
        let sync = certified_provider_sync_cursor(
            CaptureProvider::Codex,
            "test-machine",
            captured_batch_cursor_stream(&source),
            &cursor,
            observed_at,
        )
        .expect("build sync cursor");

        let decoded = CertifiedProviderCursor::decode(&sync.cursor).expect("decode sync cursor");
        assert_eq!(decoded, cursor);
        assert_eq!(sync.last_synced_at, Some(observed_at));
    }
}
