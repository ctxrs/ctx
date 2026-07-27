use rusqlite::Connection;

use super::super::schema::{
    goose_message_columns, goose_message_expressions, goose_session_columns,
    goose_session_expressions,
};
use super::create_goose_tables;

#[test]
fn goose_schema_field_specs_keep_exact_hydration_and_retained_order() {
    let conn = Connection::open_in_memory().unwrap();
    create_goose_tables(&conn);

    let session = goose_session_expressions(&goose_session_columns(&conn).unwrap(), "s");
    assert_eq!(
        session
            .hydration
            .iter()
            .map(String::as_str)
            .collect::<Vec<_>>(),
        [
            "CAST(s.id AS TEXT)",
            "s.name",
            "s.description",
            "s.user_set_name",
            "s.session_type",
            "s.working_dir",
            "s.created_at",
            "s.updated_at",
            "s.extension_data",
            "s.total_tokens",
            "s.input_tokens",
            "s.output_tokens",
            "s.accumulated_total_tokens",
            "s.accumulated_input_tokens",
            "s.accumulated_output_tokens",
            "s.accumulated_cost",
            "s.provider_name",
            "s.model_config_json",
            "s.goose_mode",
            "s.archived_at",
            "s.project_id",
        ]
    );
    assert_eq!(session.retained[0], "s.id");
    assert_eq!(session.hydration[1..], session.retained[1..]);

    let message = goose_message_expressions(&goose_message_columns(&conn).unwrap(), "m");
    assert_eq!(
        message
            .hydration
            .iter()
            .map(String::as_str)
            .collect::<Vec<_>>(),
        [
            "m.rowid",
            "m.id",
            "m.message_id",
            "CAST(m.session_id AS TEXT)",
            "m.role",
            "m.content_json",
            "m.created_timestamp",
            "m.timestamp",
            "CAST(m.tokens AS TEXT)",
            "m.metadata_json",
        ]
    );
    assert_eq!(
        message
            .retained
            .iter()
            .map(String::as_str)
            .collect::<Vec<_>>(),
        [
            "m.rowid",
            "m.id",
            "m.message_id",
            "m.session_id",
            "m.role",
            "m.content_json",
            "m.created_timestamp",
            "m.timestamp",
            "m.tokens",
            "m.metadata_json",
        ]
    );
}
