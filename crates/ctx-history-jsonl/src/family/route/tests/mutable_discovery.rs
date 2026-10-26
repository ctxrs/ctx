use super::*;
use crate::family::route::capture::classify_incomplete_first_records;
use crate::family::{
    probe_records_until_with_mutable_first_record, JsonlMutableFirstRecord, JsonlReader,
};
use std::{
    fs,
    io::Write,
    time::{Duration, SystemTime},
};

fn overwrite_title(path: &Path, title: &str, change_header: bool) {
    let mut value = serde_json::json!({
        "type": "title", "v": 1, "title": title, "pad": "",
        "updatedAt": "2026-10-17T00:00:00.000Z",
    });
    let padding = 255 - serde_json::to_vec(&value).unwrap().len();
    value["pad"] = serde_json::json!(" ".repeat(padding));
    let mut bytes = serde_json::to_vec(&value).unwrap();
    bytes.push(b'\n');
    assert_eq!(bytes.len(), 256);
    let mut file = fs::OpenOptions::new().write(true).open(path).unwrap();
    file.write_all(&bytes).unwrap();
    if change_header {
        file.write_all(b"{\"type\":\"session\",\"id\":\"modified\"}\n")
            .unwrap();
    }
    file.sync_all().unwrap();
    file.set_modified(SystemTime::now() + Duration::from_secs(5))
        .unwrap();
}

#[test]
fn title_changes_between_discovery_classification_and_scan_preserve_exact_header() {
    for change_header in [false, true] {
        let temp = crate::test_support_paths::tempdir().unwrap();
        let path = temp.path().join("session.jsonl");
        let mut bytes = vec![b' '; 256];
        bytes.extend_from_slice(
            b"{\"type\":\"session\",\"id\":\"original\"}\n{\"message\":\"original body\"}\n",
        );
        fs::write(&path, bytes).unwrap();
        overwrite_title(&path, "initial title", false);
        let discovered = TestAdapter.discover(temp.path()).unwrap();
        let member = discovered.accepted_leaves().next().unwrap();
        let opened = Arc::new(member.authority.open_file(&member.authority_path).unwrap());
        let (_, probe) = probe_records_until_with_mutable_first_record(
            &path,
            &opened,
            2,
            Some(JsonlMutableFirstRecord::OmpTitleSlotV1),
            |record| {
                Ok::<_, ctx_history_source_io::SourceIoError>(
                    (record.evidence().physical_ordinal() == 1).then_some(()),
                )
            },
        )
        .unwrap()
        .unwrap();
        let leaf = JsonlFamilyLeaf::observe_after_identity_probe(
            member.source.clone(),
            path.clone(),
            Arc::clone(&member.authority),
            member.authority_path.clone(),
            member.binding.clone(),
            probe,
            0,
        )
        .unwrap();
        let mut opening = JsonlFamilyInventory::present(
            CaptureProvider::Pi,
            temp.path(),
            Arc::clone(&member.authority),
            vec![leaf],
        )
        .unwrap();

        overwrite_title(&path, "before classification", change_header);
        classify_incomplete_first_records(&TestAdapter, &mut opening).unwrap();
        overwrite_title(&path, "before scan", false);
        let scanned = opening.accepted_leaves().next().unwrap().open_for_scan();
        if change_header {
            assert!(matches!(
                scanned,
                Err(ctx_history_source_io::SourceIoError::SourceChangedDuringCapture)
            ));
        } else {
            let (leaf, source_file) = scanned.unwrap();
            let mut reader = JsonlReader::open(
                physical_identity(&TestAdapter, &leaf),
                source_file,
                None,
                leaf.identity_probe.clone(),
            )
            .unwrap();
            let mut records = Vec::new();
            while reader
                .visit_page(&mut |record| {
                    records.push(record.bytes().to_vec());
                    Ok::<_, ctx_history_source_io::SourceIoError>(())
                })
                .unwrap()
                .is_some()
            {}
            assert_eq!(records, vec![br#"{"message":"original body"}"#.to_vec()]);
        }
    }
}
