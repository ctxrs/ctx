use super::*;

#[test]
fn astrbot_selected_conversation_scan_is_paged_and_capped() {
    let conn = Connection::open_in_memory().unwrap();
    create_tables(&conn);
    let irrelevant = ASTRBOT_PREFERENCE_SCAN_MAX_SOURCE_ROWS_PER_PAGE * 2;
    conn.execute_batch("begin").unwrap();
    for index in 0..irrelevant {
        conn.execute(
            "insert into preferences (key, value, scope) values (?1, 'ignored', 'umo')",
            [format!("irrelevant-{index:04}")],
        )
        .unwrap();
    }
    conn.execute(
        "insert into preferences (key, value, scope) \
         values ('sel_conv_id', '{\"val\":\"session-selected\"}', 'umo')",
        [],
    )
    .unwrap();
    conn.execute_batch("commit").unwrap();

    astrbot_reset_preference_scan_test_pacing();
    assert_eq!(
        astrbot_selected_conversation_bounded(&conn).unwrap(),
        Some("session-selected".to_owned())
    );
    let pacing = astrbot_preference_scan_test_pacing();
    assert_eq!(pacing.pages, 3);
    assert!(pacing.max_source_rows <= ASTRBOT_PREFERENCE_SCAN_MAX_SOURCE_ROWS_PER_PAGE);
    assert_eq!(astrbot_preference_scan_test_wait_count(), pacing.pages);
    astrbot_disable_preference_scan_test_wait_hook();
}

#[test]
fn astrbot_selected_conversation_preflight_restores_cap_before_oversize_value() {
    let conn = Connection::open_in_memory().unwrap();
    create_tables(&conn);
    let oversize = i64::try_from(CAPTURE_BATCH_MAX_OVERSIZE_RECORD_BYTES)
        .unwrap()
        .checked_add(1)
        .unwrap();
    conn.execute(
        "insert into preferences (key, value, scope) \
         values ('sel_conv_id', zeroblob(?1), 'umo')",
        [oversize],
    )
    .unwrap();
    let capped_length = 1_024 * 1_024;
    conn.set_limit(Limit::SQLITE_LIMIT_LENGTH, capped_length);

    astrbot_reset_preference_scan_test_pacing();
    let error = astrbot_selected_conversation_bounded(&conn).unwrap_err();
    astrbot_disable_preference_scan_test_wait_hook();
    assert!(error
        .to_string()
        .contains("selected-conversation preference exceeds"));
    assert_eq!(conn.limit(Limit::SQLITE_LIMIT_LENGTH), capped_length);
}

#[test]
fn astrbot_selected_conversation_fails_closed_without_native_rowid() {
    let conn = Connection::open_in_memory().unwrap();
    conn.execute_batch(
        "create table preferences ( \
             key text primary key, value text, scope text \
         ) without rowid;",
    )
    .unwrap();
    let error = astrbot_selected_conversation_bounded(&conn).unwrap_err();
    assert!(error
        .to_string()
        .contains("requires a native rowid frontier"));
}
