use std::{iter::Empty, slice, sync::Arc};

use tantivy::schema::{
    document::{ReferenceValue, ReferenceValueLeaf},
    Document, Field, Value,
};

use ctx_history_core::{
    CoreContent, CoreRecord, SourceKey, MAX_CORE_CONTENT_BYTES, MAX_ENCODED_CORE_RECORD_BYTES,
};

use crate::{Fields, IndexError, LexicalDocument, Result};

const BASE_FIELD_VALUES: usize = 33;
pub(crate) const SOURCE_EVENT_ORDER_SOURCE_PREFIX_LEN: usize = 64;
pub(crate) const SOURCE_EVENT_ORDER_KEY_LEN: usize = 104;
const SOURCE_EVENT_ORDER_EVENT_DIGEST_OFFSET: usize = SOURCE_EVENT_ORDER_SOURCE_PREFIX_LEN;
const SOURCE_EVENT_ORDER_ENCODED_BYTES_OFFSET: usize = SOURCE_EVENT_ORDER_EVENT_DIGEST_OFFSET + 32;
const SOURCE_EVENT_ORDER_CONTENT_BYTES_OFFSET: usize = SOURCE_EVENT_ORDER_ENCODED_BYTES_OFFSET + 4;
const SOURCE_EVENT_ORDER_SIZE_SUFFIX_LEN: usize = 8;
const SOURCE_EVENT_ORDER_FIELD: &str = "source_event_order";

#[derive(Clone)]
pub(super) struct IndexSourceFields {
    token: Arc<str>,
    descriptor_digest: [u8; 32],
    provider: Arc<str>,
    source_format: Arc<str>,
}

impl IndexSourceFields {
    pub(super) fn new(document_source: &ctx_history_core::SourceKey, token: &str) -> Self {
        Self {
            token: Arc::from(token),
            descriptor_digest: document_source.exact_descriptor_digest(),
            provider: Arc::from(document_source.provider()),
            source_format: Arc::from(document_source.source_format()),
        }
    }

    pub(super) fn descriptor_digest(&self) -> [u8; 32] {
        self.descriptor_digest
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct SourceEventOrderKey([u8; SOURCE_EVENT_ORDER_KEY_LEN]);

impl SourceEventOrderKey {
    fn for_document(
        source: &IndexSourceFields,
        event_digest: [u8; 32],
        encoded_core_bytes: usize,
        content_bytes: usize,
    ) -> Result<Self> {
        Self::from_parts(
            source.identity_digest,
            source.descriptor_digest,
            event_digest,
            encoded_core_bytes,
            content_bytes,
        )
    }

    fn from_parts(
        source_digest: [u8; 32],
        source_descriptor_digest: [u8; 32],
        event_digest: [u8; 32],
        encoded_core_bytes: usize,
        content_bytes: usize,
    ) -> Result<Self> {
        if encoded_core_bytes == 0 || encoded_core_bytes > MAX_ENCODED_CORE_RECORD_BYTES {
            return Err(IndexError::DocumentFieldTooLarge {
                field: "core_record",
                actual: encoded_core_bytes,
                maximum: MAX_ENCODED_CORE_RECORD_BYTES,
            });
        }
        if content_bytes > MAX_CORE_CONTENT_BYTES {
            return Err(IndexError::DocumentFieldTooLarge {
                field: "core_content",
                actual: content_bytes,
                maximum: MAX_CORE_CONTENT_BYTES,
            });
        }
        let encoded_core_bytes = u32::try_from(encoded_core_bytes).map_err(|_| {
            IndexError::WriterInvariant("encoded Core size does not fit the source order key")
        })?;
        let content_bytes = u32::try_from(content_bytes).map_err(|_| {
            IndexError::WriterInvariant("Core content size does not fit the source order key")
        })?;

        let mut key = [0_u8; SOURCE_EVENT_ORDER_KEY_LEN];
        key[..32].copy_from_slice(&source_digest);
        key[32..SOURCE_EVENT_ORDER_SOURCE_PREFIX_LEN].copy_from_slice(&source_descriptor_digest);
        key[SOURCE_EVENT_ORDER_EVENT_DIGEST_OFFSET..SOURCE_EVENT_ORDER_ENCODED_BYTES_OFFSET]
            .copy_from_slice(&event_digest);
        key[SOURCE_EVENT_ORDER_ENCODED_BYTES_OFFSET..SOURCE_EVENT_ORDER_CONTENT_BYTES_OFFSET]
            .copy_from_slice(&encoded_core_bytes.to_be_bytes());
        key[SOURCE_EVENT_ORDER_CONTENT_BYTES_OFFSET..]
            .copy_from_slice(&content_bytes.to_be_bytes());
        Ok(Self(key))
    }

    pub(crate) fn for_core_record(record: &CoreRecord, encoded_core_bytes: usize) -> Result<Self> {
        Self::from_parts(
            record.source.identity().digest(),
            record.source.exact_descriptor_digest(),
            record.event_id.digest(),
            encoded_core_bytes,
            core_content_bytes(&record.content)?,
        )
    }

    pub(crate) fn decode_for_source(source: &SourceKey, encoded: &[u8]) -> Result<Self> {
        let key: [u8; SOURCE_EVENT_ORDER_KEY_LEN] = encoded
            .try_into()
            .map_err(|_| IndexError::InvalidStoredDocumentField(SOURCE_EVENT_ORDER_FIELD))?;
        if key[..SOURCE_EVENT_ORDER_SOURCE_PREFIX_LEN] != Self::source_prefix(source) {
            return Err(IndexError::InvalidStoredDocumentField(
                SOURCE_EVENT_ORDER_FIELD,
            ));
        }
        let key = Self(key);
        if key.encoded_core_bytes() == 0
            || key.encoded_core_bytes() > MAX_ENCODED_CORE_RECORD_BYTES
            || key.content_bytes() > MAX_CORE_CONTENT_BYTES
        {
            return Err(IndexError::InvalidStoredDocumentField(
                SOURCE_EVENT_ORDER_FIELD,
            ));
        }
        Ok(key)
    }

    pub(crate) fn source_prefix(source: &SourceKey) -> [u8; SOURCE_EVENT_ORDER_SOURCE_PREFIX_LEN] {
        let mut prefix = [0_u8; SOURCE_EVENT_ORDER_SOURCE_PREFIX_LEN];
        prefix[..32].copy_from_slice(&source.identity().digest());
        prefix[32..].copy_from_slice(&source.exact_descriptor_digest());
        prefix
    }

    pub(crate) fn source_range_end(source: &SourceKey) -> Vec<u8> {
        let mut bound = Vec::with_capacity(SOURCE_EVENT_ORDER_KEY_LEN + 1);
        bound.extend_from_slice(&Self::source_prefix(source));
        bound.extend(std::iter::repeat_n(
            u8::MAX,
            SOURCE_EVENT_ORDER_KEY_LEN - SOURCE_EVENT_ORDER_SOURCE_PREFIX_LEN + 1,
        ));
        bound
    }

    pub(crate) fn source_after_bound(source: &SourceKey, event_digest: [u8; 32]) -> Vec<u8> {
        let mut bound = Vec::with_capacity(SOURCE_EVENT_ORDER_KEY_LEN + 1);
        bound.extend_from_slice(&Self::source_prefix(source));
        bound.extend_from_slice(&event_digest);
        bound.extend(std::iter::repeat_n(
            u8::MAX,
            SOURCE_EVENT_ORDER_SIZE_SUFFIX_LEN + 1,
        ));
        bound
    }

    pub(crate) fn event_digest(self) -> [u8; 32] {
        let mut digest = [0_u8; 32];
        digest.copy_from_slice(
            &self.0
                [SOURCE_EVENT_ORDER_EVENT_DIGEST_OFFSET..SOURCE_EVENT_ORDER_ENCODED_BYTES_OFFSET],
        );
        digest
    }

    pub(crate) fn encoded_core_bytes(self) -> usize {
        let mut encoded = [0_u8; 4];
        encoded.copy_from_slice(
            &self.0
                [SOURCE_EVENT_ORDER_ENCODED_BYTES_OFFSET..SOURCE_EVENT_ORDER_CONTENT_BYTES_OFFSET],
        );
        u32::from_be_bytes(encoded) as usize
    }

    pub(crate) fn content_bytes(self) -> usize {
        let mut encoded = [0_u8; 4];
        encoded.copy_from_slice(&self.0[SOURCE_EVENT_ORDER_CONTENT_BYTES_OFFSET..]);
        u32::from_be_bytes(encoded) as usize
    }

    pub(crate) fn into_bytes(self) -> [u8; SOURCE_EVENT_ORDER_KEY_LEN] {
        self.0
    }
}

pub(crate) fn core_content_bytes(content: &CoreContent) -> Result<usize> {
    let normalized_body_bytes = content.normalized_body.as_ref().map_or(0, String::len);
    let structured_content_bytes = content
        .structured_content
        .as_ref()
        .map(serde_json::to_vec)
        .transpose()?
        .map_or(0, |encoded| encoded.len());
    let content_bytes = normalized_body_bytes
        .checked_add(structured_content_bytes)
        .ok_or(IndexError::CountOverflow)?;
    if content_bytes > MAX_CORE_CONTENT_BYTES {
        return Err(IndexError::DocumentFieldTooLarge {
            field: "core_content",
            actual: content_bytes,
            maximum: MAX_CORE_CONTENT_BYTES,
        });
    }
    Ok(content_bytes)
}
pub(super) struct EncodedDocumentIdentities {
    event: [u8; ctx_history_core::StableEntityId::CANONICAL_LEN],
    session: [u8; ctx_history_core::StableEntityId::CANONICAL_LEN],
    parent: Option<[u8; ctx_history_core::StableEntityId::CANONICAL_LEN]>,
    root: [u8; ctx_history_core::StableEntityId::CANONICAL_LEN],
}

impl EncodedDocumentIdentities {
    pub(super) fn new(document: &LexicalDocument) -> Result<Self> {
        Ok(Self {
            event: document.event_id.encode_canonical()?,
            session: document.session_id.encode_canonical()?,
            parent: document
                .parent_session_id
                .map(ctx_history_core::StableEntityId::encode_canonical)
                .transpose()?,
            root: document.root_session_id.encode_canonical()?,
        })
    }
}

pub(super) struct SourceToken([u8; 64]);

impl SourceToken {
    pub(super) fn new(source_digest: &[u8; 32]) -> Self {
        const DIGITS: &[u8; 16] = b"0123456789abcdef";

        let mut encoded = [0_u8; 64];
        for (index, byte) in source_digest.iter().copied().enumerate() {
            encoded[index * 2] = DIGITS[(byte >> 4) as usize];
            encoded[index * 2 + 1] = DIGITS[(byte & 0x0f) as usize];
        }
        Self(encoded)
    }

    pub(super) fn as_str(&self) -> Result<&str> {
        std::str::from_utf8(&self.0).map_err(|_| {
            IndexError::WriterInvariant("source token encoding produced invalid UTF-8")
        })
    }
}

#[derive(Debug)]
pub(super) enum IndexValue {
    Text(String),
    SharedText(Arc<str>),
    Bytes(Vec<u8>),
    U64(u64),
    I64(i64),
}

impl<'a> Value<'a> for &'a IndexValue {
    type ArrayIter = Empty<Self>;
    type ObjectIter = Empty<(&'a str, Self)>;

    fn as_value(&self) -> ReferenceValue<'a, Self> {
        let leaf = match self {
            IndexValue::Text(value) => ReferenceValueLeaf::Str(value),
            IndexValue::SharedText(value) => ReferenceValueLeaf::Str(value),
            IndexValue::Bytes(value) => ReferenceValueLeaf::Bytes(value),
            IndexValue::U64(value) => ReferenceValueLeaf::U64(*value),
            IndexValue::I64(value) => ReferenceValueLeaf::I64(*value),
        };
        ReferenceValue::Leaf(leaf)
    }
}

pub(super) struct IndexDocument {
    fields: Vec<(Field, IndexValue)>,
}

impl IndexDocument {
    pub(super) fn with_capacity(field_values: usize) -> Self {
        Self {
            fields: Vec::with_capacity(field_values),
        }
    }

    pub(super) fn add_text(&mut self, field: Field, value: String) {
        self.fields.push((field, IndexValue::Text(value)));
    }

    pub(super) fn add_shared_text(&mut self, field: Field, value: Arc<str>) {
        self.fields.push((field, IndexValue::SharedText(value)));
    }

    pub(super) fn add_bytes(&mut self, field: Field, value: impl Into<Vec<u8>>) {
        self.fields.push((field, IndexValue::Bytes(value.into())));
    }

    pub(super) fn add_u64(&mut self, field: Field, value: u64) {
        self.fields.push((field, IndexValue::U64(value)));
    }

    pub(super) fn add_i64(&mut self, field: Field, value: i64) {
        self.fields.push((field, IndexValue::I64(value)));
    }

    pub(super) fn from_lexical(
        fields: Fields,
        document: LexicalDocument,
        locator_bytes: Vec<u8>,
        core_record_bytes: Vec<u8>,
        core_content_bytes: usize,
        identities: EncodedDocumentIdentities,
        source: IndexSourceFields,
    ) -> Result<Self> {
        let source_event_order = SourceEventOrderKey::for_document(
            &source,
            document.event_id.digest(),
            core_record_bytes.len(),
            core_content_bytes,
        )?;
        let mut target = Self::with_capacity(BASE_FIELD_VALUES + document.touched_files.len() * 2);
        target.add_text(fields.event_id, document.event_id.to_string());
        target.add_text(
            fields.event_identity_digest,
            crate::hex(&document.event_id.digest()),
        );
        target.add_bytes(fields.event_identity, identities.event);
        let event_uuid = document.event_id.as_uuid().as_u128();
        target.add_u64(fields.event_id_high, (event_uuid >> 64) as u64);
        target.add_u64(fields.event_id_low, event_uuid as u64);
        target.add_text(fields.session_id, document.session_id.to_string());
        target.add_text(
            fields.session_identity_digest,
            crate::hex(&document.session_id.digest()),
        );
        target.add_bytes(fields.session_identity, identities.session);
        if let (Some(parent_session_id), Some(parent_identity)) =
            (document.parent_session_id, identities.parent)
        {
            target.add_text(fields.parent_session_id, parent_session_id.to_string());
            target.add_bytes(fields.parent_session_identity, parent_identity);
        }
        target.add_text(fields.root_session_id, document.root_session_id.to_string());
        target.add_bytes(fields.root_session_identity, identities.root);
        target.add_shared_text(fields.source_key, source.token);
        target.add_bytes(fields.native_locator, locator_bytes);
        target.add_shared_text(fields.provider, source.provider);
        target.add_shared_text(fields.source_format, source.source_format);
        if let Some(provider_session_id) = document.provider_session_id {
            target.add_text(fields.provider_session_id, provider_session_id);
        }
        if let Some(branch) = document.branch {
            target.add_text(fields.branch, branch);
        }
        if let Some(source_path) = document.source_path {
            target.add_text(fields.workspace_filter, source_path.to_lowercase());
            target.add_text(fields.source_path, source_path);
        }
        target.add_text(fields.agent_type, document.agent_type);
        target.add_u64(fields.is_primary, u64::from(document.is_primary));
        target.add_u64(fields.event_sequence, document.event_sequence);
        if let Some(occurred_at_unix_ms) = document.occurred_at_unix_ms {
            target.add_i64(fields.occurred_at_unix_ms, occurred_at_unix_ms);
        }
        target.add_text(fields.event_type, document.event_type);
        if let Some(role) = document.role {
            target.add_text(fields.role, role);
        }
        target.add_text(fields.body_search, document.body);
        if let Some(workspace) = document.workspace {
            target.add_text(fields.workspace_filter, workspace.to_lowercase());
            target.add_text(fields.workspace, workspace);
        }
        if let Some(cwd) = document.cwd {
            target.add_text(fields.workspace_filter, cwd.to_lowercase());
            target.add_text(fields.cwd, cwd);
        }
        for touched_file in document.touched_files {
            target.add_text(fields.touched_file_filter, touched_file.to_lowercase());
            target.add_text(fields.touched_file, touched_file);
        }
        target.add_bytes(fields.core_record, core_record_bytes);
        target
    }
}

pub(super) struct IndexDocumentIter<'a>(slice::Iter<'a, (Field, IndexValue)>);

impl<'a> Iterator for IndexDocumentIter<'a> {
    type Item = (Field, &'a IndexValue);

    fn next(&mut self) -> Option<Self::Item> {
        let (field, value) = self.0.next()?;
        Some((*field, value))
    }
}

impl Document for IndexDocument {
    type Value<'a> = &'a IndexValue;
    type FieldsValuesIter<'a> = IndexDocumentIter<'a>;

    fn iter_fields_and_values(&self) -> Self::FieldsValuesIter<'_> {
        IndexDocumentIter(self.fields.iter())
    }
}

#[cfg(test)]
mod tests {
    use ctx_history_core::{
        derive_event_id, derive_session_id, EventIdentityInput, LocatorRevisionPolicy,
        NativeItemKey, NativeRecordCoordinate, NativeSessionKey, SessionIdentityInput,
        SourceAnchor, SourceKey, SourceRecordLocator, TypedKey,
    };
    use tantivy::schema::{Document, TantivyDocument};
    use tempfile::tempdir;

    use super::*;
    use crate::{fields_from_schema, lexical_schema, GenerationWriter, IndexError, WriterOptions};

    fn source(source_format: &str) -> SourceKey {
        SourceKey::derive(
            "codex",
            source_format,
            "session",
            1,
            SourceAnchor::provider_native(
                "session-file",
                TypedKey::utf8("move-backed-document-test").unwrap(),
            )
            .unwrap(),
        )
        .unwrap()
    }

    fn lexical_document(source: &SourceKey) -> LexicalDocument {
        let native_session_coordinate = TypedKey::utf8("session").unwrap();
        let session_key =
            NativeSessionKey::native_id("session", native_session_coordinate.clone()).unwrap();
        let session_id = derive_session_id(SessionIdentityInput {
            source,
            logical_session_kind: "thread",
            native_session_key: &session_key,
        })
        .unwrap();
        let native_item_key = NativeItemKey::native_id("message", TypedKey::U64(1)).unwrap();
        let event_id = derive_event_id(EventIdentityInput {
            source,
            session_id,
            logical_item_kind: "message",
            native_item_key: &native_item_key,
            subrecord_selector: None,
        })
        .unwrap();
        LexicalDocument {
            event_id,
            session_id,
            parent_session_id: None,
            root_session_id: session_id,
            source: source.clone(),
            locator: SourceRecordLocator::new(
                source.clone(),
                NativeRecordCoordinate::Jsonl {
                    byte_offset: 0,
                    byte_length: 100,
                    physical_ordinal: 1,
                    native_session_key: Some(native_session_coordinate),
                    native_event_key: Some(TypedKey::U64(1)),
                },
                LocatorRevisionPolicy::StableRecordEvidence,
                None,
                [1; 32],
            )
            .unwrap(),
            provider_session_id: None,
            branch: None,
            source_path: None,
            agent_type: "primary".to_owned(),
            is_primary: true,
            event_sequence: 1,
            occurred_at_unix_ms: None,
            event_type: "message".to_owned(),
            role: None,
            body: "body".to_owned(),
            workspace: None,
            cwd: None,
            touched_files: Vec::new(),
        }
    }

    #[test]
    fn move_backed_values_match_tantivy_document_field_semantics() {
        let schema = lexical_schema();
        let fields = fields_from_schema(&schema).unwrap();
        let body = "move-backed body".repeat(512);
        let body_pointer = body.as_ptr();
        let source = Arc::<str>::from("shared-source-token");
        let source_pointer = source.as_ptr();
        let bytes = vec![7_u8; 113];
        let bytes_pointer = bytes.as_ptr();

        let mut actual = IndexDocument::with_capacity(7);
        actual.add_text(fields.body_search, body);
        actual.add_shared_text(fields.source_key, Arc::clone(&source));
        actual.add_bytes(fields.native_locator, bytes);
        actual.add_u64(fields.event_sequence, 42);
        actual.add_i64(fields.occurred_at_unix_ms, -9);
        actual.add_text(fields.touched_file, "first.rs".to_owned());
        actual.add_text(fields.touched_file, "second.rs".to_owned());

        assert!(actual.fields.iter().any(|(field, value)| {
            *field == fields.body_search
                && matches!(value, IndexValue::Text(value) if value.as_ptr() == body_pointer)
        }));
        assert!(actual.fields.iter().any(|(field, value)| {
            *field == fields.source_key
                && matches!(value, IndexValue::SharedText(value) if value.as_ptr() == source_pointer)
        }));
        assert!(actual.fields.iter().any(|(field, value)| {
            *field == fields.native_locator
                && matches!(value, IndexValue::Bytes(value) if value.as_ptr() == bytes_pointer)
        }));

        let mut expected = TantivyDocument::default();
        expected.add_text(fields.body_search, "move-backed body".repeat(512));
        expected.add_text(fields.source_key, source.as_ref());
        expected.add_bytes(fields.native_locator, &[7_u8; 113]);
        expected.add_u64(fields.event_sequence, 42);
        expected.add_i64(fields.occurred_at_unix_ms, -9);
        expected.add_text(fields.touched_file, "first.rs");
        expected.add_text(fields.touched_file, "second.rs");

        assert_eq!(
            serde_json::to_value(actual.to_named_doc(&schema)).unwrap(),
            serde_json::to_value(expected.to_named_doc(&schema)).unwrap()
        );
    }

    #[test]
    fn stack_source_token_matches_the_persisted_token_encoding() {
        let digest = [0xa5; 32];
        let token = SourceToken::new(&digest);
        assert_eq!(token.as_str().unwrap(), crate::hex(&digest));
    }

    #[test]
    fn cached_source_descriptor_preserves_document_faults() {
        let active = source("codex_session_jsonl");
        let descriptor_alias = source("codex_prompt_history_jsonl");
        assert_eq!(active, descriptor_alias);
        assert!(!active.exact_descriptor_eq(&descriptor_alias));

        let directory = tempdir().unwrap();
        let mut writer =
            GenerationWriter::open(directory.path(), WriterOptions::default()).unwrap();
        writer.begin_source(active.clone()).unwrap();

        let mut mismatched_identity = lexical_document(&active);
        mismatched_identity.source = descriptor_alias.clone();
        mismatched_identity.locator = lexical_document(&descriptor_alias).locator;
        assert!(matches!(
            writer.add_document(mismatched_identity),
            Err(IndexError::IdentitySourceMismatch(_))
        ));
        assert!(matches!(
            writer.add_document(lexical_document(&descriptor_alias)),
            Err(IndexError::DocumentSourceNotActive)
        ));
    }
}
