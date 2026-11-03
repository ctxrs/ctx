use ctx_history_core::LiteralFactKind;
use tantivy::schema::{
    Field, IndexRecordOption, Schema, TextFieldIndexing, TextOptions, FAST, INDEXED, STORED, STRING,
};

use crate::{analyzer::BODY_ANALYZER, IndexError, Result, LEXICAL_SCHEMA_VERSION};

#[derive(Clone, Copy)]
pub struct Fields {
    pub event_id: Field,
    pub event_identity_digest: Field,
    pub event_id_high: Field,
    pub event_id_low: Field,
    pub session_id: Field,
    pub session_id_high: Field,
    pub session_id_low: Field,
    pub parent_session_id: Field,
    pub root_session_id: Field,
    pub provider_native_session_relationship: Field,
    pub event_copy_ancestor_session_id: Field,
    pub event_copy_ancestor_event_id: Field,
    pub event_copy_proof: Field,
    pub source_key: Field,
    pub provider: Field,
    pub source_format: Field,
    pub custom_provider_key: Field,
    pub custom_source_id: Field,
    pub provider_session_id: Field,
    pub agent_scope: Field,
    pub event_sequence: Field,
    pub occurred_at_unix_ms: Field,
    pub event_type: Field,
    pub role: Field,
    pub body_search: Field,
    pub fact_session_cwd: Field,
    pub fact_tool_workdir: Field,
    pub fact_file: Field,
    pub fact_url: Field,
    pub fact_forge: Field,
    pub fact_project: Field,
    pub fact_vcs: Field,
    pub fact_commit: Field,
    pub fact_pull_request: Field,
    pub fact_command: Field,
    pub fact_branch: Field,
    pub fact_workspace: Field,
    pub fact_provider_disposition: Field,
    pub core_content_bytes: Field,
    pub core_record_encoded_bytes: Field,
    pub core_record: Field,
    pub source_event_order: Field,
    pub session_event_order: Field,
    pub session_authority: Field,
    pub semantic_event_order: Field,
    pub event_range_order: Field,
    pub discovery_eligible: Field,
    pub provider_call_result: Field,
}

impl Fields {
    pub const fn literal_fact(self, kind: LiteralFactKind) -> Field {
        match kind {
            LiteralFactKind::SessionCwd => self.fact_session_cwd,
            LiteralFactKind::ToolWorkdir => self.fact_tool_workdir,
            LiteralFactKind::File => self.fact_file,
            LiteralFactKind::Url => self.fact_url,
            LiteralFactKind::Forge => self.fact_forge,
            LiteralFactKind::Project => self.fact_project,
            LiteralFactKind::Vcs => self.fact_vcs,
            LiteralFactKind::Commit => self.fact_commit,
            LiteralFactKind::PullRequest => self.fact_pull_request,
            LiteralFactKind::Command => self.fact_command,
            LiteralFactKind::Branch => self.fact_branch,
            LiteralFactKind::Workspace => self.fact_workspace,
            LiteralFactKind::ProviderDisposition => self.fact_provider_disposition,
        }
    }

    pub const fn literal_fact_fields(self) -> [Field; 13] {
        [
            self.fact_session_cwd,
            self.fact_tool_workdir,
            self.fact_file,
            self.fact_url,
            self.fact_forge,
            self.fact_project,
            self.fact_vcs,
            self.fact_commit,
            self.fact_pull_request,
            self.fact_command,
            self.fact_branch,
            self.fact_workspace,
            self.fact_provider_disposition,
        ]
    }
}

pub fn validate_schema(schema: &Schema) -> Result<()> {
    if serde_json::to_vec(schema)? != serde_json::to_vec(&lexical_schema())? {
        return Err(IndexError::SchemaMismatch(LEXICAL_SCHEMA_VERSION));
    }
    Ok(())
}

pub fn lexical_schema() -> Schema {
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
    builder.add_text_field("provider_native_session_relationship", STRING);
    builder.add_text_field("event_copy_ancestor_session_id", STRING);
    builder.add_text_field("event_copy_ancestor_event_id", STRING);
    builder.add_text_field("event_copy_proof", STRING);
    builder.add_text_field("source_key", STRING | FAST);
    builder.add_text_field("provider", STRING);
    builder.add_text_field("source_format", STRING);
    builder.add_text_field("custom_provider_key", STRING);
    builder.add_text_field("custom_source_id", STRING);
    builder.add_text_field("provider_session_id", STRING);
    builder.add_text_field("agent_scope", STRING);
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
    for name in [
        "fact_session_cwd",
        "fact_tool_workdir",
        "fact_file",
        "fact_url",
        "fact_forge",
        "fact_project",
        "fact_vcs",
        "fact_commit",
        "fact_pull_request",
        "fact_command",
        "fact_branch",
        "fact_workspace",
        "fact_provider_disposition",
    ] {
        builder.add_text_field(name, STRING);
    }
    builder.add_u64_field("core_content_bytes", FAST);
    builder.add_u64_field("core_record_encoded_bytes", FAST);
    builder.add_bytes_field("core_record", STORED);
    builder.add_bytes_field("source_event_order", INDEXED);
    builder.add_bytes_field("session_event_order", INDEXED);
    builder.add_bytes_field("session_authority", STORED | INDEXED);
    builder.add_bytes_field("semantic_event_order", INDEXED);
    builder.add_bytes_field("event_range_order", FAST | INDEXED);
    builder.add_u64_field("discovery_eligible", INDEXED);
    builder.add_bytes_field("provider_call_result", INDEXED);
    builder.build()
}

pub fn fields_from_schema(schema: &Schema) -> Result<Fields> {
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
        provider_native_session_relationship: required_field(
            schema,
            "provider_native_session_relationship",
        )?,
        event_copy_ancestor_session_id: required_field(schema, "event_copy_ancestor_session_id")?,
        event_copy_ancestor_event_id: required_field(schema, "event_copy_ancestor_event_id")?,
        event_copy_proof: required_field(schema, "event_copy_proof")?,
        source_key: required_field(schema, "source_key")?,
        provider: required_field(schema, "provider")?,
        source_format: required_field(schema, "source_format")?,
        custom_provider_key: required_field(schema, "custom_provider_key")?,
        custom_source_id: required_field(schema, "custom_source_id")?,
        provider_session_id: required_field(schema, "provider_session_id")?,
        agent_scope: required_field(schema, "agent_scope")?,
        event_sequence: required_field(schema, "event_sequence")?,
        occurred_at_unix_ms: required_field(schema, "occurred_at_unix_ms")?,
        event_type: required_field(schema, "event_type")?,
        role: required_field(schema, "role")?,
        body_search: required_field(schema, "body_search")?,
        fact_session_cwd: required_field(schema, "fact_session_cwd")?,
        fact_tool_workdir: required_field(schema, "fact_tool_workdir")?,
        fact_file: required_field(schema, "fact_file")?,
        fact_url: required_field(schema, "fact_url")?,
        fact_forge: required_field(schema, "fact_forge")?,
        fact_project: required_field(schema, "fact_project")?,
        fact_vcs: required_field(schema, "fact_vcs")?,
        fact_commit: required_field(schema, "fact_commit")?,
        fact_pull_request: required_field(schema, "fact_pull_request")?,
        fact_command: required_field(schema, "fact_command")?,
        fact_branch: required_field(schema, "fact_branch")?,
        fact_workspace: required_field(schema, "fact_workspace")?,
        fact_provider_disposition: required_field(schema, "fact_provider_disposition")?,
        core_content_bytes: required_field(schema, "core_content_bytes")?,
        core_record_encoded_bytes: required_field(schema, "core_record_encoded_bytes")?,
        core_record: required_field(schema, "core_record")?,
        source_event_order: required_field(schema, "source_event_order")?,
        session_event_order: required_field(schema, "session_event_order")?,
        session_authority: required_field(schema, "session_authority")?,
        semantic_event_order: required_field(schema, "semantic_event_order")?,
        event_range_order: required_field(schema, "event_range_order")?,
        discovery_eligible: required_field(schema, "discovery_eligible")?,
        provider_call_result: required_field(schema, "provider_call_result")?,
    })
}

pub fn required_field(schema: &Schema, name: &'static str) -> Result<Field> {
    schema
        .get_field(name)
        .map_err(|_| IndexError::MissingSchemaField(name))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lexical_schema_has_only_neutral_core_and_literal_fact_fields() {
        let schema = lexical_schema();
        for removed in [
            "event_origin_kind",
            "origin_event_id",
            "repository_produced_object_id",
            "touched_file_filter",
            "workspace_filter",
            "branch",
            "agent_type",
            "is_primary",
        ] {
            assert!(schema.get_field(removed).is_err(), "{removed} still exists");
        }

        let fields = fields_from_schema(&schema).unwrap();
        for field in fields.literal_fact_fields() {
            let entry = schema.get_field_entry(field);
            assert!(entry.is_indexed());
            assert!(!entry.is_stored());
            assert_eq!(entry.field_type().value_type(), tantivy::schema::Type::Str);
        }
    }

    #[test]
    fn provider_native_relationship_and_copy_fields_are_exact_only() {
        let schema = lexical_schema();
        for name in [
            "provider_native_session_relationship",
            "event_copy_ancestor_session_id",
            "event_copy_ancestor_event_id",
            "event_copy_proof",
        ] {
            let entry = schema.get_field_entry(schema.get_field(name).unwrap());
            assert!(entry.is_indexed(), "{name} is not indexed");
            assert!(!entry.is_stored(), "{name} duplicates stored Core");
            assert_eq!(entry.field_type().value_type(), tantivy::schema::Type::Str);
        }
    }

    #[test]
    fn authority_and_core_record_are_the_only_stored_document_representations() {
        let schema = lexical_schema();
        let stored = schema
            .fields()
            .filter_map(|(_, entry)| entry.is_stored().then_some(entry.name()))
            .collect::<Vec<_>>();

        assert_eq!(stored, vec!["core_record", "session_authority"]);
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

    #[test]
    fn discovery_eligibility_is_positive_only_indexed_metadata() {
        let schema = lexical_schema();
        let field = schema.get_field("discovery_eligible").unwrap();
        let entry = schema.get_field_entry(field);

        assert_eq!(entry.field_type().value_type(), tantivy::schema::Type::U64);
        assert!(entry.is_indexed());
        assert!(!entry.is_stored());
        assert!(!entry.is_fast());
    }
}
