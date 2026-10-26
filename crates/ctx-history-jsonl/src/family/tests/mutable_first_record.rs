use super::*;
use std::io::{Seek, SeekFrom};

fn title_record(title: &str) -> Vec<u8> {
    let mut value = serde_json::json!({
        "type": "title", "v": 1, "title": title, "pad": "",
        "source": "auto", "updatedAt": "2026-10-17T00:00:00.000Z",
    });
    let padding = 255 - serde_json::to_vec(&value).unwrap().len();
    value["pad"] = serde_json::json!(" ".repeat(padding));
    let mut bytes = serde_json::to_vec(&value).unwrap();
    bytes.push(b'\n');
    bytes
}

#[test]
fn a_stale_identity_probe_accepts_only_the_title_update() {
    for change_header in [false, true] {
        let temp = crate::test_support_paths::tempdir().unwrap();
        let path = temp.path().join("session.jsonl");
        let mut bytes = title_record("initial title");
        bytes.extend_from_slice(
            b"{\"type\":\"session\",\"id\":\"original\"}\n{\"message\":\"original body\"}\n",
        );
        fs::write(&path, &bytes).unwrap();
        let source_file = Arc::new(open_provider_source_file(&path).unwrap());
        let (_, probe) = probe_records_until_with_mutable_first_record(
            &path,
            &source_file,
            2,
            Some(JsonlMutableFirstRecord::OmpTitleSlotV1),
            |record| -> Result<Option<()>> {
                Ok((record.evidence().physical_ordinal() == 1).then_some(()))
            },
        )
        .unwrap()
        .unwrap();
        let mut file = OpenOptions::new().write(true).open(&path).unwrap();
        file.write_all(&title_record("updated title")).unwrap();
        if change_header {
            file.seek(SeekFrom::Start(256)).unwrap();
            file.write_all(b"{\"type\":\"session\",\"id\":\"modified\"}\n")
                .unwrap();
        }
        file.sync_all().unwrap();
        let reader = JsonlReader::open(
            semantic_identity(&path, "mutable-title-probe-v1"),
            source_file,
            None,
            Some(probe),
        );
        if change_header {
            assert!(matches!(
                reader,
                Err(CaptureError::SourceChangedDuringCapture)
            ));
        } else {
            assert_eq!(
                drain(&mut reader.unwrap()).unwrap(),
                vec![br#"{"message":"original body"}"#.to_vec()],
            );
        }
    }
}
