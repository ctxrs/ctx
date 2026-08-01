use ctx_history_core::SourceAnchor;
use ctx_pro_host_protocol::{CoreRecordPage, CoreSourceState, MaterializeCoreRecordPageRequest};
use serde_json::json;

use super::{
    source_backed::{
        firebender_core_record, firebender_database_path_and_source, firebender_session_id,
    },
    FirebenderRow, FIREBENDER_SOURCE_IDENTITY_REVISION,
};

#[test]
fn direct_core_projection_is_complete_and_self_contained() {
    let sources = [
        include_str!("source_backed.rs"),
        include_str!("source_backed/direct.rs"),
    ];
    let production = sources.join("\n");
    assert!(production.contains("CoreRecord::new_selected"));
    assert!(production.contains("native_event_id = Some"));
    assert!(production.contains("DIRECT_PARSER_REVISION"));
    assert!(production.contains("validate_contract"));
    assert!(production.contains("event.text"));
    for removed_api in [
        concat!("Lexical", "Document"),
        concat!("SourceRecord", "Locator"),
        concat!("hyd", "rate_"),
        concat!("resol", "ver"),
    ] {
        assert!(!production.contains(removed_api), "found {removed_api}");
    }
    assert!(!production.contains("body.truncate"));
    assert!(!production.contains("body.chars().take"));
}

#[test]
fn serialized_core_and_pro_records_do_not_disclose_firebender_database_path() {
    let physical_path = "/private/home/alice/secret-project/.idea/firebender/chat_history.db";
    let (_, source) = firebender_database_path_and_source(physical_path.as_ref()).unwrap();
    assert_eq!(
        source.provider_identity_version(),
        FIREBENDER_SOURCE_IDENTITY_REVISION
    );
    assert!(matches!(source.anchor(), SourceAnchor::CatalogLineage(_)));

    let row = FirebenderRow {
        rowid: 1,
        id: "session-1".to_owned(),
        name: "opaque lineage".to_owned(),
        created_at: 1_700_000_000_000,
        updated_at: 1_700_000_000_001,
        messages_json: "[]".to_owned(),
        metadata_json: "{}".to_owned(),
        messages: Vec::new(),
    };
    let session_id = firebender_session_id(&source, &row.id).unwrap();
    let message = json!({
        "id": "message-1",
        "role": "user",
        "content": "exact Firebender body"
    });
    let record = firebender_core_record(&source, session_id, None, &row, 0, &message)
        .unwrap()
        .unwrap();
    let core_wire = serde_json::to_string(&record).unwrap();

    let page = CoreRecordPage::new(
        "00".repeat(32),
        "11".repeat(32),
        CoreSourceState {
            source,
            source_revision_sha256: "22".repeat(32),
            event_count: 1,
        },
        0,
        0,
        true,
        vec![record],
    )
    .unwrap();
    let pro_wire = serde_json::to_string(&MaterializeCoreRecordPageRequest { page }).unwrap();
    let reversible_path_hex = physical_path
        .as_bytes()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();

    for serialized in [&core_wire, &pro_wire] {
        assert!(!serialized.contains(physical_path));
        assert!(!serialized.contains(&reversible_path_hex));
        assert!(!serialized.contains("provider-path-v1"));
    }
}
