use std::{ffi::OsString, path::PathBuf, process::ExitCode};

use ctx_history_capture::{
    ingest_codex_source_backed_v0, CodexSourceBackedCountersV0, CodexSourceBackedIngestReceiptV0,
};
use serde_json::json;

#[derive(Debug)]
struct QualificationArgs {
    source_root: PathBuf,
    index_root: PathBuf,
}

fn main() -> ExitCode {
    match run(std::env::args_os()) {
        Ok(json) => {
            println!("{json}");
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("Codex source-backed qualification failed: {error}");
            ExitCode::FAILURE
        }
    }
}

fn run(args: impl IntoIterator<Item = OsString>) -> Result<String, String> {
    let args = parse_args(args)?;
    let receipt = ingest_codex_source_backed_v0(&args.source_root, &args.index_root)
        .map_err(|error| error.to_string())?;
    qualification_json(&receipt)
}

fn qualification_json(receipt: &CodexSourceBackedIngestReceiptV0) -> Result<String, String> {
    let counters = receipt.counters;
    let legacy_operations = legacy_operation_count(counters);
    if legacy_operations != 0 {
        return Err(format!(
            "source-backed qualification observed {legacy_operations} legacy publication operations"
        ));
    }
    let changed_sources = [
        counters.cold_sources,
        counters.appended_sources,
        counters.replaced_sources,
        counters.deleted_sources,
    ]
    .into_iter()
    .fold(0_u64, u64::saturating_add);
    serde_json::to_string_pretty(&json!({
        "schema_version": 1,
        "source_epoch": "v0.26",
        "authority": "provider_sources",
        "legacy_store_fallback": false,
        "work_result": if changed_sources == 0 { "no_op" } else { "changed" },
        "input": {
            "catalog_sources": counters.catalog_sources,
            "catalog_source_bytes": counters.catalog_source_bytes,
        },
        "lifecycle": {
            "cold_sources": counters.cold_sources,
            "appended_sources": counters.appended_sources,
            "replaced_sources": counters.replaced_sources,
            "replayed_sources": counters.replayed_sources,
            "deleted_sources": counters.deleted_sources,
        },
        "scanner": {
            "workers": counters.scanner_workers,
            "staged_documents": counters.staged_documents,
            "complete_records": counters.complete_records_scanned,
            "retained_records": counters.retained_records_scanned,
            "rejected_records": counters.rejected_records_scanned,
            "ignored_records": counters.ignored_records_scanned,
            "bytes_read": counters.scanner_bytes_read,
            "legacy_operations": legacy_operations,
        },
        "generation": {
            "generation_id": receipt.commit.generation_id,
            "indexed_documents": receipt.commit.indexed_documents,
            "certified_sources": receipt.commit.certified_sources,
            "certified_source_bytes": receipt.commit.certified_source_bytes,
        },
    }))
    .map_err(|error| error.to_string())
}

fn legacy_operation_count(counters: CodexSourceBackedCountersV0) -> u64 {
    [
        counters.scanner_legacy_body_json_serializations,
        counters.scanner_legacy_row_json_serializations,
        counters.scanner_legacy_json_serialized_bytes,
        counters.scanner_legacy_normalized_payload_hashes,
        counters.scanner_legacy_file_touch_rows,
        counters.scanner_legacy_complete_content_locators,
        counters.scanner_legacy_duplicate_preview_allocations,
        counters.scanner_legacy_page_owner_json_serializations,
        counters.scanner_legacy_page_identity_owner_json_serializations,
        counters.scanner_legacy_page_identity_row_json_serializations,
    ]
    .into_iter()
    .fold(0_u64, u64::saturating_add)
}

fn parse_args(args: impl IntoIterator<Item = OsString>) -> Result<QualificationArgs, String> {
    let mut args = args.into_iter();
    let program = args
        .next()
        .unwrap_or_else(|| OsString::from("codex_nativepath_qualification"));
    let usage = || {
        format!(
            "usage: {} SOURCE_ROOT INDEX_ROOT",
            program.to_string_lossy()
        )
    };
    let source_root = args.next().map(PathBuf::from).ok_or_else(&usage)?;
    let index_root = args.next().map(PathBuf::from).ok_or_else(&usage)?;
    if args.next().is_some() {
        return Err(usage());
    }
    Ok(QualificationArgs {
        source_root,
        index_root,
    })
}

#[cfg(test)]
mod tests {
    use std::fs;

    use serde_json::Value;

    use super::*;

    fn write_changed_fixture(source_root: &std::path::Path) -> PathBuf {
        fs::create_dir_all(source_root).unwrap();
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
                        "text": "typed receipts came from the source-backed importer"
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

    fn qualify(source_root: &std::path::Path, index_root: &std::path::Path) -> Value {
        let json = run([
            OsString::from("qualify"),
            source_root.as_os_str().to_owned(),
            index_root.as_os_str().to_owned(),
        ])
        .unwrap();
        serde_json::from_str(&json).unwrap()
    }

    #[test]
    fn fresh_source_ingest_emits_deterministic_typed_runtime_counters() {
        let temp = tempfile::tempdir().unwrap();
        let source_root = temp.path().join("sessions");
        let source = write_changed_fixture(&source_root);
        let first_root = temp.path().join("first");
        let second_root = temp.path().join("second");
        fs::create_dir_all(&first_root).unwrap();
        let legacy_path = first_root.join("work.sqlite");
        let legacy_bytes = b"opaque v0.25 prior-epoch rollback sentinel\n";
        fs::write(&legacy_path, legacy_bytes).unwrap();

        let first = qualify(&source_root, &first_root.join("search/lexical"));
        let second = qualify(&source_root, &second_root.join("search/lexical"));

        assert_eq!(first, second);
        assert_eq!(first["schema_version"], 1);
        assert_eq!(first["source_epoch"], "v0.26");
        assert_eq!(first["authority"], "provider_sources");
        assert_eq!(first["legacy_store_fallback"], false);
        assert_eq!(first["work_result"], "changed");
        assert_eq!(first["input"]["catalog_sources"], 1);
        assert_eq!(
            first["input"]["catalog_source_bytes"],
            source.metadata().unwrap().len()
        );
        assert_eq!(first["lifecycle"]["cold_sources"], 1);
        assert_eq!(first["scanner"]["workers"], 1);
        assert_eq!(first["scanner"]["staged_documents"], 2);
        assert_eq!(first["scanner"]["complete_records"], 3);
        assert_eq!(first["scanner"]["retained_records"], 2);
        assert_eq!(first["scanner"]["legacy_operations"], 0);
        assert_eq!(first["generation"]["indexed_documents"], 2);
        assert_eq!(first["generation"]["certified_sources"], 1);
        assert_eq!(
            first["generation"]["certified_source_bytes"],
            source.metadata().unwrap().len()
        );
        assert_eq!(
            first["generation"]["generation_id"].as_str().unwrap().len(),
            64
        );
        assert_eq!(fs::read(&legacy_path).unwrap(), legacy_bytes);
    }

    #[test]
    fn provider_source_replacement_is_authority_and_replay_is_noop() {
        let temp = tempfile::tempdir().unwrap();
        let source_root = temp.path().join("sessions");
        let source = write_changed_fixture(&source_root);
        let index_root = temp.path().join("search/lexical");

        let initial = qualify(&source_root, &index_root);
        let initial_generation = initial["generation"]["generation_id"].clone();
        let replacement = fs::read_to_string(&source).unwrap().replace(
            "qualification changed fixture",
            "qualification revised fixture",
        );
        fs::write(&source, replacement).unwrap();

        let replaced = qualify(&source_root, &index_root);
        assert_eq!(replaced["work_result"], "changed");
        assert_eq!(replaced["lifecycle"]["replaced_sources"], 1);
        assert_ne!(replaced["generation"]["generation_id"], initial_generation);

        let replay = qualify(&source_root, &index_root);
        assert_eq!(replay["work_result"], "no_op");
        assert_eq!(replay["lifecycle"]["replayed_sources"], 1);
        assert_eq!(
            replay["generation"]["generation_id"],
            replaced["generation"]["generation_id"]
        );
        assert_eq!(replay["scanner"]["staged_documents"], 0);
        assert_eq!(replay["scanner"]["bytes_read"], 0);
    }

    #[test]
    fn empty_root_records_true_source_backed_noop() {
        let temp = tempfile::tempdir().unwrap();
        let source_root = temp.path().join("empty");
        fs::create_dir(&source_root).unwrap();

        let evidence = qualify(&source_root, &temp.path().join("search/lexical"));

        assert_eq!(evidence["work_result"], "no_op");
        assert_eq!(evidence["input"]["catalog_sources"], 0);
        assert_eq!(evidence["scanner"]["workers"], 0);
        assert_eq!(evidence["scanner"]["staged_documents"], 0);
        assert_eq!(evidence["scanner"]["legacy_operations"], 0);
        assert_eq!(evidence["generation"]["indexed_documents"], 0);
        assert_eq!(evidence["generation"]["certified_sources"], 0);
    }

    #[test]
    fn caller_supplied_counter_document_is_rejected_at_process_boundary() {
        let forged = r#"{"scanner":{"workers":999}}"#;
        let result = parse_args([
            OsString::from("qualify"),
            OsString::from("/sessions"),
            OsString::from("/index"),
            OsString::from(forged),
        ]);

        assert!(result.is_err());
        assert_eq!(result.unwrap_err(), "usage: qualify SOURCE_ROOT INDEX_ROOT");
    }
}
