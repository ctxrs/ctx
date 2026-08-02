use tantivy::schema::{
    Field, IndexRecordOption, Schema, TextFieldIndexing, TextOptions, FAST, INDEXED, STORED, STRING,
};

use crate::{analyzer::BODY_ANALYZER, IndexError, Result, LEXICAL_SCHEMA_VERSION};

#[derive(Clone, Copy)]
pub(crate) struct Fields {
    pub(crate) event_id: Field,
    pub(crate) event_identity_digest: Field,
    pub(crate) event_id_high: Field,
    pub(crate) event_id_low: Field,
    pub(crate) session_id: Field,
    pub(crate) session_id_high: Field,
    pub(crate) session_id_low: Field,
    pub(crate) parent_session_id: Field,
    pub(crate) root_session_id: Field,
    pub(crate) source_key: Field,
    pub(crate) provider: Field,
    pub(crate) source_format: Field,
    pub(crate) custom_provider_key: Field,
    pub(crate) custom_source_id: Field,
    pub(crate) provider_session_id: Field,
    pub(crate) branch: Field,
    pub(crate) agent_type: Field,
    pub(crate) is_primary: Field,
    pub(crate) event_sequence: Field,
    pub(crate) occurred_at_unix_ms: Field,
    pub(crate) event_type: Field,
    pub(crate) role: Field,
    pub(crate) body_search: Field,
    pub(crate) repository_produced_object_id: Field,
    pub(crate) workspace_filter: Field,
    pub(crate) touched_file_filter: Field,
    pub(crate) core_content_bytes: Field,
    pub(crate) core_record_encoded_bytes: Field,
    pub(crate) core_record: Field,
    pub(crate) source_event_order: Field,
    pub(crate) session_event_order: Field,
    pub(crate) semantic_event_order: Field,
}

pub(crate) fn validate_schema(schema: &Schema) -> Result<()> {
    if serde_json::to_vec(schema)? != serde_json::to_vec(&lexical_schema())? {
        return Err(IndexError::SchemaMismatch(LEXICAL_SCHEMA_VERSION));
    }
    Ok(())
}

pub(crate) fn lexical_schema() -> Schema {
    let mut builder = Schema::builder();
    builder.add_text_field("event_id", STRING);
    builder.add_text_field("event_identity_digest", STRING | FAST);
    builder.add_u64_field("event_id_high", FAST);
    builder.add_u64_field("event_id_low", FAST);
    builder.add_text_field("session_id", STRING);
    builder.add_u64_field("session_id_high", FAST);
    builder.add_u64_field("session_id_low", FAST);
    builder.add_text_field("parent_session_id", STRING);
    builder.add_text_field("root_session_id", STRING);
    builder.add_text_field("source_key", STRING | FAST);
    builder.add_text_field("provider", STRING);
    builder.add_text_field("source_format", STRING);
    builder.add_text_field("custom_provider_key", STRING);
    builder.add_text_field("custom_source_id", STRING);
    builder.add_text_field("provider_session_id", STRING);
    builder.add_text_field("branch", STRING);
    builder.add_text_field("agent_type", STRING);
    builder.add_u64_field("is_primary", INDEXED);
    builder.add_u64_field("event_sequence", FAST | INDEXED);
    builder.add_i64_field("occurred_at_unix_ms", FAST | INDEXED);
    builder.add_text_field("event_type", STRING);
    builder.add_text_field("role", STRING);
    let body_indexing = TextFieldIndexing::default()
        .set_tokenizer(BODY_ANALYZER)
        .set_index_option(IndexRecordOption::WithFreqsAndPositions);
    builder.add_text_field(
        "body_search",
        TextOptions::default().set_indexing_options(body_indexing),
    );
    builder.add_text_field("repository_produced_object_id", STRING);
    builder.add_text_field("workspace_filter", STRING);
    builder.add_text_field("touched_file_filter", STRING);
    builder.add_u64_field("core_content_bytes", FAST);
    builder.add_u64_field("core_record_encoded_bytes", FAST);
    builder.add_bytes_field("core_record", STORED);
    builder.add_bytes_field("source_event_order", INDEXED);
    builder.add_bytes_field("session_event_order", INDEXED);
    builder.add_bytes_field("semantic_event_order", INDEXED);
    builder.build()
}

pub(crate) fn fields_from_schema(schema: &Schema) -> Result<Fields> {
    Ok(Fields {
        event_id: required_field(schema, "event_id")?,
        event_identity_digest: required_field(schema, "event_identity_digest")?,
        event_id_high: required_field(schema, "event_id_high")?,
        event_id_low: required_field(schema, "event_id_low")?,
        session_id: required_field(schema, "session_id")?,
        session_id_high: required_field(schema, "session_id_high")?,
        session_id_low: required_field(schema, "session_id_low")?,
        parent_session_id: required_field(schema, "parent_session_id")?,
        root_session_id: required_field(schema, "root_session_id")?,
        source_key: required_field(schema, "source_key")?,
        provider: required_field(schema, "provider")?,
        source_format: required_field(schema, "source_format")?,
        custom_provider_key: required_field(schema, "custom_provider_key")?,
        custom_source_id: required_field(schema, "custom_source_id")?,
        provider_session_id: required_field(schema, "provider_session_id")?,
        branch: required_field(schema, "branch")?,
        agent_type: required_field(schema, "agent_type")?,
        is_primary: required_field(schema, "is_primary")?,
        event_sequence: required_field(schema, "event_sequence")?,
        occurred_at_unix_ms: required_field(schema, "occurred_at_unix_ms")?,
        event_type: required_field(schema, "event_type")?,
        role: required_field(schema, "role")?,
        body_search: required_field(schema, "body_search")?,
        repository_produced_object_id: required_field(schema, "repository_produced_object_id")?,
        workspace_filter: required_field(schema, "workspace_filter")?,
        touched_file_filter: required_field(schema, "touched_file_filter")?,
        core_content_bytes: required_field(schema, "core_content_bytes")?,
        core_record_encoded_bytes: required_field(schema, "core_record_encoded_bytes")?,
        core_record: required_field(schema, "core_record")?,
        source_event_order: required_field(schema, "source_event_order")?,
        session_event_order: required_field(schema, "session_event_order")?,
        semantic_event_order: required_field(schema, "semantic_event_order")?,
    })
}

pub(crate) fn required_field(schema: &Schema, name: &'static str) -> Result<Field> {
    schema
        .get_field(name)
        .map_err(|_| IndexError::MissingSchemaField(name))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lexical_schema_omits_retired_stored_touched_file() {
        assert!(lexical_schema().get_field("touched_file").is_err());
    }

    #[test]
    fn produced_object_id_is_exact_indexed_and_not_stored() {
        let schema = lexical_schema();
        let field = schema.get_field("repository_produced_object_id").unwrap();
        let entry = schema.get_field_entry(field);

        assert!(entry.is_indexed());
        assert!(!entry.is_stored());
        assert_eq!(entry.field_type().value_type(), tantivy::schema::Type::Str);
    }

    #[test]
    fn core_record_is_the_only_stored_document_representation() {
        let schema = lexical_schema();
        let stored = schema
            .fields()
            .filter_map(|(_, entry)| entry.is_stored().then_some(entry.name()))
            .collect::<Vec<_>>();

        assert_eq!(stored, vec!["core_record"]);
        for removed in [
            "query_metadata",
            "event_identity",
            "session_identity_digest",
            "session_identity",
            "parent_session_identity",
            "root_session_identity",
            "workspace",
            "cwd",
        ] {
            assert!(schema.get_field(removed).is_err(), "{removed} still exists");
        }
    }

    #[test]
    fn core_record_encoded_size_is_u64_metadata_and_not_stored_or_indexed() {
        let schema = lexical_schema();
        let field = schema.get_field("core_record_encoded_bytes").unwrap();
        let entry = schema.get_field_entry(field);

        assert_eq!(entry.field_type().value_type(), tantivy::schema::Type::U64);
        assert!(!entry.is_stored());
        assert!(!entry.is_indexed());
    }
}
