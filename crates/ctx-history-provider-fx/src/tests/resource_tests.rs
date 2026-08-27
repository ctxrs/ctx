use std::{fs, sync::Arc};

#[test]
fn inventory_retains_one_root_authority_for_many_sessions() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("sessions");
    for index in 0..64 {
        let session = format!("session-{index}");
        let directory = root.join(&session);
        fs::create_dir_all(&directory).unwrap();
        fs::write(
            directory.join("session.json"),
            format!(
                r#"{{"schema_version":2,"id":"{session}","created_at_ms":1,"updated_at_ms":1,"workspace_root":"/fixture","conversation_language":"en","history_len":0,"history":[]}}"#,
            ),
        )
        .unwrap();
    }
    let inventory = crate::source_backed::test_inventory(&root).unwrap();
    let authority = inventory.accepted_leaves().next().unwrap().authority();
    assert_eq!(inventory.accepted_len(), 64);
    assert!(inventory
        .accepted_leaves()
        .all(|leaf| Arc::ptr_eq(authority, leaf.authority())));
}
