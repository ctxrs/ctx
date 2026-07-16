mod tests {
    use std::{
        fs::{self, OpenOptions},
        io::Write,
        path::Path,
    };

    use serde_json::json;
    use tempfile::tempdir;

    use super::*;
    use crate::provider::importer::{
        provider_normalization_has_real_message, PROVIDER_NORMALIZATION_STREAM_BATCH_UNITS,
    };
    use crate::{open_provider_jsonl, CodexSessionJsonlAdapter, ProviderCaptureAdapter};

    fn options(
        source_path: &Path,
        inventory_source_format: &str,
        material_source_format: &str,
        mode: ProviderAppendFileImportMode,
    ) -> ProviderAppendFileImportOptions {
        ProviderAppendFileImportOptions {
            machine_id: "append-test-machine".to_owned(),
            inventory_source_format: inventory_source_format.to_owned(),
            material_source_format: material_source_format.to_owned(),
            source_path: source_path.to_path_buf(),
            source_root: source_path.parent().unwrap().to_path_buf(),
            imported_at: "2026-07-14T12:00:00Z".parse().unwrap(),
            history_record_id: None,
            mode,
        }
    }

    fn adapter_context(source_path: &Path) -> ProviderAdapterContext {
        ProviderAdapterContext {
            machine_id: "append-test-machine".to_owned(),
            source_path: Some(source_path.to_path_buf()),
            source_root: source_path.parent().map(Path::to_path_buf),
            imported_at: "2026-07-14T12:00:00Z".parse().unwrap(),
        }
    }

    fn write_raw(path: &Path, contents: &str) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, contents).unwrap();
    }

    fn append_raw(path: &Path, contents: &str) {
        OpenOptions::new()
            .append(true)
            .open(path)
            .unwrap()
            .write_all(contents.as_bytes())
            .unwrap();
    }

    fn jsonl(value: Value) -> String {
        format!("{}\n", serde_json::to_string(&value).unwrap())
    }

    fn imported(decision: ProviderAppendFileImportDecision) -> ProviderAppendFileImportResult {
        match decision {
            ProviderAppendFileImportDecision::Imported(result) => result,
            other => panic!("expected imported decision, got {other:?}"),
        }
    }

    fn admitted(checkpoint: ProviderJsonlAppendCheckpoint) -> ProviderAppendFileImportMode {
        ProviderAppendFileImportMode::Append(
            ProviderAdmittedJsonlAppendCheckpoint::from_persisted_admitted_replacement(checkpoint),
        )
    }

    fn codex_header(id: &str) -> Value {
        json!({
            "timestamp": "2026-07-14T12:00:00Z",
            "type": "session_meta",
            "payload": {
                "id": id,
                "timestamp": "2026-07-14T12:00:00Z",
                "cwd": "/workspace"
            }
        })
    }

    fn codex_message(role: &str, text: &str, second: u32) -> Value {
        json!({
            "timestamp": format!("2026-07-14T12:00:{second:02}Z"),
            "type": "response_item",
            "payload": {
                "type": "message",
                "role": role,
                "content": [{
                    "type": if role == "user" { "input_text" } else { "output_text" },
                    "text": text
                }]
            }
        })
    }

    fn codex_call(call_id: &str, second: u32) -> Value {
        json!({
            "timestamp": format!("2026-07-14T12:00:{second:02}Z"),
            "type": "response_item",
            "payload": {
                "type": "function_call",
                "name": "exec_command",
                "arguments": "{\"cmd\":\"cargo test\"}",
                "call_id": call_id
            }
        })
    }

    fn codex_output(call_id: &str, output: &str, second: u32) -> Value {
        json!({
            "timestamp": format!("2026-07-14T12:00:{second:02}Z"),
            "type": "response_item",
            "payload": {
                "type": "function_call_output",
                "call_id": call_id,
                "output": output
            }
        })
    }

    include!("tests/cases_a.rs");
    include!("tests/cases_b.rs");
    include!("tests/cases_c.rs");
}
