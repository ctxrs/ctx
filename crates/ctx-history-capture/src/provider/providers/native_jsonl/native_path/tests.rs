use std::fs;

use chrono::DateTime;
use ctx_history_core::CaptureProvider;

use super::open_direct_jsonl_pages;
use crate::test_support_paths::tempdir;

#[test]
fn direct_jsonl_nativepath_withholds_incomplete_tail() {
    let temp = tempdir().unwrap();
    let path = temp.path().join("transcript").join("events.jsonl");
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(
        &path,
        concat!(
            "{\"type\":\"session.start\",\"data\":{\"sessionId\":\"copilot-session\"}}\n",
            "{\"type\":\"assistant.message\",\"data\":{\"content\":\"complete\"}}\n",
            "{\"type\":\"assistant.message\",\"data\":{\"content\":\"incomplete\"}}"
        ),
    )
    .unwrap();
    let mut reader = open_direct_jsonl_pages(
        CaptureProvider::CopilotCli,
        crate::COPILOT_CLI_SOURCE_FORMAT,
        &path,
        Some(temp.path().to_path_buf()),
        DateTime::from_timestamp(0, 0).unwrap(),
        false,
        None,
    )
    .unwrap();
    let page = reader.next_page().unwrap().unwrap();
    assert_eq!(page.events.len(), 2);
    assert!(!page.terminal);
    assert!(reader.next_page().unwrap().is_none());
    assert!(!reader.outcome().unwrap().checkpoint.terminal);
    assert_eq!(reader.outcome().unwrap().checkpoint.next_raw_ordinal, 2);
}
