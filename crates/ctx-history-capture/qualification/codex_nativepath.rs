use std::{ffi::OsString, path::PathBuf, process::ExitCode};

use chrono::{DateTime, Utc};
use ctx_history_capture::{qualify_codex_native_session_root, CodexSessionImportOptions};
use ctx_history_store::Store;

#[derive(Debug)]
struct QualificationArgs {
    source_root: PathBuf,
    store_path: PathBuf,
    machine_id: String,
    imported_at: DateTime<Utc>,
}

fn main() -> ExitCode {
    match run(std::env::args_os()) {
        Ok(json) => {
            println!("{json}");
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("Codex NativePath qualification failed: {error}");
            ExitCode::FAILURE
        }
    }
}

fn run(args: impl IntoIterator<Item = OsString>) -> Result<String, String> {
    let args = parse_args(args)?;
    let mut store = Store::open(&args.store_path).map_err(|error| error.to_string())?;
    let evidence = qualify_codex_native_session_root(
        &args.source_root,
        &mut store,
        CodexSessionImportOptions {
            machine_id: args.machine_id,
            source_path: Some(args.source_root.clone()),
            imported_at: args.imported_at,
            ..CodexSessionImportOptions::default()
        },
    )
    .map_err(|error| error.to_string())?;
    evidence.to_json().map_err(|error| error.to_string())
}

fn parse_args(args: impl IntoIterator<Item = OsString>) -> Result<QualificationArgs, String> {
    let mut args = args.into_iter();
    let program = args
        .next()
        .unwrap_or_else(|| OsString::from("codex_nativepath_qualification"));
    let usage = || {
        format!(
            "usage: {} SOURCE_ROOT STORE_PATH MACHINE_ID IMPORTED_AT_RFC3339",
            program.to_string_lossy()
        )
    };
    let source_root = args.next().map(PathBuf::from).ok_or_else(&usage)?;
    let store_path = args.next().map(PathBuf::from).ok_or_else(&usage)?;
    let machine_id = args
        .next()
        .and_then(|value| value.into_string().ok())
        .filter(|value| !value.is_empty())
        .ok_or_else(&usage)?;
    let imported_at = args
        .next()
        .and_then(|value| value.into_string().ok())
        .ok_or_else(&usage)?
        .parse::<DateTime<Utc>>()
        .map_err(|_| usage())?;
    if args.next().is_some() {
        return Err(usage());
    }
    Ok(QualificationArgs {
        source_root,
        store_path,
        machine_id,
        imported_at,
    })
}

#[cfg(test)]
mod tests {
    use std::fs;

    use serde_json::Value;

    use super::*;

    fn fixture_options(source_root: &std::path::Path) -> CodexSessionImportOptions {
        CodexSessionImportOptions {
            machine_id: "codex-nativepath-qualification-test".to_owned(),
            source_path: Some(source_root.to_path_buf()),
            imported_at: "2026-07-26T12:00:00Z".parse().unwrap(),
            ..CodexSessionImportOptions::default()
        }
    }

    fn write_changed_fixture(source_root: &std::path::Path) -> PathBuf {
        fs::create_dir(source_root).unwrap();
        let source = source_root
            .join("rollout-2026-07-26T12-00-00-00000000-0000-0000-0000-000000000026.jsonl");
        let records = [
            serde_json::json!({
                "timestamp": "2026-07-26T12:00:00Z",
                "type": "session_meta",
                "payload": {
                    "id": "00000000-0000-0000-0000-000000000026",
                    "timestamp": "2026-07-26T12:00:00Z",
                    "cwd": "/workspace",
                    "source": "cli"
                }
            }),
            serde_json::json!({
                "timestamp": "2026-07-26T12:00:01Z",
                "type": "response_item",
                "payload": {
                    "type": "message",
                    "role": "user",
                    "content": [{
                        "type": "input_text",
                        "text": "qualification changed fixture"
                    }]
                }
            }),
            serde_json::json!({
                "timestamp": "2026-07-26T12:00:02Z",
                "type": "response_item",
                "payload": {
                    "type": "message",
                    "role": "assistant",
                    "content": [{
                        "type": "output_text",
                        "text": "typed receipts came from the normal importer"
                    }]
                }
            }),
        ];
        let contents = records
            .iter()
            .map(serde_json::Value::to_string)
            .collect::<Vec<_>>()
            .join("\n")
            + "\n";
        fs::write(&source, contents).unwrap();
        source
    }

    fn qualify(
        source_root: &std::path::Path,
        store_path: &std::path::Path,
    ) -> ctx_history_capture::CodexNativePathQualificationEvidence {
        let mut store = Store::open(store_path).unwrap();
        qualify_codex_native_session_root(source_root, &mut store, fixture_options(source_root))
            .unwrap()
    }

    #[test]
    fn changed_import_emits_deterministic_typed_runtime_counters() {
        let temp = tempfile::tempdir().unwrap();
        let source_root = temp.path().join("sessions");
        let source = write_changed_fixture(&source_root);

        let first = qualify(&source_root, &temp.path().join("first.sqlite"));
        let second = qualify(&source_root, &temp.path().join("second.sqlite"));

        assert_eq!(first, second);
        assert_eq!(first.summary().imported_sessions, 1);
        assert_eq!(first.summary().imported_events, 2);
        assert_eq!(first.input().catalog_sources(), 1);
        assert_eq!(
            first.input().catalog_bytes(),
            source.metadata().unwrap().len()
        );
        assert_eq!(first.input().observation_sha256().len(), 64);
        assert_eq!(first.producer().worker_count(), 1);
        assert_eq!(first.producer().peak_overlap(), 1);
        assert!(first.producer().peak_preparation_bytes() > 0);
        assert!(first.store().groups() > 0);
        assert!(first.store().mutation_units() > 0);
        assert!(first.store().core_bound_bytes() > 0);
        assert!(first.store().journal_records() > 0);
        assert!(first.store().journal_bytes() > 0);
        assert_eq!(first.store().checkpoint_receipts(), first.store().groups());
        assert!(first.store().checkpoint_advances() > 0);
        assert!(first.store().first_checkpoint().is_some());
        assert!(first.store().last_checkpoint().is_some());

        let json = first.to_json().unwrap();
        let value: Value = serde_json::from_str(&json).unwrap();
        assert_eq!(value["schema_version"], 1);
        assert_eq!(value["work_result"], "changed");
        assert_eq!(value["producer"]["blocked_reservations"], 0);
        assert_eq!(value["build"]["source_commit"].as_str().unwrap().len(), 40);
        assert_eq!(
            value["build"]["cargo_lock_sha256"].as_str().unwrap().len(),
            64
        );
        assert_eq!(
            value["build"]["importer_source_sha256"]
                .as_str()
                .unwrap()
                .len(),
            64
        );
    }

    #[test]
    fn empty_root_records_true_noop_as_zero_producer_and_store_work() {
        let temp = tempfile::tempdir().unwrap();
        let source_root = temp.path().join("empty");
        fs::create_dir(&source_root).unwrap();

        let evidence = qualify(&source_root, &temp.path().join("noop.sqlite"));

        assert_eq!(evidence.summary().imported, 0);
        assert_eq!(evidence.summary().skipped, 0);
        assert_eq!(evidence.producer().worker_count(), 0);
        assert_eq!(evidence.producer().peak_overlap(), 0);
        assert_eq!(evidence.producer().peak_preparation_bytes(), 0);
        assert_eq!(evidence.producer().blocked_reservations(), 0);
        assert_eq!(evidence.store().groups(), 0);
        assert_eq!(evidence.store().mutation_units(), 0);
        assert_eq!(evidence.store().core_bound_bytes(), 0);
        assert_eq!(evidence.store().journal_records(), 0);
        assert_eq!(evidence.store().journal_bytes(), 0);
        assert_eq!(evidence.store().checkpoint_receipts(), 0);
        assert_eq!(evidence.store().checkpoint_advances(), 0);
        assert!(evidence.store().first_checkpoint().is_none());
        assert!(evidence.store().last_checkpoint().is_none());

        let value: Value = serde_json::from_str(&evidence.to_json().unwrap()).unwrap();
        assert_eq!(value["work_result"], "no_op");
        assert_eq!(value["input"]["catalog_sources"], 0);
    }

    #[test]
    fn caller_supplied_counter_document_is_rejected_at_process_boundary() {
        let forged = r#"{"producer":{"worker_count":999},"store":{"groups":999}}"#;
        let result = parse_args([
            OsString::from("qualify"),
            OsString::from("/sessions"),
            OsString::from("/store.sqlite"),
            OsString::from("machine"),
            OsString::from("2026-07-26T12:00:00Z"),
            OsString::from(forged),
        ]);

        assert!(result.is_err());
        assert!(result.unwrap_err().starts_with("usage: qualify "));
    }
}
